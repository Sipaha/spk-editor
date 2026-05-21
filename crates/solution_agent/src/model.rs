use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use acp_thread::AcpThread;
use agent_client_protocol::schema as acp;
use chrono::{DateTime, Utc};
use gpui::{Context, Entity, EventEmitter, SharedString, Subscription};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use solutions::SolutionId;

/// Length of a `SolutionSessionId` in ASCII characters. 8 chars over a
/// 36-char alphabet ≈ 36⁸ ≈ 2.8 × 10¹² combinations — comfortably
/// collision-free for the realistic upper bound of a few thousand
/// sessions per user, while staying short enough to read at a glance
/// and to use as a filesystem path component (`<root>/.agents/<id>/`).
const SHORT_ID_LEN: usize = 8;

/// Alphabet for fresh session ids: lowercase ASCII + digits. Avoids
/// shell-special chars, mixed case, and `_-` so the id is safe in
/// filesystem paths, JSON-RPC frames, and shell commands without
/// quoting.
const SHORT_ID_ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// SPK-Editor-internal session id. Distinct from `acp::SessionId`,
/// which is the per-subprocess ACP-level identifier.
///
/// Stored as a fixed-size ASCII array so the type stays `Copy` (lots
/// of HashMap-key usage), but rendered as a string everywhere it
/// crosses the I/O boundary (DB rows, MCP JSON, file paths). Old
/// 36-char UUID strings persisted by earlier builds are still
/// parseable: see `parse` for the legacy migration.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SolutionSessionId([u8; SHORT_ID_LEN]);

impl SolutionSessionId {
    pub fn new() -> Self {
        // Rejection-sample so the alphabet stays uniform: `% 36` on a
        // single u8 would over-represent the first 4 letters (256 mod
        // 36 = 4). Sampling 1 byte at a time and dropping anything ≥
        // 252 (= 7 × 36) gives a flat distribution.
        let mut bytes = [0u8; SHORT_ID_LEN];
        let mut rng = rand::rng();
        let mut buf = [0u8; 1];
        for slot in &mut bytes {
            loop {
                rng.fill_bytes(&mut buf);
                let x = buf[0];
                if (x as usize) < 252 {
                    *slot = SHORT_ID_ALPHABET[(x as usize) % 36];
                    break;
                }
            }
        }
        Self(bytes)
    }

    /// Accepts:
    ///   1. The current `[a-z0-9]{8}` form (fresh ids written by `new`).
    ///   2. A legacy hyphenated UUID (any version) persisted by older
    ///      builds — collapsed to the first 8 hex chars of its u128
    ///      representation. Lossy in theory (8 hex chars = 32 bits)
    ///      but every UUID prefix is uniquely identifying inside one
    ///      user's DB, and we'd rather migrate in-place than wipe the
    ///      session history.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        if s.len() == SHORT_ID_LEN && s.bytes().all(|b| SHORT_ID_ALPHABET.contains(&b)) {
            let mut bytes = [0u8; SHORT_ID_LEN];
            bytes.copy_from_slice(s.as_bytes());
            return Ok(Self(bytes));
        }
        if let Ok(uuid) = uuid::Uuid::parse_str(s) {
            // `:032x` pads to 32 hex chars regardless of leading-zero
            // UUIDs so the `take` below always finds 8 characters.
            let hex = format!("{:032x}", uuid.as_u128());
            let mut bytes = [0u8; SHORT_ID_LEN];
            for (i, slot) in bytes.iter_mut().enumerate() {
                *slot = hex.as_bytes()[i];
            }
            return Ok(Self(bytes));
        }
        anyhow::bail!("invalid session id: {s:?}")
    }

    pub fn as_str(&self) -> &str {
        // Bytes are constructed only from ASCII alphabet entries (in
        // `new`) or copied from an already-validated string (in
        // `parse`), so the array is always valid UTF-8.
        std::str::from_utf8(&self.0).expect("invariant: short id is ASCII")
    }
}

impl std::fmt::Display for SolutionSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Debug for SolutionSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SolutionSessionId({})", self.as_str())
    }
}

impl Serialize for SolutionSessionId {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SolutionSessionId {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Identifier of a registered `AgentServer` (e.g. `claude-acp`, `codex`).
/// Mirrors `acp_thread::AgentId` / `agent_servers` naming for transparent passing.
pub type AgentServerId = SharedString;

#[derive(Clone, Debug)]
pub enum SessionState {
    Idle,
    Running { started_at: Instant, notified: bool },
    AwaitingInput,
    Errored(SharedString),
}

impl SessionState {
    pub fn is_terminal_for_notification(&self) -> bool {
        matches!(self, Self::Idle | Self::AwaitingInput | Self::Errored(_))
    }

    /// Short, user-facing status label. Use this in the UI instead of
    /// `format!("{:?}", state)` — Debug renders the `Running` variant as
    /// `Running { started_at: Instant { tv_sec: 148873, tv_nsec: ... } }`,
    /// which is what we previously leaked into the session-view header.
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Running { .. } => "Running",
            Self::AwaitingInput => "Awaiting input",
            Self::Errored(_) => "Error",
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.short_label())
    }
}

/// 1-based counter of how many context-windows this session has
/// chewed through. Starts at 1 and increments each time the user
/// compacts: the dump dir for the *current* context is named after
/// this number (`.agents/<sid>/c01/`, `c02/`, …) so all rotations of
/// one logical conversation share a single `<sid>` directory.
pub type SessionContextCount = u32;

/// One conversation entry as persisted in the `solution_sessions.acp_thread_blob`
/// payload. Used by the navigator's cold-tab renderer to display the
/// dialog before the agent subprocess is spawned (and by MCP tools that
/// archive transcripts). `markdown` is what `acp_thread::AgentThreadEntry::to_markdown`
/// produced at save time; `role` is the only extra info the cold
/// renderer needs to apply user/assistant/tool styling.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedEntry {
    pub role: PersistedRole,
    pub markdown: String,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PersistedRole {
    User,
    Assistant,
    /// Tool call + result. Rendered as a collapsed block in cold mode.
    Tool,
    /// Agent-emitted plan (`AgentThreadEntry::CompletedPlan`). Rendered
    /// like an assistant message but tagged so future styling tweaks
    /// can distinguish.
    Plan,
    /// Legacy blob entry where the per-entry role wasn't recorded —
    /// `entry_summaries` is the only data we have, and an idx-based
    /// User/Assistant guess mis-roles every conversation that includes
    /// tool calls. The cold renderer paints these as a neutral block
    /// so a misleading role label doesn't smear across the dialog.
    Archived,
}

/// Sentinel stored in `SolutionSession::entry_created_ms` (and the persisted
/// mirror) for an entry whose creation time was never captured — e.g. a
/// message that predates the timestamp feature, surfaced through a resumed
/// pre-feature session. Real unix-millis timestamps are always positive, so a
/// negative marker is unambiguous. The wire layer maps this to "no time"
/// rather than fabricating one.
pub(crate) const NO_TIMESTAMP_MS: i64 = -1;

/// Live, in-memory representation of one Solution-scoped AI session.
///
/// `acp_thread` is `Option` because a `SolutionSession` may exist briefly
/// without a constructed `AcpThread` (e.g. during test scaffolding before
/// Task 3.3 wires the real subprocess pool, or in the moment between
/// session row creation and ACP `new_session` resolving). Production callers
/// added in Task 3.3 will populate it before exposing the session to the UI.
pub struct SolutionSession {
    pub id: SolutionSessionId,
    pub solution_id: SolutionId,
    pub agent_id: AgentServerId,
    pub acp_session_id: acp::SessionId,
    /// Live thread, when one is attached. **Private**: any write must
    /// go through [`SolutionSession::set_acp_thread`] so subscribers
    /// (notably `SolutionSessionView::_thread_subscription`) re-attach.
    /// Read with [`SolutionSession::acp_thread`].
    acp_thread: Option<Entity<AcpThread>>,
    pub title: SharedString,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub state: SessionState,
    /// Working directory the session was originally created against.
    /// claude-acp buckets sessions by encoded cwd under
    /// `~/.claude/projects/<encoded-cwd>/<acp-session-id>.jsonl`, so
    /// resume must replay the SAME cwd or the agent returns
    /// `Resource not found`. Empty `PathBuf` means "fall back to
    /// `solution.root`" — used for legacy DB rows that pre-date the
    /// column.
    pub cwd: PathBuf,
    /// 1-based count of how many context-windows this session has
    /// burned through. `1` for a fresh session, `2` after one
    /// compact, etc. The compact dump dir for the *current* context
    /// is named `c<context_count>` so a single `.agents/<sid>/`
    /// directory groups every rotation of one logical conversation.
    pub context_count: SessionContextCount,
    /// Project the session was created against. Cached so `restart_agent`
    /// can re-issue `create_session` without the caller having to reach
    /// back into a workspace window. `None` for prebuilt-session test
    /// scaffolding that never went through `create_session`.
    pub project: Option<Entity<project::Project>>,
    /// Subscription to the `AcpThread`'s `AcpThreadEvent` stream. Held so
    /// the callback registered by `SolutionAgentStore::subscribe_to_session`
    /// stays alive for the lifetime of the session. Underscore-prefixed
    /// because nothing reads it back; dropping it implicitly unsubscribes.
    pub _acp_subscription: Option<Subscription>,
    /// FIFO queue of user messages submitted while the session was
    /// already running. The store flushes the queue on `Stopped` —
    /// matches the Claude Code CLI experience where you can keep
    /// typing follow-ups while the agent is still working.
    pub pending_messages: VecDeque<Vec<acp::ContentBlock>>,
    /// One-shot signal set by `interrupt_and_flush_pending`: tells the
    /// next `Stopped(Cancelled)` handler to FLUSH `pending_messages`
    /// instead of clearing them. Without it, `Cancelled` (the user
    /// pressed Stop) drops the queue — which is the right default,
    /// but the "Send now" button needs the inverse behaviour.
    pub flush_after_cancel: bool,
    /// Reconstructed `AgentThreadEntry` list loaded from
    /// `solution_sessions.acp_thread_blob` at panel-open time.
    /// Populated only for sessions restored by
    /// `SolutionAgentStore::restore_open_tabs` — i.e. tabs whose
    /// `acp_thread` is `None` because we deferred the agent subprocess
    /// until the user actually sends a message. The session view feeds
    /// these directly into the same virtualized list path as live mode
    /// (each carries fresh `Markdown` widgets recreated by
    /// `cold_persistence::from_persisted`), so the cold conversation
    /// paints identically to its live form. Cleared once
    /// `resume_session` attaches a real `AcpThread`.
    pub cold_entries: Vec<acp_thread::AgentThreadEntry>,
    /// Unix-millis creation time per thread entry, index-aligned with the
    /// live thread's absolute entry indices (and with `cold_entries` when
    /// the session is cold). Stamped once at first append — never restamped
    /// on in-place streaming updates. Truncated in lockstep with the
    /// entries on rewind/reset so index alignment holds. A length shorter
    /// than the entries means "trailing entries are absent". An absent entry
    /// at any index holds [`NO_TIMESTAMP_MS`] — e.g. entries that predate this
    /// feature, surfaced through a resumed pre-feature session whose history
    /// gap is filled with the sentinel so the vector stays 1:1 with the
    /// entries. Callers surface absent entries as no timestamp rather than
    /// fabricating one.
    pub entry_created_ms: Vec<i64>,
    /// Wall-clock duration of the most recently completed turn (set on
    /// `Running → Idle`). The status row reads this to render
    /// "Done in 2m15s" instead of a bare "Idle" so the user has an
    /// explicit signal that the agent finished — the desktop-notification
    /// path only fires when the panel is unfocused, leaving an
    /// in-foreground user with only "Thinking…" disappearing as the cue.
    /// Cleared the moment a new turn starts (state flips back to Running).
    /// Not persisted across restarts — purely a foreground-UX hint.
    pub last_turn_duration: Option<std::time::Duration>,
    /// Last-known total token count for the conversation, used by the
    /// status-row meter to keep showing "X / Y · Z%" on a cold tab
    /// (no live `AcpThread` → no `TokenUsage` to read). Populated on
    /// `restore_open_tabs` from the persisted metadata, refreshed
    /// whenever the live thread emits a `TokenUsageUpdated` event so
    /// the cached value stays in sync until the next cold restore.
    /// `None` for fresh sessions whose first turn hasn't shipped yet.
    pub cached_total_tokens: Option<u64>,
    /// Last-known max context window for the conversation. Sibling to
    /// `cached_total_tokens` — populated on every live `TokenUsageUpdated`
    /// event so MCP consumers (the phone client's context-fill meter)
    /// can read a model-specific limit even when the live `AcpThread`
    /// hasn't shipped a token usage yet. NOT persisted to disk: the
    /// agent re-emits `TokenUsageUpdated` (with `max_tokens`) on the
    /// first turn of any cold-resumed session, so the cache rebuilds
    /// itself naturally. `None` when no live event has been observed
    /// since the session entity was hydrated.
    pub cached_max_tokens: Option<u64>,
    /// F: parent session reference for sub-agent indication. `None` for
    /// top-level sessions. Set at creation time via
    /// `solution_agent.create_session({parent_session_id})` and
    /// persisted in `solution_sessions.parent_session_id` so the
    /// parent / child link survives editor restarts. The session view's
    /// sub-agents strip uses this to render a child under its parent
    /// (and to navigate up from a child to its parent).
    pub parent_session_id: Option<SolutionSessionId>,
}

impl SolutionSession {
    /// Fresh, idle session with no live `AcpThread` attached. All
    /// optional state (`title`, `cwd`, `cold_entries`, …) defaults to
    /// "empty"; callers should poke the relevant `pub` fields after
    /// construction. Use [`set_acp_thread`](Self::set_acp_thread) to
    /// attach the live thread once available.
    ///
    /// This is the only legal way to materialise a `SolutionSession`
    /// outside `model` — direct struct-literal construction is blocked
    /// by the private `acp_thread` field, which is exactly the point:
    /// every entry-point goes through a constructor where the thread
    /// starts unattached and reaches the entity only via `set_acp_thread`.
    pub fn new_idle(
        id: SolutionSessionId,
        solution_id: SolutionId,
        agent_id: AgentServerId,
        acp_session_id: acp::SessionId,
    ) -> Self {
        Self {
            id,
            solution_id,
            agent_id,
            acp_session_id,
            acp_thread: None,
            title: SharedString::default(),
            created_at: Utc::now(),
            last_activity_at: Utc::now(),
            state: SessionState::Idle,
            cwd: PathBuf::new(),
            context_count: 1,
            project: None,
            _acp_subscription: None,
            pending_messages: VecDeque::new(),
            flush_after_cancel: false,
            cold_entries: Vec::new(),
            entry_created_ms: Vec::new(),
            last_turn_duration: None,
            cached_total_tokens: None,
            cached_max_tokens: None,
            parent_session_id: None,
        }
    }

    /// `true` when this session was restored from the DB but the agent
    /// subprocess hasn't been spawned yet — so rendering must come
    /// from `cold_entries` rather than `acp_thread.entries()`.
    pub fn is_cold(&self) -> bool {
        self.acp_thread.is_none()
    }

    /// Live thread reference. `None` for cold tabs.
    pub fn acp_thread(&self) -> Option<&Entity<AcpThread>> {
        self.acp_thread.as_ref()
    }

    /// Replace the live `AcpThread` on this session. Atomically emits
    /// `SolutionSessionEvent::ThreadReplaced` and `cx.notify()` so
    /// `SolutionSessionView` can re-attach its per-thread subscription
    /// (`_thread_subscription`) to the new thread.
    ///
    /// All callers MUST go through this method instead of poking
    /// `acp_thread` directly. Direct assignment inside a nested
    /// `session_entity.update(cx, |s, _| ...)` does not reliably
    /// trigger `cx.observe(&session)` callbacks (auto-notify can be
    /// dropped by the outer flush's deduplication), which strands
    /// `SessionView::_thread_subscription` on the dead thread and
    /// silently halts conversation-list rendering for that session.
    pub fn set_acp_thread(
        &mut self,
        thread: Option<Entity<AcpThread>>,
        cx: &mut Context<Self>,
    ) {
        self.acp_thread = thread;
        cx.emit(SolutionSessionEvent::ThreadReplaced);
        cx.notify();
    }
}

/// Events emitted by a `SolutionSession` entity. Currently only the
/// thread-swap signal — extend as new push channels become necessary.
#[derive(Debug, Clone, Copy)]
pub enum SolutionSessionEvent {
    /// The session's `acp_thread` was just replaced (compact, `/clear`,
    /// cold→live, restart_agent reuse path). Subscribers must drop any
    /// per-thread state and re-attach to the new `AcpThread`.
    ThreadReplaced,
}

impl EventEmitter<SolutionSessionEvent> for SolutionSession {}

/// Lightweight metadata row used for navigator listing without hydrating
/// the full conversation blob.
#[derive(Clone, Debug)]
pub struct SolutionSessionMetadata {
    pub id: SolutionSessionId,
    pub solution_id: SolutionId,
    pub agent_id: AgentServerId,
    pub acp_session_id: acp::SessionId,
    pub title: SharedString,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    /// First user prompt (truncated) so the History popover can disambiguate
    /// otherwise-identical "Session <uuid>" rows. `None` for sessions that
    /// haven't received a user message yet.
    pub preview: Option<SharedString>,
    /// Cumulative tokens (input + output) reported by the agent in
    /// session_update events. Surfaces in the History popover so the user
    /// can pick a heavy/light session.
    pub total_tokens: Option<u64>,
    /// Persisted copy of [`SolutionSession::context_count`] so a session
    /// re-hydrated from the DB on editor restart resumes its compact
    /// numbering instead of resetting to 1.
    pub context_count: SessionContextCount,
    /// Persisted copy of [`SolutionSession::cwd`]. Empty for rows
    /// written before the column existed; the resume path treats
    /// empty as "use `solution.root`".
    pub cwd: PathBuf,
    /// F: persisted copy of [`SolutionSession::parent_session_id`].
    /// `None` for top-level sessions and for legacy rows written
    /// before the column existed.
    pub parent_session_id: Option<SolutionSessionId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn build_session() -> SolutionSession {
        SolutionSession {
            id: SolutionSessionId::new(),
            solution_id: SolutionId("sol".into()),
            agent_id: SharedString::from("claude-acp"),
            acp_session_id: acp::SessionId::new("acp-mock"),
            acp_thread: None,
            title: SharedString::from("test"),
            created_at: Utc::now(),
            last_activity_at: Utc::now(),
            state: SessionState::Idle,
            cwd: PathBuf::new(),
            context_count: 1,
            project: None,
            _acp_subscription: None,
            pending_messages: VecDeque::new(),
            flush_after_cancel: false,
            cold_entries: Vec::new(),
            entry_created_ms: Vec::new(),
            last_turn_duration: None,
            cached_total_tokens: None,
            cached_max_tokens: None,
            parent_session_id: None,
        }
    }

    /// `set_acp_thread` is the load-bearing contract that keeps
    /// `SolutionSessionView::_thread_subscription` from going stale when
    /// a session swaps its `AcpThread` (compact, `/clear`, cold→live).
    /// If anyone reverts to direct `s.acp_thread = ...` assignment
    /// inside a nested `update`, observers wired through `cx.observe`
    /// may be silently skipped — this test pins both signals so that
    /// regression is caught at unit-test time.
    #[gpui::test]
    fn set_acp_thread_emits_thread_replaced_and_notifies(cx: &mut TestAppContext) {
        let session = cx.update(|cx| cx.new(|_| build_session()));

        let emit_count = Arc::new(AtomicUsize::new(0));
        let observe_count = Arc::new(AtomicUsize::new(0));

        cx.update(|cx| {
            let emit = emit_count.clone();
            cx.subscribe(
                &session,
                move |_session: Entity<SolutionSession>,
                      event: &SolutionSessionEvent,
                      _cx| {
                    let SolutionSessionEvent::ThreadReplaced = event;
                    emit.fetch_add(1, Ordering::SeqCst);
                },
            )
            .detach();
            let observe = observe_count.clone();
            cx.observe(
                &session,
                move |_session: Entity<SolutionSession>, _cx| {
                    observe.fetch_add(1, Ordering::SeqCst);
                },
            )
            .detach();
        });

        cx.run_until_parked();
        assert_eq!(emit_count.load(Ordering::SeqCst), 0);
        assert_eq!(observe_count.load(Ordering::SeqCst), 0);

        session.update(cx, |s, cx| s.set_acp_thread(None, cx));
        cx.run_until_parked();

        assert_eq!(
            emit_count.load(Ordering::SeqCst),
            1,
            "set_acp_thread must emit exactly one ThreadReplaced event"
        );
        assert_eq!(
            observe_count.load(Ordering::SeqCst),
            1,
            "set_acp_thread must wake cx.observe subscribers via cx.notify()"
        );
    }
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use agent_client_protocol::schema as acp;
use anyhow::{Result, anyhow};
use chrono::Utc;
use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, SharedString, Subscription,
    Task,
};
use solutions::{Solution, SolutionId, SolutionStore, SolutionStoreEvent};
use util::ResultExt;

use crate::adapter::AdapterRegistry;
use crate::db::SolutionAgentDb;
use crate::metrics_emitter::MetricsEmitter;
use crate::model::{
    AgentServerId, SessionContextCount, SessionState, SolutionSession, SolutionSessionId,
    SolutionSessionMetadata, SubagentTab,
};
use crate::notifier;
use crate::pool::SubprocessPool;

mod connection_pool;
mod queue;
#[cfg(test)]
pub(crate) mod tests;

pub(crate) use queue::{QUEUE_MARKER_BODY_SEP, QUEUE_MARKER_PREFIX};

pub struct SolutionAgentStore {
    sessions: HashMap<SolutionSessionId, Entity<SolutionSession>>,
    by_solution: HashMap<SolutionId, Vec<SolutionSessionId>>,
    pool: parking_lot::Mutex<SubprocessPool>,
    persistence: Option<Arc<SolutionAgentDb>>,
    pub(crate) adapters: Arc<AdapterRegistry>,
    /// Map of `AgentServerId -> Rc<dyn AgentServer>`. Real `agent_servers`
    /// instances live per-Project (via `Project::agent_server_store`), but
    /// `SolutionAgentStore` is global-scoped — so we keep a fork-local lookup
    /// table that production wiring will populate at app init and tests
    /// populate manually. Held in an `Rc` because `dyn AgentServer` is `!Sync`.
    server_registry: HashMap<AgentServerId, Rc<dyn agent_servers::AgentServer>>,
    /// Set by the navigator (Phase 6) so `mutate_state` can ask "is this
    /// session currently focused in the UI?" before deciding whether to
    /// fire an OS notification. Stored as `Fn(&App) -> bool` rather than
    /// `Fn(&Context<Self>) -> bool` because `Context` is parameterised on
    /// `Self`, which makes the trait object generic and unstorable. `&App`
    /// is the strict supertype the resolver actually needs.
    pub focus_resolver: Option<Arc<dyn Fn(SolutionSessionId, &gpui::App) -> bool + Send + Sync>>,
    /// In-flight debounce slots for `AcpThreadEvent::EntryUpdated` events.
    /// Tool-call arg deltas, assistant-text chunks, and status flips on an
    /// existing entry all funnel through `EntryUpdated`; without this map
    /// they would either spam MCP notifications (one per token) or — as the
    /// pre-fix behaviour did — get dropped on the floor entirely because
    /// the catch-all match arm ignored them.
    ///
    /// Each key is a `(session_id, entry_index)` pair; the value is the
    /// pending trailing-edge `SessionMessageAppended` emit task. Updates
    /// while a task is in flight replace the entry (dropping the old `Task`
    /// cancels its `timer().await`), restarting the debounce window. The
    /// `first_dirty_at` field captures when the FIRST update for this
    /// debounce window arrived so we can force-emit on a max-stale
    /// breach — a continuously-streaming entry mustn't be able to starve
    /// the trailing-edge emit indefinitely.
    entry_update_throttles: HashMap<(SolutionSessionId, usize), EntryUpdateThrottle>,
    /// One per-session background-agent watcher task — alive as long as
    /// the session has >=1 registered `background_agents`. Stored as
    /// `Task<()>` so dropping kills the watcher cleanly. Populated by
    /// `ensure_background_agent_watcher` (called from the tool-call
    /// handler in a later task of the Background Agents Strip plan).
    background_agent_watchers: HashMap<SolutionSessionId, gpui::Task<()>>,
    /// Throttler for `workspace.session_metrics_changed` notifications.
    /// Caps emit rate at ~1 per 2 seconds per session so chatty fields
    /// (`last_activity_at`, `total_tokens`, `max_tokens`) don't flood
    /// the wire on every token-usage update. Non-sequenced: missed metric
    /// notifications do NOT trigger resync on the client.
    metrics_emitter: MetricsEmitter,
    _solution_subscription: Option<Subscription>,
    /// 1 Hz healthcheck loop that drives `tick_background_agents`.
    /// Held so the timer cancels when the store is dropped.
    _bg_agents_tick: Option<Task<()>>,
}

struct EntryUpdateThrottle {
    first_dirty_at: std::time::Instant,
    /// Stored only to keep the debounce timer alive: dropping this
    /// `Task` cancels its `timer().await` (the trailing-edge emit).
    /// Read implicitly via `Drop`, never by name.
    _task: Task<()>,
}

#[derive(Debug)]
pub enum SolutionAgentStoreEvent {
    /// A new session was registered in the store. `parent_session_id`
    /// is `Some` for sub-agent sessions (F: sub-agent indication) —
    /// the sub-agents-strip event coordinator forwards this through
    /// the wire payload so remote clients can update their tree
    /// without a follow-up `get_session_children` round-trip.
    SessionCreated {
        id: SolutionSessionId,
        parent_session_id: Option<SolutionSessionId>,
    },
    SessionClosed(SolutionSessionId),
    /// The set of sessions whose `tab_order IS NOT NULL` changed for
    /// `solution_id`. Emitted by `persist_tab_order` so that local UI
    /// consumers (notably `ConsolePanel`) can reactively add/remove the
    /// actual tabs in response to mutations driven from outside the
    /// panel — most importantly the wire-side
    /// `workspace.{open,close}_session` RPCs from the mobile client,
    /// which previously updated `tab_order` + the wire notification but
    /// left the desktop tab strip stale.
    ///
    /// `opened` and `closed` carry the diff against the pre-mutation
    /// set; both lists can be empty when `persist_tab_order` was called
    /// for a reorder-only change (same set, different order).
    TabsChanged {
        solution_id: SolutionId,
        opened: Vec<SolutionSessionId>,
        closed: Vec<SolutionSessionId>,
    },
    SessionStateChanged(SolutionSessionId),
    SessionTitleChanged(SolutionSessionId),
    /// Carries the entry index that was appended / updated so external
    /// MCP consumers (the WS proxy + Android client) can render the new
    /// entry without a follow-up `get_session` round-trip. The index is
    /// captured at emit time from the live `AcpThread.entries().len()
    /// - 1`, so a tight burst of appends can race — the consumer should
    /// treat the index as a hint and re-fetch the full session if the
    /// numbers don't line up.
    SessionMessageAppended(SolutionSessionId, usize),
    /// `pending_messages` on the session changed (push, drain, clear,
    /// or merge into back-of-queue). External MCP consumers use this
    /// to render server-side queued bundles as Queued bubbles in real
    /// time on every paired client — without it a desktop-typed
    /// follow-up while the agent is mid-turn stays invisible on the
    /// mobile until the eventual flush, and vice-versa.
    SessionQueueChanged(SolutionSessionId),
    SessionNotified(SolutionSessionId, notifier::NotifyKind),
    /// The session's [`SolutionSession::active_subagents`] map (and its
    /// parallel `active_subagent_order` vector) changed: a `Task` / `Agent`
    /// subagent was either spawned (parent ToolCall flipped to `InProgress`)
    /// or finished (parent ToolCall flipped to a terminal status). Emitted
    /// only when the map *actually* changed — a duplicate spawn event for a
    /// known id, or a terminal status on an unknown id, is silently
    /// ignored to keep the event stream debounce-friendly.
    ///
    /// Subscribers: the session_view's subagent-tabs strip (Etap 4) and the
    /// MCP wire's `session_active_subagents_changed` notification (Etap 5),
    /// so both desktop and mobile redraw without polling the session entity.
    SessionSubagentsChanged(SolutionSessionId),
    /// `SolutionSession::background_agents` changed — registration, snapshot
    /// update, dead-detection, or removal. Same debounce semantics as
    /// `SessionSubagentsChanged`: emitted only when the map actually changed.
    SessionBackgroundAgentsChanged(SolutionSessionId),
    /// Emitted when a session's conversation context has just been wiped
    /// in-place by `/clear` (`reset_context`) or `/compact`
    /// (`rotate_context`). Remote clients use this to drop their cached
    /// entry list for the session and re-fetch from scratch (the
    /// `session_id` is stable across the swap — only the transcript is
    /// gone). `context_count` is the post-operation value (incremented
    /// by `rotate_context`, left as-is by `reset_context`).
    SessionContextReset {
        id: SolutionSessionId,
        context_count: SessionContextCount,
    },
}

impl EventEmitter<SolutionAgentStoreEvent> for SolutionAgentStore {}

/// Which "view" of a session the user has selected — Main = parent
/// thread only, Task(id) = an in-flight inline Task subagent's
/// filtered slice, Background(id) = a Managed Agent's standalone
/// JSONL transcript. Replaces the older `Option<SharedString>` shape
/// where `None`=Main and `Some(id)` was ambiguously a Task id;
/// adding Background made an explicit sum-type necessary.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SubagentView {
    #[default]
    Main,
    Task(SharedString),
    Background(crate::background_agent::BackgroundAgentId),
}

impl SubagentView {
    /// True when the view sources its entries from the parent
    /// `AcpThread.entries` (Main + Task filter both do); false when
    /// the view sources from JSONL on disk (Background).
    pub fn is_parent_thread_view(&self) -> bool {
        matches!(self, Self::Main | Self::Task(_))
    }

    /// Predicate for parent-thread entry filtering. `Main` matches
    /// only entries with no `subagent_id`; `Task(id)` matches only
    /// entries stamped with exactly that id; `Background` matches
    /// nothing (it doesn't draw from parent entries).
    pub fn matches_parent_entry(&self, entry_subagent: Option<&SharedString>) -> bool {
        match (self, entry_subagent) {
            (Self::Main, None) => true,
            (Self::Task(sel), Some(eid)) => sel == eid,
            _ => false,
        }
    }
}

/// Compute the canonical subagents-dir path for a session. Mirrors
/// Anthropic's `~/.claude/projects/<encoded-cwd>/<session-id>/subagents/`
/// layout. `encoded-cwd` is "every char in `cwd.to_string_lossy()`
/// with `/` and `.` replaced by `-`". Returns `None` when `cwd` is
/// empty (legacy session) — those can't host managed agents anyway.
/// Case-insensitive match for the claude `Agent` tool name. Lives next
/// to `background_agent_dir_for` because both feed the managed-agent
/// registration path; keeping them adjacent makes the wiring obvious.
fn tool_name_is_agent(name: Option<&str>) -> bool {
    matches!(name, Some(n) if n.eq_ignore_ascii_case("agent"))
}

/// Seconds of file inactivity before a managed (background) agent
/// is considered dead. V1 hardcoded; a settings key can be added in
/// V2 once we have a real need for per-installation tuning.
const MANAGED_AGENT_STALE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(120);
/// Seconds a dead managed-agent pill lingers in the strip before
/// auto-disappearing. Same V1 hardcoded rationale as above.
const MANAGED_AGENT_DEAD_LINGER: std::time::Duration =
    std::time::Duration::from_secs(300);

fn background_agent_dir_for(cwd: &std::path::Path, acp_session_id: &str) -> Option<PathBuf> {
    if cwd.as_os_str().is_empty() {
        return None;
    }
    let raw = cwd.to_string_lossy();
    let mut encoded = String::with_capacity(raw.len() + 1);
    for c in raw.chars() {
        match c {
            '/' | '.' => encoded.push('-'),
            other => encoded.push(other),
        }
    }
    Some(
        dirs::home_dir()?
            .join(".claude")
            .join("projects")
            .join(encoded)
            .join(acp_session_id)
            .join("subagents"),
    )
}

/// Last 4 chars of a `toolu_xxx` id, used as the short-id suffix in
/// fallback subagent tab labels (`general-purpose#a1b2`, `Agent #a1b2`).
/// Lower bound guarded: an id shorter than 4 chars (defensive — claude's
/// real ids are 24+ chars) falls back to the whole id rather than
/// panicking on the slice bound.
fn short_id_suffix(id: &str) -> &str {
    let len = id.len();
    if len <= 4 { id } else { &id[len - 4..] }
}

/// Subagent-tab label fallback chain when the parent ToolCall's
/// `raw_input["description"]` is missing.
///
///   1. `<subagent_type>#<short-id>` — e.g. `general-purpose#a1b2`. Used
///      when claude's `Task` SDK populated `subagent_type` but the agent
///      author didn't bother with a description.
///   2. `Agent <short-id>` — last-resort label, should only hit in
///      adversarial / malformed inputs since claude always ships at
///      least `subagent_type` for a real `Task` call.
fn label_fallback(id: &SharedString, subagent_type: Option<&str>) -> SharedString {
    let short = short_id_suffix(id.as_ref());
    match subagent_type {
        Some(stype) if !stype.is_empty() => SharedString::from(format!("{stype}#{short}")),
        _ => SharedString::from(format!("Agent {short}")),
    }
}

struct GlobalSolutionAgentStore(Entity<SolutionAgentStore>);
impl Global for GlobalSolutionAgentStore {}

/// Decode a persisted blob into `(cold_entries, entry_created_ms)`. Shared
/// by `restore_open_tabs` (editor startup) and `resume_session`'s
/// fresh-entity branch (close→reopen within the same editor session) —
/// without this in the latter, the visible conversation goes empty on
/// reopen because `claude --resume` does not re-emit the transcript
/// through stream-json and the blob is the only source of the prior
/// dialog. Prefers the structured v2 payload; legacy v1 / pre-v1 blobs
/// degrade to a single Assistant-shaped entry per row containing the
/// flat markdown summary (no per-role bubbles for archived sessions,
/// but the text shows up — not worth a migration round-trip).
pub(crate) fn cold_entries_from_persisted(
    persisted: Option<PersistedSession>,
    cx: &mut gpui::App,
) -> (Vec<acp_thread::AgentThreadEntry>, Vec<i64>) {
    let Some(persisted) = persisted else {
        return (Vec::new(), Vec::new());
    };
    // `entry_created_ms` is index-aligned with `entries_v2`; the v2 path
    // below maps every element 1:1 into `cold_entries`, so the restored
    // vectors stay aligned. Legacy blobs carry an empty timestamps vec.
    let restored_created_ms = persisted.entry_created_ms.clone();
    let cold_entries: Vec<acp_thread::AgentThreadEntry> = if !persisted.entries_v2.is_empty() {
        persisted
            .entries_v2
            .into_iter()
            .map(|p| crate::cold_persistence::from_persisted(p, cx))
            .collect()
    } else {
        let legacy_sources: Vec<String> = if !persisted.entry_summaries.is_empty() {
            persisted.entry_summaries
        } else {
            persisted
                .entries
                .into_iter()
                .map(|e| e.markdown)
                .collect()
        };
        legacy_sources
            .into_iter()
            .map(|md| {
                crate::cold_persistence::from_persisted(
                    crate::cold_persistence::PersistedEntryV2::Assistant(
                        crate::cold_persistence::PersistedAssistantMessage {
                            chunks: vec![
                                crate::cold_persistence::PersistedAssistantChunk::Message(md),
                            ],
                        },
                    ),
                    cx,
                )
            })
            .collect()
    };
    (cold_entries, restored_created_ms)
}

/// On-disk snapshot of a session. Persisted as a JSON blob in the
/// `acp_thread_blob` column so MCP / future archive UIs can rehydrate
/// the conversation transcript even after the session was closed.
///
/// Public so downstream tools (`solution_agent.read_session_history`)
/// can deserialize the same blob the store wrote.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct PersistedSession {
    pub title: String,
    /// Legacy v1 per-entry record (role + flat markdown summary). Kept
    /// for blobs written by builds before `entries_v2` landed — those
    /// are rendered through the simplified Archived path. New blobs
    /// populate `entries_v2` and leave this empty (`#[serde(default)]`
    /// on read accepts both shapes).
    #[serde(default)]
    pub entries: Vec<PersistedEntry>,
    /// Legacy flat markdown summaries — one string per thread entry.
    /// Kept populated alongside `entries` for backwards compat with the
    /// `solution_agent.read_session_history` MCP tool, which slices
    /// this list directly.
    pub entry_summaries: Vec<String>,
    /// Structured per-entry payload used to reconstruct the live
    /// conversation visually 1:1 after an editor restart. Each variant
    /// captures everything the render path reads (markdown sources,
    /// raw chunks for image previews, tool-call statuses + per-content
    /// markdown, plan entries, …). In-flight tool calls (`Pending` /
    /// `WaitingForConfirmation` / `InProgress`) are dropped at save
    /// time — see [`crate::cold_persistence::to_persisted`].
    #[serde(default)]
    pub entries_v2: Vec<crate::cold_persistence::PersistedEntryV2>,
    /// Unix-millis creation time per persisted entry, index-aligned with
    /// `entries_v2` (built with the same drop-in-flight-tool-calls filter).
    /// `#[serde(default)]` → blobs written before this feature decode to an
    /// empty vec, which the loader treats as "no captured times".
    #[serde(default)]
    pub entry_created_ms: Vec<i64>,
}

pub use crate::model::{PersistedEntry, PersistedRole};
pub(crate) use queue::summarize_blocks_for_log;

/// First user prompt, normalised to a single line and truncated, for the
/// History popover label. Returns `None` if the thread has no user message
/// yet — caller's COALESCE keeps the previously-stored preview in that case.
fn extract_preview(entries: &[acp_thread::AgentThreadEntry]) -> Option<gpui::SharedString> {
    let first_user = entries.iter().find_map(|entry| match entry {
        acp_thread::AgentThreadEntry::UserMessage(msg) => Some(msg),
        _ => None,
    })?;
    // `chunks` is the raw ACP payload from the agent and contains the user's
    // typed text verbatim; `content` is the same data wrapped in a render-
    // ready `Markdown` entity that requires `&App` to read. We don't have
    // `cx` here (called from event-handler contexts that already hold a
    // mutable borrow of the store), so we walk chunks instead.
    let mut text = String::new();
    for chunk in &first_user.chunks {
        let chunk_text = match chunk {
            acp::ContentBlock::Text(t) => t.text.as_str(),
            _ => continue,
        };
        if !text.is_empty() && !text.ends_with(' ') {
            text.push(' ');
        }
        text.push_str(chunk_text);
        if text.len() >= 200 {
            break;
        }
    }
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let truncated = if collapsed.chars().count() > 80 {
        let mut s: String = collapsed.chars().take(77).collect();
        s.push('…');
        s
    } else {
        collapsed
    };
    Some(gpui::SharedString::from(truncated))
}

/// Placeholder title for a brand-new session, before claude-acp emits a
/// `TitleUpdated` describing the actual conversation. Keeps the tab
/// readable: 5 hex chars of the UUID is enough to disambiguate adjacent
/// tabs without smearing the entire UUID across the strip.
#[allow(dead_code)]
fn short_session_title(session_id: SolutionSessionId) -> SharedString {
    // SolutionSessionId is already 8 chars — no trimming needed; the
    // raw form is short enough to read at a glance and uniquely
    // identifies the session in `.agents/<id>/` paths.
    SharedString::from(session_id.to_string())
}

/// Returns `true` when the formatted error string from a `load_session` /
/// `resume_session` attempt indicates "the ACP server doesn't know about
/// this session id at this cwd" — as opposed to an auth/transport/allow-list
/// failure where retrying with a different cwd is pointless.
///
/// Match list is empirical because the wire shape of these errors isn't
/// part of the ACP contract:
///   - `Resource not found` / `-32002`: the canonical JSON-RPC code the
///     spec recommends for missing resources.
///   - `No conversation found`: claude-code-acp throws a plain `Error(...)`,
///     which marshals to `code: -32603 (Internal error)` with this text in
///     the message. Pre-fix, this string fell through the predicate and
///     `resume_session` broke out of the cwd-attempts loop on the first
///     failure, hiding the existing `solution.root` fallback (and the
///     `new_session` re-mint fallback below) and surfacing a raw
///     "No conversation found with session ID: …" snackbar on the user's
///     editor restart.
fn is_session_gone_error(err_str: &str) -> bool {
    err_str.contains("Resource not found")
        || err_str.contains("-32002")
        || err_str.contains("No conversation found")
}

/// Resolve the catalog project name for `cwd` if `cwd` matches one of
/// `solution.members`'s `local_path`s. Returns `None` for `solution.root`
/// (the "Solution root" choice in the New Session popover) and for any
/// path that doesn't map to a registered member — caller decides how to
/// label those (status row says "ROOT", title default uses
/// `solution.name`).
pub(crate) fn project_name_for_cwd(
    solution: &Solution,
    cwd: &std::path::Path,
    cx: &App,
) -> Option<SharedString> {
    if cwd.as_os_str().is_empty() || cwd == solution.root {
        return None;
    }
    let member = solution.members.iter().find(|m| m.local_path == cwd)?;
    let store = SolutionStore::try_global(cx)?;
    store.read_with(cx, |s, _| {
        s.catalog()
            .iter()
            .find(|c| c.id == member.catalog_id)
            .map(|c| SharedString::from(c.name.clone()))
    })
}

/// Pick a tab title that doesn't collide with any existing session in
/// the same Solution. First call returns `base`; subsequent collisions
/// get ` 2`, ` 3`, … appended (matching the "Untitled 2 / 3" convention
/// the rest of the editor uses for duplicate names). Caps at 1000 just
/// to avoid an infinite loop on a pathological state — practically
/// nobody opens 1000 sessions of the same project in one Solution.
fn unique_session_title(
    base: &str,
    store: &SolutionAgentStore,
    solution_id: &SolutionId,
    cx: &App,
) -> SharedString {
    let existing: std::collections::HashSet<String> = store
        .by_solution
        .get(solution_id)
        .into_iter()
        .flatten()
        .filter_map(|sid| store.sessions.get(sid))
        .map(|s| s.read(cx).title.to_string())
        .collect();
    if !existing.contains(base) {
        return SharedString::from(base.to_string());
    }
    for n in 2..1000 {
        let candidate = format!("{base} {n}");
        if !existing.contains(&candidate) {
            return SharedString::from(candidate);
        }
    }
    SharedString::from(base.to_string())
}

fn serializable_snapshot(session: &SolutionSession, cx: &App) -> Vec<u8> {
    // The visible conversation is the cold+live concatenation (see the
    // render path in `session_view.rs::render_conversation_body` and the
    // matching `sync_thread_subscription` list_state sizing). The blob
    // we persist must mirror that — otherwise persist drops cold_entries
    // every snapshot and the next reload shows only this-session
    // entries (the "history disappears after first send" regression
    // observed once cold→live render concatenation was wired up).
    //
    // Each `(role, persisted_v2_payload, ms)` triple is built once and
    // both the v1 mirror (`entries` + flat `entry_summaries` for the
    // MCP read tool) and the v2 structured payload are filled from it,
    // so the two stay aligned. In-flight tool calls drop out via
    // `to_persisted` returning `None`; their ms is dropped too so
    // `entries_v2` and `entry_created_ms` keep their 1:1 invariant.
    let cold_count = session.cold_entries.len();
    let live_entries: Vec<&acp_thread::AgentThreadEntry> = session
        .acp_thread()
        .map(|thread| thread.read(cx).entries().iter().collect())
        .unwrap_or_default();
    let combined: Vec<(usize, &acp_thread::AgentThreadEntry)> = session
        .cold_entries
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e))
        .chain(
            live_entries
                .iter()
                .enumerate()
                .map(|(i, e)| (cold_count + i, *e)),
        )
        .collect();

    let mut entries = Vec::with_capacity(combined.len());
    let mut entry_summaries = Vec::with_capacity(combined.len());
    let mut entries_v2 = Vec::with_capacity(combined.len());
    let mut entry_created_ms = Vec::with_capacity(combined.len());
    for (global_index, entry) in &combined {
        if let Some(persisted) = crate::cold_persistence::to_persisted(entry, cx) {
            let markdown = entry.to_markdown(cx);
            entries.push(PersistedEntry {
                role: persisted_role_for(entry),
                markdown: markdown.clone(),
            });
            entry_summaries.push(markdown);
            entries_v2.push(persisted);
            entry_created_ms.push(
                session
                    .entry_created_ms
                    .get(*global_index)
                    .copied()
                    .unwrap_or(crate::model::NO_TIMESTAMP_MS),
            );
        }
    }
    let snapshot = PersistedSession {
        title: session.title.to_string(),
        entries,
        entry_summaries,
        entries_v2,
        entry_created_ms,
    };
    serde_json::to_vec(&snapshot).unwrap_or_default()
}

fn persisted_role_for(entry: &acp_thread::AgentThreadEntry) -> PersistedRole {
    match entry {
        acp_thread::AgentThreadEntry::UserMessage(_) => PersistedRole::User,
        acp_thread::AgentThreadEntry::AssistantMessage(_) => PersistedRole::Assistant,
        acp_thread::AgentThreadEntry::ToolCall(_) => PersistedRole::Tool,
        acp_thread::AgentThreadEntry::CompletedPlan(_) => PersistedRole::Plan,
    }
}

impl SolutionAgentStore {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalSolutionAgentStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalSolutionAgentStore>()
            .map(|g| g.0.clone())
    }

    pub fn init_global(cx: &mut App, adapters: Arc<AdapterRegistry>) {
        let entity = cx.new(|cx| Self::new_in_app(adapters, cx));
        cx.set_global(GlobalSolutionAgentStore(entity));
    }

    fn new_in_app(adapters: Arc<AdapterRegistry>, cx: &mut Context<Self>) -> Self {
        // SolutionStore subscription is opt-in here: in tests SolutionStore
        // may not be initialised, so we tolerate its absence by checking
        // `try_global` (the public sentinel for "is solutions::init done?").
        let solution_subscription = SolutionStore::try_global(cx)
            .map(|store| cx.subscribe(&store, Self::on_solution_event));
        // 1 Hz background-agent healthcheck. Drops done agents and prunes
        // long-dead ones; rendering-side "dead" detection (orange pill) uses
        // `MANAGED_AGENT_STALE_TIMEOUT` directly off the snapshot mtime, so
        // the tick is only responsible for eventual cleanup, not the
        // first-observation transition.
        let bg_agents_tick = cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this
                    .update(cx, |this, cx| this.tick_background_agents(cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            sessions: HashMap::new(),
            by_solution: HashMap::new(),
            pool: parking_lot::Mutex::new(SubprocessPool::new()),
            persistence: None,
            adapters,
            server_registry: HashMap::new(),
            focus_resolver: None,
            entry_update_throttles: HashMap::new(),
            background_agent_watchers: HashMap::new(),
            metrics_emitter: MetricsEmitter::new(),
            _solution_subscription: solution_subscription,
            _bg_agents_tick: Some(bg_agents_tick),
        }
    }

    /// Register an `AgentServer` instance under the given id so that
    /// `create_session` can look it up. Production wiring registers
    /// `CustomAgentServer::new(...)` for each known agent at app init;
    /// tests register a `MockAgentServer`.
    pub fn register_agent_server(
        &mut self,
        agent_id: AgentServerId,
        server: Rc<dyn agent_servers::AgentServer>,
    ) {
        self.server_registry.insert(agent_id, server);
    }

    pub fn registered_agent_server(
        &self,
        agent_id: &AgentServerId,
    ) -> Option<Rc<dyn agent_servers::AgentServer>> {
        self.server_registry.get(agent_id).cloned()
    }

    pub fn set_persistence(&mut self, db: Arc<SolutionAgentDb>) {
        self.persistence = Some(db);
    }

    /// Returns the database handle if set. Used by the navigator to list
    /// historic sessions (those persisted across editor restarts) for the
    /// "Resume" / "Continue last session" affordances.
    pub fn db(&self) -> Option<Arc<SolutionAgentDb>> {
        self.persistence.clone()
    }

    /// Create a new ACP session for `(solution_id, agent_id)`, multiplexed
    /// onto a shared subprocess via the pool. The caller passes the `project`
    /// to use for the session: production callers pass the active workspace's
    /// `Entity<Project>`; tests pass a `Project::test`-built entity.
    ///
    /// Synthetic single-worktree projects per session were considered (see
    /// `pool::make_production_project_for_solution`) but defer to a follow-up
    /// — the AgentServer's `connect()` path is tightly coupled to a
    /// per-Project `AgentServerStore`, so re-using the workspace project is
    /// the diff-minimal choice today.
    pub fn create_session(
        &mut self,
        solution_id: SolutionId,
        agent_id: AgentServerId,
        project: Entity<project::Project>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        self.create_session_with_cwd(solution_id, agent_id, project, None, cx)
    }

    /// Same as `create_session`, but lets the caller pin the session's
    /// working directory to a specific path inside the solution (e.g.
    /// a member project root) instead of defaulting to `solution.root`.
    /// Pass `None` for the default behavior.
    pub fn create_session_with_cwd(
        &mut self,
        solution_id: SolutionId,
        agent_id: AgentServerId,
        project: Entity<project::Project>,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        self.create_session_with_parent(solution_id, agent_id, project, cwd, None, cx)
    }

    /// Full variant. `parent_session_id` (F: sub-agent indication) marks
    /// the new session as a child of `parent_session_id` so the session-
    /// view's sub-agents strip renders it under its parent. The parent
    /// MUST already exist in the same solution — the caller is
    /// responsible for that validation; the in-process store accepts
    /// any value here and only writes it through.
    pub fn create_session_with_parent(
        &mut self,
        solution_id: SolutionId,
        agent_id: AgentServerId,
        project: Entity<project::Project>,
        cwd: Option<PathBuf>,
        parent_session_id: Option<SolutionSessionId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        let pair = (solution_id.clone(), agent_id.clone());

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // 1. Resolve the solution. Cloned out so we don't hold the store
            //    borrow across the connection await.
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", solution_id))
                    })
            })?;

            // 2. Get-or-spawn the pooled connection for (solution, agent).
            //    Build the session-prompt `_meta` here too: needs the live
            //    `adapters` registry on the store, and we already have the
            //    store borrow open.
            let (connection_task, acp_meta) = this.update(cx, |store, cx| {
                let task =
                    store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx);
                let meta = store.build_session_meta(&pair.1, &solution);
                (task, meta)
            })?;
            let connection = connection_task.await?;

            // 3. Create an ACP session on that connection.
            let work_dir = cwd.unwrap_or_else(|| solution.root.clone());
            log::info!(
                target: "solution_agent::resume",
                "creating session in solution={:?} agent={} cwd={} (solution_root={})",
                solution_id,
                agent_id,
                work_dir.to_string_lossy(),
                solution.root.to_string_lossy(),
            );
            let work_dirs =
                util::path_list::PathList::new(&[work_dir.to_string_lossy().into_owned()]);
            let session_cwd = work_dir.clone();
            let acp_thread_task = cx.update(|cx| {
                connection
                    .clone()
                    .new_session_with_meta(project.clone(), work_dirs, acp_meta, cx)
            });
            let acp_thread = match acp_thread_task.await {
                Ok(thread) => thread,
                Err(err) => {
                    // Spawn succeeded but new_session failed — release our
                    // refcount on the pooled connection so it can debounce-
                    // close if no other sessions are active.
                    this.update(cx, |store, cx| {
                        store.pool_release_session(pair.clone(), cx);
                    })
                    .ok();
                    return Err(err);
                }
            };

            // 4. Register the session and emit `SessionCreated`.
            let session_id = this.update(cx, |store, cx| {
                let acp_session_id = acp_thread.read(cx).session_id().clone();
                let session_id = SolutionSessionId::new();
                // Default tab title = name of the project that's the
                // session's cwd: catalog name for a member, else the
                // Solution name (covers the "Solution root" choice).
                // Dedup'd against existing sessions in the same Solution
                // so successive same-cwd opens land as `name`, `name 2`,
                // `name 3`, …
                let title_base: SharedString = project_name_for_cwd(&solution, &session_cwd, cx)
                    .unwrap_or_else(|| SharedString::from(solution.name.clone()));
                let title = unique_session_title(&title_base, store, &solution_id, cx);
                let entity = cx.new(|cx| {
                    let mut s = SolutionSession::new_idle(
                        session_id,
                        solution_id.clone(),
                        agent_id.clone(),
                        acp_session_id,
                    );
                    s.title = title;
                    s.project = Some(project.clone());
                    s.cwd = session_cwd.clone();
                    s.parent_session_id = parent_session_id;
                    s.set_acp_thread(Some(acp_thread.clone()), cx);
                    s
                });
                store.sessions.insert(session_id, entity);
                let by_sol = store.by_solution.entry(solution_id.clone()).or_default();
                if !by_sol.contains(&session_id) {
                    by_sol.push(session_id);
                }
                let sub = store.subscribe_to_session(session_id, acp_thread, cx);
                store
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("session vanished after insert"))?
                    .update(cx, |s, _| s._acp_subscription = Some(sub));
                store.persist_session_row(session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionCreated {
                    id: session_id,
                    parent_session_id,
                });
                cx.notify();
                anyhow::Ok(session_id)
            })??;

            Ok(session_id)
        })
    }

    /// Build the `_meta` payload for a `NewSessionRequest` so the agent
    /// receives the solution-context system prompt. Wraps the adapter's
    /// `build_initial_system_prompt` output in the shape claude-agent-acp
    /// expects: `{ "systemPrompt": { "append": "<prompt>" } }`. The
    /// `append` form preserves Claude's default `claude_code` preset and
    /// concatenates our text after it (string-form would replace the
    /// preset entirely — wrong for our needs since we want the standard
    /// CLI behavior plus solution awareness).
    ///
    /// Returns `None` when no adapter is registered for `agent_id` or
    /// the adapter produced an empty prompt; ACP agents that don't
    /// understand `_meta.systemPrompt` ignore unknown keys per the
    /// protocol contract, so emitting it is safe even for non-Claude
    /// adapters.
    ///
    /// Called at every fresh-session site (`create_session`,
    /// `rotate_context` for `/compact`, `reset_context` for `/clear`)
    /// so the system prompt is re-asserted whenever the underlying ACP
    /// session is recreated — that's how it survives `/clear`.
    fn build_session_meta(
        &self,
        agent_id: &AgentServerId,
        solution: &Solution,
    ) -> Option<acp::Meta> {
        let prompt = self
            .adapters
            .get(agent_id)?
            .build_initial_system_prompt(solution);
        if prompt.is_empty() {
            return None;
        }
        Some(acp::Meta::from_iter([(
            "systemPrompt".to_string(),
            serde_json::json!({ "append": prompt }),
        )]))
    }

    /// Persist the row for `session_id` to the DB so the History popover and
    /// "Continue last session" CTA pick it up across editor restarts. No-op
    /// when persistence is disabled (test contexts).
    fn persist_session_row(&self, session_id: SolutionSessionId, cx: &mut Context<Self>) {
        let Some(db) = self.persistence.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(&session_id) else {
            return;
        };
        let s = session.read(cx);
        // Pull the dialog preview + total token count from the live thread.
        // Both are `None` until the user sends the first prompt and the agent
        // emits a usage update, respectively. The DB write uses COALESCE so a
        // None on a follow-up insert never clobbers a previously-stored value.
        let (preview, total_tokens) = s
            .acp_thread()
            .map(|thread| {
                let thread = thread.read(cx);
                // `used_tokens` is the cumulative context usage that
                // claude-acp reports via `SessionUpdate::UsageUpdate`
                // — same number the status-row meter shows live. We
                // used to persist `input_tokens + output_tokens`,
                // which only covers the LAST turn (gated by the
                // ACP-beta response.usage path), so a 33k-token
                // session resumed as 700 tokens. Saving used_tokens
                // keeps the persisted value aligned with the meter.
                (
                    extract_preview(thread.entries()),
                    thread.token_usage().map(|u| u.used_tokens),
                )
            })
            .unwrap_or((None, None));
        let meta = SolutionSessionMetadata {
            id: session_id,
            solution_id: s.solution_id.clone(),
            agent_id: s.agent_id.clone(),
            acp_session_id: s.acp_session_id.clone(),
            title: s.title.clone(),
            created_at: s.created_at,
            last_activity_at: s.last_activity_at,
            preview,
            total_tokens,
            context_count: s.context_count,
            cwd: s.cwd.clone(),
            parent_session_id: s.parent_session_id,
        };
        db.save_metadata(meta).detach_and_log_err(cx);
    }

    /// Resume a session from its persisted metadata: spawns / reuses the
    /// pooled connection and asks the agent to attach to the saved
    /// `acp_session_id`. Falls back to `resume_session` (history-less
    /// reattach) if `load_session` (full replay) isn't supported. If the
    /// metadata is already in-memory the existing session is returned.
    ///
    /// Returns the live `SolutionSessionId`. The caller can then look up
    /// the entity via `session(id)` and open it in the navigator.
    pub fn resume_session(
        &mut self,
        meta: SolutionSessionMetadata,
        project: Entity<project::Project>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        // Already hot (`acp_thread` attached)? Return the existing
        // session id directly. A cold session — registered by
        // `restore_open_tabs` with `acp_thread: None` — falls through
        // and triggers the real spawn path so the user's pending Send
        // makes it to a live agent.
        if let Some(existing) = self
            .by_solution
            .get(&meta.solution_id)
            .into_iter()
            .flatten()
            .find(|sid| {
                self.sessions
                    .get(sid)
                    .map(|s| {
                        let s = s.read(cx);
                        s.acp_session_id == meta.acp_session_id && s.acp_thread().is_some()
                    })
                    .unwrap_or(false)
            })
            .cloned()
        {
            return Task::ready(Ok(existing));
        }

        let pair = (meta.solution_id.clone(), meta.agent_id.clone());

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == meta.solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", meta.solution_id))
                    })
            })?;

            let connection_task = this.update(cx, |store, cx| {
                store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx)
            })?;
            let connection = connection_task.await?;

            // Empty `cwd` = legacy row written before the column existed —
            // fall back to `solution.root` (matches the pre-fix resume
            // behaviour, so already-broken sessions don't get any worse).
            let primary_cwd = if meta.cwd.as_os_str().is_empty() {
                solution.root.clone()
            } else {
                meta.cwd.clone()
            };
            let acp_session_id = meta.acp_session_id.clone();
            let title_for_load = Some(meta.title.clone());

            // Resume cwd resolution + fallback. claude-acp keys session
            // jsonl files by the cwd that was active when the session
            // was *created* (`~/.claude/projects/<sanitized cwd>/<id>.jsonl`).
            // Because the `(solution, agent)` connection pool spawns one
            // subprocess per solution with `process.cwd = solution.root`,
            // *all* jsonls for a solution land under `sanitize(solution.root)`
            // — regardless of the member-dir cwd we asked for in
            // `NewSessionRequest::work_dirs`. So we try `solution.root`
            // FIRST when it differs from the persisted `primary_cwd`;
            // the primary_cwd attempt stays as a fallback for sessions
            // that legitimately stored a non-root path (older rows, or a
            // future per-member pool model). On a successful attempt we
            // also write the applied cwd back into `session.cwd` so the
            // *next* resume hits straight away with no retries.
            let attempts: Vec<PathBuf> = if primary_cwd != solution.root {
                vec![solution.root.clone(), primary_cwd.clone()]
            } else {
                vec![primary_cwd.clone()]
            };
            log::info!(
                target: "solution_agent::resume",
                "session={} acp_session={} attempting resume with cwds={:?}",
                meta.id,
                acp_session_id.0,
                attempts
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            );
            let mut last_err: Option<anyhow::Error> = None;
            let mut attached: Option<(Entity<acp_thread::AcpThread>, PathBuf)> = None;
            // `true` only while EVERY cwd candidate so far has failed
            // with `Resource not found`. A single non-RNF error
            // (transport, auth, allow-list, …) flips this to `false`
            // and disables the new-session fallback below — the
            // failure isn't a "claude-acp forgot the session" case
            // and re-creating wouldn't help.
            let mut all_resource_gone = true;
            for attempt_cwd in attempts {
                let work_dirs = util::path_list::PathList::new(&[attempt_cwd
                    .to_string_lossy()
                    .into_owned()]);
                let acp_thread_task: Task<Result<Entity<acp_thread::AcpThread>>> = cx
                    .update(|cx| {
                        if connection.supports_load_session() {
                            Ok(connection.clone().load_session(
                                acp_session_id.clone(),
                                project.clone(),
                                work_dirs.clone(),
                                title_for_load.clone(),
                                cx,
                            ))
                        } else if connection.supports_resume_session() {
                            Ok(connection.clone().resume_session(
                                acp_session_id.clone(),
                                project.clone(),
                                work_dirs.clone(),
                                title_for_load.clone(),
                                cx,
                            ))
                        } else {
                            Err(anyhow!(
                                "agent {:?} does not support loading or resuming sessions",
                                meta.agent_id,
                            ))
                        }
                    })?;
                match acp_thread_task.await {
                    Ok(thread) => {
                        attached = Some((thread, attempt_cwd));
                        break;
                    }
                    Err(err) => {
                        let err_str = format!("{err:#}");
                        let resource_gone = is_session_gone_error(&err_str);
                        if !resource_gone {
                            // Non-recoverable (auth, transport, …). Fall
                            // through with this error — fallback would
                            // just hit the same wall.
                            all_resource_gone = false;
                            last_err = Some(err);
                            break;
                        }
                        log::warn!(
                            target: "solution_agent::resume",
                            "session={} cwd={} returned session-gone error ({}); will try next candidate",
                            meta.id,
                            attempt_cwd.to_string_lossy(),
                            err_str,
                        );
                        last_err = Some(err);
                    }
                }
            }
            // If every cwd candidate returned "Resource not found" the
            // ACP session is genuinely gone (claude-acp lost its jsonl,
            // was restarted, or the agent rotated state under us) and
            // no further resume attempt against the SAME acp_session_id
            // can recover. Mint a fresh ACP session on the same
            // connection so the caller's pending prompt still lands —
            // the alternative is bouncing the user's message with an
            // unactionable "Resource not found" snackbar.
            //
            // The new ACP session has NO conversation history from
            // claude-acp's perspective. We log the transition loudly so
            // the user-visible side ("agent forgot the previous turns,
            // but my message went through") is at least traceable. The
            // SolutionSession entity below picks up the new session id
            // via `acp_thread.read(cx).session_id()`, so persistence and
            // the navigator stay aligned with claude-acp on the next
            // round-trip.
            if attached.is_none() && all_resource_gone {
                let acp_meta = this
                    .update(cx, |store, _| store.build_session_meta(&pair.1, &solution))?;
                let fallback_cwd = if primary_cwd != solution.root {
                    primary_cwd.clone()
                } else {
                    solution.root.clone()
                };
                let work_dirs = util::path_list::PathList::new(&[fallback_cwd
                    .to_string_lossy()
                    .into_owned()]);
                log::warn!(
                    target: "solution_agent::resume",
                    "session={} every cwd candidate returned Resource not found — \
                     claude-acp lost session {}; minting a NEW ACP session on the \
                     same connection (conversation history will appear empty to the \
                     agent on the next turn)",
                    meta.id,
                    acp_session_id.0,
                );
                let new_session_task: Task<Result<Entity<acp_thread::AcpThread>>> =
                    cx.update(|cx| {
                        connection.clone().new_session_with_meta(
                            project.clone(),
                            work_dirs,
                            acp_meta,
                            cx,
                        )
                    });
                match new_session_task.await {
                    Ok(thread) => {
                        attached = Some((thread, fallback_cwd));
                    }
                    Err(err) => {
                        log::error!(
                            target: "solution_agent::resume",
                            "session={} new_session fallback failed after exhausting \
                             resume candidates: {err:#}",
                            meta.id,
                        );
                        last_err = Some(err);
                    }
                }
            }

            let (acp_thread, applied_cwd) = match attached {
                Some(pair) => pair,
                None => {
                    this.update(cx, |store, cx| {
                        store.pool_release_session(pair.clone(), cx);
                    })
                    .ok();
                    return Err(last_err.unwrap_or_else(|| {
                        anyhow!("resume_session: no cwd candidates produced a thread")
                    }));
                }
            };
            // Reflect the cwd the agent actually accepted in the rest
            // of the resume — store update + persist below — so a
            // future resume hits this cwd first instead of replaying
            // the same primary→fallback search.
            let resume_cwd = applied_cwd;

            // Best-effort preload of the persisted transcript blob. Used
            // by the fresh-entity branch below to seed `cold_entries`
            // when the user closed the session within the current
            // editor lifetime and is now reopening it from History.
            // The hot-path (existing in-memory session) keeps its
            // already-populated `cold_entries` untouched, so a blob
            // load here is wasted work — but resume_session is a rare,
            // user-triggered action and a single sqlite read is
            // negligible compared to the agent subprocess spawn we
            // already paid for above. Errors are logged and treated as
            // "no blob": worst case the user sees an empty conversation,
            // which is exactly what was happening BEFORE this fix.
            let preloaded_persisted: Option<PersistedSession> = {
                let load_task = this.update(cx, |store, _| {
                    store.persistence().map(|db| db.load_blob(meta.id))
                })?;
                match load_task {
                    Some(task) => match task.await {
                        Ok(Some(bytes)) => {
                            match serde_json::from_slice::<PersistedSession>(&bytes) {
                                Ok(p) => Some(p),
                                Err(err) => {
                                    log::warn!(
                                        target: "solution_agent::resume",
                                        "session={} blob decode failed on reopen: {err}",
                                        meta.id
                                    );
                                    None
                                }
                            }
                        }
                        Ok(None) => None,
                        Err(err) => {
                            log::warn!(
                                target: "solution_agent::resume",
                                "session={} blob load failed on reopen: {err}",
                                meta.id
                            );
                            None
                        }
                    },
                    None => None,
                }
            };

            let session_id = this.update(cx, |store, cx| {
                // Reuse the metadata's existing internal id — minting a fresh
                // SolutionSessionId on every resume duplicated the row in the
                // History popover (each restart added another "Session
                // <new-uuid>" pointing at the same `acp_session_id`).
                let session_id = meta.id;
                let new_thread_session_id = acp_thread.read(cx).session_id().clone();
                if let Some(existing) = store.sessions.get(&session_id).cloned() {
                    // Cold-session path: this id was hydrated by
                    // `restore_open_tabs` with `acp_thread: None` and
                    // populated `cold_entries`. Update the existing
                    // `Entity` in place instead of replacing it — the
                    // navigator's `SolutionSessionView` already holds
                    // this handle, so a swap would leave the UI bound
                    // to a stale entity. The `cx.notify()` is what
                    // wakes the view's `cx.observe(&session)` callback
                    // — without it, `sync_thread_subscription` never
                    // attaches to the new `AcpThread` (view sees no
                    // streaming) and `flush_pending_send_if_ready`
                    // never dispatches the message the user typed
                    // while the tab was cold (Send button gets stuck
                    // because `resuming` stays `true`).
                    let had_pending = existing.update(cx, |session, cx| {
                        let had_pending = !session.pending_messages.is_empty();
                        if had_pending {
                            // Cold→live transition with queued messages
                            // shouldn't normally happen (cold sessions
                            // can't queue), but log if it ever does so
                            // we don't lose them silently.
                            let previews: Vec<String> = session
                                .pending_messages
                                .iter()
                                .map(|b| queue::summarize_blocks_for_log(b))
                                .collect();
                            log::warn!(
                                target: "solution_agent::queue",
                                "session={session_id} dropped {} queued bundle(s) on resume_session cold→live promotion — content: [{}]",
                                session.pending_messages.len(),
                                previews.join(" | "),
                            );
                        }
                        session.acp_session_id = new_thread_session_id;
                        session.last_activity_at = Utc::now();
                        session.state = SessionState::Idle;
                        session.context_count = meta.context_count;
                        session.project = Some(project.clone());
                        session.pending_messages.clear();
                        session.flush_after_cancel = false;
                        session.cwd = resume_cwd.clone();
                        // KEEP `cold_entries`: claude --resume does NOT re-emit
                        // the transcript through stream-json, so clearing them
                        // wipes the chat history from the UI — old code assumed
                        // a replay that the native backend doesn't get. The
                        // build-entries path now concatenates cold + live.
                        // `set_acp_thread` emits ThreadReplaced + notify;
                        // it must be the last mutation so SessionView
                        // observers see a fully-populated session when
                        // they wake up to re-attach.
                        session.set_acp_thread(Some(acp_thread.clone()), cx);
                        had_pending
                    });
                    if had_pending {
                        cx.emit(SolutionAgentStoreEvent::SessionQueueChanged(session_id));
                    }
                } else {
                    // Hydrate cold_entries from the preloaded blob
                    // BEFORE attaching the live thread. claude --resume
                    // does NOT re-emit the transcript through
                    // stream-json, and `build_entries` concatenates
                    // cold + live: skipping this seeds an empty
                    // conversation visually even though the agent
                    // subprocess will happily continue from where it
                    // left off in the background (the close→reopen
                    // empty-history bug).
                    let (cold_entries, restored_created_ms) =
                        cold_entries_from_persisted(preloaded_persisted, cx);
                    let entity = cx.new(|cx| {
                        let mut s = SolutionSession::new_idle(
                            session_id,
                            meta.solution_id.clone(),
                            meta.agent_id.clone(),
                            new_thread_session_id,
                        );
                        s.title = meta.title.clone();
                        s.created_at = meta.created_at;
                        s.context_count = meta.context_count;
                        s.project = Some(project.clone());
                        // Persist the same cwd we resumed against so the
                        // next restart finds the row aligned with the
                        // agent state.
                        s.cwd = resume_cwd.clone();
                        s.cached_total_tokens = meta.total_tokens;
                        s.parent_session_id = meta.parent_session_id;
                        s.cold_entries = cold_entries;
                        s.entry_created_ms = restored_created_ms;
                        s.set_acp_thread(Some(acp_thread.clone()), cx);
                        s
                    });
                    store.sessions.insert(session_id, entity);
                }
                let by_sol = store
                    .by_solution
                    .entry(meta.solution_id.clone())
                    .or_default();
                if !by_sol.contains(&session_id) {
                    by_sol.push(session_id);
                }
                // Re-seed token usage from the persisted metadata so the
                // status-row meter doesn't claim "0 tokens" for a long
                // resumed conversation. We only have a coarse aggregate
                // (`total_tokens`); the model will fill in the
                // input/output split + max_tokens on the next turn via
                // session_update events.
                if let Some(total) = meta.total_tokens {
                    acp_thread.update(cx, |thread, cx| {
                        thread.update_token_usage(
                            Some(acp_thread::TokenUsage {
                                used_tokens: total,
                                ..Default::default()
                            }),
                            cx,
                        );
                    });
                }
                let sub = store.subscribe_to_session(session_id, acp_thread, cx);
                store
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("session vanished after insert"))?
                    .update(cx, |s, _| s._acp_subscription = Some(sub));
                store.persist_session_row(session_id, cx);
                // Resume re-livens a previously soft-closed row. Clear
                // the marker so MCP `read_session_history` (and any
                // future "Archived sessions" UI) reports it as live
                // again until the user closes the tab next time.
                if let Some(db) = &store.persistence {
                    db.mark_closed(session_id, None).detach_and_log_err(cx);
                }
                cx.emit(SolutionAgentStoreEvent::SessionCreated {
                    id: session_id,
                    parent_session_id: meta.parent_session_id,
                });
                cx.notify();
                anyhow::Ok(session_id)
            })??;

            Ok(session_id)
        })
    }

    pub fn sessions_for(&self, solution_id: &SolutionId) -> Vec<Entity<SolutionSession>> {
        self.by_solution
            .get(solution_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.sessions.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn session(&self, id: SolutionSessionId) -> Option<Entity<SolutionSession>> {
        self.sessions.get(&id).cloned()
    }

    pub fn all_sessions(&self) -> impl Iterator<Item = Entity<SolutionSession>> + '_ {
        self.sessions.values().cloned()
    }

    /// Test-only helper: register a session whose `acp_thread` was constructed
    /// elsewhere (or left `None`). Real `create_session` (Task 3.3) replaces
    /// this for production use.
    #[cfg(any(feature = "test-support", test))]
    pub fn register_prebuilt_session(
        &mut self,
        session: SolutionSession,
        cx: &mut Context<Self>,
    ) -> SolutionSessionId {
        let id = session.id;
        let solution_id = session.solution_id.clone();
        let parent_session_id = session.parent_session_id;
        let entity = cx.new(|_| session);
        self.sessions.insert(id, entity);
        self.by_solution.entry(solution_id).or_default().push(id);
        cx.emit(SolutionAgentStoreEvent::SessionCreated {
            id,
            parent_session_id,
        });
        cx.notify();
        id
    }

    /// Test-only helper: insert a minimal `SolutionSession` (idle, no acp
    /// thread) into the store for the given solution. Returns the new session
    /// id. Used by integration tests that need a session without going through
    /// the full `create_session` flow.
    #[cfg(any(test, feature = "test-support"))]
    pub fn create_for_test_minimal(
        &mut self,
        solution_id: &SolutionId,
        title: &str,
        cx: &mut Context<Self>,
    ) -> SolutionSessionId {
        let id = SolutionSessionId::new();
        let mut session = SolutionSession::new_idle(
            id,
            solution_id.clone(),
            SharedString::from("mock-agent"),
            acp::SessionId::new(format!("acp-{}", id.as_str())),
        );
        session.title = SharedString::from(title);
        self.register_prebuilt_session(session, cx)
    }

    /// Restore tabs the user had open the last time they closed this
    /// Solution, **without spawning the agent subprocess**. For each
    /// session id where `tab_order IS NOT NULL`, hydrate a
    /// `SolutionSession` with `acp_thread: None` and `cold_entries`
    /// populated from the persisted JSON blob. The session view will
    /// render those entries as a read-only conversation; the live
    /// `AcpThread` is only attached if/when the user submits a new
    /// message via `resume_session`.
    ///
    /// Sessions that already exist in `self.sessions` (created earlier
    /// in this process — e.g. via MCP from another window) are left
    /// untouched: they keep their live `acp_thread` and the navigator
    /// will pick them up via the normal reconcile path.
    ///
    /// Returns the ordered ids matching `tab_order ASC`. Caller (the
    /// navigator) uses that order directly to populate the strip,
    /// instead of relying on `created_at` sort.
    pub fn restore_open_tabs(
        &self,
        solution_id: SolutionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<SolutionSessionId>>> {
        let Some(db) = self.persistence.clone() else {
            return Task::ready(Ok(Vec::new()));
        };
        let already_open: std::collections::HashSet<SolutionSessionId> =
            self.sessions.keys().copied().collect();
        cx.spawn(async move |this, cx| {
            let ordered_ids = db.list_open_tabs(solution_id.clone()).await?;
            if ordered_ids.is_empty() {
                return Ok(Vec::new());
            }
            // Pull metadata for the whole solution once (single query) and
            // index by id. Cheaper than N round-trips when the user had
            // five-plus tabs open.
            let metas = db.list_for_solution(solution_id.clone()).await?;
            let by_id: std::collections::HashMap<SolutionSessionId, SolutionSessionMetadata> =
                metas.into_iter().map(|m| (m.id, m)).collect();
            // Load blobs for all rows we'll hydrate (skip ones already in
            // the in-memory store — they're already live or being
            // resumed).
            let mut blobs: std::collections::HashMap<SolutionSessionId, Vec<u8>> =
                std::collections::HashMap::new();
            for id in &ordered_ids {
                if already_open.contains(id) {
                    continue;
                }
                let blob = db.load_blob(*id).await?;
                if let Some(bytes) = blob {
                    blobs.insert(*id, bytes);
                }
            }
            // Apply on the foreground thread so the cx.new + emit
            // observe-callbacks all happen in the GPUI scheduler.
            // Collect the ids that survive into a result vec — orphans
            // (tab_order pointing at deleted metadata) and
            // hydration failures must NOT appear in the navigator's
            // restored strip, so the returned Vec only contains ids
            // that are now backed by a live `Entity<SolutionSession>`.
            let result_ids: Vec<SolutionSessionId> = this.update(cx, |this, cx| {
                let mut hydrated: Vec<SolutionSessionId> = Vec::with_capacity(ordered_ids.len());
                for (tab_idx, id) in ordered_ids.iter().enumerate() {
                    let tab_order = Some(tab_idx as i64);
                    if let Some(entity) = this.sessions.get(id) {
                        // Session already live — just stamp the tab_order so the
                        // in-memory view stays consistent with the DB column.
                        entity.update(cx, |s, _| s.tab_order = tab_order);
                        hydrated.push(*id);
                        continue;
                    }
                    let Some(meta) = by_id.get(id) else {
                        // tab_order pointed at a session whose metadata
                        // was deleted out from under it. Skip — the
                        // navigator never sees this id in the
                        // returned slice.
                        log::warn!("restore_open_tabs: orphaned tab_order for {id}");
                        continue;
                    };
                    // Reconstruct the persisted dialog as live-shape
                    // `AgentThreadEntry`s so the cold-tab render goes
                    // through the same virtualized list path as a real
                    // session. Prefer the structured v2 payload when
                    // present; legacy v1 / pre-v1 blobs degrade
                    // gracefully to a single Assistant-shaped entry
                    // per row containing the flat markdown summary
                    // (no bubbles for User vs Assistant, but at least
                    // the text shows up — not worth a full migration
                    // round-trip just to recolour archived sessions).
                    let persisted = blobs
                        .remove(id)
                        .and_then(|bytes| serde_json::from_slice::<PersistedSession>(&bytes).ok());
                    let (cold_entries, restored_created_ms) =
                        cold_entries_from_persisted(persisted, cx);
                    let entity = cx.new(|_| {
                        let mut s = SolutionSession::new_idle(
                            meta.id,
                            meta.solution_id.clone(),
                            meta.agent_id.clone(),
                            meta.acp_session_id.clone(),
                        );
                        s.title = meta.title.clone();
                        s.created_at = meta.created_at;
                        s.last_activity_at = meta.last_activity_at;
                        s.context_count = meta.context_count;
                        s.cwd = meta.cwd.clone();
                        s.cold_entries = cold_entries;
                        s.entry_created_ms = restored_created_ms;
                        // Seed from the persisted metadata so the
                        // status-row meter shows the last-known total
                        // for cold tabs (no live thread → no
                        // `TokenUsage`). The live path refreshes this
                        // on every `TokenUsageUpdated` event.
                        s.cached_total_tokens = meta.total_tokens;
                        s.parent_session_id = meta.parent_session_id;
                        s.tab_order = tab_order;
                        s
                    });
                    this.sessions.insert(meta.id, entity);
                    this.by_solution
                        .entry(solution_id.clone())
                        .or_default()
                        .push(meta.id);
                    cx.emit(SolutionAgentStoreEvent::SessionCreated {
                        id: meta.id,
                        parent_session_id: meta.parent_session_id,
                    });
                    hydrated.push(meta.id);
                }
                cx.notify();
                hydrated
            })?;
            Ok(result_ids)
        })
    }

    /// Like [`restore_open_tabs`], but loads **every** session row for the
    /// solution — including ones with `tab_order IS NULL` (closed tabs).
    /// Sessions already in `self.sessions` are skipped. Each freshly-
    /// hydrated session gets a `cold_entries` reconstruction from its
    /// persisted blob, so subsequent `get_session` / `list_sessions`
    /// calls see the full conversation history without needing the
    /// subprocess respawned.
    ///
    /// Driven by `solution_agent.list_sessions` so an MCP-only consumer
    /// (the phone) can see closed-tab sessions — the desktop's tab strip
    /// path was the only thing populating the in-memory store before,
    /// which left closed sessions invisible to MCP regardless of how
    /// much data was on disk.
    pub fn hydrate_all_for_solution(
        &self,
        solution_id: SolutionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<SolutionSessionId>>> {
        let Some(db) = self.persistence.clone() else {
            return Task::ready(Ok(Vec::new()));
        };
        let already_open: std::collections::HashSet<SolutionSessionId> =
            self.sessions.keys().copied().collect();
        cx.spawn(async move |this, cx| {
            // `list_open_session_ids` filters out rows whose `closed_at`
            // is set — sessions the user explicitly closed via the
            // desktop's close-tab affordance. Without this, every
            // refresh after a close would re-hydrate the closed
            // session back into self.sessions, undoing the close from
            // the phone's perspective on the very next list_sessions.
            let open_ids: std::collections::HashSet<SolutionSessionId> = db
                .list_open_session_ids(solution_id.clone())
                .await?
                .into_iter()
                .collect();
            // Fetch the ordered tab-strip list so we can stamp
            // `tab_order` on freshly-hydrated sessions. Sessions not
            // in this list get `tab_order = None` (closed/hidden tab).
            let tabbed_ids: Vec<SolutionSessionId> =
                db.list_open_tabs(solution_id.clone()).await.unwrap_or_default();
            let tab_order_map: std::collections::HashMap<SolutionSessionId, i64> = tabbed_ids
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, i as i64))
                .collect();
            if open_ids.is_empty() {
                return Ok(Vec::new());
            }
            let metas = db.list_for_solution(solution_id.clone()).await?;
            if metas.is_empty() {
                return Ok(Vec::new());
            }
            let to_hydrate: Vec<&SolutionSessionMetadata> = metas
                .iter()
                .filter(|m| open_ids.contains(&m.id) && !already_open.contains(&m.id))
                .collect();
            if to_hydrate.is_empty() {
                return Ok(Vec::new());
            }
            // Load every blob first off the foreground thread. Missing
            // blobs (NULL acp_thread_blob) just mean the session has
            // never had any conversation content — those still get
            // hydrated, just with an empty cold_entries vec.
            let mut blobs: std::collections::HashMap<SolutionSessionId, Vec<u8>> =
                std::collections::HashMap::new();
            for meta in &to_hydrate {
                if let Some(bytes) = db.load_blob(meta.id).await? {
                    blobs.insert(meta.id, bytes);
                }
            }
            let result_ids: Vec<SolutionSessionId> = this.update(cx, |this, cx| {
                let mut hydrated: Vec<SolutionSessionId> = Vec::with_capacity(to_hydrate.len());
                for meta in &to_hydrate {
                    if this.sessions.contains_key(&meta.id) {
                        continue;
                    }
                    let persisted = blobs
                        .remove(&meta.id)
                        .and_then(|bytes| serde_json::from_slice::<PersistedSession>(&bytes).ok());
                    let restored_created_ms = persisted
                        .as_ref()
                        .map(|p| p.entry_created_ms.clone())
                        .unwrap_or_default();
                    let cold_entries: Vec<acp_thread::AgentThreadEntry> = persisted
                        .map(|persisted| {
                            if !persisted.entries_v2.is_empty() {
                                persisted
                                    .entries_v2
                                    .into_iter()
                                    .map(|p| crate::cold_persistence::from_persisted(p, cx))
                                    .collect()
                            } else {
                                let legacy_sources: Vec<String> =
                                    if !persisted.entry_summaries.is_empty() {
                                        persisted.entry_summaries
                                    } else {
                                        persisted
                                            .entries
                                            .into_iter()
                                            .map(|e| e.markdown)
                                            .collect()
                                    };
                                legacy_sources
                                    .into_iter()
                                    .map(|md| {
                                        crate::cold_persistence::from_persisted(
                                            crate::cold_persistence::PersistedEntryV2::Assistant(
                                                crate::cold_persistence::PersistedAssistantMessage {
                                                    chunks: vec![
                                                        crate::cold_persistence::PersistedAssistantChunk::Message(
                                                            md,
                                                        ),
                                                    ],
                                                },
                                            ),
                                            cx,
                                        )
                                    })
                                    .collect()
                            }
                        })
                        .unwrap_or_default();
                    let session_tab_order = tab_order_map.get(&meta.id).copied();
                    let entity = cx.new(|_| {
                        let mut s = SolutionSession::new_idle(
                            meta.id,
                            meta.solution_id.clone(),
                            meta.agent_id.clone(),
                            meta.acp_session_id.clone(),
                        );
                        s.title = meta.title.clone();
                        s.created_at = meta.created_at;
                        s.last_activity_at = meta.last_activity_at;
                        s.context_count = meta.context_count;
                        s.cwd = meta.cwd.clone();
                        s.cold_entries = cold_entries;
                        s.entry_created_ms = restored_created_ms;
                        s.cached_total_tokens = meta.total_tokens;
                        s.parent_session_id = meta.parent_session_id;
                        s.tab_order = session_tab_order;
                        s
                    });
                    // Insert into `self.sessions` so the phone's
                    // list_sessions (via all_sessions()) and get_session
                    // (via self.sessions.get()) can find it. INTENTIONALLY
                    // skip `by_solution` and the SessionCreated event —
                    // those are the desktop navigator's input. The
                    // navigator's reconcile_open_sessions_with_store
                    // reads sessions_for() (= by_solution lookup), so
                    // leaving by_solution alone keeps the navigator
                    // ignorant of cold-hydrated sessions, which is what
                    // we want: hydration is read-only metadata exposure
                    // for the phone, not a 'reopen all closed tabs'
                    // command. If/when the user genuinely reopens one
                    // of these via the tab strip, restore_open_tabs's
                    // contains_key check will skip the re-insert but
                    // the navigator's own open_session path will add
                    // it to by_solution at that point.
                    this.sessions.insert(meta.id, entity);
                    hydrated.push(meta.id);
                }
                // Fan out `workspace.session_opened` for every freshly-hydrated
                // session that ended up tab-pinned. The store path that drives
                // the sequenced delta (`persist_tab_order`) is NOT invoked
                // here because the tab_order was set directly on the in-memory
                // entity above; without this manual emit a mobile client
                // that's already connected to the desktop process would never
                // hear about the just-hydrated sessions (their `tab_order` is
                // populated but no notification ever fired). The mobile-side
                // mirror would only learn via the next `workspace.snapshot`
                // round-trip — which doesn't happen until the user toggles
                // reconnect or backgrounds and resumes the app. Symptom:
                // opening a previously-closed solution from the picker
                // showed the row with zero consoles even though the desktop
                // had restored them. The emit shape is identical to
                // `persist_tab_order`'s; the mobile applier is idempotent
                // on duplicate session_opened with the same id.
                if let Some(coord) =
                    editor_mcp::workspace_seq::WorkspaceEventCoordinator::try_global(cx)
                {
                    for id in &hydrated {
                        let Some(entity) = this.sessions.get(id) else {
                            continue;
                        };
                        let (is_tabbed, summary) = entity.read_with(cx, |s, cx| {
                            (s.tab_order.is_some(), crate::mcp::session_summary(s, cx))
                        });
                        if !is_tabbed {
                            continue;
                        }
                        coord.emit_sequenced(
                            cx,
                            "workspace.session_opened",
                            serde_json::json!({
                                "solution_id": solution_id.as_str(),
                                "session": summary,
                            }),
                        );
                    }
                }
                if !hydrated.is_empty() {
                    cx.notify();
                }
                hydrated
            })?;
            Ok(result_ids)
        })
    }

    pub fn close_session(&mut self, id: SolutionSessionId, cx: &mut Context<Self>) -> Result<()> {
        let removed = self
            .sessions
            .remove(&id)
            .ok_or_else(|| anyhow!("unknown session {id}"))?;
        // If the session is being torn down with queued messages still
        // unflushed, surface them in the log — closing a tab silently
        // drops everything in `pending_messages` (no Stopped event ever
        // fires for the torn-down thread).
        let session_read = removed.read(cx);
        if !session_read.pending_messages.is_empty() {
            let previews: Vec<String> = session_read
                .pending_messages
                .iter()
                .map(|b| queue::summarize_blocks_for_log(b))
                .collect();
            log::warn!(
                target: "solution_agent::queue",
                "session={id} dropped {} queued bundle(s) on close_session — content: [{}]",
                session_read.pending_messages.len(),
                previews.join(" | "),
            );
        }
        let solution_id = session_read.solution_id.clone();
        if let Some(list) = self.by_solution.get_mut(&solution_id) {
            list.retain(|sid| *sid != id);
        }
        // Drop any per-entry update throttles for the closed session;
        // each holds a live debounce `Task`, so leaving them would leak
        // for the process lifetime (the throttle is only otherwise
        // removed when its own timer fires against a still-open session).
        self.entry_update_throttles.retain(|(sid, _), _| *sid != id);
        // Soft-close: keep the persisted blob so downstream tooling
        // (MCP read_session_history, future "View archived sessions"
        // UI, etc.) can still read the transcript. Hard-delete only
        // happens when the whole solution is removed via
        // `delete_for_solution`.
        if let Some(db) = &self.persistence {
            db.mark_closed(id, Some(Utc::now())).detach_and_log_err(cx);
        }
        cx.emit(SolutionAgentStoreEvent::SessionClosed(id));
        // Emit sequenced workspace notification so remote clients can
        // drop the session from their in-memory maps immediately.
        // `solution_id` was captured above while `session_read` was live
        // (before the entity was removed from `self.sessions`).
        // Guard with `try_global` so test contexts that don't install the
        // MCP layer don't panic.
        if let Some(coord) = editor_mcp::workspace_seq::WorkspaceEventCoordinator::try_global(cx) {
            coord.emit_sequenced(cx, "workspace.session_deleted", serde_json::json!({
                "solution_id": solution_id.as_str(),
                "session_id": id.to_string(),
            }));
        }
        cx.notify();
        Ok(())
    }

    /// Update the user-visible title of a session and persist the change
    /// (best-effort). Emits `SessionTitleChanged` so the navigator
    /// re-renders the row immediately.
    pub fn rename_session(
        &mut self,
        session_id: SolutionSessionId,
        title: SharedString,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let session = self
            .sessions
            .get(&session_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        session.update(cx, |s, _| s.title = title.clone());
        // Reuse `persist_session_row` so preview + token columns get
        // populated from the live thread instead of being NULL'd by this
        // title-only write path.
        self.persist_session_row(session_id, cx);
        cx.emit(SolutionAgentStoreEvent::SessionTitleChanged(session_id));
        cx.notify();
        Ok(())
    }

    /// Restart the agent backing `session_id`: drop the pool entry so the
    /// next `create_session` call forces a fresh subprocess spawn, close
    /// the existing session, and open a new one against the cached project.
    /// v1 does not replay history — the new session starts empty (deferred
    /// per Phase-5 spec "Open implementation questions" item 5).
    ///
    /// Returns the freshly minted `SolutionSessionId` so callers can
    /// reattach navigator focus to it.
    pub fn restart_agent(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        let Some(session) = self.sessions.get(&session_id).cloned() else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };
        let (solution_id, agent_id, project, previous_cwd) = {
            let s = session.read(cx);
            let project = match s.project.clone() {
                Some(project) => project,
                None => {
                    return Task::ready(Err(anyhow!(
                        "session {session_id} has no cached project — was it created via \
                         register_prebuilt_session?"
                    )));
                }
            };
            // Preserve the session's working directory across restart. Without
            // this the fresh session falls back to `solution.root` (the
            // `create_session` default), silently relocating a member-project
            // session — for the user that looks like "claude lost the project
            // root after I clicked Restart". Empty cwd is the legacy-row
            // marker meaning "fall back to solution.root"; pass `None` in
            // that case so `create_session_with_cwd` takes its own default.
            let cwd_override = if s.cwd.as_os_str().is_empty() {
                None
            } else {
                Some(s.cwd.clone())
            };
            (s.solution_id.clone(), s.agent_id.clone(), project, cwd_override)
        };
        let pair = (solution_id.clone(), agent_id.clone());
        {
            let mut pool = self.pool.lock();
            pool.remove(&pair);
        }
        // Mark the old session as restarting so the UI can show feedback
        // before the new session is registered.
        session.update(cx, |s, _| {
            s.state = SessionState::Errored(SharedString::from("restarting…"));
        });
        cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
        self.emit_session_state_changed_workspace(&session_id, cx);
        // Best-effort close of the old session; we still spawn the new
        // one even if removal fails so the user isn't stranded.
        if let Err(err) = self.close_session(session_id, cx) {
            log::warn!("restart_agent: close_session({session_id}) failed: {err:?}");
        }
        let create_task =
            self.create_session_with_cwd(solution_id, agent_id, project, previous_cwd, cx);
        cx.spawn(async move |_this, _cx: &mut AsyncApp| create_task.await)
    }

    /// In-place context rotation: drop the current AcpThread, spawn a
    /// fresh ACP-level session against the SAME pooled connection, and
    /// graft it onto the existing `SolutionSession`. The user-facing
    /// `SolutionSessionId` and tab identity stay stable so dump
    /// directories from successive compacts cluster under one
    /// `<root>/.agents/<sid>/` tree, distinguishable only by the
    /// `context_count` (= which rotation).
    ///
    /// Different from `restart_agent` in two ways:
    ///   1. Keeps `SolutionSessionId` (restart_agent mints a fresh
    ///      one because its goal is "this session is broken — please
    ///      give me a clean slate" while rotate's goal is "same
    ///      conversation, just freed up the context window").
    ///   2. Reuses the same pooled subprocess (restart_agent drops
    ///      the pool entry to force a subprocess respawn).
    pub fn rotate_context(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<u32>> {
        let Some(session_entity) = self.sessions.get(&session_id).cloned() else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };
        let (solution_id, agent_id, project, current_count, session_cwd) = {
            let s = session_entity.read(cx);
            let project = match s.project.clone() {
                Some(project) => project,
                None => {
                    return Task::ready(Err(anyhow!(
                        "session {session_id} has no cached project — rotate_context not supported \
                         for prebuilt test sessions"
                    )));
                }
            };
            (
                s.solution_id.clone(),
                s.agent_id.clone(),
                project,
                s.context_count,
                s.cwd.clone(),
            )
        };
        let pair = (solution_id.clone(), agent_id);

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // Resolve the live Solution so `connection.new_session`
            // gets a real cwd.
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", solution_id))
                    })
            })?;
            let (connection_task, acp_meta) = this.update(cx, |store, cx| {
                let task =
                    store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx);
                let meta = store.build_session_meta(&pair.1, &solution);
                (task, meta)
            })?;
            let connection = connection_task.await?;
            // Preserve the session's per-tab working directory across
            // /compact. Without this the rotated thread would be created
            // with cwd=solution.root, so the agent's bash tool — which
            // inherits NewSessionRequest.cwd as its "Primary working
            // directory" — would silently switch from the member subdir
            // (e.g. `voxelcraft`) to the solution root after compaction
            // and then fail commands that depend on `Cargo.toml` /
            // `.git` being present.
            let work_dir = if session_cwd.as_os_str().is_empty() {
                solution.root.clone()
            } else {
                session_cwd.clone()
            };
            let work_dirs =
                util::path_list::PathList::new(&[work_dir.to_string_lossy().into_owned()]);
            let new_thread_task = cx.update(|cx| {
                connection
                    .clone()
                    .new_session_with_meta(project.clone(), work_dirs, acp_meta, cx)
            });
            let new_thread = new_thread_task.await?;

            let new_count = this.update(cx, |store, cx| {
                let new_acp_session_id = new_thread.read(cx).session_id().clone();
                let new_count = current_count.saturating_add(1);
                session_entity.update(cx, |s, cx| {
                    s.acp_session_id = new_acp_session_id;
                    s.context_count = new_count;
                    s.state = SessionState::Idle;
                    s.last_activity_at = Utc::now();
                    // Status-row meter falls back to `cached_total_tokens`
                    // when the live thread has no `token_usage` yet (the
                    // freshly-spawned thread does not). Without a reset,
                    // the meter would keep reading the pre-rotation count
                    // until the agent emits its first `TokenUsageUpdated`
                    // — confusing right after a context rotation. Same
                    // story for `last_turn_duration` (the "Done in Xs"
                    // hint should not survive past the rotation).
                    s.cached_total_tokens = None;
                    s.last_turn_duration = None;
                    s.entry_created_ms.clear();
                    // Compact archives the prior context and continues
                    // in a fresh ACP session under the same tab. The
                    // render path concatenates `cold_entries` ahead of
                    // the live thread, so without clearing them the
                    // rotated tab would keep painting the
                    // already-archived conversation. Both must be
                    // wiped together so the post-rotate UI starts from
                    // the (empty) live thread only.
                    s.cold_entries.clear();
                    // `set_acp_thread` emits ThreadReplaced + notify;
                    // last so SessionView re-attaches against a fully
                    // updated session struct.
                    s.set_acp_thread(Some(new_thread.clone()), cx);
                });
                // Re-subscribe to the new AcpThread's event stream.
                // Dropping the old subscription unhooks us from the
                // dead thread automatically.
                let new_sub = store.subscribe_to_session(session_id, new_thread, cx);
                session_entity.update(cx, |s, _| s._acp_subscription = Some(new_sub));
                store.persist_session_row(session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
                store.emit_session_state_changed_workspace(&session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionContextReset {
                    id: session_id,
                    context_count: new_count,
                });
                cx.notify();
                new_count
            })?;

            Ok(new_count)
        })
    }

    /// Reset the session's conversation context: drop the current
    /// `AcpThread` and spawn a fresh ACP-level session under the same
    /// `SolutionSessionId` and pooled subprocess.
    ///
    /// Different from [`rotate_context`](Self::rotate_context) in that
    /// `context_count` is left untouched (no `c<N>` directory bump) —
    /// this is the path wired to the user-facing `/clear` slash command,
    /// where the intent is "wipe this conversation, keep the tab"
    /// rather than "archive a long-running conversation as a numbered
    /// rotation". Agent-agnostic: nothing is forwarded to the agent
    /// subprocess; the new ACP session has zero history by construction.
    ///
    /// Returns the same `SolutionSessionId` for caller convenience (so
    /// the call site can chain "reset then dispatch follow-up" without
    /// re-plumbing the id).
    pub fn reset_context(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        let Some(session_entity) = self.sessions.get(&session_id).cloned() else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };
        // `project` is None for a COLD session (loaded from the DB, never
        // promoted to live this run) — the common case for `/clear` on a
        // session whose conversation was generated in a previous editor
        // run. Rather than bail, we resolve a headless project from the
        // solution below (same fallback the cold→live auto-wake path uses
        // in `queue::send_message_blocks_with_wake`), so reset works on
        // cold sessions too.
        let (solution_id, agent_id, cached_project, session_cwd) = {
            let s = session_entity.read(cx);
            (
                s.solution_id.clone(),
                s.agent_id.clone(),
                s.project.clone(),
                s.cwd.clone(),
            )
        };
        let pair = (solution_id.clone(), agent_id);

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", solution_id))
                    })
            })?;
            let project = match cached_project {
                Some(project) => project,
                None => {
                    let solution = solution.clone();
                    cx.update(move |cx| {
                        SolutionAgentStore::make_headless_project_for_solution(&solution, cx)
                    })?
                }
            };
            let (connection_task, acp_meta) = this.update(cx, |store, cx| {
                let task =
                    store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx);
                let meta = store.build_session_meta(&pair.1, &solution);
                (task, meta)
            })?;
            let connection = connection_task.await?;
            // Preserve the session's per-tab working directory across
            // /clear. Same reason as `rotate_context` above: the rotated
            // thread otherwise inherits cwd=solution.root and the agent's
            // bash tool silently switches away from the member subdir
            // the tab was bound to.
            let work_dir = if session_cwd.as_os_str().is_empty() {
                solution.root.clone()
            } else {
                session_cwd.clone()
            };
            let work_dirs =
                util::path_list::PathList::new(&[work_dir.to_string_lossy().into_owned()]);
            let new_thread_task = cx.update(|cx| {
                connection
                    .clone()
                    .new_session_with_meta(project.clone(), work_dirs, acp_meta, cx)
            });
            let new_thread = new_thread_task.await?;

            this.update(cx, |store, cx| {
                let new_acp_session_id = new_thread.read(cx).session_id().clone();
                let had_pending = session_entity.update(cx, |s, cx| {
                    let had_pending = !s.pending_messages.is_empty();
                    if had_pending {
                        // `/clear` wipes the session's conversation —
                        // queued follow-ups are tied to the OLD context
                        // and don't apply to a freshly-empty thread, so
                        // discard. WARN log so post-mortem of "I typed
                        // a follow-up then hit /clear and lost it" is
                        // recoverable from the log.
                        let previews: Vec<String> = s
                            .pending_messages
                            .iter()
                            .map(|b| queue::summarize_blocks_for_log(b))
                            .collect();
                        log::warn!(
                            target: "solution_agent::queue",
                            "session={session_id} dropped {} queued bundle(s) on /clear (reset_context) — content: [{}]",
                            s.pending_messages.len(),
                            previews.join(" | "),
                        );
                    }
                    s.acp_session_id = new_acp_session_id;
                    s.state = SessionState::Idle;
                    s.last_activity_at = Utc::now();
                    s.pending_messages.clear();
                    s.flush_after_cancel = false;
                    // Status-row meter falls back to `cached_total_tokens`
                    // when the live thread has no `token_usage` yet — the
                    // freshly-spawned thread does not. Without a reset
                    // here the meter would keep reading the pre-`/clear`
                    // count (the bug this whole change exists to fix).
                    // `last_turn_duration` is cleared for the same reason
                    // — "Done in Xs" must not survive a context wipe.
                    s.cached_total_tokens = None;
                    s.last_turn_duration = None;
                    s.cold_entries.clear();
                    s.entry_created_ms.clear();
                    // Cache the (possibly freshly-built headless) project so
                    // a subsequent reset/restart on this now-live session
                    // doesn't have to rebuild it.
                    s.project = Some(project.clone());
                    // `set_acp_thread` emits ThreadReplaced + notify;
                    // last so SessionView re-attaches against a fully
                    // wiped session struct.
                    s.set_acp_thread(Some(new_thread.clone()), cx);
                    had_pending
                });
                let new_sub = store.subscribe_to_session(session_id, new_thread, cx);
                session_entity.update(cx, |s, _| s._acp_subscription = Some(new_sub));
                store.persist_session_row(session_id, cx);
                // `reset_context` does not bump `context_count` (only
                // `rotate_context` does), so read the current value to
                // forward as-is on the wire.
                let context_count = session_entity.read(cx).context_count;
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
                store.emit_session_state_changed_workspace(&session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionContextReset {
                    id: session_id,
                    context_count,
                });
                if had_pending {
                    cx.emit(SolutionAgentStoreEvent::SessionQueueChanged(session_id));
                }
                cx.notify();
            })?;

            Ok(session_id)
        })
    }

    /// Returns a clone of the persistence handle if one was configured
    /// (i.e. the editor is running with a real on-disk DB, not the test
    /// in-memory mode). Used by MCP tools that need to read archived
    /// session blobs without re-hydrating the full session.
    pub fn persistence(&self) -> Option<Arc<crate::db::SolutionAgentDb>> {
        self.persistence.clone()
    }

    /// Persists the tab strip's open-session order for `solution_id`.
    /// Sessions in `ordered_ids` get `tab_order = 0..N`; everything else
    /// for the solution is set to `tab_order = NULL`. Called from the
    /// navigator on reorder, open, and close so the strip survives an
    /// editor restart.
    pub fn persist_tab_order(
        &self,
        solution_id: SolutionId,
        ordered_ids: Vec<SolutionSessionId>,
        cx: &mut Context<Self>,
    ) {
        // Capture the OLD set of in-strip session ids (tab_order.is_some())
        // BEFORE the apply mutates in-memory state.
        let old_set: std::collections::HashSet<SolutionSessionId> = self
            .sessions
            .values()
            .filter_map(|entity| {
                let s = entity.read(cx);
                if s.solution_id == solution_id && s.tab_order.is_some() {
                    Some(s.id)
                } else {
                    None
                }
            })
            .collect();

        // Update the in-memory field first (synchronous, on the foreground
        // thread) so that `workspace.snapshot` sees the new strip state
        // immediately — before the async DB write completes.
        self.apply_tab_order_to_memory(&solution_id, &ordered_ids, cx);

        // Compute NEW set from the ordered_ids that were just applied.
        let new_set: std::collections::HashSet<SolutionSessionId> =
            ordered_ids.iter().cloned().collect();

        // Diff and emit one workspace.session_opened / workspace.session_closed
        // per actual transition so downstream clients stay in sync without a
        // full snapshot refresh. Guard with `try_global` so test contexts that
        // don't install the MCP layer don't panic.
        let opened_ids: Vec<SolutionSessionId> =
            new_set.difference(&old_set).copied().collect();
        let closed_ids: Vec<SolutionSessionId> =
            old_set.difference(&new_set).copied().collect();
        if let Some(coord) =
            editor_mcp::workspace_seq::WorkspaceEventCoordinator::try_global(cx)
        {
            for opened_id in &opened_ids {
                if let Some(entity) = self.sessions.get(opened_id) {
                    let summary =
                        entity.read_with(cx, |s, cx| crate::mcp::session_summary(s, cx));
                    coord.emit_sequenced(cx, "workspace.session_opened", serde_json::json!({
                        "solution_id": solution_id.as_str(),
                        "session": summary,
                    }));
                }
            }
            for closed_id in &closed_ids {
                coord.emit_sequenced(cx, "workspace.session_closed", serde_json::json!({
                    "solution_id": solution_id.as_str(),
                    "session_id": closed_id.to_string(),
                }));
            }
        }

        // Local fan-out: the ConsolePanel observes this to add / remove the
        // actual tab on the desktop strip in response to mutations driven
        // from outside the panel (notably the wire-side
        // `workspace.{open,close}_session` RPCs from mobile clients).
        // Always emit, even when both lists are empty (a pure reorder) —
        // future consumers may want to react to that too; current
        // `ConsolePanel` subscriber filters out the empty case.
        cx.emit(SolutionAgentStoreEvent::TabsChanged {
            solution_id: solution_id.clone(),
            opened: opened_ids,
            closed: closed_ids,
        });

        let Some(db) = self.persistence.clone() else {
            return;
        };
        cx.background_spawn(async move {
            db.update_tab_orders(solution_id, ordered_ids)
                .await
                .log_err();
        })
        .detach();
    }

    /// Update the in-memory `tab_order` field on every session that belongs to
    /// `solution_id`. Sessions whose id appears in `ordered_ids` receive their
    /// 0-based index; all others are cleared to `None` (tab closed / hidden).
    ///
    /// Must be called from the foreground thread (takes `cx` for entity access).
    fn apply_tab_order_to_memory(
        &self,
        solution_id: &SolutionId,
        ordered_ids: &[SolutionSessionId],
        cx: &mut Context<Self>,
    ) {
        for entity in self.sessions.values() {
            let entity = entity.clone();
            let belongs = entity.read(cx).solution_id == *solution_id;
            if !belongs {
                continue;
            }
            let id = entity.read(cx).id;
            let new_order = ordered_ids.iter().position(|oid| *oid == id).map(|i| i as i64);
            entity.update(cx, |s, _| s.tab_order = new_order);
        }
    }

    /// Schedule a debounce-friendly write of the session's serialised snapshot
    /// to the persistence backend (if configured). The serialisation runs on
    /// the foreground thread because it must read the `AcpThread` entity; the
    /// SQLite write itself is dispatched to the background executor by
    /// `SolutionAgentDb::save_blob`.
    pub fn persist_session_blob(&self, session_id: SolutionSessionId, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(&session_id).cloned() else {
            return;
        };
        let Some(db) = self.persistence.clone() else {
            return;
        };
        cx.spawn(async move |_this, cx: &mut AsyncApp| {
            let blob: Vec<u8> = cx.update(|cx| {
                let s = session.read(cx);
                serializable_snapshot(s, cx)
            });
            if !blob.is_empty() {
                db.save_blob(session_id, blob).await.log_err();
            }
        })
        .detach();
    }

    /// Subscribe to a session's `AcpThread` event stream so that ACP-level
    /// state changes (turn completion, tool authorization, errors, etc.)
    /// translate into `SessionState` transitions on `SolutionSession`.
    /// Returns the `Subscription` — caller must store it on the session
    /// (in `_acp_subscription`) or it will drop and unsubscribe immediately.
    fn subscribe_to_session(
        &mut self,
        session_id: SolutionSessionId,
        acp_thread: Entity<acp_thread::AcpThread>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(&acp_thread, move |store, _thread, event, cx| {
            store.handle_acp_event(session_id, event, cx);
        })
    }

    /// Subagent-tab lifecycle hook. Inspects the entry at `entry_index` in
    /// the session's live `AcpThread` and:
    ///   * if it's a brand-new `Task`/`Agent` ToolCall in `InProgress` and
    ///     not already tracked → registers it on
    ///     `SolutionSession::active_subagents` (+ insertion-order vec) and
    ///     emits [`SolutionAgentStoreEvent::SessionSubagentsChanged`];
    ///   * if it's a tracked id whose status just flipped to a terminal
    ///     state (`Completed`/`Failed`/`Rejected`/`Canceled`) → removes it
    ///     and emits the same event.
    ///
    /// Any other shape (non-tool entry, non-Task tool, status still
    /// `InProgress`/`Pending` on an already-tracked id, terminal status on
    /// an unknown id) is a no-op and emits nothing. Map mutations are gated
    /// behind a structural check to keep `SessionSubagentsChanged` from
    /// firing on every chunk of a streaming Task subagent's body.
    ///
    /// The cold-thread branch is excluded: an entry only exists in a live
    /// `AcpThread`, so when the session is cold (`acp_thread()` is `None`)
    /// there is nothing to track yet. The next live attach will replay the
    /// in-flight tool calls through `NewEntry`, which re-enters this hook.
    fn apply_subagent_lifecycle(
        &mut self,
        session_id: SolutionSessionId,
        entry_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(session_entity) = self.sessions.get(&session_id).cloned() else {
            return;
        };
        // Capture the relevant ToolCall fields in a small read scope so we
        // can mutate the session entity right after without overlapping
        // borrows.
        struct Snapshot {
            id: SharedString,
            is_task_like: bool,
            is_in_progress: bool,
            is_terminal: bool,
            label_from_raw_input: Option<SharedString>,
            subagent_type: Option<String>,
            /// The tool's programmatic name (e.g. `"Task"`, `"Agent"`)
            /// captured so the post-lifecycle branch can dispatch on
            /// `eq_ignore_ascii_case("agent")` without re-borrowing the
            /// entry from the thread.
            tool_name: Option<String>,
            /// JSON-encoded `raw_output` payload (only meaningful for the
            /// terminal `Agent` branch — claude's managed-agent dispatcher
            /// stashes `agentId` + `output_file` here when the tool call
            /// completes). Empty for in-progress / non-Agent calls.
            raw_output_text: Option<String>,
        }
        let snapshot = {
            let session = session_entity.read(cx);
            let Some(thread) = session.acp_thread() else {
                return;
            };
            let thread_ref = thread.read(cx);
            let Some(entry) = thread_ref.entries().get(entry_index) else {
                return;
            };
            let acp_thread::AgentThreadEntry::ToolCall(call) = entry else {
                return;
            };
            let tool_name = call
                .tool_name
                .as_ref()
                .map(|s| s.as_ref())
                .unwrap_or_default();
            let is_task_like = matches!(tool_name, "Task" | "Agent");
            let is_in_progress = matches!(call.status, acp_thread::ToolCallStatus::InProgress);
            let is_terminal = matches!(
                call.status,
                acp_thread::ToolCallStatus::Completed
                    | acp_thread::ToolCallStatus::Failed
                    | acp_thread::ToolCallStatus::Rejected
                    | acp_thread::ToolCallStatus::Canceled
            );
            let (label_from_raw_input, subagent_type) = match call.raw_input.as_ref() {
                Some(raw) => {
                    let desc = raw
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| SharedString::from(s.to_owned()));
                    let stype = raw
                        .get("subagent_type")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_owned());
                    (desc, stype)
                }
                None => (None, None),
            };
            let tool_name_owned = if tool_name.is_empty() {
                None
            } else {
                Some(tool_name.to_string())
            };
            let raw_output_text = call
                .raw_output
                .as_ref()
                .and_then(|v| serde_json::to_string(v).ok());
            Snapshot {
                id: SharedString::from(call.id.0.to_string()),
                is_task_like,
                is_in_progress,
                is_terminal,
                label_from_raw_input,
                subagent_type,
                tool_name: tool_name_owned,
                raw_output_text,
            }
        };

        if !snapshot.is_task_like {
            return;
        }
        let id = snapshot.id;

        let changed = if snapshot.is_in_progress {
            // Defensive: a duplicate NewEntry for the same id (or an
            // InProgress→InProgress EntryUpdated as raw_input streams in) must
            // not re-insert or re-emit. Only the first observation registers
            // the tab.
            let already_tracked = session_entity
                .read(cx)
                .active_subagents
                .contains_key(&id);
            if already_tracked {
                // Label is intentionally locked at first observation. Later
                // EntryUpdated events that finally fill in raw_input.description
                // are discarded here on purpose — otherwise a streamed tool_use
                // input would relabel the tab mid-flight and flicker the strip.
                false
            } else {
                let label = snapshot
                    .label_from_raw_input
                    .unwrap_or_else(|| label_fallback(&id, snapshot.subagent_type.as_deref()));
                let id_for_closure = id.clone();
                session_entity.update(cx, |s, _| {
                    s.active_subagents.insert(
                        id_for_closure.clone(),
                        SubagentTab {
                            label,
                            started_at: chrono::Utc::now(),
                        },
                    );
                    s.active_subagent_order.push(id_for_closure);
                });
                true
            }
        } else if snapshot.is_terminal {
            // Symmetric defensive guard: a terminal-status EntryUpdated on an
            // id we never registered (e.g. the InProgress event arrived after
            // a status flip on a cold→live transition) is a no-op.
            let tracked = session_entity
                .read(cx)
                .active_subagents
                .contains_key(&id);
            if tracked {
                session_entity.update(cx, |s, _| {
                    s.active_subagents.remove(&id);
                    s.active_subagent_order.retain(|tracked_id| tracked_id != &id);
                });
                true
            } else {
                false
            }
        } else {
            // Pending / WaitingForConfirmation transitions on a Task/Agent
            // tool call are not lifecycle signals — claude almost never goes
            // through these for subagents (they spawn directly into
            // InProgress), but be defensive in case future SDK shapes do.
            false
        };

        if changed {
            cx.emit(SolutionAgentStoreEvent::SessionSubagentsChanged(session_id));
        }

        // Managed-agent registration (Task 8 of the Background Agents Strip
        // plan). claude_code's `Agent` tool is its async sub-agent dispatch;
        // when the call completes its `raw_output` carries `agentId: <hex>`
        // + `output_file: <path>.output` so we can tail the JSONL transcript
        // the worker is appending to. We register a `BackgroundAgent` for
        // every fresh announcement and spawn the per-session directory
        // watcher (idempotent — `ensure_background_agent_watcher` no-ops on
        // a duplicate call). The Task branch above already removed the
        // subagent pill, so the Agent dispatch briefly shows as an active
        // subagent and then transitions to a background-agent strip entry —
        // matches the pre-feature behaviour for `Task` and adds the strip
        // on top.
        if snapshot.is_terminal && tool_name_is_agent(snapshot.tool_name.as_deref()) {
            let raw_output_text = snapshot.raw_output_text.unwrap_or_default();
            if let Some((agent_id_str, output_file)) =
                crate::background_agent::parse_managed_agent_announcement(&raw_output_text)
            {
                let canonical =
                    std::fs::read_link(&output_file).unwrap_or_else(|_| output_file.clone());
                let id = crate::background_agent::BackgroundAgentId::new(agent_id_str);
                let already = session_entity
                    .read(cx)
                    .background_agents
                    .contains_key(&id);
                if !already {
                    let id_for_insert = id.clone();
                    let path_for_insert = canonical.clone();
                    session_entity.update(cx, |s, _| {
                        s.background_agents.insert(
                            id_for_insert.clone(),
                            crate::background_agent::BackgroundAgent {
                                id: id_for_insert.clone(),
                                jsonl_path: path_for_insert,
                                registered_at: chrono::Utc::now(),
                                latest: None,
                            },
                        );
                        s.background_agent_order.push(id_for_insert);
                    });
                    cx.emit(SolutionAgentStoreEvent::SessionBackgroundAgentsChanged(
                        session_id,
                    ));

                    // Persist to SQLite if the store has a backing DB.
                    // In-memory test stores leave `persistence` as `None`
                    // and rely on the in-RAM map only.
                    if let Some(db) = self.persistence.clone() {
                        let row = crate::db::BackgroundAgentRow {
                            solution_session_id: session_id.to_string(),
                            agent_id: id.as_str().to_string(),
                            jsonl_path: canonical.to_string_lossy().into_owned(),
                            registered_at_ms: chrono::Utc::now().timestamp_millis(),
                            last_seen_label: None,
                            last_mtime_ms: None,
                            stop_reason: None,
                        };
                        cx.background_spawn(async move {
                            db.save_background_agent(row).await.log_err();
                        })
                        .detach();
                    }

                    // The watcher needs a `fs::Fs` handle. `SolutionAgentStore`
                    // has no `fs` field; source it from the session's project
                    // (most live sessions have one). A session without a
                    // project just skips the watcher — the row is still
                    // registered and the UI can render the pill, but live
                    // tailing waits for a project attach.
                    if let Some(fs) = session_entity
                        .read(cx)
                        .project
                        .as_ref()
                        .map(|p| p.read(cx).fs().clone())
                    {
                        self.ensure_background_agent_watcher(session_id, fs, cx);
                    }

                    // Close the registration→watcher-subscribe race window:
                    // claude writes the first JSONL line nearly instantly
                    // after `Agent` returns, but `fs.watch` resolves on a
                    // background task — so without an inline refresh the
                    // first snapshot can be missed entirely and the pill
                    // would sit at the default `Generating…` until the
                    // sub-agent's next write.
                    self.refresh_background_agent_snapshot(session_id, id, cx);
                }
            }
        }
    }

    /// Spawn (idempotently) a per-session watcher on the
    /// `~/.claude/projects/<encoded-cwd>/<session-id>/subagents/`
    /// directory. Each `PathEvent` on an `agent-<id>.jsonl` filename
    /// triggers a `refresh_background_agent_snapshot` for the matching
    /// tracked `BackgroundAgent`. The watcher task lives in
    /// `background_agent_watchers` keyed by `session_id` — drop the
    /// entry (or drop the store) to cancel.
    ///
    /// Called from the tool-call handler (Task 8) when claude announces
    /// a managed agent. Safe to call repeatedly: a second call for the
    /// same session is a no-op.
    pub(crate) fn ensure_background_agent_watcher(
        &mut self,
        session_id: SolutionSessionId,
        fs: Arc<dyn fs::Fs>,
        cx: &mut Context<Self>,
    ) {
        if self.background_agent_watchers.contains_key(&session_id) {
            return;
        }
        let Some(session) = self.session(session_id) else {
            return;
        };
        let acp_session_id = session.read(cx).acp_session_id.clone();
        let cwd = session.read(cx).cwd.clone();
        let subagents_dir = match background_agent_dir_for(&cwd, acp_session_id.0.as_ref()) {
            Some(p) => p,
            None => {
                log::warn!(
                    "background_agents: cannot resolve subagents dir for session {}",
                    session_id
                );
                return;
            }
        };
        let task = cx.spawn(async move |this, cx| {
            let (mut stream, _watcher) = fs
                .watch(&subagents_dir, std::time::Duration::from_millis(200))
                .await;
            use futures::StreamExt;
            while let Some(events) = stream.next().await {
                for event in events {
                    let Some(name) = event.path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    if !name.starts_with("agent-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    let agent_id_str = name
                        .trim_start_matches("agent-")
                        .trim_end_matches(".jsonl")
                        .to_string();
                    // Dropping the Result is the established cancellation
                    // signal: if the store entity is gone, the watcher
                    // task is about to be dropped anyway.
                    let _ = this.update(cx, |this, cx| {
                        this.refresh_background_agent_snapshot(
                            session_id,
                            crate::background_agent::BackgroundAgentId::new(agent_id_str),
                            cx,
                        );
                    });
                }
            }
        });
        self.background_agent_watchers.insert(session_id, task);
    }

    /// Tail the JSONL file for `agent_id` on `session_id`, parse the
    /// last line into a [`BackgroundAgentSnapshot`], write it to
    /// `BackgroundAgent::latest`, and emit
    /// [`SolutionAgentStoreEvent::SessionBackgroundAgentsChanged`] iff
    /// the snapshot was actually stored. No-op when the session has
    /// gone away, the agent isn't tracked anymore, the file can't be
    /// read, or it has no usable last line.
    pub(crate) fn refresh_background_agent_snapshot(
        &mut self,
        session_id: SolutionSessionId,
        agent_id: crate::background_agent::BackgroundAgentId,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session(session_id) else {
            return;
        };
        let Some(jsonl_path) = session
            .read(cx)
            .background_agents
            .get(&agent_id)
            .map(|ba| ba.jsonl_path.clone())
        else {
            return;
        };
        let tail = match crate::background_agent::tail_jsonl(&jsonl_path, 0) {
            Ok(t) => t,
            Err(_) => return,
        };
        let Some(line) = tail.last_line else { return };
        let mut snapshot = crate::background_agent::parse_jsonl_snapshot(&line);
        snapshot.mtime = tail.mtime;
        let mut changed = false;
        session.update(cx, |s, _| {
            if let Some(ba) = s.background_agents.get_mut(&agent_id) {
                ba.latest = Some(snapshot);
                changed = true;
            }
        });
        if changed {
            cx.emit(SolutionAgentStoreEvent::SessionBackgroundAgentsChanged(
                session_id,
            ));
        }
    }

    /// One pass over every session's background agents. Removes agents
    /// whose latest snapshot carries a `stop_reason` (terminal done),
    /// plus agents that have been silently dead beyond
    /// `MANAGED_AGENT_STALE_TIMEOUT + MANAGED_AGENT_DEAD_LINGER`. Dead
    /// detection itself (orange pill) is rendering-side using the same
    /// stale timeout — the tick just drops the entries that have
    /// fully expired.
    pub fn tick_background_agents(&mut self, cx: &mut Context<Self>) {
        let now = std::time::SystemTime::now();
        let session_ids: Vec<SolutionSessionId> =
            self.all_sessions().map(|e| e.read(cx).id).collect();
        for session_id in session_ids {
            let Some(session) = self.session(session_id) else {
                continue;
            };
            // Skip sessions with no registered agents — the vast majority of
            // sessions never spawn a managed agent, and `update` is not free.
            if session.read(cx).background_agents.is_empty() {
                continue;
            }
            let to_remove: Vec<crate::background_agent::BackgroundAgentId> =
                session.update(cx, |s, _| {
                    let candidates: Vec<crate::background_agent::BackgroundAgentId> = s
                        .background_agent_order
                        .iter()
                        .filter(|id| {
                            let Some(ba) = s.background_agents.get(id) else {
                                return false;
                            };
                            let Some(snap) = ba.latest.as_ref() else {
                                return false;
                            };
                            if snap.stop_reason.is_some() {
                                return true;
                            }
                            let elapsed =
                                now.duration_since(snap.mtime).unwrap_or_default();
                            elapsed > MANAGED_AGENT_STALE_TIMEOUT + MANAGED_AGENT_DEAD_LINGER
                        })
                        .cloned()
                        .collect();
                    for id in &candidates {
                        s.background_agents.remove(id);
                        s.background_agent_order.retain(|x| x != id);
                    }
                    candidates
                });
            if !to_remove.is_empty() {
                cx.emit(SolutionAgentStoreEvent::SessionBackgroundAgentsChanged(
                    session_id,
                ));
                if let Some(db) = self.persistence.clone() {
                    let session_id_string = session_id.to_string();
                    for agent_id in to_remove {
                        let db = db.clone();
                        let session_id_string = session_id_string.clone();
                        let agent_id_string = agent_id.as_str().to_string();
                        cx.background_spawn(async move {
                            db.delete_background_agent(session_id_string, agent_id_string)
                                .await
                                .log_err();
                        })
                        .detach();
                    }
                }
            }
        }
    }

    fn handle_acp_event(
        &mut self,
        session_id: SolutionSessionId,
        event: &acp_thread::AcpThreadEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(session_entity) = self.sessions.get(&session_id).cloned() else {
            return;
        };
        match event {
            acp_thread::AcpThreadEvent::NewEntry => {
                self.mutate_state(
                    session_id,
                    |state| {
                        if matches!(state, SessionState::Idle | SessionState::AwaitingInput) {
                            *state = SessionState::Running {
                                started_at: std::time::Instant::now(),
                                notified: false,
                            };
                        }
                    },
                    cx,
                );
                if let Some(s) = self.sessions.get(&session_id).cloned() {
                    s.update(cx, |s, _| s.last_activity_at = Utc::now());
                }
                // First user message appends a NewEntry — refresh DB so the
                // History popover preview stops being NULL.
                self.persist_session_row(session_id, cx);
                // Also flush the transcript blob: a mid-Running crash/restart
                // used to lose every entry added since the last successful
                // turn end (`persist_session_blob` was called only from the
                // queue's success path). Re-snapshot on every new entry so a
                // resume after a crash shows up-to-date history.
                self.persist_session_blob(session_id, cx);
                // `entry_index` on AcpThreadEvent is LOCAL to the live
                // thread's entries vector. `entry_created_ms` is sized
                // over the GLOBAL cold+live concatenation (mirrors the
                // virtualized list, the persisted blob, and the render
                // path), so we offset by `cold_count` before stamping.
                // Without the offset, the first live entry after a
                // cold→live transition would land on the cold[0] slot,
                // overwriting the persisted timestamp of the first
                // cold message.
                let (cold_count, entry_index) = {
                    let session = session_entity.read(cx);
                    let cold = session.cold_entries.len();
                    let live_last = session
                        .acp_thread()
                        .map(|thread| thread.read(cx).entries().len().saturating_sub(1))
                        .unwrap_or(0);
                    (cold, cold + live_last)
                };
                let _ = cold_count; // recorded for clarity; the global index already folds it in
                // Stamp creation time the first time an absolute index appears. The
                // vector length is the high-water mark: a streamed in-place
                // EntryUpdated reuses an existing index and must not grow or
                // rewrite the vector.
                let now_ms = Utc::now().timestamp_millis();
                session_entity.update(cx, |s, _| {
                    // Fill any gap below the new index with the absent sentinel: those are
                    // pre-existing entries (e.g. a resumed pre-feature session's history) whose
                    // real creation time we never captured — we must not fabricate it. Only the
                    // genuinely-new entry that just arrived at `entry_index` gets `now_ms`.
                    while s.entry_created_ms.len() < entry_index {
                        s.entry_created_ms.push(crate::model::NO_TIMESTAMP_MS);
                    }
                    if s.entry_created_ms.len() == entry_index {
                        s.entry_created_ms.push(now_ms);
                    }
                    // len > entry_index → in-place EntryUpdated on an existing entry: leave it.
                });
                cx.emit(SolutionAgentStoreEvent::SessionMessageAppended(
                    session_id,
                    entry_index,
                ));
                // Subagent-tab lifecycle: a brand-new Task/Agent ToolCall in
                // InProgress is a spawn signal. The `local_entry_index` here is
                // the live thread's local index (entries.len() - 1), which is
                // what `apply_subagent_lifecycle` needs to look up the entry.
                let local_entry_index = session_entity
                    .read(cx)
                    .acp_thread()
                    .map(|thread| thread.read(cx).entries().len().saturating_sub(1));
                if let Some(idx) = local_entry_index {
                    self.apply_subagent_lifecycle(session_id, idx, cx);
                }
            }
            acp_thread::AcpThreadEvent::Stopped(_) => {
                // Snapshot the Running turn's elapsed time BEFORE the
                // state flip — `mutate_state` overwrites `started_at`
                // with `SessionState::Idle` so we can't recover it
                // after. Stamped onto the session for the status row's
                // "Done in Xs" indicator (cleared on the next Running).
                let elapsed = self.sessions.get(&session_id).and_then(|entity| {
                    if let SessionState::Running { started_at, .. } = &entity.read(cx).state {
                        Some(started_at.elapsed())
                    } else {
                        None
                    }
                });
                self.mutate_state(session_id, |state| *state = SessionState::Idle, cx);
                if let Some(s) = self.sessions.get(&session_id).cloned() {
                    s.update(cx, |s, _| {
                        s.last_activity_at = Utc::now();
                        if let Some(d) = elapsed {
                            s.last_turn_duration = Some(d);
                        }
                    });
                    // Emit a metrics notification on turn completion so the
                    // mobile client sees an updated last_activity_at without
                    // waiting for the next TokenUsageUpdated. Throttled
                    // (2 s window) and non-sequenced per spec.
                    let (last_activity_at, total_tokens, max_tokens) = {
                        let r = s.read(cx);
                        (r.last_activity_at, r.cached_total_tokens, r.cached_max_tokens)
                    };
                    self.metrics_emitter.emit_if_ready(
                        cx,
                        &session_id,
                        serde_json::json!({
                            "session_id": session_id.to_string(),
                            "last_activity_at": last_activity_at,
                            "total_tokens": total_tokens,
                            "max_tokens": max_tokens,
                        }),
                    );
                }
                // Token usage is finalised on turn completion — refresh DB
                // so the History popover token column reflects the latest.
                self.persist_session_row(session_id, cx);
                // Flush queued follow-ups (if any). All pending entries
                // are drained and concatenated into ONE send — the user
                // typed them as a fast-fire stream while the agent was
                // working, so it's their joint intent for the next turn
                // rather than N independent prompts. A Cancelled stop
                // (user pressed Stop) is treated as "abandon what I
                // queued too": the queue is cleared without sending.
                if let acp_thread::AcpThreadEvent::Stopped(reason) = event {
                    // `flush_after_cancel` (set by `interrupt_and_flush_pending`)
                    // flips Cancelled's default semantics from "abandon the
                    // queue too" to "cancel the current turn but immediately
                    // start the next one with the queued follow-ups". One-
                    // shot — clear the flag whether or not the queue had
                    // anything left to send.
                    let flush_after_cancel = self
                        .sessions
                        .get(&session_id)
                        .map(|s| {
                            s.update(cx, |s, _| {
                                let was = s.flush_after_cancel;
                                s.flush_after_cancel = false;
                                was
                            })
                        })
                        .unwrap_or(false);
                    let cancelled =
                        matches!(reason, agent_client_protocol::schema::StopReason::Cancelled);
                    if cancelled && !flush_after_cancel {
                        // Silent-drop path: user pressed Stop, queue
                        // gets discarded without surfacing what was in
                        // it. Log the dropped bundles BEFORE the clear
                        // so post-mortem of "where did my queued
                        // message go?" can reconstruct it from the
                        // log line. WARN level (not INFO) — this is
                        // user-typed content vanishing without a
                        // trace, which is exactly the failure mode we
                        // want to be able to grep for.
                        let had_pending = if let Some(s) = self.sessions.get(&session_id).cloned() {
                            s.update(cx, |s, _| {
                                let dropped = s.pending_messages.len();
                                if dropped > 0 {
                                    let previews: Vec<String> = s
                                        .pending_messages
                                        .iter()
                                        .map(|bundle| {
                                            queue::summarize_blocks_for_log(bundle)
                                        })
                                        .collect();
                                    log::warn!(
                                        target: "solution_agent::queue",
                                        "session={session_id} dropped {dropped} queued bundle(s) on Cancelled stop \
                                         (no flush_after_cancel) — content: [{}]",
                                        previews.join(" | "),
                                    );
                                }
                                s.pending_messages.clear();
                                dropped > 0
                            })
                        } else {
                            false
                        };
                        if had_pending {
                            cx.emit(SolutionAgentStoreEvent::SessionQueueChanged(session_id));
                        }
                    } else {
                        let drained: Vec<_> = self
                            .sessions
                            .get(&session_id)
                            .cloned()
                            .map(|s| {
                                s.update(cx, |s, _| {
                                    s.pending_messages.drain(..).collect::<Vec<_>>()
                                })
                            })
                            .unwrap_or_default();
                        let had_pending = !drained.is_empty();
                        if had_pending {
                            cx.emit(SolutionAgentStoreEvent::SessionQueueChanged(session_id));
                        }
                        if !drained.is_empty() {
                            let bundle_count = drained.len();
                            // Flatten N queued messages into one Vec.
                            // Each was its own send-press, but we coalesce
                            // them so the agent gets a single prompt.
                            let combined: Vec<_> = drained.into_iter().flatten().collect();
                            if !combined.is_empty() {
                                log::info!(
                                    target: "solution_agent::queue",
                                    "session={session_id} flushing {bundle_count} queued bundle(s) \
                                     ({} blocks total, flush_after_cancel={flush_after_cancel}) preview={}",
                                    combined.len(),
                                    queue::summarize_blocks_for_log(&combined),
                                );
                                self.send_message_blocks(session_id, combined, cx).detach();
                            }
                        }
                    }
                }
            }
            acp_thread::AcpThreadEvent::TokenUsageUpdated => {
                // claude-acp ships incremental usage during a turn, not
                // just at the end. Persist on every update so a session
                // closed mid-turn (or right before `Stopped` fires)
                // resumes with the correct meter — without this the DB
                // value lags behind the live meter and a resume drops
                // back to whatever the previous Stopped wrote.
                // Also mirror the new total onto `cached_total_tokens`
                // so the next cold-restore (or any read of the session
                // entity bypassing the live thread) sees the latest
                // figure without the meter regressing to zero.
                if let Some(s) = self.sessions.get(&session_id).cloned() {
                    let usage = s
                        .read(cx)
                        .acp_thread()
                        .and_then(|t| t.read(cx).token_usage().cloned());
                    let total = usage.as_ref().map(|u| u.used_tokens);
                    // `max_tokens == 0` is the "agent didn't fill it in"
                    // sentinel claude-acp ships under some beta paths.
                    // Treat that as None so MCP consumers can fall back
                    // to `DEFAULT_CONTEXT_WINDOW` instead of rendering
                    // "X / 0" on the meter.
                    let max = usage.as_ref().map(|u| u.max_tokens).filter(|m| *m > 0);
                    s.update(cx, |s, _| {
                        s.cached_total_tokens = total;
                        s.cached_max_tokens = max;
                    });
                    // Throttled non-sequenced notification — at most one
                    // emit per 2 s per session. The client treats a
                    // missed metric notify as "check on next snapshot
                    // resync"; no gap-detection or seq field needed.
                    let (last_activity_at, total_tokens, max_tokens) = {
                        let r = s.read(cx);
                        (r.last_activity_at, r.cached_total_tokens, r.cached_max_tokens)
                    };
                    self.metrics_emitter.emit_if_ready(
                        cx,
                        &session_id,
                        serde_json::json!({
                            "session_id": session_id.to_string(),
                            "last_activity_at": last_activity_at,
                            "total_tokens": total_tokens,
                            "max_tokens": max_tokens,
                        }),
                    );
                }
                self.persist_session_row(session_id, cx);
            }
            acp_thread::AcpThreadEvent::Error | acp_thread::AcpThreadEvent::LoadError(_) => {
                self.mutate_state(
                    session_id,
                    |state| *state = SessionState::Errored(SharedString::from("agent error")),
                    cx,
                );
            }
            acp_thread::AcpThreadEvent::ToolAuthorizationRequested(_) => {
                self.mutate_state(session_id, |state| *state = SessionState::AwaitingInput, cx);
            }
            acp_thread::AcpThreadEvent::ToolAuthorizationReceived(_) => {
                self.mutate_state(
                    session_id,
                    |state| {
                        if matches!(state, SessionState::AwaitingInput) {
                            *state = SessionState::Running {
                                started_at: std::time::Instant::now(),
                                notified: false,
                            };
                        }
                    },
                    cx,
                );
            }
            acp_thread::AcpThreadEvent::TitleUpdated => {
                let new_title = session_entity
                    .read(cx)
                    .acp_thread()
                    .and_then(|t| t.read(cx).title())
                    .unwrap_or_default();
                session_entity.update(cx, |s, _| s.title = new_title);
                cx.emit(SolutionAgentStoreEvent::SessionTitleChanged(session_id));
            }
            acp_thread::AcpThreadEvent::EntriesRemoved(range) => {
                // Keep the parallel timestamp vector aligned: a rewind truncates the
                // thread to `range.start`, so drop every stamp at or after it.
                session_entity.update(cx, |s, _| {
                    s.entry_created_ms.truncate(range.start);
                });
                // The user-facing `/clear` does NOT reach this branch:
                // it's intercepted client-side and routed through
                // `reset_context` (which spawns a brand-new `AcpThread`
                // and never emits `EntriesRemoved`); the corresponding
                // token-meter reset lives at the swap site in
                // `reset_context` / `rotate_context`.
                //
                // What this branch covers is a thread-local truncation
                // that happens to remove every entry — today the only
                // in-tree producer is `acp_thread::rewind` /
                // refusal-truncate (`acp_thread.rs:2369`, `:2491`)
                // when rewinding to before the very first user message.
                // The post-event `entries().is_empty()` check
                // discriminates this "rewind to zero" case from a
                // partial rewind: the latter leaves a surviving
                // prefix whose token usage is still meaningful, and
                // the agent will emit a fresh `TokenUsageUpdated`
                // against that prefix on the next turn — so we MUST
                // NOT preemptively wipe state in the partial case.
                let thread = session_entity.read(cx).acp_thread().cloned();
                let cleared = thread
                    .as_ref()
                    .map(|t| t.read(cx).entries().is_empty())
                    .unwrap_or(false);
                if cleared {
                    if let Some(t) = thread {
                        t.update(cx, |t, cx| t.update_token_usage(None, cx));
                    }
                    session_entity.update(cx, |s, _| {
                        s.cached_total_tokens = None;
                        s.last_turn_duration = None;
                    });
                    self.persist_session_row(session_id, cx);
                }
            }
            acp_thread::AcpThreadEvent::EntryUpdated(idx) => {
                // Subagent-tab lifecycle: a tracked Task/Agent ToolCall that
                // just flipped to a terminal status is a finish signal. We
                // run this BEFORE the EntryUpdated throttle plumbing so the
                // `SessionSubagentsChanged` emit happens on the same tick
                // the parent thread's `EntryUpdated` is observed, without
                // waiting for the 500 ms debounce that gates
                // `SessionMessageAppended`.
                self.apply_subagent_lifecycle(session_id, *idx, cx);
                // Tool-call arg deltas, assistant-text chunks, and tool-
                // status transitions on an existing entry all surface
                // here. The pre-fix behaviour fell through to the
                // `_ => {}` catch-all, so external MCP consumers (the
                // Android client) never learned the entry changed and
                // displayed only the initial empty `args_preview = "{}"`
                // for a tool call or the first preview snapshot of a
                // streaming assistant reply.
                //
                // Coalesced via a trailing-edge debounce: a 500 ms quiet
                // window collapses a token-by-token streaming burst
                // into roughly 2 emits/sec, and a 2 s max-stale guard
                // forces an emit when an entry is continuously dirty so
                // the consumer doesn't starve. Replacing an entry in
                // `entry_update_throttles` drops the previous `Task`,
                // which cancels its inflight timer → only the latest
                // debounce window's task survives to fire.
                let key = (session_id, *idx);
                let now = std::time::Instant::now();
                let existing_first_dirty_at = self
                    .entry_update_throttles
                    .get(&key)
                    .map(|t| t.first_dirty_at);
                let max_stale_breached = existing_first_dirty_at
                    .map(|t| {
                        now.saturating_duration_since(t) >= std::time::Duration::from_millis(2000)
                    })
                    .unwrap_or(false);
                if max_stale_breached {
                    self.entry_update_throttles.remove(&key);
                    cx.emit(SolutionAgentStoreEvent::SessionMessageAppended(
                        session_id, *idx,
                    ));
                    // Crash-recovery: flush the transcript blob on the same
                    // trailing-edge so a mid-Running restart doesn't lose
                    // streamed text since the last NewEntry persist.
                    self.persist_session_blob(session_id, cx);
                } else {
                    let first_dirty_at = existing_first_dirty_at.unwrap_or(now);
                    let entry_index = *idx;
                    let task = cx.spawn(async move |this, cx: &mut AsyncApp| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(500))
                            .await;
                        this.update(cx, |this, cx| {
                            if this.entry_update_throttles.remove(&key).is_some() {
                                cx.emit(SolutionAgentStoreEvent::SessionMessageAppended(
                                    session_id,
                                    entry_index,
                                ));
                                this.persist_session_blob(session_id, cx);
                            }
                        })
                        .ok();
                    });
                    self.entry_update_throttles.insert(
                        key,
                        EntryUpdateThrottle {
                            first_dirty_at,
                            _task: task,
                        },
                    );
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Emit a sequenced `workspace.session_state_changed` notification for
    /// `session_id`. Reads the current session state from `self.sessions`
    /// and builds the wire payload using the same `session_summary` helper
    /// that the MCP `list_sessions` / `get_session` tools use, so remote
    /// clients receive a fully consistent state object.
    ///
    /// No-ops gracefully when the session is not found (already removed) or
    /// when `WorkspaceEventCoordinator` is not installed (test contexts that
    /// don't initialise the MCP layer).
    fn emit_session_state_changed_workspace(
        &self,
        session_id: &SolutionSessionId,
        cx: &App,
    ) {
        let Some(coord) = editor_mcp::workspace_seq::WorkspaceEventCoordinator::try_global(cx)
        else {
            return;
        };
        let Some(entity) = self.sessions.get(session_id) else {
            return;
        };
        let summary = entity.read_with(cx, |s, cx| crate::mcp::session_summary(s, cx));
        coord.emit_sequenced(cx, "workspace.session_state_changed", serde_json::json!({
            "solution_id": summary.solution_id,
            "session_id": summary.id,
            "state": summary.state,
        }));
    }

    /// Wraps a `SessionState` mutation so notifier hooks fire uniformly:
    ///   1. Snapshot previous state.
    ///   2. Apply `f` to mutate state.
    ///   3. Emit `SessionStateChanged` only when the discriminant changed.
    ///   4. Ask the notifier whether the transition warrants a desktop
    ///      notification, dispatch it, emit `SessionNotified`, and mark
    ///      the session's `Running { notified: true }` to suppress dupes.
    ///
    /// Side-channel updates (e.g. `last_activity_at`) stay outside `f` so
    /// they don't accidentally affect the notification decision.
    pub(crate) fn mutate_state<F: FnOnce(&mut SessionState)>(
        &mut self,
        session_id: SolutionSessionId,
        f: F,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.sessions.get(&session_id).cloned() else {
            return;
        };
        let previous = session.read(cx).state.clone();
        session.update(cx, |s, _| f(&mut s.state));
        let next = session.read(cx).state.clone();
        if std::mem::discriminant(&previous) != std::mem::discriminant(&next) {
            cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
            self.emit_session_state_changed_workspace(&session_id, cx);
        }
        // Drop the Stopping safety-net task whenever the session leaves
        // Stopping by any path (Stopped event handler, Error handler,
        // force_idle, restart_agent's restarting flip, …). Leaving a
        // stale task armed would let it fire 40s later onto a now-Idle
        // session — a harmless no-op but a noisy warn-log we'd then
        // have to explain.
        if matches!(previous, SessionState::Stopping { .. })
            && !matches!(next, SessionState::Stopping { .. })
        {
            session.update(cx, |s, _| s.stopping_safety_net = None);
        }
        let now = std::time::Instant::now();
        let is_focused = self
            .focus_resolver
            .as_ref()
            .map(|f| f(session_id, cx))
            .unwrap_or(false);
        let has_pending_messages = !session.read(cx).pending_messages.is_empty();
        if let Some(decision) = notifier::decide_notification(
            session_id,
            &previous,
            &next,
            now,
            is_focused,
            has_pending_messages,
        ) {
            let (title, body) = {
                let s = session.read(cx);
                let title = format!("SPK Editor — {} ({})", s.agent_id, s.title);
                let body = match decision.kind {
                    notifier::NotifyKind::Completed => {
                        format!("Done after {} min", decision.elapsed.as_secs() / 60)
                    }
                    notifier::NotifyKind::AwaitingInput => format!(
                        "Awaiting your input after {} min",
                        decision.elapsed.as_secs() / 60
                    ),
                    notifier::NotifyKind::Errored => match &next {
                        SessionState::Errored(msg) => format!("Failed: {msg}"),
                        _ => "Failed".to_string(),
                    },
                };
                (title, body)
            };
            notifier::dispatch(&decision, &title, &body, cx);
            cx.emit(SolutionAgentStoreEvent::SessionNotified(
                session_id,
                decision.kind,
            ));
            session.update(cx, |s, _| {
                if let SessionState::Running { notified, .. } = &mut s.state {
                    *notified = true;
                }
            });
        }
    }

    fn on_solution_event(
        &mut self,
        _: Entity<SolutionStore>,
        event: &SolutionStoreEvent,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, SolutionStoreEvent::Changed) {
            self.gc_orphan_solutions(cx);
        }
    }

    /// Construct a headless `project::Project` bound to nothing in
    /// particular — no worktree, no env, no window/workspace. Used by
    /// the MCP-driven auto-wake path (`queue::send_message_blocks_with_wake`):
    /// when a client (the mobile app) sends to a Cold session and the
    /// desktop has no window open for the solution, we still need a
    /// project handle to feed into `resume_session`.
    ///
    /// The `_solution` arg is taken for symmetry with the call site
    /// (and to make the intent obvious at call sites) but isn't used —
    /// `resume_session` keys claude-acp's jsonl lookup off the
    /// metadata's `cwd`, not the project's worktree. Empty worktree is
    /// fine.
    ///
    /// Pulls dependencies from `workspace::AppState::global` — the
    /// editor's `main.rs` sets this before any MCP server can hit us,
    /// so absence is a programmer error in init order (returns Err so
    /// the caller surfaces it instead of panicking).
    pub(crate) fn make_headless_project_for_solution(
        _solution: &solutions::Solution,
        cx: &mut App,
    ) -> Result<Entity<project::Project>> {
        let app_state = workspace::AppState::try_global(cx)
            .ok_or_else(|| anyhow!("workspace::AppState global is not initialised"))?;
        Ok(project::Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags {
                init_worktree_trust: false,
                ..Default::default()
            },
            cx,
        ))
    }

    fn gc_orphan_solutions(&mut self, cx: &mut Context<Self>) {
        let Some(store) = SolutionStore::try_global(cx) else {
            return;
        };
        let alive: std::collections::HashSet<SolutionId> = store
            .read(cx)
            .solutions()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let orphan_ids: Vec<SolutionId> = self
            .by_solution
            .keys()
            .filter(|sid| !alive.contains(*sid))
            .cloned()
            .collect();
        for sid in orphan_ids {
            if let Some(session_ids) = self.by_solution.remove(&sid) {
                for session_id in session_ids {
                    self.sessions.remove(&session_id);
                    if let Some(db) = &self.persistence {
                        db.delete(session_id).detach_and_log_err(cx);
                    }
                    cx.emit(SolutionAgentStoreEvent::SessionClosed(session_id));
                }
            }
            if let Some(db) = &self.persistence {
                db.delete_for_solution(sid).detach_and_log_err(cx);
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod label_unit_tests {
    use super::{label_fallback, short_id_suffix};
    use gpui::SharedString;

    #[test]
    fn short_id_suffix_truncates_long_ids() {
        assert_eq!(short_id_suffix("toolu_01abcdef"), "cdef");
    }

    #[test]
    fn short_id_suffix_returns_full_id_when_short() {
        assert_eq!(short_id_suffix("abc"), "abc");
        assert_eq!(short_id_suffix(""), "");
    }

    #[test]
    fn label_fallback_uses_subagent_type_when_present() {
        let id = SharedString::from("toolu_xyzwabcd");
        assert_eq!(
            label_fallback(&id, Some("general-purpose")).as_ref(),
            "general-purpose#abcd"
        );
    }

    #[test]
    fn label_fallback_falls_back_to_agent_short_when_subagent_type_missing() {
        let id = SharedString::from("toolu_xyzwabcd");
        assert_eq!(label_fallback(&id, None).as_ref(), "Agent abcd");
    }

    #[test]
    fn label_fallback_treats_empty_subagent_type_as_missing() {
        let id = SharedString::from("toolu_xyzwabcd");
        assert_eq!(label_fallback(&id, Some("")).as_ref(), "Agent abcd");
    }
}

#[cfg(test)]
mod background_agent_dir_tests {
    #[test]
    fn background_agent_dir_for_encodes_cwd() {
        let dir = super::background_agent_dir_for(
            std::path::Path::new("/home/spk/projects/foo.bar"),
            "ses-xyz",
        );
        let dir = dir.expect("home_dir must resolve in test env");
        assert!(
            dir.to_string_lossy()
                .contains("-home-spk-projects-foo-bar"),
            "expected encoded cwd in path, got {:?}",
            dir
        );
        assert!(dir.ends_with("subagents"));
    }

    #[test]
    fn background_agent_dir_for_empty_cwd_returns_none() {
        assert!(
            super::background_agent_dir_for(std::path::Path::new(""), "ses-x").is_none()
        );
    }
}

#[cfg(test)]
mod subagent_view_tests {
    use super::*;

    #[test]
    fn subagent_view_main_matches_only_parentless_entries() {
        let v = SubagentView::Main;
        assert!(v.matches_parent_entry(None));
        assert!(!v.matches_parent_entry(Some(&"toolu_xyz".into())));
    }

    #[test]
    fn subagent_view_task_matches_exact_id() {
        let v = SubagentView::Task("toolu_a".into());
        assert!(v.matches_parent_entry(Some(&"toolu_a".into())));
        assert!(!v.matches_parent_entry(Some(&"toolu_b".into())));
        assert!(!v.matches_parent_entry(None));
    }

    #[test]
    fn subagent_view_background_matches_no_parent_entry() {
        let v = SubagentView::Background(crate::background_agent::BackgroundAgentId::new("a30f"));
        assert!(!v.matches_parent_entry(None));
        assert!(!v.matches_parent_entry(Some(&"toolu_x".into())));
    }

    #[test]
    fn subagent_view_is_parent_thread_view() {
        assert!(SubagentView::Main.is_parent_thread_view());
        assert!(SubagentView::Task("x".into()).is_parent_thread_view());
        assert!(
            !SubagentView::Background(crate::background_agent::BackgroundAgentId::new("a30f"))
                .is_parent_thread_view()
        );
    }
}

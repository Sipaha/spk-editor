use std::collections::VecDeque;
use std::time::Instant;

use agent_client_protocol::schema as acp;
use acp_thread::AcpThread;
use chrono::{DateTime, Utc};
use gpui::{Entity, SharedString, Subscription};
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
        if s.len() == SHORT_ID_LEN
            && s.bytes().all(|b| SHORT_ID_ALPHABET.contains(&b))
        {
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
    pub acp_thread: Option<Entity<AcpThread>>,
    pub title: SharedString,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub state: SessionState,
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
}

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
}

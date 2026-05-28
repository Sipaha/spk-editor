//! Tracking surface for Claude Code's built-in **Managed Agents** —
//! the async sub-agents that the parent claude dispatches via the
//! `Agent` tool. Unlike inline `Task` subagents whose transcript
//! is interleaved into the parent's `AcpThread.entries`, a managed
//! agent gets its own JSONL file at
//! `~/.claude/projects/<encoded-cwd>/<session-id>/subagents/agent-<id>.jsonl`
//! and runs autonomously until it emits a terminal `stop_reason`.
//!
//! This module owns:
//!
//! - [`BackgroundAgentId`] — newtype around the hex id Claude Code
//!   prints in the tool output.
//! - [`BackgroundAgent`] + [`BackgroundAgentSnapshot`] — in-memory
//!   tracking state per agent.
//! - [`parse_managed_agent_announcement`] — the regex parser run on
//!   completed `Agent`-tool_call `raw_output`.
//! - JSONL tail / convert helpers (added in later tasks).

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gpui::SharedString;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BackgroundAgentId(SharedString);

impl BackgroundAgentId {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    /// First 6 hex chars — what the pill renders so the user has
    /// something glanceable instead of the full 17-32 char id.
    pub fn short(&self) -> String {
        self.0.chars().take(6).collect()
    }
}

impl std::fmt::Display for BackgroundAgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_ref())
    }
}

#[derive(Clone, Debug)]
pub struct BackgroundAgent {
    pub id: BackgroundAgentId,
    /// Canonical (symlink-resolved) JSONL path on disk.
    pub jsonl_path: PathBuf,
    pub registered_at: DateTime<Utc>,
    pub latest: Option<BackgroundAgentSnapshot>,
}

#[derive(Clone, Debug)]
pub struct BackgroundAgentSnapshot {
    pub mtime: SystemTime,
    pub activity_label: SharedString,
    pub stop_reason: Option<SharedString>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_agent_id_short_returns_first_six_chars() {
        let id = BackgroundAgentId::new("a30f92a688e431edc");
        assert_eq!(id.short(), "a30f92");
    }

    #[test]
    fn background_agent_id_short_handles_id_shorter_than_six() {
        let id = BackgroundAgentId::new("abc");
        assert_eq!(id.short(), "abc");
    }
}

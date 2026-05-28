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
use std::sync::OnceLock;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gpui::SharedString;
use regex::Regex;

static AGENT_ID_RE: OnceLock<Regex> = OnceLock::new();
static OUTPUT_FILE_RE: OnceLock<Regex> = OnceLock::new();

fn agent_id_re() -> &'static Regex {
    AGENT_ID_RE.get_or_init(|| {
        Regex::new(r"agentId:\s+([0-9a-f]{16,32})\b").expect("static regex compiles")
    })
}

fn output_file_re() -> &'static Regex {
    OUTPUT_FILE_RE.get_or_init(|| {
        Regex::new(r"output_file:\s+(\S+\.output)\b").expect("static regex compiles")
    })
}

/// Best-effort parse of an `Agent`-tool_call's `raw_output`. Returns
/// `Some((agent_id, output_file_path))` when both markers are present
/// AND the id is 16–32 hex chars AND the path ends `.output`.
/// `None` otherwise — caller silently skips registration so a future
/// claude version that reshapes the output doesn't spam the log.
///
/// Path is returned as-is (often a symlink under `/tmp/claude-<uid>/`);
/// caller resolves via `read_link` to the canonical JSONL location.
pub fn parse_managed_agent_announcement(raw_output: &str) -> Option<(String, PathBuf)> {
    let id = agent_id_re()
        .captures(raw_output)?
        .get(1)?
        .as_str()
        .to_string();
    let path = output_file_re()
        .captures(raw_output)?
        .get(1)?
        .as_str();
    Some((id, PathBuf::from(path)))
}

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

    #[test]
    fn parse_managed_agent_announcement_happy_path() {
        let raw = "Async agent launched successfully.\n\
                   agentId: a30f92a688e431edc (internal ID)\n\
                   output_file: /tmp/claude-1000/x/abc/tasks/a30f92a688e431edc.output";
        let parsed = parse_managed_agent_announcement(raw);
        assert!(parsed.is_some());
        let (id, path) = parsed.unwrap();
        assert_eq!(id, "a30f92a688e431edc");
        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/claude-1000/x/abc/tasks/a30f92a688e431edc.output"
            )
        );
    }

    #[test]
    fn parse_managed_agent_announcement_missing_agent_id_returns_none() {
        let raw = "output_file: /tmp/x/y.output";
        assert!(parse_managed_agent_announcement(raw).is_none());
    }

    #[test]
    fn parse_managed_agent_announcement_missing_output_file_returns_none() {
        let raw = "agentId: a30f92a688e431edc";
        assert!(parse_managed_agent_announcement(raw).is_none());
    }

    #[test]
    fn parse_managed_agent_announcement_ignores_surrounding_text() {
        let raw = "Random words.\n\
                   Do not duplicate this agent's work.\n\
                   agentId:    a30f92a688e431edc\n\
                   More noise. \n\
                   output_file:    /tmp/x/foo.output\n\
                   Trailing line.";
        let parsed = parse_managed_agent_announcement(raw);
        assert!(parsed.is_some());
        let (id, path) = parsed.unwrap();
        assert_eq!(id, "a30f92a688e431edc");
        assert_eq!(path, PathBuf::from("/tmp/x/foo.output"));
    }

    #[test]
    fn parse_managed_agent_announcement_rejects_non_hex_id() {
        let raw = "agentId: NOT-HEX-ID\noutput_file: /tmp/x.output";
        assert!(parse_managed_agent_announcement(raw).is_none());
    }

    #[test]
    fn parse_managed_agent_announcement_rejects_short_id() {
        let raw = "agentId: abcd\noutput_file: /tmp/x.output";
        assert!(parse_managed_agent_announcement(raw).is_none());
    }

    #[test]
    fn parse_managed_agent_announcement_requires_dot_output_suffix() {
        let raw = "agentId: a30f92a688e431edc\noutput_file: /tmp/x.jsonl";
        assert!(parse_managed_agent_announcement(raw).is_none());
    }
}

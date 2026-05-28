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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gpui::SharedString;
use regex::Regex;
use serde_json::Value;

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

/// 64 KiB cap on individual JSONL line size. A claude tool_use entry
/// is well under 4 KiB in practice; an entry past this cap is treated
/// as `Generating…` so a pathological line can't blow our memory.
const JSONL_LINE_CAP: usize = 64 * 1024;

/// Pure JSON → snapshot. Public so the watcher (Task 7) can feed it
/// arbitrary strings; never panics, returns `Generating…` for any
/// shape it doesn't recognise.
pub fn parse_jsonl_snapshot(line: &str) -> BackgroundAgentSnapshot {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return generating_snapshot(),
    };
    let typ = value.get("type").and_then(Value::as_str).unwrap_or("");
    match typ {
        "system" => {
            let subtype = value
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("");
            if subtype == "init" {
                BackgroundAgentSnapshot {
                    mtime: SystemTime::now(),
                    activity_label: SharedString::new_static("Starting…"),
                    stop_reason: None,
                }
            } else {
                generating_snapshot()
            }
        }
        "assistant" => {
            let message = value.get("message").cloned().unwrap_or(Value::Null);
            let stop_reason = message
                .get("stop_reason")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(SharedString::from);
            let label = derive_assistant_label(&message);
            BackgroundAgentSnapshot {
                mtime: SystemTime::now(),
                activity_label: label,
                stop_reason,
            }
        }
        _ => generating_snapshot(),
    }
}

fn generating_snapshot() -> BackgroundAgentSnapshot {
    BackgroundAgentSnapshot {
        mtime: SystemTime::now(),
        activity_label: SharedString::new_static("Generating…"),
        stop_reason: None,
    }
}

fn derive_assistant_label(message: &Value) -> SharedString {
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    for block in content {
        let typ = block.get("type").and_then(Value::as_str).unwrap_or("");
        if typ == "tool_use" {
            let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
            let input_preview = block
                .get("input")
                .and_then(|v| v.as_object())
                .and_then(|m| m.values().next())
                .and_then(Value::as_str)
                .unwrap_or("");
            const ARG_BUDGET: usize = 30;
            let truncated = if input_preview.chars().count() > ARG_BUDGET {
                let head: String = input_preview.chars().take(ARG_BUDGET).collect();
                format!("{name}: {head}…")
            } else if input_preview.is_empty() {
                name.to_string()
            } else {
                format!("{name}: {input_preview}")
            };
            return SharedString::from(truncated);
        }
    }
    SharedString::new_static("Generating…")
}

#[derive(Debug, Clone)]
pub struct Tail {
    /// Last non-empty, in-cap line of the file. `None` when:
    ///   * file is empty
    ///   * all lines past `since_offset` are blank
    ///   * the last line exceeds [`JSONL_LINE_CAP`]
    pub last_line: Option<String>,
    /// Offset just past EOF after the read; pass back as
    /// `since_offset` on the next call for incremental tails.
    pub new_offset: u64,
    pub mtime: SystemTime,
}

/// Seek a JSONL file to `since_offset`, read to EOF, return the last
/// non-empty line within the cap. Never loads more than
/// [`JSONL_LINE_CAP`] bytes for the final-line slice — earlier lines
/// in the read window are ignored, since only the latest one drives
/// the snapshot.
pub fn tail_jsonl(path: &Path, since_offset: u64) -> std::io::Result<Tail> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let metadata = f.metadata()?;
    let mtime = metadata.modified()?;
    let len = metadata.len();
    if since_offset >= len {
        return Ok(Tail {
            last_line: None,
            new_offset: len,
            mtime,
        });
    }
    // Read tail up to JSONL_LINE_CAP + some slack so we can locate
    // line boundaries. If the final line is larger than the cap,
    // we'll detect that and drop it.
    let slack = JSONL_LINE_CAP + 4096;
    let read_start = std::cmp::max(since_offset, len.saturating_sub(slack as u64));
    f.seek(SeekFrom::Start(read_start))?;
    let mut buf = String::new();
    f.take(len - read_start).read_to_string(&mut buf)?;
    let last = buf
        .split('\n')
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|s| s.to_string());
    let last_line = match last {
        Some(l) if l.len() > JSONL_LINE_CAP => None,
        other => other,
    };
    Ok(Tail {
        last_line,
        new_offset: len,
        mtime,
    })
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
    fn parse_managed_agent_announcement_rejects_fifteen_char_hex_id() {
        let raw = "agentId: a30f92a688e431e\noutput_file: /tmp/x.output";
        assert!(parse_managed_agent_announcement(raw).is_none());
    }

    #[test]
    fn parse_managed_agent_announcement_accepts_sixteen_char_hex_id() {
        let raw = "agentId: a30f92a688e431ed\noutput_file: /tmp/x.output";
        let parsed = parse_managed_agent_announcement(raw);
        assert!(parsed.is_some());
        let (id, path) = parsed.unwrap();
        assert_eq!(id, "a30f92a688e431ed");
        assert_eq!(path, std::path::PathBuf::from("/tmp/x.output"));
    }

    #[test]
    fn parse_managed_agent_announcement_requires_dot_output_suffix() {
        let raw = "agentId: a30f92a688e431edc\noutput_file: /tmp/x.jsonl";
        assert!(parse_managed_agent_announcement(raw).is_none());
    }

    #[test]
    fn parse_jsonl_snapshot_tool_use() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test --release"}}]}}"#;
        let snap = parse_jsonl_snapshot(line);
        assert_eq!(snap.activity_label.as_ref(), "Bash: cargo test --release");
        assert!(snap.stop_reason.is_none());
    }

    #[test]
    fn parse_jsonl_snapshot_tool_use_truncates_long_args() {
        let long = "x".repeat(200);
        let line = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Bash","input":{{"command":"{long}"}}}}]}}}}"#
        );
        let snap = parse_jsonl_snapshot(&line);
        let label = snap.activity_label.as_ref();
        assert!(label.starts_with("Bash: "));
        assert!(label.ends_with('…'), "expected ellipsis, got: {label:?}");
        assert!(label.len() <= 40, "label too long: {} chars", label.len());
    }

    #[test]
    fn parse_jsonl_snapshot_assistant_text_without_stop_reason() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Sure, let me…"}]}}"#;
        let snap = parse_jsonl_snapshot(line);
        assert_eq!(snap.activity_label.as_ref(), "Generating…");
        assert!(snap.stop_reason.is_none());
    }

    #[test]
    fn parse_jsonl_snapshot_terminal_stop_reason_end_turn() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}],"stop_reason":"end_turn"}}"#;
        let snap = parse_jsonl_snapshot(line);
        assert_eq!(snap.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn parse_jsonl_snapshot_system_init() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/x","tools":[]}"#;
        let snap = parse_jsonl_snapshot(line);
        assert_eq!(snap.activity_label.as_ref(), "Starting…");
        assert!(snap.stop_reason.is_none());
    }

    #[test]
    fn parse_jsonl_snapshot_malformed_returns_unknown() {
        let snap = parse_jsonl_snapshot("not json at all");
        assert_eq!(snap.activity_label.as_ref(), "Generating…");
        assert!(snap.stop_reason.is_none());
    }

    #[test]
    fn tail_jsonl_reads_last_nonempty_line() -> std::io::Result<()> {
        use std::io::Write;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("agent.jsonl");
        let mut f = std::fs::File::create(&path)?;
        writeln!(f, r#"{{"type":"system","subtype":"init"}}"#)?;
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"hi"}}]}}}}"#)?;
        f.write_all(b"\n")?; // trailing blank line
        let tail = tail_jsonl(&path, 0)?;
        assert!(tail.last_line.is_some());
        let last = tail.last_line.unwrap();
        assert!(last.contains(r#""type":"assistant""#));
        assert!(tail.new_offset > 0);
        Ok(())
    }

    #[test]
    fn tail_jsonl_caps_oversize_last_line() -> std::io::Result<()> {
        use std::io::Write;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("huge.jsonl");
        let mut f = std::fs::File::create(&path)?;
        // 80 KiB single line — past the 64 KiB cap.
        let huge = "x".repeat(80 * 1024);
        writeln!(f, "{}", huge)?;
        let tail = tail_jsonl(&path, 0)?;
        // Cap behaviour: last_line is None when the line exceeds the cap.
        assert!(tail.last_line.is_none(), "oversize line should be dropped");
        Ok(())
    }
}

//! Tracking surface for Claude Code's **background shells** — Bash commands
//! launched with `run_in_background=true` from the parent claude process.
//! Unlike inline Bash calls whose output is returned inline in the tool
//! result, a background shell runs detached and writes its combined
//! stdout/stderr to an on-disk `.output` file whose path is surfaced in the
//! launch announcement printed to the parent's conversation transcript.
//!
//! This module owns:
//!
//! - [`BackgroundShellId`] — newtype around the short random token Claude Code
//!   assigns to each background task (e.g. `bvb4ful1z`).
//! - [`BackgroundShell`] + [`BackgroundShellSnapshot`] — in-memory tracking
//!   state per shell.
//! - [`ShellRuntimeState`] — running / exited / killed lifecycle enum.
//!
//! Parsers (launch-announcement regex) and fs-watch / incremental-tail helpers
//! land in later tasks; this module is pure data scaffolding.

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gpui::SharedString;

/// Opaque identifier assigned by Claude Code to a background shell task.
/// Short random token (e.g. `bvb4ful1z`), not a hex digest.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BackgroundShellId(SharedString);

impl BackgroundShellId {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    /// First 9 chars — shell ids are short random tokens (e.g. `bvb4ful1z`),
    /// so this usually returns the whole id; the cap guards a pathological id.
    pub fn short(&self) -> String {
        self.0.chars().take(9).collect()
    }
}

impl std::fmt::Display for BackgroundShellId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_ref())
    }
}

/// Runtime lifecycle of a background shell process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellRuntimeState {
    Running,
    /// Process exited; inner value is the exit code when known.
    Exited(Option<i32>),
    Killed,
}

/// In-memory tracking record for one background shell.
#[derive(Clone, Debug)]
pub struct BackgroundShell {
    pub id: BackgroundShellId,
    /// Command line captured at launch (truncated to ~120 chars at the call-site).
    pub command: SharedString,
    /// The `/tmp/claude-<uid>/.../tasks/<id>.output` path from the launch announcement.
    pub output_path: PathBuf,
    pub registered_at: DateTime<Utc>,
    pub latest: Option<BackgroundShellSnapshot>,
    /// Byte offset past the last bytes tailed from `output_path`; carried across
    /// fs-watch events for incremental tails. (Used by a later task.)
    pub last_offset: u64,
    pub state: ShellRuntimeState,
}

/// A point-in-time snapshot of a background shell's output file.
#[derive(Clone, Debug)]
pub struct BackgroundShellSnapshot {
    pub mtime: SystemTime,
    /// Trailing chunk of the shell's stdout/stderr, capped (later task).
    pub output_tail: SharedString,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn background_shell_id_short_returns_whole_id_when_short() {
        let id = BackgroundShellId::new("bvb4ful1z");
        assert_eq!(id.short(), "bvb4ful1z");
    }

    #[test]
    fn background_shell_id_short_caps_at_nine_chars() {
        let id = BackgroundShellId::new("abcdefghij_toolong");
        assert_eq!(id.short(), "abcdefghi");
    }

    #[test]
    fn background_shell_id_short_handles_id_shorter_than_nine() {
        let id = BackgroundShellId::new("abc");
        assert_eq!(id.short(), "abc");
    }

    #[test]
    fn background_shell_clone_round_trip_ids_equal() {
        let shell = BackgroundShell {
            id: BackgroundShellId::new("bvb4ful1z"),
            command: SharedString::from("cargo build --bin spk-editor"),
            output_path: PathBuf::from("/tmp/claude-1000/tasks/bvb4ful1z.output"),
            registered_at: chrono::Utc::now(),
            latest: None,
            last_offset: 0,
            state: ShellRuntimeState::Running,
        };
        let cloned = shell.clone();
        assert_eq!(shell.id, cloned.id);
    }
}

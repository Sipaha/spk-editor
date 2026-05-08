//! Persistent undo registry — every destructive operation records a row so
//! the user can rewind with `editor.git.undo_last`.
//!
//! Persistence lives at `~/.config/spk-editor/git_undo.json`.
//!
//! Skeleton — full implementation lands in S-BAK
//! (`docs/superpowers/plans/git-panel-plan.md`).

use anyhow::Result;
use std::path::Path;

/// One undo row.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub id: u64,
    pub repo_path: std::path::PathBuf,
    pub op: String,
    pub timestamp_unix: i64,
    pub branch: String,
    pub before_sha: String,
    pub after_sha: Option<String>,
    pub failed: bool,
}

/// Append a fresh entry. Returns the assigned id.
pub fn record(
    _repo_path: &Path,
    _op: &str,
    _branch: &str,
    _before_sha: &str,
) -> Result<u64> {
    anyhow::bail!("undo_registry::record not yet implemented (S-BAK)")
}

/// Mark `id` as completed with the resulting `after_sha`.
pub fn complete(_id: u64, _after_sha: &str) -> Result<()> {
    anyhow::bail!("undo_registry::complete not yet implemented (S-BAK)")
}

/// Mark `id` as failed.
pub fn mark_failed(_id: u64) -> Result<()> {
    anyhow::bail!("undo_registry::mark_failed not yet implemented (S-BAK)")
}

/// List entries newer than `since_unix`, most recent first.
pub fn list(_since_unix: i64) -> Result<Vec<UndoEntry>> {
    anyhow::bail!("undo_registry::list not yet implemented (S-BAK)")
}

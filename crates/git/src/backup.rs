//! Backup-refs framework — `refs/spke/backup/<branch>/<timestamp>-<op>` is
//! created before every destructive operation so a misstep can be undone via
//! `editor.git.undo_last`.
//!
//! Skeleton — full implementation lands in S-BAK
//! (`docs/superpowers/plans/git-panel-plan.md`).

use anyhow::Result;
use std::path::Path;

/// One backup-ref entry. The reference itself lives at
/// `refs/spke/backup/<branch>/<timestamp>-<op>` and points at the tip of
/// `branch` immediately before the operation ran.
#[derive(Debug, Clone)]
pub struct BackupRef {
    pub repo_path: std::path::PathBuf,
    pub branch: String,
    pub op: String,
    pub timestamp_unix: i64,
    pub before_sha: String,
}

/// Create a backup-ref for `branch` in `repo_path`. Returns the materialized
/// [`BackupRef`].
pub fn create(_repo_path: &Path, _branch: &str, _op: &str) -> Result<BackupRef> {
    anyhow::bail!("backup::create not yet implemented (S-BAK)")
}

/// List backup-refs in `repo_path`, optionally filtered to a single `branch`
/// or to entries newer than `since_unix`.
pub fn list(
    _repo_path: &Path,
    _branch: Option<&str>,
    _since_unix: Option<i64>,
) -> Result<Vec<BackupRef>> {
    anyhow::bail!("backup::list not yet implemented (S-BAK)")
}

/// Delete backup-refs older than `older_than_days` in `repo_path`. Returns
/// the count of removed refs.
pub fn cleanup(_repo_path: &Path, _older_than_days: u32) -> Result<usize> {
    anyhow::bail!("backup::cleanup not yet implemented (S-BAK)")
}

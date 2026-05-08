//! Per-repo busy guard (P-10) — only one git operation at tier ≥ Write may run
//! at a time per repository. Uses `.git/spke/op.lock` plus a sanity check on
//! `.git/index.lock` (which would mean an external `git` invocation is mid-flight).
//!
//! Skeleton — full implementation lands in S-BAK
//! (`docs/superpowers/plans/git-panel-plan.md`).

use anyhow::Result;
use std::path::Path;

/// Reason the repo is busy. Surfaced through [`RepoBusyError::other_op`] so
/// callers can show a useful message ("Repository busy: cherry_pick in progress").
#[derive(Debug, Clone)]
pub enum BusyReason {
    /// Another spk-editor operation is in flight, identified by `AtomicGitOp::op_name`.
    OtherOp(String),
    /// `.git/index.lock` is present — an external `git` process is running.
    ExternalGit,
}

#[derive(Debug, thiserror::Error)]
#[error("repository busy: {reason:?}")]
pub struct RepoBusyError {
    pub reason: BusyReason,
}

/// Guard returned by [`acquire`]. The lock is released on drop.
pub struct RepoLock {
    _repo_path: std::path::PathBuf,
    _op: &'static str,
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        // Real impl: remove `.git/spke/op.lock`. S-BAK.
    }
}

/// Try to acquire the busy guard for `repo_path` for operation `op_name`.
/// Returns `Err(RepoBusyError)` if another operation is already running.
pub fn acquire(_repo_path: &Path, _op_name: &'static str) -> Result<RepoLock, RepoBusyError> {
    Err(RepoBusyError {
        reason: BusyReason::OtherOp(String::from("repo_lock not yet implemented (S-BAK)")),
    })
}

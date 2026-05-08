//! High-level atomic git operations with auto-backup, undo registry, and
//! repo-busy guard.
//!
//! Each user-facing destructive operation (cherry-pick, revert, reset, drop,
//! squash, fixup, edit-message, move, rebase, interactive-rebase, merge) is a
//! struct that implements [`AtomicGitOp`]. UI handlers in
//! `git_ui::handlers::*` construct the struct and call [`OpRunner::run`]; no
//! operation invokes git CLI directly bypassing the runner.
//!
//! Skeleton — concrete operations are added as their owning S-* tasks land
//! (S-DST, S-RBL, etc.). See `docs/superpowers/plans/git-panel-plan.md`.

use anyhow::Result;
use std::path::PathBuf;

/// Stable identifier for an operation. Used both for backup-ref naming
/// (`refs/spke/backup/<branch>/<timestamp>-<op_name>`) and for undo-registry
/// rows.
pub trait AtomicGitOp {
    type Output;

    /// Stable identifier for backup-ref naming and undo registry. Examples:
    /// `"cherry_pick"`, `"drop"`, `"squash"`, `"rebase_interactive"`.
    fn op_name(&self) -> &'static str;

    /// Whether this operation can lose work without a backup. Default `false`.
    /// Explicit opt-in per P-3 (no implicit detection).
    fn is_destructive(&self) -> bool {
        false
    }

    /// Branches whose tips should be backed up before [`Self::run`]. Empty
    /// for ops that don't affect refs (e.g. pure index/working-tree changes).
    fn affected_branches(&self, repo_path: &PathBuf) -> Vec<String>;

    /// Tries to extract the target branch from the operation payload for
    /// branch-protection enforcement (see `solution_git::branch_protection`
    /// in S-SOL-PRT). `None` means the op isn't tied to a single branch and
    /// protection is skipped.
    fn affects_branch(&self) -> Option<String> {
        None
    }
}

/// Runs an [`AtomicGitOp`] under the safety umbrella: repo-busy guard,
/// backup-ref creation, undo registration.
///
/// Stubbed — full implementation lands in S-BAK.
pub struct OpRunner;

impl OpRunner {
    /// Execute `op` with backup + undo registration + repo-busy guard.
    ///
    /// 1. Acquire repo lock via [`crate::repo_lock`] — fail with `RepoBusy` if held.
    /// 2. For each branch in `op.affected_branches()`: create a backup-ref via [`crate::backup`].
    /// 3. Register the undo entry via [`crate::undo_registry`].
    /// 4. Run the operation; on `Err` mark the undo entry failed.
    /// 5. Release the lock.
    pub fn run<O: AtomicGitOp>(_op: O, _repo_path: &PathBuf) -> Result<O::Output> {
        anyhow::bail!("OpRunner::run not yet implemented (S-BAK)")
    }
}

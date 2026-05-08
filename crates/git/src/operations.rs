//! High-level atomic git operations with auto-backup, undo registry, and
//! repo-busy guard.
//!
//! Each user-facing destructive operation (cherry-pick, revert, reset, drop,
//! squash, fixup, edit-message, move, rebase, interactive-rebase, merge) is a
//! struct that implements [`AtomicGitOp`]. UI handlers in
//! `git_ui::handlers::*` construct the struct and call [`OpRunner::run`]; no
//! operation invokes git CLI directly bypassing the runner.
//!
//! Concrete operations are added as their owning S-* tasks land
//! (S-DST, S-RBL, etc.). See `docs/superpowers/plans/git-panel-plan.md`.

use anyhow::Result;
use std::path::Path;
use util::ResultExt as _;

use crate::{backup, repo_lock, undo_registry};

/// A single atomic git operation. Implementors describe their identity and
/// affected branches; [`OpRunner::run`] handles the safety umbrella (lock,
/// backup, undo registration, error path).
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
    fn affected_branches(&self, repo_path: &Path) -> Vec<String>;

    /// Tries to extract the target branch from the operation payload for
    /// branch-protection enforcement (see `solution_git::branch_protection`
    /// in S-SOL-PRT). `None` means the op isn't tied to a single branch and
    /// protection is skipped.
    fn affects_branch(&self) -> Option<String> {
        None
    }

    /// Execute the operation. Called under the repo-busy lock with backup
    /// refs already in place. Takes `&mut self` so [`OpRunner::run`] can
    /// invoke [`Self::on_failure`] afterwards if `run` errors out.
    fn run(&mut self, repo_path: &Path) -> Result<Self::Output>;

    /// Hook invoked when [`Self::run`] returns `Err`. Default: no-op. Use
    /// for operation-specific cleanup (clearing intermediate state, etc.) —
    /// the backup ref and the undo entry are managed by the runner itself.
    fn on_failure(&self, _repo_path: &Path, _err: &anyhow::Error) -> Result<()> {
        Ok(())
    }
}

/// Runs an [`AtomicGitOp`] under the safety umbrella: repo-busy guard,
/// backup-ref creation, undo registration.
pub struct OpRunner;

impl OpRunner {
    /// Execute `op` with backup + undo registration + repo-busy guard.
    ///
    /// 1. Acquire repo lock via [`crate::repo_lock`] — propagate `RepoBusyError`.
    /// 2. For each branch in `op.affected_branches()`: create a backup-ref via [`crate::backup`].
    /// 3. If `op.is_destructive()` and at least one backup exists: register an undo entry.
    /// 4. Run the operation.
    /// 5. On `Ok`: complete the undo entry with the new branch tip. On `Err`:
    ///    mark it failed and call `op.on_failure`.
    /// 6. Release the lock (drop guard).
    pub fn run<O: AtomicGitOp>(mut op: O, repo_path: &Path) -> Result<O::Output> {
        let op_name = op.op_name();
        let _lock = repo_lock::acquire(repo_path, op_name)?;

        let branches = op.affected_branches(repo_path);
        let mut backups = Vec::with_capacity(branches.len());
        for branch in &branches {
            match backup::create(repo_path, branch, op_name) {
                Ok(b) => backups.push(b),
                Err(err) => {
                    log::warn!(
                        "git::operations: failed to back up {branch} for {op_name}: {err}"
                    );
                }
            }
        }

        let undo_id = if op.is_destructive() {
            backups.first().and_then(|first| {
                undo_registry::record(repo_path, op_name, &first.branch, &first.before_sha)
                    .log_err()
            })
        } else {
            None
        };

        let primary_branch = backups.first().map(|b| b.branch.clone());
        let result = op.run(repo_path);

        match &result {
            Ok(_) => {
                if let (Some(id), Some(branch)) = (undo_id, primary_branch.as_deref()) {
                    match backup::read_branch_tip(repo_path, branch) {
                        Ok(after) => {
                            undo_registry::complete(id, &after).log_err();
                        }
                        Err(err) => {
                            log::warn!(
                                "git::operations: completed {op_name} but couldn't read {branch} tip: {err}"
                            );
                        }
                    }
                }
            }
            Err(err) => {
                if let Some(id) = undo_id {
                    undo_registry::mark_failed(id).log_err();
                }
                if let Err(hook_err) = op.on_failure(repo_path, err) {
                    log::warn!("git::operations: on_failure hook for {op_name} errored: {hook_err}");
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::tempdir;

    #[allow(clippy::disallowed_methods)]
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("spawn git");
        assert!(status.success(), "`git {}` failed", args.join(" "));
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("README.md"), "x").expect("write");
        git(dir.path(), &["add", "README.md"]);
        git(dir.path(), &["-c", "user.name=T", "-c", "user.email=t@x", "commit", "-qm", "init"]);
        dir
    }

    struct NoopOp {
        ran: std::sync::Arc<AtomicBool>,
    }

    impl AtomicGitOp for NoopOp {
        type Output = ();
        fn op_name(&self) -> &'static str {
            "test_noop"
        }
        fn is_destructive(&self) -> bool {
            true
        }
        fn affected_branches(&self, _: &Path) -> Vec<String> {
            vec!["main".into()]
        }
        fn run(&mut self, _: &Path) -> Result<()> {
            self.ran.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn run_creates_backup_and_records_undo() {
        let dir = init_repo();
        crate::undo_registry::test_override::set(dir.path().to_path_buf());
        let ran = std::sync::Arc::new(AtomicBool::new(false));
        OpRunner::run(NoopOp { ran: ran.clone() }, dir.path()).expect("run");
        assert!(ran.load(Ordering::SeqCst));
        let backups = backup::list(dir.path(), None, None).expect("list");
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].op, "test_noop");
        let undos = undo_registry::list(0).expect("list");
        let entry = undos.iter().find(|e| e.op == "test_noop").expect("entry");
        assert!(entry.after_sha.is_some());
        assert!(!entry.failed);
        crate::undo_registry::test_override::clear();
    }

    struct FailingOp;
    impl AtomicGitOp for FailingOp {
        type Output = ();
        fn op_name(&self) -> &'static str {
            "test_failing"
        }
        fn is_destructive(&self) -> bool {
            true
        }
        fn affected_branches(&self, _: &Path) -> Vec<String> {
            vec!["main".into()]
        }
        fn run(&mut self, _: &Path) -> Result<()> {
            anyhow::bail!("nope")
        }
    }

    #[test]
    fn run_marks_failed_on_error() {
        let dir = init_repo();
        crate::undo_registry::test_override::set(dir.path().to_path_buf());
        let err = OpRunner::run(FailingOp, dir.path()).expect_err("must fail");
        assert!(err.to_string().contains("nope"));
        let undos = undo_registry::list(0).expect("list");
        let entry = undos
            .iter()
            .find(|e| e.op == "test_failing")
            .expect("entry");
        assert!(entry.failed);
        crate::undo_registry::test_override::clear();
    }
}

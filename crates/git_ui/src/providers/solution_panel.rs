//! Provider trait for solution-wide commit panel rendering and dispatch.
//!
//! Implemented in `solution_git::commit::SolutionCommitOrchestrator` (S-SOL-CMT).
//! The trait is intentionally narrow — `git_ui` calls four methods only
//! (`is_active`, `member_ids`, `render_solution_commit_panel`, `commit_all`)
//! so it can stay free of all `solutions` / `solution_git` types.
//!
//! ## Why `render_solution_commit_panel` takes only `&mut App`
//!
//! Renders are normally driven by `Render::render` which receives `&mut
//! Window` + `&mut Context<View>`. This trait method is invoked from
//! inside `GitPanel::render` and the orchestrator stitches its own
//! `Entity<SolutionCommitView>` into the returned element — the entity
//! owns its window-bound state internally, so the trait surface can
//! stay window-agnostic.

use anyhow::Result;
use gpui::{AnyElement, App, SharedString, Task};

/// Per-member commit status reported by [`SolutionPanelProvider::commit_all`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitStatus {
    /// Member had no staged changes — left untouched.
    Skipped,
    /// Pre-commit checks failed; this member was aborted before any
    /// `git commit` call. Other members may also have been aborted.
    PreCommitFailed,
    /// Member committed successfully.
    Committed,
    /// Member was committed earlier in the run but rolled back via
    /// `git reset --soft <backup>` after a later member's commit failed.
    RolledBack,
    /// Member was committed but the rollback attempt failed — the new
    /// commit is still on the branch. User must recover via the backup
    /// ref.
    PartiallyFailed,
}

/// One member's outcome from [`SolutionPanelProvider::commit_all`].
#[derive(Debug, Clone)]
pub struct MemberCommitResult {
    pub member_id: SharedString,
    pub status: CommitStatus,
    /// Error message captured from `git` (or the pre-commit runner) when
    /// the operation failed. `None` on success / `Skipped`.
    pub error: Option<String>,
    /// Backup-ref name (`refs/spke/backup/<branch>/<ts>-solution_commit`)
    /// created before the commit attempt — populated for every member
    /// the orchestrator tried to commit, regardless of outcome.
    pub backup_ref: Option<String>,
}

/// Aggregate outcome of a `commit_all` run.
#[derive(Debug, Clone, Default)]
pub struct CommitAllOutcome {
    pub member_results: Vec<MemberCommitResult>,
    /// Per-member rollback errors collected when at least one rollback
    /// in a partial-failure path itself errored. Empty when every member
    /// either committed cleanly or rolled back cleanly.
    pub rollback_errors: Vec<String>,
}

impl CommitAllOutcome {
    pub fn committed_count(&self) -> usize {
        self.member_results
            .iter()
            .filter(|r| r.status == CommitStatus::Committed)
            .count()
    }

    pub fn rolled_back_count(&self) -> usize {
        self.member_results
            .iter()
            .filter(|r| r.status == CommitStatus::RolledBack)
            .count()
    }

    pub fn partial_failure_count(&self) -> usize {
        self.member_results
            .iter()
            .filter(|r| r.status == CommitStatus::PartiallyFailed)
            .count()
    }

    /// Members that ended in a state requiring manual recovery via
    /// `refs/spke/backup/...`.
    pub fn members_needing_recovery(&self) -> Vec<SharedString> {
        self.member_results
            .iter()
            .filter(|r| r.status == CommitStatus::PartiallyFailed)
            .map(|r| r.member_id.clone())
            .collect()
    }
}

/// Solution-wide commit-panel hooks.
///
/// `git_panel.rs` checks if a provider is registered AND the provider
/// reports `is_active()`, then either renders the solution panel via
/// this trait or falls back to the single-repo flow.
pub trait SolutionPanelProvider: Send + Sync {
    /// True when a Solution is open with `≥ 2` members. The toggle in
    /// the commit panel is rendered only when this returns `true`.
    fn is_active(&self) -> bool;

    /// Catalog ids of members in the active Solution, in display order.
    /// `git_panel` uses the count to decide whether to expose the
    /// toggle and the orchestrator uses the same list to drive
    /// per-member iteration.
    fn member_ids(&self) -> Vec<SharedString>;

    /// Render the per-member file-grouped commit panel as the central
    /// content of `GitPanel`. Replaces the single-repo file list when
    /// `Solution-wide` is toggled on.
    fn render_solution_commit_panel(&self, cx: &mut App) -> AnyElement;

    /// Run the atomic commit orchestrator with `message`, optional
    /// auto-trailer (`X-Spke-Solution: <name>`), and pre-commit checks
    /// toggle. `members` filters the per-member set; `None` ⇒ all
    /// members of the active Solution.
    ///
    /// Returns the aggregate [`CommitAllOutcome`]. The outer `Result`
    /// signals "couldn't even start" (no Solution / store missing); a
    /// per-member failure is reflected on the corresponding
    /// [`MemberCommitResult`] inside `Ok(_)`.
    fn commit_all(
        &self,
        message: SharedString,
        add_solution_trailer: bool,
        run_pre_commit_checks: bool,
        members: Option<Vec<SharedString>>,
        cx: &mut App,
    ) -> Task<Result<CommitAllOutcome>>;
}

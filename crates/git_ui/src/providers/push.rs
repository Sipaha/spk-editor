//! Provider trait for solution-wide push dialog.
//!
//! Implemented in `solution_git::push::SolutionPushOrchestrator` (S-SOL-PSH).
//! Stub now.

pub trait SolutionPushProvider: Send + Sync {
    /// True if Solution-wide push UI should replace the per-repo push for
    /// the currently-open Solution.
    fn is_active(&self) -> bool;
}

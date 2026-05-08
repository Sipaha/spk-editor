//! Provider trait for solution-wide commit panel rendering and dispatch.
//!
//! Implemented in `solution_git::commit::SolutionCommitOrchestrator` (S-SOL-CMT).
//! Stub now to break the cyclic dep `git_ui ↔ solution_git`.

/// Solution-wide commit-panel hooks.
///
/// `git_panel.rs` checks if a provider is registered AND a Solution is open,
/// then either renders the solution panel via this trait or falls back to the
/// single-repo flow.
pub trait SolutionPanelProvider: Send + Sync {
    /// True if Solution-wide UI should be exposed in the commit panel for
    /// the currently-open Solution. The provider decides based on Solution
    /// state (e.g. ≥ 2 members).
    fn is_active(&self) -> bool;
}

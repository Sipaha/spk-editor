//! Provider trait for swapping the git-graph log data source between
//! single-repo (default) and solution-wide aggregated.
//!
//! Implemented in `solution_git::aggregator::SolutionLogDataSource` (S-SOL-LOG).
//! Stub now.

pub trait LogDataSource: Send + Sync {
    /// True if the aggregated source is currently available (a Solution is
    /// open with ≥ 1 member). The git_graph toolbar offers the Per-Repo /
    /// Solution-wide toggle only when this returns `true`.
    fn is_active(&self) -> bool;
}

//! Solution-aware git operations: aggregated log, status dashboard, solution-wide
//! commit/push, cross-member cherry-pick, branch protection.
//!
//! Skeleton crate. Real implementation lands across milestone M4
//! (S-SOL-LOG / S-SOL-DSH / S-SOL-CMT / S-SOL-PSH / S-SOL-CHP / S-SOL-PRT in
//! `docs/superpowers/plans/git-panel-plan.md`).
//!
//! Owns the `solution.git.*` MCP tool namespace.
//!
//! Built on top of `solution`, `git_ui`, and existing `git` crates. Per
//! P-9 (inversion of control), this crate depends *downward* on `git_ui`
//! and registers trait providers (`git_ui::providers::*`) at `init()` —
//! `git_ui` never depends on `solution_git`.

use gpui::App;

pub fn init(_cx: &mut App) {
    // M4 implementation:
    // - register `solution.git.*` MCP tools
    // - install providers via `git_ui::providers::set_solution_panel_provider` etc.
    // - subscribe to SolutionStore events for dashboard refresh
}

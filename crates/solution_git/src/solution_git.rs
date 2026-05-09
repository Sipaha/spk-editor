//! Solution-aware git operations: aggregated log, status dashboard, solution-wide
//! commit/push, cross-member cherry-pick, branch protection.
//!
//! Per P-9 (inversion of control), this crate depends *downward* on `git_ui`
//! and registers trait providers (`git_ui::providers::*`) at `init()` —
//! `git_ui` never depends on `solution_git`.
//!
//! Owns the `solution.git.*` MCP tool namespace.

pub mod aggregator;
pub mod branch_protection;
pub mod dashboard;
pub mod mcp;

use gpui::App;
use settings::Settings as _;
use solutions::SolutionsSettings;

pub use aggregator::{
    DEFAULT_MAX_TOTAL_COMMITS, MEMBER_PALETTE_LEN, SolutionGitAggregator, member_color,
};
pub use dashboard::{OpenStatusDashboard, SolutionStatusDashboard};

pub fn init(cx: &mut App) {
    // S-SOL-LOG: build an aggregator wired to the global `SolutionStore`
    // (when present) and register it as the `LogDataSource` provider.
    // The aggregator follows the active Solution dynamically — pulling
    // `SolutionStore::solutions()` on every `fetch_log` call — so we
    // don't need to re-register on `ActiveSolutionChanged`. Providers
    // are `OnceLock`-backed (see `git_ui::providers`); registering here
    // keeps `solution_git::init` idempotent across hot-reload-like
    // flows.
    let cap = SolutionsSettings::get_global(cx)
        .aggregated_log
        .max_total_commits as usize;
    if let Some(aggregator) = aggregator::build_global_aggregator(cx, cap) {
        git_ui::providers::set_log_data_source(Box::new(aggregator));
    } else {
        log::debug!(
            "solution_git::init: SolutionStore global not installed — \
             LogDataSource not registered (likely a non-solution test context)"
        );
    }

    // Register MCP tools owned by this crate (`solution.git.*`).
    mcp::register(cx);
    dashboard::register_mcp(cx);

    // S-SOL-DSH — wire the `solution_git::OpenStatusDashboard` workspace
    // action so the command palette can open the dashboard pane item.
    cx.observe_new(|workspace: &mut workspace::Workspace, _, _| {
        dashboard::register(workspace);
    })
    .detach();
}

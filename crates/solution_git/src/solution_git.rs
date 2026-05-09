//! Solution-aware git operations: aggregated log, status dashboard, solution-wide
//! commit/push, cross-member cherry-pick, branch protection.
//!
//! Per P-9 (inversion of control), this crate depends *downward* on `git_ui`
//! and registers trait providers (`git_ui::providers::*`) at `init()` —
//! `git_ui` never depends on `solution_git`.
//!
//! Owns the `solution.git.*` MCP tool namespace.

pub mod aggregator;
pub mod ai_cherry_pick_suggest;
pub mod branch_protection;
pub mod commit;
pub mod cross_cherry_pick;
pub mod dashboard;
pub mod mcp;
pub mod push;

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

    // S-SOL-CMT: register the solution-wide commit orchestrator as the
    // `SolutionPanelProvider`. `git_panel` reaches in through the trait
    // when the user toggles `Solution-wide`. Registration is idempotent
    // (`OnceLock`-backed) so re-running `init` is safe.
    if let Some(orchestrator) = commit::build_global_orchestrator(cx) {
        let boxed: Box<dyn git_ui::providers::SolutionPanelProvider> = Box::new(orchestrator);
        git_ui::providers::set_solution_panel_provider(boxed);
    } else {
        log::debug!(
            "solution_git::init: SolutionStore global not installed — \
             SolutionPanelProvider not registered (likely a non-solution test context)"
        );
    }

    // S-SOL-PSH: register the solution-wide push orchestrator as the
    // `SolutionPushProvider`. `git_panel` (and the command-palette
    // `solution_git::PushAll` action) reach in through the trait when
    // the user triggers Push All. Idempotent (`OnceLock`-backed).
    if let Some(orchestrator) = push::build_global_orchestrator(cx) {
        let boxed: Box<dyn git_ui::providers::SolutionPushProvider> = Box::new(orchestrator);
        git_ui::providers::set_solution_push_provider(boxed);
    } else {
        log::debug!(
            "solution_git::init: SolutionStore global not installed — \
             SolutionPushProvider not registered (likely a non-solution test context)"
        );
    }

    // Register MCP tools owned by this crate (`solution.git.*`).
    mcp::register(cx);
    commit::mcp::register(cx);
    dashboard::register_mcp(cx);
    push::mcp::register(cx);
    cross_cherry_pick::mcp::register(cx);

    // S-SOL-DSH — wire the `solution_git::OpenStatusDashboard` workspace
    // action so the command palette can open the dashboard pane item.
    // S-SOL-PSH — wire `solution_git::PushAll` for the same surface plus
    // the dashboard's Push All toolbar button.
    // S-SOL-CHP — wire `solution_git::CrossCherryPick` so the command
    // palette and the git-graph context menu can dispatch it (the
    // context menu builds the action dynamically by name to avoid
    // adding a build-time dep from `git_ui` to `solution_git`).
    cx.observe_new(|workspace: &mut workspace::Workspace, _, _| {
        dashboard::register(workspace);
        push::register(workspace);
        cross_cherry_pick::register(workspace);
    })
    .detach();
}

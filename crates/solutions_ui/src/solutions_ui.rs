//! UI layer for the solutions crate: dock panel, picker, modals,
//! title-bar segment, status-bar widget, welcome integration.

mod actions;
mod dock_panel;
mod picker;

pub use actions::{
    NewSolution, OpenSolution, RefreshCacheForCurrent, ToggleSolutionsPanel,
};
pub use dock_panel::{SolutionsPanel, load};

use gpui::App;

pub fn init(cx: &mut App) {
    dock_panel::init(cx);
    cx.observe_new(picker::OpenSolutionModal::register).detach();
}

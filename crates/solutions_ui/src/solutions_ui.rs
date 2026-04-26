//! UI layer for the solutions crate: dock panel, picker, modals,
//! title-bar segment, status-bar widget, welcome integration.

mod actions;
mod add_member_picker;
mod dock_panel;
mod modals;
mod picker;
mod status_bar;
mod welcome;

pub use status_bar::SolutionsStatusItem;

pub use actions::{
    NewSolution, OpenSolution, RefreshCacheForCurrent, ToggleSolutionsPanel,
};
pub use dock_panel::{SolutionsPanel, load};

use gpui::App;

pub fn init(cx: &mut App) {
    dock_panel::init(cx);
    cx.observe_new(picker::OpenSolutionModal::register).detach();
    cx.observe_new(modals::register).detach();
    welcome::init(cx);
}

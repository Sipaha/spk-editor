//! Solutions: catalog of remote git projects + named groups (Solutions) that
//! open as a single editor window with all members mounted as worktrees.

mod model;
mod persistence;
mod settings;
mod slug;
mod store;

pub use model::{CatalogId, CatalogProject, Solution, SolutionId, SolutionMember};
pub use settings::SolutionsSettings;
pub use store::SolutionStore;

use ::settings::Settings;
use gpui::App;

pub fn init(cx: &mut App) {
    SolutionsSettings::register(cx);
    SolutionStore::init_global(cx);
}

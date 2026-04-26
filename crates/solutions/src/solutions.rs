//! Solutions: catalog of remote git projects + named groups (Solutions) that
//! open as a single editor window with all members mounted as worktrees.

mod cache;
mod event_sources;
mod git;
pub mod mcp;
mod model;
mod persistence;
mod settings;
mod slug;
mod store;

pub use cache::default_cache_root;
pub use event_sources::install as install_event_sources_for_test;
pub use model::{CatalogId, CatalogProject, Solution, SolutionId, SolutionMember};
pub use settings::SolutionsSettings;
pub use store::{SolutionStore, SolutionStoreEvent, install_global_for_test};

use ::settings::Settings;
use gpui::App;

pub fn init(cx: &mut App) {
    SolutionsSettings::register(cx);
    SolutionStore::init_global(cx);
    mcp::register(cx);
    event_sources::install(cx);
}

//! Solutions: catalog of remote git projects + named groups (Solutions) that
//! open as a single editor window with all members mounted as worktrees.

mod add_member;
mod auto_trust;
mod cache;
pub mod db;
mod event_sources;
pub mod git;
pub mod mcp;
mod model;
mod persistence;
mod settings;
mod slug;
mod store;
mod tabs_snapshot;

pub use add_member::{AddProgressCallback, PendingAddView};
pub use cache::default_cache_root;
pub use event_sources::install as install_event_sources_for_test;
pub use model::{CatalogId, CatalogProject, Solution, SolutionId, SolutionMember};
pub use settings::SolutionsSettings;
pub use store::{SolutionStore, SolutionStoreEvent, install_global_for_test};
pub use tabs_snapshot::{SolutionTabsSnapshot, TabSnapshots};

use ::settings::Settings;
use gpui::App;

pub fn init(cx: &mut App) {
    SolutionsSettings::register(cx);
    SolutionStore::init_global(cx);
    mcp::register(cx);
    event_sources::install(cx);
    // Auto-trust the root of any Solution whose member opens in a
    // workspace. Catalog membership IS the trust signal — see the
    // `auto_trust` module docs.
    auto_trust::init(cx).detach();
}

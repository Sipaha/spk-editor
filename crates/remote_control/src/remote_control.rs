//! Remote Control: state model + JSON-file persistence for the editor's
//! external Remote Control surface.
//!
//! R-1 is the UI / state-management slice — there is no network listener,
//! no QR rendering, no encryption. `RemoteControlStore::set_enabled(true)`
//! merely flips the persisted bit; the listener arrives in R-2.

mod model;
mod settings;
mod store;

pub use model::{AuthorizedClient, RemoteControlSettings};
pub use store::{RemoteControlStore, RemoteControlStoreEvent};

use gpui::App;

pub fn init(cx: &mut App) {
    RemoteControlStore::init_global(cx);
}

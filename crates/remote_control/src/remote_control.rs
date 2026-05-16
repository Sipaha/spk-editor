//! Remote Control: state model + JSON-file persistence + network listener
//! for the editor's external Remote Control surface.
//!
//! R-1 shipped settings + UI; R-1.5 added the QR popover; R-2 (this slice)
//! adds the network listener — TLS 1.3 + WebSocket upgrade + per-client
//! HMAC challenge auth + JSON-RPC dispatch — driven by toggling
//! `RemoteControlStore::set_enabled(true)`. Transport rationale is
//! ADR-0003.

pub mod allow_list;
pub mod auth;
pub mod cert;
pub mod dispatch;
pub mod listener;
mod model;
pub mod proxy;
mod settings;
mod store;

pub use model::{AuthorizedClient, RemoteControlSettings};
pub use store::{RemoteControlStore, RemoteControlStoreEvent};

use gpui::App;

pub fn init(cx: &mut App) {
    RemoteControlStore::init_global(cx);
}

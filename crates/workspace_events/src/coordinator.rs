use gpui::{App, Global};
use std::sync::atomic::AtomicU64;

#[allow(dead_code)] // removed in Task A2
pub struct WorkspaceEventCoordinator {
    pub(crate) seq: AtomicU64,
}

#[allow(dead_code)] // removed in Task A2
struct Global_(WorkspaceEventCoordinator);
impl Global for Global_ {}

pub fn install(cx: &mut App) {
    if cx.try_global::<Global_>().is_some() {
        return;
    }
    cx.set_global(Global_(WorkspaceEventCoordinator {
        seq: AtomicU64::new(0),
    }));
}

#[allow(dead_code)] // removed in Task A2
#[derive(Debug, Clone)]
pub enum WorkspaceEvent {}

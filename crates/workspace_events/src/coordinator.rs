use gpui::{App, Global};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct WorkspaceEventCoordinator {
    pub(crate) seq: AtomicU64,
}

impl WorkspaceEventCoordinator {
    pub fn global(cx: &App) -> &Self {
        &cx.global::<GlobalWorkspaceEventCoordinator>().0
    }

    pub fn current_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    /// Increment and return the new value. Use this on every mutation that
    /// emits a sequenced workspace event.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }
}

struct GlobalWorkspaceEventCoordinator(WorkspaceEventCoordinator);
impl Global for GlobalWorkspaceEventCoordinator {}

pub fn install(cx: &mut App) {
    if cx.try_global::<GlobalWorkspaceEventCoordinator>().is_some() {
        return;
    }
    cx.set_global(GlobalWorkspaceEventCoordinator(WorkspaceEventCoordinator {
        seq: AtomicU64::new(0),
    }));
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum WorkspaceEvent {}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn seq_starts_at_zero_and_increments(cx: &mut TestAppContext) {
        cx.update(install);
        cx.update(|cx| {
            let coord = WorkspaceEventCoordinator::global(cx);
            assert_eq!(coord.current_seq(), 0);
            assert_eq!(coord.next_seq(), 1);
            assert_eq!(coord.next_seq(), 2);
            assert_eq!(coord.current_seq(), 2);
        });
    }

    #[gpui::test]
    async fn install_is_idempotent(cx: &mut TestAppContext) {
        cx.update(install);
        cx.update(install);
        cx.update(|cx| {
            let coord = WorkspaceEventCoordinator::global(cx);
            assert_eq!(coord.current_seq(), 0);
        });
    }
}

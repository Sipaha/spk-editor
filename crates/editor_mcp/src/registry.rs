//! Tool registry: holds boxed registration callbacks until `start_server`
//! drains them and applies to the live `McpServer`.
use context_server::listener::McpServer;
use gpui::{App, Global};
use std::cell::RefCell;

type Registration = Box<dyn FnOnce(&mut McpServer) + 'static>;

#[derive(Default)]
pub(crate) struct Registry {
    pending: RefCell<Vec<Registration>>,
    started: RefCell<bool>,
}

impl Global for Registry {}

pub fn init(cx: &mut App) {
    if cx.try_global::<Registry>().is_none() {
        cx.set_global(Registry::default());
        register_builtin_tools(cx);
    }
}

pub fn register_tool<F>(cx: &mut App, registration: F)
where
    F: FnOnce(&mut McpServer) + 'static,
{
    let registry = cx.global::<Registry>();
    if *registry.started.borrow() {
        debug_assert!(false, "register_tool called after start_server");
        log::error!("editor_mcp: register_tool called after start_server — tool not registered");
        return;
    }
    registry.pending.borrow_mut().push(Box::new(registration));
}

pub(crate) fn drain(cx: &mut App) -> Vec<Registration> {
    let registry = cx.global::<Registry>();
    std::mem::take(&mut *registry.pending.borrow_mut())
}

pub(crate) fn mark_started(cx: &mut App) {
    let registry = cx.global::<Registry>();
    *registry.started.borrow_mut() = true;
}

#[cfg(test)]
pub(crate) fn pending_count(cx: &App) -> usize {
    cx.global::<Registry>().pending.borrow().len()
}

pub(crate) fn register_builtin_tools(cx: &mut App) {
    register_tool(cx, |server| {
        server.add_tool(crate::tools::capabilities::CapabilitiesTool);
    });
    register_tool(cx, |server| {
        server.add_tool(crate::tools::handle_cli_args::HandleCliArgsTool);
    });
    register_tool(cx, |server| {
        server.add_tool(crate::tools::windows::ListWindowsTool);
    });
    register_tool(cx, |server| {
        server.add_tool(crate::tools::windows::FocusWindowTool);
    });
    register_tool(cx, |server| {
        server.add_tool(crate::tools::windows::CloseWindowTool);
    });
    register_tool(cx, |server| {
        server.add_tool(crate::tools::windows::DispatchActionTool);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn registry_collects_registrations(cx: &mut TestAppContext) {
        cx.update(|cx| {
            init(cx);
            let baseline = pending_count(cx);
            register_tool(cx, |_server| {
                // captures, doesn't need to actually do anything
            });
            register_tool(cx, |_server| {});
            assert_eq!(pending_count(cx), baseline + 2);
        });
    }

    #[gpui::test]
    async fn drain_removes_pending(cx: &mut TestAppContext) {
        cx.update(|cx| {
            init(cx);
            let baseline = pending_count(cx);
            register_tool(cx, |_| {});
            register_tool(cx, |_| {});
            let drained = drain(cx);
            assert_eq!(drained.len(), baseline + 2);
            assert_eq!(pending_count(cx), 0);
        });
    }

    #[gpui::test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "register_tool called after start_server")]
    async fn register_after_start_panics_in_debug(cx: &mut TestAppContext) {
        cx.update(|cx| {
            init(cx);
            mark_started(cx);
            register_tool(cx, |_| {});
        });
    }
}

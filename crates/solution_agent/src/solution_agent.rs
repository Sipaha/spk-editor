//! Solution-scoped AI sessions: N parallel Claude Code-style chats per Solution,
//! multiplexed onto a shared subprocess per (solution, agent) pair.
//!
//! See `docs/superpowers/specs/2026-04-26-solution-scoped-ai-sessions-design.md`
//! for the design rationale.

pub mod actions;
pub mod model;
pub mod adapter;
pub mod claude_adapter;
pub(crate) mod db;
pub mod event_sources;
pub mod mcp;
pub mod navigator;
pub mod notifier;
pub(crate) mod pool;
pub mod session_view;
pub mod status_item;
pub mod store;

#[cfg(any(feature = "test-support", test))]
pub mod test_support;

pub use model::{
    AgentServerId, SessionState, SolutionSession, SolutionSessionId, SolutionSessionMetadata,
};

use std::rc::Rc;
use std::sync::Arc;

use agent_servers::CustomAgentServer;
use gpui::{App, AppContext, AsyncApp, SharedString};
use project::agent_server_store::AgentId;

pub fn init(cx: &mut App) {
    let mut adapters = adapter::AdapterRegistry::new();
    adapters.register(Arc::new(claude_adapter::ClaudeAcpAdapter));
    let adapters = Arc::new(adapters);

    store::SolutionAgentStore::init_global(cx, adapters);

    // Register the AgentServer instance for `claude-acp`. `CustomAgentServer`
    // is a thin wrapper — its `connect()` looks up the actual subprocess
    // command via the per-Project `AgentServerStore` at session-creation time,
    // so this single registration is enough to enable real `claude` spawning
    // for any open Solution that the user has the CLI installed for.
    let claude_id = AgentId(SharedString::from(claude_adapter::CLAUDE_ACP_AGENT_ID));
    let claude_server: Rc<dyn agent_servers::AgentServer> =
        Rc::new(CustomAgentServer::new(claude_id));
    store::SolutionAgentStore::global(cx).update(cx, |store, _cx| {
        store.register_agent_server(
            SharedString::from(claude_adapter::CLAUDE_ACP_AGENT_ID),
            claude_server,
        );
    });

    // Connect the persistence DB asynchronously and wire it into the store
    // once it's ready. Failure to open the DB is logged but non-fatal — the
    // store falls back to in-memory state.
    let db_task = db::SolutionAgentDb::connect(cx);
    cx.spawn(async move |cx: &mut AsyncApp| {
        match db_task.await {
            Ok(db) => {
                cx.update(|cx| {
                    let store = store::SolutionAgentStore::global(cx);
                    store.update(cx, |store, _| store.set_persistence(db));
                });
            }
            Err(err) => {
                log::error!("solution_agent: failed to open persistence DB: {err}");
            }
        }
    })
    .detach();

    mcp::register(cx);
    event_sources::install(cx);

    // Workspace hook for navigator + status item registration. NOTE: the
    // navigator's `set_active_solution` is intentionally left UNWIRED in v1 —
    // the navigator will render an empty "Sessions" panel until a setter is
    // called from outside (e.g. when the workspace's first project root maps
    // to a Solution). Wiring that signal is deferred to a follow-up; the live
    // MCP probe (Task 7.2) will surface this gap if it matters.
    cx.observe_new::<workspace::Workspace>(|workspace, window, cx| {
        let Some(window) = window else {
            return;
        };

        let weak = workspace.weak_handle();
        let navigator = cx.new(|cx| navigator::SolutionSessionsNavigator::new(weak, cx));
        workspace.add_panel(navigator, window, cx);

        let status_item = cx.new(|cx| status_item::SolutionAgentStatusItem::new(cx));
        workspace.status_bar().update(cx, |bar, cx| {
            bar.add_right_item(status_item, window, cx);
        });
    })
    .detach();
}

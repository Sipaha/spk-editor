//! Solution-scoped AI sessions: N parallel Claude Code-style chats per Solution,
//! multiplexed onto a shared subprocess per (solution, agent) pair.
//!
//! See `docs/superpowers/specs/2026-04-26-solution-scoped-ai-sessions-design.md`
//! for the design rationale.

pub mod actions;
pub mod adapter;
pub mod agent_settings;
pub mod claude_adapter;
pub(crate) mod cold_persistence;
pub(crate) mod compact;
pub(crate) mod conversation_render;
pub(crate) mod db;
pub mod event_sources;
pub(crate) mod expanded_compose;
pub mod mcp;
pub mod message_generator;
pub mod model;
pub mod navigator;
pub mod notifier;
pub(crate) mod pool;
pub(crate) mod rename_session_modal;
pub mod session_view;
pub(crate) mod slash_commands;
pub mod status_item;
pub(crate) mod status_row;
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
    use ::settings::Settings as _;
    agent_settings::SolutionAgentSettings::register(cx);

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
    cx.spawn(async move |cx: &mut AsyncApp| match db_task.await {
        Ok(db) => {
            cx.update(|cx| {
                let store = store::SolutionAgentStore::global(cx);
                store.update(cx, |store, _| store.set_persistence(db));
            });
        }
        Err(err) => {
            log::error!("solution_agent: failed to open persistence DB: {err}");
        }
    })
    .detach();

    mcp::register(cx);
    event_sources::install(cx);

    // Workspace hook for navigator + status item registration. The navigator
    // derives its active Solution from the workspace's project worktrees on
    // construction; SolutionStore subscriptions inside the navigator itself
    // refresh that derivation when Solutions change. We re-derive on every
    // project event too, so adding/removing a worktree retargets the panel.
    cx.observe_new::<workspace::Workspace>(|workspace, window, cx| {
        let Some(window) = window else {
            return;
        };

        let weak = workspace.weak_handle();
        let weak_project = workspace.project().downgrade();
        let navigator =
            cx.new(|cx| navigator::SolutionSessionsNavigator::new(weak, weak_project, window, cx));

        // Initial active-solution derivation is deferred to the next App tick
        // so it runs *after* the surrounding `observe_new<Workspace>` update
        // closes — calling `workspace.read(cx)` synchronously here panics
        // with "cannot read Workspace while it is already being updated".
        // `Window::defer` so the deferred closure also receives the window
        // refresh_active_solution needs for tab-strip reconciliation.
        window.defer(cx, {
            let nav = navigator.downgrade();
            move |window, cx| {
                nav.update(cx, |nav, cx| nav.refresh_active_solution(window, cx))
                    .ok();
            }
        });

        // Project worktrees can come and go after the workspace opens (think
        // `solutions.add_member` mid-session). Drive `refresh_active_solution`
        // from project events so the panel retargets without the user having
        // to close and reopen the workspace.
        let project = workspace.project().clone();
        cx.subscribe_in(&project, window, {
            let nav = navigator.downgrade();
            move |_, _, _: &project::Event, window, cx| {
                nav.update(cx, |nav, cx| nav.refresh_active_solution(window, cx))
                    .ok();
            }
        })
        .detach();

        workspace.add_panel(navigator, window, cx);

        // Without this handler the panel's `toggle_action` (returned from
        // `Panel::toggle_action`) dispatches into a void: the sidebar icon
        // click and the keybind both look wired but the dock never reveals.
        workspace.register_action(|workspace, _: &actions::FocusNavigator, window, cx| {
            workspace.toggle_panel_focus::<navigator::SolutionSessionsNavigator>(window, cx);
        });

        let status_item = cx.new(|cx| status_item::SolutionAgentStatusItem::new(cx));
        workspace.status_bar().update(cx, |bar, cx| {
            bar.add_right_item(status_item, window, cx);
        });
    })
    .detach();
}

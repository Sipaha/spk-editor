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
pub mod upload;

#[cfg(any(feature = "test-support", test))]
pub mod test_support;

pub use model::{
    AgentServerId, SessionState, SolutionSession, SolutionSessionId, SolutionSessionMetadata,
};

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

    // Chunked-upload manager: shared between the listener (pure tokio
    // binary-frame handler) and the `solution_agent.upload_*` MCP tools
    // (GPUI context). Bytes land under `<editor_mcp::runtime_dir>/uploads/`
    // so they share the lifetime of the editor's runtime root — tests can
    // pin this via `editor_mcp::set_runtime_dir_for_test`.
    let tmp_root = editor_mcp::runtime_dir().join("uploads");
    match upload::UploadManager::new(tmp_root) {
        Ok(manager) => {
            let handle = Arc::new(Mutex::new(manager));
            upload::install(handle);
            spawn_upload_ack_drainer(cx);
            spawn_upload_gc(cx);
        }
        Err(err) => {
            log::error!("solution_agent: failed to init upload manager: {err}");
        }
    }

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

/// Drain queued chunk-ack events from the `UploadManager` and broadcast each
/// one as an `upload_chunk_acked` MCP notification. The listener (pure tokio)
/// can't call `editor_mcp::emit_notification` directly because the underlying
/// `McpServer` uses `RefCell` and must be touched from the GPUI thread, so the
/// ack queue inside `UploadManager` is the cross-thread hand-off.
///
/// 100ms tick is fast enough that mobile progress bars feel live but slow
/// enough that an idle editor isn't waking up for nothing. The drainer only
/// emits when the queue has acks — empty drains are a single Vec::take + early
/// continue.
fn spawn_upload_ack_drainer(cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            // `AsyncApp::update` panics if the App was dropped — the task
            // is detached, so the panic is contained to this task (matches
            // every other detached `cx.spawn` site in the crate).
            cx.update(|cx| {
                let acks = upload::with_manager(|m| m.drain_acks()).unwrap_or_default();
                for ack in acks {
                    log::info!(
                        target: "solution_agent::upload",
                        "drainer emit upload_chunk_acked: upload_id={} received={}",
                        ack.upload_id,
                        ack.received_bytes,
                    );
                    let payload = serde_json::json!({
                        "upload_id": ack.upload_id,
                        "received_bytes": ack.received_bytes,
                    });
                    editor_mcp::emit_notification(cx, "upload_chunk_acked", payload);
                }
            });
        }
    })
    .detach();
}

/// Reap stale uploads every 5 minutes. An attacker who could exhaust disk by
/// init-ing thousands of uploads + never finishing is bounded by the
/// per-session cap inside `UploadManager`, but the periodic GC catches the
/// "legitimate client uploaded and crashed" case too.
fn spawn_upload_gc(cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor()
                .timer(Duration::from_secs(5 * 60))
                .await;
            cx.update(|_cx| {
                upload::with_manager(|m| {
                    let n = m.gc(std::time::Instant::now(), upload::UPLOAD_TTL);
                    if n > 0 {
                        log::info!("upload::gc: reaped {n} expired entries");
                    }
                });
            });
        }
    })
    .detach();
}

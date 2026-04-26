//! MCP event-source wiring.
//!
//! Owns a global coordinator entity that subscribes to in-process event
//! emitters (SolutionStore, BufferStore, LspStore, MultiWorkspace activation)
//! and republishes them as `editor/notification` MCP messages so connected
//! clients can react in real time without polling.
//!
//! Wired here because this crate already depends on `editor_mcp`, `workspace`,
//! `project`, and `language` — adding any of those as deps of `editor_mcp`
//! would create the architectural cycles that earlier phases already had to
//! resolve.
//!
//! Wired event kinds: `solution_changed`, `buffer_opened`, `buffer_closed`,
//! `buffer_saved`, `buffer_dirty_changed`, `diagnostic_updated`,
//! `lsp_started`, `lsp_stopped`, `window_focused`.
//!
//! Deferred:
//! - `selection_changed` lives on `Editor` (not `Buffer`); needs a separate
//!   wiring point once a stable cursor-event source is identified.
//! - `cli_args_received` would fire from the handoff RPC handler.
//! - `server_shutting_down` requires a shutdown hook that doesn't exist yet.
//!
//! Known limitation: `Workspace` instances added to an already-existing
//! `MultiWorkspace` (post-`observe_new`) are not auto-wired. In practice
//! workspaces are configured at window-open time; dynamic add/remove via
//! `MultiWorkspaceEvent::WorkspaceAdded` is a follow-up.

use crate::{SolutionStore, SolutionStoreEvent};
use collections::HashMap;
use gpui::{App, AppContext as _, Entity, Global, Subscription};
use language::{Buffer, BufferEvent, BufferId, File as _};
use project::{
    LspStoreEvent, buffer_store::BufferStoreEvent, lsp_store::LspStore,
};
use serde_json::json;
use std::path::PathBuf;
use workspace::MultiWorkspace;

pub struct EventSourceCoordinator {
    /// Long-lived subscriptions: SolutionStore, observe_new::<MultiWorkspace>,
    /// per-window/project (BufferStore + LspStore + window activation).
    /// Order doesn't matter; they live until the coordinator is dropped.
    subscriptions: Vec<Subscription>,
    /// Per-buffer Saved/DirtyChanged subs, keyed by `BufferId`. Removed on
    /// `BufferStoreEvent::BufferDropped`. Without this map we couldn't drop
    /// the subscription in response to the buffer going away.
    buffer_subs: HashMap<BufferId, Subscription>,
    /// `BufferId → last-known absolute path` so we can include `path` in
    /// `buffer_closed` payloads (the `BufferDropped` event itself only
    /// carries the id).
    buffer_paths: HashMap<BufferId, Option<String>>,
}

struct GlobalEventSourceCoordinator(#[allow(dead_code)] Entity<EventSourceCoordinator>);
impl Global for GlobalEventSourceCoordinator {}

/// Install the coordinator as a global. Idempotent: a second call is a
/// no-op (useful in tests that re-enter `solutions::init`).
pub fn install(cx: &mut App) {
    if cx.try_global::<GlobalEventSourceCoordinator>().is_some() {
        return;
    }

    let coordinator = cx.new(|_| EventSourceCoordinator {
        subscriptions: Vec::new(),
        buffer_subs: HashMap::default(),
        buffer_paths: HashMap::default(),
    });

    coordinator.update(cx, |this, cx| {
        if let Some(store) = SolutionStore::try_global(cx) {
            this.subscriptions.push(cx.subscribe(
                &store,
                |_this, _store, event, cx| match event {
                    SolutionStoreEvent::Changed => {
                        editor_mcp::emit_notification(cx, "solution_changed", json!({}));
                    }
                },
            ));
        }
    });

    let observe_sub = {
        let coordinator_weak = coordinator.downgrade();
        cx.observe_new::<MultiWorkspace>(move |multi, mut window_opt, cx| {
            let activation_sub = window_opt.as_deref_mut().map(|window| {
                cx.observe_window_activation(window, |_multi, window, cx| {
                    if window.is_window_active() {
                        editor_mcp::emit_notification(
                            cx,
                            "window_focused",
                            json!({
                                "window_id": editor_mcp::format_window_id(
                                    window.window_handle().window_id()
                                ),
                            }),
                        );
                    }
                })
            });

            let workspaces: Vec<_> = multi.workspaces().cloned().collect();

            if let Some(coord) = coordinator_weak.upgrade() {
                coord.update(cx, |this, cx| {
                    if let Some(sub) = activation_sub {
                        this.subscriptions.push(sub);
                    }
                    for workspace in workspaces {
                        let project = workspace.read(cx).project().clone();
                        wire_project(this, &project, cx);
                    }
                });
            }
        })
    };

    coordinator.update(cx, |this, _cx| {
        this.subscriptions.push(observe_sub);
    });

    cx.set_global(GlobalEventSourceCoordinator(coordinator));
}

fn wire_project(
    this: &mut EventSourceCoordinator,
    project: &Entity<project::Project>,
    cx: &mut gpui::Context<EventSourceCoordinator>,
) {
    let lsp_store = project.read(cx).lsp_store();
    this.subscriptions
        .push(cx.subscribe(&lsp_store, on_lsp_event));

    let buffer_store = project.read(cx).buffer_store().clone();
    this.subscriptions.push(cx.subscribe(
        &buffer_store,
        |this, _store, event, cx| match event {
            BufferStoreEvent::BufferAdded(buffer) => {
                let buffer_id = buffer.read(cx).remote_id();
                let path = buffer_abs_path(buffer, cx);
                this.buffer_paths.insert(buffer_id, path.clone());
                editor_mcp::emit_notification(
                    cx,
                    "buffer_opened",
                    json!({
                        "buffer_id": u64::from(buffer_id),
                        "path": path,
                    }),
                );

                let sub = cx.subscribe(buffer, |this, buffer_entity, event, cx| {
                    on_buffer_event(this, &buffer_entity, event, cx);
                });
                this.buffer_subs.insert(buffer_id, sub);
            }
            BufferStoreEvent::BufferDropped(buffer_id) => {
                let path = this.buffer_paths.remove(buffer_id).flatten();
                this.buffer_subs.remove(buffer_id);
                editor_mcp::emit_notification(
                    cx,
                    "buffer_closed",
                    json!({
                        "buffer_id": u64::from(*buffer_id),
                        "path": path,
                    }),
                );
            }
            BufferStoreEvent::BufferChangedFilePath { buffer, .. } => {
                let buffer_id = buffer.read(cx).remote_id();
                let new_path = buffer_abs_path(buffer, cx);
                this.buffer_paths.insert(buffer_id, new_path);
            }
            _ => {}
        },
    ));
}

fn on_lsp_event(
    _this: &mut EventSourceCoordinator,
    _store: Entity<LspStore>,
    event: &LspStoreEvent,
    cx: &mut gpui::Context<EventSourceCoordinator>,
) {
    match event {
        LspStoreEvent::LanguageServerAdded(server_id, name, worktree_id) => {
            editor_mcp::emit_notification(
                cx,
                "lsp_started",
                json!({
                    "server_id": server_id.0 as u64,
                    "name": name.0.as_ref(),
                    "worktree_id": worktree_id.map(|id| id.to_proto()),
                }),
            );
        }
        LspStoreEvent::LanguageServerRemoved(server_id) => {
            editor_mcp::emit_notification(
                cx,
                "lsp_stopped",
                json!({ "server_id": server_id.0 as u64 }),
            );
        }
        LspStoreEvent::DiagnosticsUpdated { server_id, paths } => {
            let path_strs: Vec<String> = paths
                .iter()
                .map(|p| p.path.as_unix_str().to_string())
                .collect();
            editor_mcp::emit_notification(
                cx,
                "diagnostic_updated",
                json!({
                    "server_id": server_id.0 as u64,
                    "paths": path_strs,
                }),
            );
        }
        _ => {}
    }
}

fn on_buffer_event(
    this: &mut EventSourceCoordinator,
    buffer: &Entity<Buffer>,
    event: &BufferEvent,
    cx: &mut gpui::Context<EventSourceCoordinator>,
) {
    let buffer_id = buffer.read(cx).remote_id();
    match event {
        BufferEvent::Saved => {
            let path = buffer_abs_path(buffer, cx);
            this.buffer_paths.insert(buffer_id, path.clone());
            editor_mcp::emit_notification(
                cx,
                "buffer_saved",
                json!({
                    "buffer_id": u64::from(buffer_id),
                    "path": path,
                }),
            );
        }
        BufferEvent::DirtyChanged => {
            let path = this
                .buffer_paths
                .get(&buffer_id)
                .cloned()
                .unwrap_or_else(|| buffer_abs_path(buffer, cx));
            let is_dirty = buffer.read(cx).is_dirty();
            editor_mcp::emit_notification(
                cx,
                "buffer_dirty_changed",
                json!({
                    "buffer_id": u64::from(buffer_id),
                    "path": path,
                    "is_dirty": is_dirty,
                }),
            );
        }
        _ => {}
    }
}

fn buffer_abs_path(buffer: &Entity<Buffer>, cx: &App) -> Option<String> {
    let file = buffer.read(cx).file()?.clone();
    // For local buffers, prefer `LocalFile::abs_path` (full filesystem path);
    // remote buffers fall back to `File::full_path` (worktree-relative-with-root,
    // the only path representation available cross-host).
    let abs: PathBuf = project::File::from_dyn(Some(&file))
        .filter(|f| f.is_local())
        .map(|f| <project::File as language::LocalFile>::abs_path(f, cx))
        .unwrap_or_else(|| file.full_path(cx));
    Some(abs.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::install_global_for_test;
    use gpui::TestAppContext;
    use tempfile::tempdir;

    #[gpui::test]
    async fn install_is_idempotent(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            install_global_for_test(store, cx);
            install(cx);
            install(cx);
            assert!(cx.try_global::<GlobalEventSourceCoordinator>().is_some());
        });
    }

    #[gpui::test]
    async fn solution_changed_event_does_not_panic(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let dir = tempdir().expect("tempdir");
            let store = SolutionStore::for_test(dir.path().join("c.json"), cx);
            install_global_for_test(store.clone(), cx);
            install(cx);
            // Emit Changed via the store (no MCP server connected — emit is a
            // no-op, but we exercise the subscription path end-to-end).
            store.update(cx, |_s, cx| {
                cx.emit(SolutionStoreEvent::Changed);
            });
        });
        cx.run_until_parked();
    }
}

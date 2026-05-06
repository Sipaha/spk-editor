//! In-place Solution switch within a single open window.
//!
//! The earlier path (`open::open_solution` with `OpenIntent::SameWindow`)
//! built a *new* `Workspace` for every Solution-switch, retained the
//! old one, and let `MultiWorkspace::activate` flip between them. That
//! preserves the old Solution's panel state because the entity stays
//! alive — but only at the cost of visible UI churn on every switch:
//! the new Workspace mounts with default panels, then the user has to
//! re-establish their layout / re-find their tabs.
//!
//! `switch_active_solution_in_place` flips this around: keep the same
//! `Workspace`/`Project`/dock entities alive, swap *worktrees* inside
//! the existing `Project` to match the target Solution's members
//! (`Workspace::swap_worktrees_to`), and snapshot/replay the per-
//! Solution open-tab list through `SolutionStore::tab_snapshots`.
//! Upstream panels (`ProjectPanel`, `OutlinePanel`, …) react to
//! `project::Event::WorktreeAdded`/`Removed` automatically; fork
//! panels listen to `SolutionStoreEvent::ActiveSolutionChanged`.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui::{App, Entity, Task, WeakEntity, Window};
use solutions::{SolutionId, SolutionStore, SolutionTabsSnapshot};
use util::ResultExt as _;
use workspace::{OpenOptions, OpenVisible, SaveIntent, Workspace};

/// Run an in-place Solution switch on the given `Workspace`. Steps:
///
/// 1. Identify the *previous* Solution by scanning the workspace's
///    visible worktrees through `SolutionStore::solution_for_path`.
///    If found, snapshot its open tabs into
///    `SolutionStore::tab_snapshots` so a future switch back can
///    restore them.
/// 2. `touch_last_opened(target_id)` — bumps the activity stamp and
///    fires `Changed` + `ActiveSolutionChanged(target_id)` so fork
///    panels can refresh their content.
/// 3. Resolve `target_id`'s member paths and call
///    `Workspace::swap_worktrees_to` to reconcile.
/// 4. Replay `target_id`'s saved open-tab snapshot (if any) by
///    closing every currently-open editor and re-opening the
///    snapshot's `open_paths` in order, activating `active_path`
///    last.
///
/// Snapshot save failures (step 1) are logged-and-continued — the
/// user wants to *get to* the new Solution; losing one tab list is
/// recoverable. A worktree-swap failure (step 3) is propagated as
/// `Err` so callers can surface it via toast / status.
pub fn switch_active_solution_in_place(
    workspace: WeakEntity<Workspace>,
    target_id: SolutionId,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<()>> {
    window.spawn(cx, async move |cx| {
        let workspace = workspace.upgrade().context("workspace dropped")?;

        // Step 1: snapshot current.
        let prev_id = previous_solution_id(&workspace, cx)?;
        if let Some(prev_id) = prev_id.clone() {
            // `Entity::update` on `AsyncWindowContext` returns `R`
            // directly (not `Result<R>`); the entity-released case
            // surfaces only when calling through `update_in`. Plain
            // reads here can't observe a dropped entity (we just
            // upgraded the Weak).
            let snapshot = workspace.update(cx, |workspace, cx| {
                let app: &App = cx;
                SolutionTabsSnapshot {
                    open_paths: workspace.open_item_abs_paths(app),
                    active_path: workspace
                        .active_item(app)
                        .and_then(|item| item.project_path(app))
                        .and_then(|pp| workspace.project().read(app).absolute_path(&pp, app)),
                }
            });
            cx.update(|_, cx| {
                if let Some(store) = SolutionStore::try_global(cx) {
                    store.update(cx, |store, cx| {
                        store.store_tab_snapshot(prev_id, snapshot, cx);
                    });
                }
            })
            .ok();
        }

        // Step 2: bump active id (also fires ActiveSolutionChanged).
        cx.update(|_, cx| {
            if let Some(store) = SolutionStore::try_global(cx) {
                store
                    .update(cx, |s, cx| s.touch_last_opened(&target_id, cx))
                    .log_err();
            }
        })
        .ok();

        // Step 3: resolve + swap worktrees.
        let target_paths: Vec<PathBuf> = cx
            .update(|_, cx| {
                SolutionStore::try_global(cx)
                    .and_then(|store| {
                        store.read_with(cx, |s, _| s.paths_for_open(&target_id).log_err())
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        if target_paths.is_empty() {
            // Target Solution has no members yet (newly-created or a
            // legacy row with empty member list). Nothing to swap to —
            // we still keep `active` flipped so the panel chrome
            // reflects the new selection, but don't tear down the
            // existing worktrees because that'd leave the user
            // staring at a panel-less editor.
            return Ok(());
        }
        let swap_task = workspace.update_in(cx, |workspace, window, cx| {
            workspace.swap_worktrees_to(target_paths, window, cx)
        })?;
        swap_task.await?;

        // Step 4: restore tabs.
        let snapshot = cx
            .update(|_, cx| {
                SolutionStore::try_global(cx).and_then(|store| {
                    store.read_with(cx, |s, _| s.tab_snapshot(&target_id).cloned())
                })
            })
            .ok()
            .flatten();
        if let Some(snapshot) = snapshot {
            close_all_editor_items(&workspace, cx).await?;
            for path in &snapshot.open_paths {
                let task = workspace.update_in(cx, |workspace, window, cx| {
                    let mut options = OpenOptions::default();
                    options.visible = Some(OpenVisible::None);
                    workspace.open_abs_path(path.clone(), options, window, cx)
                })?;
                let _ = task.await;
            }
            if let Some(active) = snapshot.active_path {
                let task = workspace.update_in(cx, |workspace, window, cx| {
                    workspace.open_abs_path(active, OpenOptions::default(), window, cx)
                })?;
                let _ = task.await;
            }
        }
        Ok(())
    })
}

fn previous_solution_id(
    workspace: &Entity<Workspace>,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<Option<SolutionId>> {
    cx.update(|_, cx| {
        let Some(store) = SolutionStore::try_global(cx) else {
            return None;
        };
        let store_read = store.read(cx);
        let project = workspace.read(cx).project().clone();
        project
            .read(cx)
            .visible_worktrees(cx)
            .find_map(|wt| store_read.solution_for_path(&wt.read(cx).abs_path()))
            .map(|sol| sol.id.clone())
    })
    .map_err(Into::into)
}

async fn close_all_editor_items(
    workspace: &Entity<Workspace>,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<()> {
    let item_ids: Vec<_> = workspace.update(cx, |workspace, cx| {
        let app: &App = cx;
        workspace
            .items(app)
            .map(|item| item.item_id())
            .collect::<Vec<_>>()
    });
    for id in item_ids {
        let close_task = workspace.update_in(cx, |workspace, window, cx| {
            let active_pane = workspace.active_pane().clone();
            active_pane.update(cx, |pane, cx| {
                pane.close_item_by_id(id, SaveIntent::Skip, window, cx)
            })
        })?;
        let _ = close_task.await;
    }
    Ok(())
}

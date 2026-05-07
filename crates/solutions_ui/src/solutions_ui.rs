//! UI layer for the solutions crate: title-bar tab strip, picker,
//! modals, status-bar widget, welcome integration.

mod actions;
pub mod active_project_selector;
mod add_member_picker;
pub mod delete_confirm_modal;
mod empty_solution_page;
mod modals;
mod open;
mod picker;
pub mod solution_picker_dropdown;
pub mod solution_tab;
pub mod solution_tab_strip;
mod status_bar;
mod switch;
mod welcome;
mod welcome_trigger;
pub mod window_helpers;

pub use active_project_selector::ActiveProjectSelector;
pub use empty_solution_page::EmptySolutionPage;
pub use open::{OpenIntent, open_solution};
pub use status_bar::SolutionsStatusItem;
pub use switch::switch_active_solution_in_place;

pub use actions::{DeleteSolution, NewSolution, OpenSolution, RefreshCacheForCurrent};

use gpui::{App, AppContext as _, Window};
use solutions::{SolutionId, SolutionStore};
use ui::SharedString;
use workspace::Workspace;

use crate::actions::{
    CloseSolutionFromTabBar, DeleteSolutionFromTabBar, RenameSolution, RemoveMember,
    RevealSolutionFolder, SwitchToNextProjectInPanel, SwitchToPrevProjectInPanel,
    SwitchToNextSolution, SwitchToPrevSolution,
};

pub fn init(cx: &mut App) {
    cx.observe_new(picker::OpenSolutionModal::register).detach();
    cx.observe_new(modals::register).detach();
    cx.observe_new(register_tab_actions).detach();
    welcome::init(cx);
    switch::register_mcp(cx);
}

fn register_tab_actions(
    workspace: &mut Workspace,
    _: Option<&mut Window>,
    _: &mut gpui::Context<Workspace>,
) {
    workspace.register_action(|workspace, action: &CloseSolutionFromTabBar, window, cx| {
        let id = SolutionId(action.id.clone());
        close_solution(workspace, id, window, cx);
    });
    workspace.register_action(|workspace, action: &DeleteSolutionFromTabBar, window, cx| {
        let id = SolutionId(action.id.clone());
        let store = SolutionStore::global(cx);
        let Some((name, root)) = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == id)
                .map(|sol| (sol.name.clone(), sol.root.clone()))
        }) else {
            return;
        };
        let folder_label = SharedString::from(format!("Folder {}", root.display()));
        crate::delete_confirm_modal::open_delete_confirm(
            workspace,
            SharedString::from(format!("Delete solution \"{name}\"?")),
            "This will permanently delete:",
            vec![
                crate::delete_confirm_modal::DeleteConfirmItem {
                    label: "Registry entry".into(),
                    path: None,
                },
                crate::delete_confirm_modal::DeleteConfirmItem {
                    label: folder_label,
                    path: Some(root),
                },
            ],
            move |_window, cx| {
                cx.dispatch_action(&crate::actions::DeleteSolution { id: id.0 });
            },
            window,
            cx,
        );
    });
    workspace.register_action(|_workspace, action: &RevealSolutionFolder, _window, cx| {
        let id = SolutionId(action.id.clone());
        let store = SolutionStore::global(cx);
        let Some(root) = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == id)
                .map(|sol| sol.root.clone())
        }) else {
            return;
        };
        cx.reveal_path(&root);
    });
    workspace.register_action(|workspace, action: &RenameSolution, window, cx| {
        let id = SolutionId(action.id.clone());
        crate::modals::open_rename_solution(workspace, id, window, cx);
    });
    workspace.register_action(|_workspace, _: &SwitchToNextSolution, _, cx| {
        crate::switch::cycle_solution(1, cx);
    });
    workspace.register_action(|_workspace, _: &SwitchToPrevSolution, _, cx| {
        crate::switch::cycle_solution(-1, cx);
    });
    workspace.register_action(|workspace, action: &SwitchToNextProjectInPanel, _, cx| {
        cycle_project_in_panel(workspace, &action.panel_kind, 1, cx);
    });
    workspace.register_action(|workspace, action: &SwitchToPrevProjectInPanel, _, cx| {
        cycle_project_in_panel(workspace, &action.panel_kind, -1, cx);
    });
    workspace.register_action(|workspace, action: &RemoveMember, window, cx| {
        use util::ResultExt as _;

        let sol_id = SolutionId(action.solution_id.clone());
        let cat_id = solutions::CatalogId(action.catalog_id.clone());
        let store = SolutionStore::global(cx);
        let Some((sol_name, member_path, member_label)) = store.read_with(cx, |s, _| {
            let sol = s.solutions().iter().find(|sol| sol.id == sol_id)?;
            let m = sol.members.iter().find(|m| m.catalog_id == cat_id)?;
            let label = m
                .local_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| m.catalog_id.0.clone());
            Some((sol.name.clone(), m.local_path.clone(), label))
        }) else {
            return;
        };
        let folder_label = SharedString::from(format!("Folder {}", member_path.display()));
        let title = SharedString::from(format!(
            "Remove project \"{}\" from solution \"{}\"?",
            member_label, sol_name,
        ));
        let path_for_rm = member_path.clone();
        crate::delete_confirm_modal::open_delete_confirm(
            workspace,
            title,
            "This will permanently delete:",
            vec![
                crate::delete_confirm_modal::DeleteConfirmItem {
                    label: "Member entry from this solution".into(),
                    path: None,
                },
                crate::delete_confirm_modal::DeleteConfirmItem {
                    label: folder_label,
                    path: Some(member_path),
                },
            ],
            move |_window, cx| {
                let store = SolutionStore::global(cx);
                store
                    .update(cx, |s, cx| s.remove_member(&sol_id, &cat_id, cx))
                    .log_err();
                let path = path_for_rm.clone();
                cx.background_spawn(async move {
                    let result: std::io::Result<()> =
                        smol::unblock(move || std::fs::remove_dir_all(&path)).await;
                    if let Err(err) = result {
                        if err.kind() != std::io::ErrorKind::NotFound {
                            log::warn!(
                                "RemoveMember: removing {} failed: {err} (orphaned files left on disk)",
                                path_for_rm.display(),
                            );
                        }
                    }
                })
                .detach();
            },
            window,
            cx,
        );
    });
}

/// Close every workspace (active or retained) that hosts `sol_id` and
/// stop its AI sessions. Originally lived on the (retired) dock panel —
/// extracted so the title-bar tab strip can call it without a panel
/// dependency. Iterates every window: the caller's window via the
/// workspace's `MultiWorkspace` handle (so we don't double-lease the
/// in-flight window), then the rest by downcasting `cx.windows()`.
fn close_solution(
    workspace: &mut Workspace,
    sol_id: SolutionId,
    window: &mut Window,
    cx: &mut gpui::Context<Workspace>,
) {
    use solution_agent::store::SolutionAgentStore;
    use util::ResultExt as _;
    use workspace::MultiWorkspace;

    if let Some(agent_store) = SolutionAgentStore::try_global(cx) {
        agent_store.update(cx, |store, cx| {
            let session_ids: Vec<_> = store
                .sessions_for(&sol_id)
                .into_iter()
                .map(|session| session.read(cx).id)
                .collect();
            for id in session_ids {
                store.close_session(id, cx).log_err();
            }
        });
    }
    if let Some(mw_weak) = workspace.multi_workspace().cloned()
        && let Some(mw) = mw_weak.upgrade()
    {
        mw.update(cx, |mw, cx| {
            close_solution_workspaces_in(mw, &sol_id, window, cx);
        });
    }
    let skip = window.window_handle().window_id();
    let other_windows: Vec<_> = cx
        .windows()
        .into_iter()
        .filter(|handle| handle.window_id() != skip)
        .filter_map(|handle| handle.downcast::<MultiWorkspace>())
        .collect();
    for handle in other_windows {
        let sol_id = sol_id.clone();
        handle
            .update(cx, move |mw, window, cx| {
                close_solution_workspaces_in(mw, &sol_id, window, cx);
            })
            .log_err();
    }
}

/// Advances or retreats the per-panel project selection for the active
/// solution by `dir` steps (`+1` = next, `-1` = previous), wrapping at
/// both ends. No-op if the workspace has no active solution, the
/// solution has no members, or the panel kind isn't recognised.
fn cycle_project_in_panel(
    workspace: &Workspace,
    panel_kind: &str,
    dir: isize,
    cx: &mut gpui::App,
) {
    use util::ResultExt as _;

    let panel = match panel_kind {
        "tree" => solutions::db::PanelKind::Tree,
        "git" => solutions::db::PanelKind::Git,
        _ => return,
    };
    let Some(sol_id) = crate::window_helpers::active_solution_in_workspace(workspace, cx) else {
        return;
    };
    let store = SolutionStore::global(cx);
    let Some((members, current)) = store.read_with(cx, |s, _| {
        let sol = s.solutions().iter().find(|sol| sol.id == sol_id)?;
        let members: Vec<solutions::CatalogId> =
            sol.members.iter().map(|m| m.catalog_id.clone()).collect();
        if members.is_empty() {
            return None;
        }
        let current = s
            .panel_member_selection(&sol.id, panel)
            .cloned()
            .unwrap_or_else(|| members[0].clone());
        Some((members, current))
    }) else {
        return;
    };
    let new_idx = cycle_index(
        members.iter().position(|c| *c == current).unwrap_or(0),
        members.len(),
        dir,
    );
    let new_catalog = members[new_idx].clone();
    store
        .update(cx, |s, cx| {
            s.set_panel_member_selection(sol_id, panel, new_catalog, cx)
        })
        .log_err();
}

fn cycle_index(cur: usize, len: usize, dir: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let n = len as isize;
    let new = (((cur as isize + dir) % n) + n) % n;
    new as usize
}

#[cfg(test)]
mod tests {
    use super::cycle_index;

    #[test]
    fn cycle_index_forward() {
        assert_eq!(cycle_index(0, 3, 1), 1);
    }

    #[test]
    fn cycle_index_wrap_forward() {
        assert_eq!(cycle_index(2, 3, 1), 0);
    }

    #[test]
    fn cycle_index_wrap_backward() {
        assert_eq!(cycle_index(0, 3, -1), 2);
    }

    #[test]
    fn cycle_index_single_element() {
        assert_eq!(cycle_index(0, 1, 1), 0);
        assert_eq!(cycle_index(0, 1, -1), 0);
    }
}

fn close_solution_workspaces_in(
    mw: &mut workspace::MultiWorkspace,
    sol_id: &SolutionId,
    window: &mut Window,
    cx: &mut gpui::Context<workspace::MultiWorkspace>,
) {
    use util::ResultExt as _;

    let to_close: Vec<_> = mw
        .workspaces()
        .filter(|ws| crate::open::workspace_has_solution(ws, sol_id, cx))
        .cloned()
        .collect();
    if to_close.is_empty() {
        return;
    }
    let close_tasks: Vec<_> = to_close
        .into_iter()
        .map(|ws| mw.close_workspace(&ws, window, cx))
        .collect();
    // Spawn one coordinator that awaits every close before checking
    // whether this window still hosts any solution. Awaiting
    // sequentially is fine — close ordering doesn't matter for the
    // emptiness check, and join_all would pull `futures` in just to
    // save microseconds on an action triggered by a human click.
    cx.spawn_in(window, async move |this, cx| {
        for task in close_tasks {
            task.await.log_err();
        }
        this.update(cx, |mw, cx| {
            crate::welcome_trigger::open_welcome_if_window_empty(mw, cx);
        })
        .log_err();
    })
    .detach();
}

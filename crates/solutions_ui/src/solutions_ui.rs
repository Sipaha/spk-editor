//! UI layer for the solutions crate: dock panel, picker, modals,
//! title-bar segment, status-bar widget, welcome integration.

mod actions;
mod add_member_picker;
pub mod delete_confirm_modal;
mod dock_panel;
mod empty_solution_page;
mod modals;
mod open;
mod picker;
pub mod solution_picker_dropdown;
pub mod solution_tab;
mod status_bar;
mod switch;
mod welcome;
pub mod window_helpers;

pub use empty_solution_page::EmptySolutionPage;
pub use open::{OpenIntent, open_solution};
pub use status_bar::SolutionsStatusItem;
pub use switch::switch_active_solution_in_place;

pub use actions::{
    DeleteSolution, NewSolution, OpenSolution, RefreshCacheForCurrent, ToggleSolutionsPanel,
};
pub use dock_panel::{SolutionsPanel, load};

use gpui::{App, Window};
use solutions::{SolutionId, SolutionStore};
use ui::SharedString;
use workspace::Workspace;

use crate::actions::{
    CloseSolutionFromTabBar, DeleteSolutionFromTabBar, RenameSolution, RevealSolutionFolder,
};

pub fn init(cx: &mut App) {
    dock_panel::init(cx);
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
}

/// Close every workspace (active or retained) that hosts `sol_id` and
/// stop its AI sessions. Mirrors the (retired) dock panel's
/// close-solution flow — extracted so the title-bar tab strip can call
/// it without depending on `SolutionsPanel`. Iterates every window: the
/// caller's window via the workspace's `MultiWorkspace` handle (so we
/// don't double-lease the in-flight window), then the rest by
/// downcasting `cx.windows()`.
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

fn close_solution_workspaces_in(
    mw: &mut workspace::MultiWorkspace,
    sol_id: &SolutionId,
    window: &mut Window,
    cx: &mut gpui::Context<workspace::MultiWorkspace>,
) {
    let to_close: Vec<_> = mw
        .workspaces()
        .filter(|ws| crate::open::workspace_has_solution(ws, sol_id, cx))
        .cloned()
        .collect();
    for ws in to_close {
        mw.close_workspace(&ws, window, cx).detach_and_log_err(cx);
    }
}

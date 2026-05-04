//! Shared "open this Solution" entry point used by the welcome page,
//! the Solutions dock panel and the picker. Centralises three concerns
//! that were previously copy-pasted:
//!
//! 1. Reuse an already-open window for the Solution if one exists.
//! 2. Decide between replacing the current window's workspace
//!    (`OpenIntent::SameWindow`, default for left-click) and opening a
//!    fresh window (`OpenIntent::NewWindow`, middle-click).
//! 3. Append the `EmptySolutionPage` CTA when the Solution has no members.

use std::path::PathBuf;

use anyhow::anyhow;
use gpui::{App, AppContext, WindowHandle};
use solutions::{SolutionId, SolutionStore};
use util::ResultExt as _;
use workspace::{AppState, MultiWorkspace, OpenMode, OpenOptions, OpenVisible};

/// Skips the currently-active window when iterating other windows.
/// `cx.read_window` on the window whose event we're handling panics with
/// "attempted to read a window that is already on the stack" because GPUI
/// has temporarily moved it out of the registry.
fn skip_window_id(cx: &App) -> Option<gpui::WindowId> {
    cx.active_window().map(|w| w.window_id())
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpenIntent {
    /// Replace the current window's workspace with the Solution. If the
    /// Solution is already open in any window, that window is focused
    /// instead.
    SameWindow,
    /// Always open the Solution in a new window.
    NewWindow,
}

pub fn open_solution(
    sol_id: SolutionId,
    source_window: Option<WindowHandle<MultiWorkspace>>,
    intent: OpenIntent,
    cx: &mut App,
) {
    // Focus an already-open window for this Solution, except when the
    // user explicitly asked for a new window via middle-click.
    if intent == OpenIntent::SameWindow
        && let Some(existing) = find_window_for_solution(&sol_id, cx)
    {
        existing
            .update(cx, |_, window, _| window.activate_window())
            .log_err();
        if let Some(store) = SolutionStore::try_global(cx) {
            store
                .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
                .log_err();
        }
        // If the click came from a different window (e.g. the welcome
        // launcher), retire it now that the user has chosen a target.
        if let Some(src) = source_window
            && src.window_id() != existing.window_id()
        {
            src.update(cx, |_, window, _| window.remove_window())
                .log_err();
        }
        return;
    }

    let Some(store) = SolutionStore::try_global(cx) else {
        return;
    };

    struct OpenInfo {
        paths: Vec<PathBuf>,
        name: String,
        is_empty: bool,
    }
    let info = match store.read_with(cx, |s, _| -> anyhow::Result<OpenInfo> {
        let solution = s
            .solutions()
            .iter()
            .find(|sol| sol.id == sol_id)
            .ok_or_else(|| anyhow!("solution not found: {}", sol_id.as_str()))?;
        let is_empty = solution.members.is_empty();
        let name = solution.name.clone();
        let paths = if is_empty {
            vec![solution.root.clone()]
        } else {
            s.paths_for_open(&sol_id)?
        };
        Ok(OpenInfo {
            paths,
            name,
            is_empty,
        })
    }) {
        Ok(info) => info,
        Err(err) => {
            log::error!(
                "solutions_ui: resolving paths for {} failed: {err}",
                sol_id.as_str()
            );
            return;
        }
    };

    store
        .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
        .log_err();

    let app_state = AppState::global(cx);
    let mut options = OpenOptions::default();
    // For an "empty" solution (no member projects yet) we still need a
    // worktree at `solution.root` so navigator code can identify the
    // active solution by path, but it shouldn't show in the project
    // panel — there's nothing meaningful inside, just the on-disk
    // directory. `OpenVisible::None` keeps the worktree attached but
    // hidden, leaving the panel clean for the EmptySolutionPage CTA.
    if info.is_empty {
        options.visible = Some(OpenVisible::None);
    }
    match intent {
        OpenIntent::SameWindow => {
            // Two-stage swap: first build the new workspace silently with
            // `OpenMode::Add` so it loads worktrees and items off-screen
            // while the source workspace stays visible. Once the task
            // resolves we call `multi_workspace.activate(...)` ourselves
            // — the user sees the old workspace transition directly to a
            // fully-populated new one, instead of an empty new workspace
            // streaming content in.
            options.open_mode = OpenMode::Add;
            options.requesting_window = source_window;
        }
        OpenIntent::NewWindow => {
            options.open_mode = OpenMode::NewWindow;
            options.requesting_window = None;
        }
    }
    let task = workspace::open_paths(&info.paths, app_state, options, cx);

    // Capture the launcher window (if any) up front so we can retire
    // it after the new workspace window appears — otherwise the user
    // ends up with both Welcome and the Solution window open, which
    // is rarely what they want.
    let welcome_window = workspace::welcome::find_existing(cx);

    let sol_id_for_page = sol_id.clone();
    let sol_id_for_lookup = sol_id;
    cx.spawn(async move |cx| {
        let Some(opened) = task.await.log_err() else {
            return;
        };
        // Stage two of the silent-prepare swap: now that the new
        // workspace has loaded its worktrees / items, activate it.
        // Before activating, retain the currently-active workspace so
        // its solution_agent sessions keep running in the background;
        // without this, MultiWorkspace::activate drops the previous
        // workspace and tears down its agent connections.
        if intent == OpenIntent::SameWindow {
            let new_workspace = opened.workspace.clone();
            let target_sol_id = sol_id_for_lookup;
            cx.update(|cx| {
                opened
                    .window
                    .update(cx, |multi_workspace, window, cx| {
                        // If a retained workspace for this Solution already
                        // exists in this window (user clicked back to a
                        // solution they previously had open here), prefer
                        // that one so we don't end up with two retained
                        // copies of the same solution.
                        let existing = multi_workspace
                            .workspaces()
                            .find(|ws| {
                                ws != &&new_workspace
                                    && workspace_has_solution(ws, &target_sol_id, cx)
                            })
                            .cloned();
                        multi_workspace.retain_active_workspace(cx);
                        let to_activate = existing.unwrap_or(new_workspace);
                        multi_workspace.activate(to_activate, None, window, cx);
                    })
                    .log_err();
            });
        }
        if info.is_empty {
            let sol_id_for_page = sol_id_for_page.clone();
            let name_for_page = info.name.clone();
            cx.update(|cx| {
                opened
                    .window
                    .update(cx, |multi_workspace, window, cx| {
                        let workspace = multi_workspace.workspace().clone();
                        let weak_workspace = workspace.downgrade();
                        workspace.update(cx, |ws, cx| {
                            let page = cx.new(|cx| {
                                crate::empty_solution_page::EmptySolutionPage::new(
                                    sol_id_for_page,
                                    name_for_page,
                                    weak_workspace,
                                    cx,
                                )
                            });
                            ws.add_item_to_active_pane(
                                Box::new(page),
                                None,
                                true,
                                window,
                                cx,
                            );
                        });
                    })
                    .log_err();
            });
        }
        // Close the launcher window (if any) once the workspace
        // window is up — Welcome is single-purpose, the user picked
        // their target.
        if let Some(welcome) = welcome_window {
            cx.update(|cx| {
                welcome
                    .update(cx, |_, window, _| window.remove_window())
                    .log_err();
            });
        }

        // For SameWindow we reused the source via `OpenMode::Activate`, so
        // there is nothing to clean up. For NewWindow the user explicitly
        // asked for a separate window — leave the source alone.
        let _ = source_window;
        let _ = intent;
    })
    .detach();
}

pub(crate) fn workspace_has_solution(
    workspace: &gpui::Entity<workspace::Workspace>,
    sol_id: &SolutionId,
    cx: &App,
) -> bool {
    let project = workspace.read(cx).project().clone();
    let Some(store) = SolutionStore::try_global(cx) else {
        return false;
    };
    let store_read = store.read(cx);
    project.read(cx).worktrees(cx).any(|tree| {
        store_read
            .solution_for_path(&tree.read(cx).abs_path())
            .is_some_and(|sol| &sol.id == sol_id)
    })
}

fn find_window_for_solution(
    sol_id: &SolutionId,
    cx: &App,
) -> Option<WindowHandle<MultiWorkspace>> {
    let skip = skip_window_id(cx);
    for handle in cx.windows() {
        if Some(handle.window_id()) == skip {
            continue;
        }
        let Some(mw_handle) = handle.downcast::<MultiWorkspace>() else {
            continue;
        };
        // Iterate active + retained workspaces — a retained workspace
        // for this solution counts as "already open" because the user's
        // sessions are still alive in it.
        let matches = mw_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspaces()
                    .any(|workspace| workspace_has_solution(workspace, sol_id, cx))
            })
            .ok()
            .unwrap_or(false);
        if matches {
            return Some(mw_handle);
        }
    }
    None
}

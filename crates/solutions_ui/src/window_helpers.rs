//! Helpers for locating Solution-bearing windows. Extracted from the
//! retired left-dock panel; reused by the title-bar tab strip and the
//! Welcome trigger.

use gpui::{App, WindowHandle};
use solutions::SolutionId;
use workspace::MultiWorkspace;

use crate::open::workspace_has_solution;

/// Returns the window handle that currently has `sol_id` as one of its
/// open solutions, or `None` if no such window exists.
pub fn find_window_for_solution(
    sol_id: &SolutionId,
    cx: &App,
) -> Option<WindowHandle<MultiWorkspace>> {
    cx.windows()
        .into_iter()
        .find_map(|handle| {
            let Some(mw_handle) = handle.downcast::<MultiWorkspace>() else {
                return None;
            };
            mw_handle
                .read_with(cx, |mw, cx| {
                    mw.workspaces()
                        .any(|ws| workspace_has_solution(ws, sol_id, cx))
                })
                .unwrap_or(false)
                .then_some(mw_handle)
        })
}

pub fn is_solution_open_anywhere(sol_id: &SolutionId, cx: &App) -> bool {
    find_window_for_solution(sol_id, cx).is_some()
}

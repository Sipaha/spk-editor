//! Horizontal solution-tab strip rendered in the title bar after the
//! hamburger. Hosts the open solutions as `SolutionTab` children, plus
//! a trailing `+` button that opens [`SolutionPickerDropdown`] (or
//! dispatches [`crate::actions::NewSolution`] directly when the catalog
//! has no closed solutions to pick from — in that case the dropdown
//! would only show the "Create new solution…" entry, so we skip it).
//!
//! Source of truth:
//!   * `MultiWorkspace::workspaces()` for the list of open workspaces in
//!     this window (the active one plus retained ones), each mapped to a
//!     `SolutionId` via the `solutions::SolutionStore` worktree lookup.
//!   * `MultiWorkspace::workspace()` for the active workspace, whose
//!     solution is highlighted as the active tab.
//!   * `SolutionStore::solutions()` for the displayed name and to count
//!     closed solutions for the `+` button branching.
//!   * `SolutionStore::pending_adds_for(&id)` for the clone-in-flight
//!     spinner on each tab.
//!   * `SolutionAgentStore::sessions_for(&id)` for the live AI session
//!     count badge on each tab.
//!
//! Re-render triggers (registered in [`SolutionTabStrip::new`]):
//!   * `SolutionStoreEvent` — covers solution add/remove/rename and
//!     pending-add stage transitions.
//!   * `SolutionAgentStoreEvent` — covers session create/close so the
//!     AI badge stays in sync.
//!   * `cx.observe(&multi_workspace)` — covers active-workspace switch
//!     and retained-workspace open/close, since `MultiWorkspace` calls
//!     `cx.notify()` on each of those transitions and `observe` fires
//!     on every notify.

use gpui::{
    Entity, IntoElement, ParentElement, Render, Styled, Subscription, WeakEntity, Window, div,
};
use solution_agent::store::{SolutionAgentStore, SolutionAgentStoreEvent};
use solutions::{Solution, SolutionId, SolutionStore, SolutionStoreEvent};
use ui::{IconButton, IconName, Tooltip, prelude::*};
use workspace::{MultiWorkspace, Workspace};

use crate::solution_picker_dropdown::SolutionPickerDropdown;
use crate::solution_tab::SolutionTab;
use crate::window_helpers::is_solution_open_anywhere;

pub struct SolutionTabStrip {
    workspace: WeakEntity<Workspace>,
    multi_workspace: WeakEntity<MultiWorkspace>,
    _subscriptions: Vec<Subscription>,
}

impl SolutionTabStrip {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        multi_workspace: WeakEntity<MultiWorkspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();

        let store = SolutionStore::global(cx);
        subscriptions.push(cx.subscribe(
            &store,
            |_, _, _: &SolutionStoreEvent, cx| {
                cx.notify();
            },
        ));

        // Agent store may not be initialised in headless / test contexts;
        // only subscribe when present.
        if let Some(agent_store) = SolutionAgentStore::try_global(cx) {
            subscriptions.push(cx.subscribe(
                &agent_store,
                |_, _, _: &SolutionAgentStoreEvent, cx| {
                    cx.notify();
                },
            ));
        }

        // Re-render whenever the multi-workspace's open list or active
        // workspace changes. `MultiWorkspace::activate` /
        // `retain_active_workspace` / `close_workspace` all call
        // `cx.notify()`, so a plain `observe` is enough — no event types
        // to filter on.
        if let Some(mw) = multi_workspace.upgrade() {
            subscriptions.push(cx.observe(&mw, |_, _, cx| cx.notify()));
        }

        Self {
            workspace,
            multi_workspace,
            _subscriptions: subscriptions,
        }
    }
}

/// Walk a `Workspace`'s worktrees and return the first one that maps to
/// a registered Solution. Mirrors the logic inside
/// `workspace_has_solution` — extracted here so we can build the
/// (id, name, badges) tuple list in a single pass.
fn solution_id_for_workspace(
    workspace: &Entity<Workspace>,
    store: &SolutionStore,
    cx: &App,
) -> Option<SolutionId> {
    let project = workspace.read(cx).project().clone();
    project.read(cx).worktrees(cx).find_map(|tree| {
        store
            .solution_for_path(&tree.read(cx).abs_path())
            .map(|sol| sol.id.clone())
    })
}

impl Render for SolutionTabStrip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(mw) = self.multi_workspace.upgrade() else {
            return h_flex().h_full().into_any_element();
        };

        let store = SolutionStore::global(cx);
        let agent_store = SolutionAgentStore::try_global(cx);

        // Snapshot the data each tab needs in one pass so we don't borrow
        // across mutating callbacks. Each entry is (id, name, is_active,
        // ai_count, in_flight). `SolutionTab` is a `RenderOnce` that
        // takes the values by move, so we don't need to keep `&Solution`
        // references alive past this map.
        let active_workspace = mw.read(cx).workspace().clone();
        let store_read = store.read(cx);
        let active_solution_id = solution_id_for_workspace(&active_workspace, store_read, cx);

        let mut seen_ids: Vec<SolutionId> = Vec::new();
        let mut tabs: Vec<(SolutionId, SharedString, bool, usize, bool)> = Vec::new();
        for ws in mw.read(cx).workspaces() {
            let Some(sol_id) = solution_id_for_workspace(ws, store_read, cx) else {
                continue;
            };
            // A retained workspace and the active workspace can map to
            // the same Solution; avoid duplicating the tab in that case.
            if seen_ids.iter().any(|id| id == &sol_id) {
                continue;
            }
            let Some(sol) = store_read
                .solutions()
                .iter()
                .find(|s: &&Solution| s.id == sol_id)
            else {
                continue;
            };
            let is_active = active_solution_id.as_ref() == Some(&sol_id);
            let ai_count = agent_store
                .as_ref()
                .map(|s| s.read(cx).sessions_for(&sol_id).len())
                .unwrap_or(0);
            let in_flight = !store_read.pending_adds_for(&sol_id).is_empty();
            tabs.push((
                sol_id.clone(),
                SharedString::from(sol.name.clone()),
                is_active,
                ai_count,
                in_flight,
            ));
            seen_ids.push(sol_id);
        }

        // The `+` button branches on whether at least one solution in the
        // catalog is currently closed. The picker dropdown filters its
        // rows by `is_solution_open_anywhere`, so if every solution is
        // already open the dropdown would just show "Create new
        // solution…" — skip the popover and dispatch `NewSolution`
        // directly per the spec.
        let any_closed = store_read
            .solutions()
            .iter()
            .any(|s| !is_solution_open_anywhere(&s.id, cx));

        let weak_workspace = self.workspace.clone();
        let plus_button = IconButton::new("solution-tab-strip-plus", IconName::Plus)
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .tooltip(Tooltip::text(if any_closed {
                "Open or create a solution"
            } else {
                "Create new solution"
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                // Recompute `any_closed` at click time — between render
                // and click another window could have closed a solution
                // and we want to honour the latest state.
                let store = SolutionStore::global(cx);
                let any_closed_now = store
                    .read(cx)
                    .solutions()
                    .iter()
                    .any(|s| !is_solution_open_anywhere(&s.id, cx));
                if !any_closed_now {
                    cx.dispatch_action(&crate::actions::NewSolution);
                    return;
                }
                let Some(workspace) = this.workspace.upgrade() else {
                    return;
                };
                let picker_workspace = this.workspace.clone();
                workspace.update(cx, move |workspace, cx| {
                    workspace.toggle_modal(window, cx, move |window, cx| {
                        SolutionPickerDropdown::new(picker_workspace, window, cx)
                    });
                });
            }));

        h_flex()
            .id("solution-tab-strip")
            .h_full()
            .overflow_x_scroll()
            .children(tabs.into_iter().map(
                |(id, name, is_active, ai_count, in_flight)| {
                    SolutionTab::new(
                        id,
                        name,
                        is_active,
                        ai_count,
                        in_flight,
                        weak_workspace.clone(),
                    )
                },
            ))
            .child(div().px_1().child(plus_button))
            .into_any_element()
    }
}

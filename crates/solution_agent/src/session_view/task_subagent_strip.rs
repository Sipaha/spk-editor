//! Subagent tabs strip — the horizontal pill row painted right above
//! the status row when the current session has one or more claude
//! `Task` / `Agent` subagents in flight. The strip lets the user
//! switch the visible conversation between "Main" (parent-only
//! entries) and each subagent's own log, mirroring the Claude Code
//! TUI behaviour.
//!
//! Hidden entirely when `SolutionSession::active_subagents.is_empty()`
//! — a degenerate strip with only the "Main" pill would just waste a
//! row of vertical space. Iteration over `active_subagent_order`
//! (NOT `active_subagents` directly) so tab order matches spawn order
//! and stays stable across renders; the HashMap on its own has
//! random hash-seed iteration order.
//!
//! V1 deliberately omits the per-tab close button: per the plan, tabs
//! disappear naturally when the parent `Task` ToolCall completes /
//! fails / is cancelled, so there is no manual-close affordance to
//! expose. The store-level lifecycle is already in place (Etap 3) and
//! the view auto-switches on `SessionSubagentsChanged` (Etap 4).

use gpui::{AnyElement, Entity, IntoElement, ParentElement, SharedString, Styled};
use ui::prelude::*;
use ui::{Label, LabelSize, Tooltip};

use super::SolutionSessionView;
use crate::model::SolutionSession;

/// Build the subagent-tabs strip. Returns `None` when the session
/// has no in-flight subagents so the caller can `when_some(...)` the
/// element into the layout without reserving an empty row.
pub(super) fn render_task_subagent_strip(
    view: &SolutionSessionView,
    session: &Entity<SolutionSession>,
    cx: &mut Context<SolutionSessionView>,
) -> Option<AnyElement> {
    let session_ref = session.read(cx);
    if session_ref.active_subagents.is_empty() {
        return None;
    }
    // Snapshot label / id pairs so the click listeners (which need
    // `'static` data) don't have to borrow back through the session
    // entity inside their closures.
    let tabs: Vec<(SharedString, SharedString)> = session_ref
        .active_subagent_order
        .iter()
        .filter_map(|id| {
            session_ref
                .active_subagents
                .get(id)
                .map(|tab| (id.clone(), tab.label.clone()))
        })
        .collect();
    let selected = view.selected_subagent.clone();

    let main_active = selected.is_none();
    let main_pill = pill(
        SharedString::from("task-subagent-strip-main"),
        SharedString::from("Main"),
        main_active,
        cx,
        move |this, _, _, cx| {
            if this.selected_subagent.is_some() {
                this.selected_subagent = None;
                cx.notify();
            }
        },
    );

    let mut row = h_flex()
        .id("task-subagent-strip")
        .w_full()
        .flex_none()
        .gap_1()
        .px_2()
        .py_1()
        .overflow_x_scroll()
        .border_t_1()
        .border_color(cx.theme().colors().border)
        .bg(cx.theme().colors().panel_background)
        .child(main_pill);

    for (id, label) in tabs {
        let is_active = selected.as_ref() == Some(&id);
        let id_for_listener = id.clone();
        let pill_id = SharedString::from(format!("task-subagent-strip-{}", id));
        row = row.child(pill(
            pill_id,
            label,
            is_active,
            cx,
            move |this, _, _, cx| {
                let next = Some(id_for_listener.clone());
                if this.selected_subagent != next {
                    this.selected_subagent = next;
                    cx.notify();
                }
            },
        ));
    }
    Some(row.into_any_element())
}

/// One pill button. Accent background + bolder label for the active
/// tab; muted hover for the rest. The click handler is provided by
/// the caller because the "Main" pill and each subagent pill need
/// different captures, and trying to share one closure across them
/// would force a runtime branch on the id inside every listener.
fn pill<F>(
    id: SharedString,
    label: SharedString,
    is_active: bool,
    cx: &mut Context<SolutionSessionView>,
    on_click: F,
) -> AnyElement
where
    F: Fn(&mut SolutionSessionView, &gpui::ClickEvent, &mut Window, &mut Context<SolutionSessionView>)
        + 'static,
{
    let colors = cx.theme().colors();
    let (bg, label_color) = if is_active {
        (colors.element_selected, Color::Default)
    } else {
        (colors.element_background, Color::Muted)
    };
    let tooltip_text = SharedString::from(format!("Show {}", label));
    let label_size = if is_active {
        LabelSize::Default
    } else {
        LabelSize::Small
    };
    h_flex()
        .id(id)
        .flex_none()
        .h(px(24.0))
        .px_2()
        .gap_1()
        .items_center()
        .rounded_md()
        .bg(bg)
        .cursor_pointer()
        .hover(|s| s.bg(colors.element_hover))
        .tooltip(Tooltip::text(tooltip_text))
        .child(
            Label::new(label)
                .size(label_size)
                .color(label_color)
                .truncate(),
        )
        .on_click(cx.listener(on_click))
        .into_any_element()
}

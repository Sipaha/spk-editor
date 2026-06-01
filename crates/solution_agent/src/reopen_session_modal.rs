//! Modal that reopens a closed / unpinned Solution chat session.
//!
//! "Closing" a chat tab only unpins it (`tab_order` → NULL); the session
//! and its transcript stay on disk. Before the panel merge the AI panel
//! had a session-history surface to bring such a session back; that
//! affordance was lost. This modal restores it: it lists the active
//! solution's sessions that aren't currently in the strip and reopens the
//! selected one via [`SolutionAgentStore::open_session_in_strip`] — the
//! same "open a session" path create and the wire RPC use.

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Window, div, rems,
};
use ui::prelude::*;
use ui::{Label, LabelSize};
use workspace::ModalView;

use crate::model::SolutionSessionId;
use crate::store::SolutionAgentStore;

/// A session that can be reopened: its id plus a display title.
#[derive(Clone)]
pub struct ReopenableSession {
    pub id: SolutionSessionId,
    pub title: SharedString,
}

pub struct ReopenSessionModal {
    sessions: Vec<ReopenableSession>,
    focus_handle: FocusHandle,
}

impl ReopenSessionModal {
    pub fn new(
        sessions: Vec<ReopenableSession>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            sessions,
            focus_handle: cx.focus_handle(),
        }
    }

    fn reopen(&mut self, id: SolutionSessionId, cx: &mut Context<Self>) {
        let store = SolutionAgentStore::global(cx);
        store.update(cx, |store, cx| store.open_session_in_strip(id, cx));
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for ReopenSessionModal {}

impl Focusable for ReopenSessionModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for ReopenSessionModal {
    fn debug_kind(&self) -> &'static str {
        "ReopenSession"
    }
}

impl Render for ReopenSessionModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut container = div()
            .key_context("ReopenSessionModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .flex()
            .flex_col()
            .gap_2()
            .w(rems(30.))
            .p_4()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_md()
            .child(Label::new("Reopen Closed Chat").size(LabelSize::Large));

        if self.sessions.is_empty() {
            return container.child(
                Label::new("No closed chats in this solution.")
                    .size(LabelSize::Default)
                    .color(Color::Muted),
            );
        }

        let mut list = v_flex()
            .id("reopen-session-list")
            .gap_px()
            .max_h(rems(20.))
            .overflow_y_scroll();
        for session in self.sessions.clone() {
            let id = session.id;
            list = list.child(
                ui::ListItem::new(SharedString::from(id.to_string()))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(Icon::new(IconName::Sparkle).size(IconSize::Small))
                            .child(Label::new(session.title.clone()).truncate()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.reopen(id, cx))),
            );
        }
        container = container.child(list);
        container
    }
}

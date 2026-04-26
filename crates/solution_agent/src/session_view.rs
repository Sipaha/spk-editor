use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, WeakEntity, Window, div,
};
use ui::prelude::*;
use ui::{Icon, IconName, Label};
use workspace::{Workspace, item::Item};

use crate::model::{SolutionSession, SolutionSessionId};

pub struct SolutionSessionView {
    session_id: SolutionSessionId,
    session: Entity<SolutionSession>,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl SolutionSessionView {
    pub fn new(
        session_id: SolutionSessionId,
        session: Entity<SolutionSession>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self {
            session_id,
            session,
            focus_handle: cx.focus_handle(),
            workspace,
        }
    }
}

impl Focusable for SolutionSessionView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub enum SolutionSessionViewEvent {}

impl EventEmitter<SolutionSessionViewEvent> for SolutionSessionView {}

impl Render for SolutionSessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let header = format!("{} • {:?}", session.agent_id, session.state);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(
                div()
                    .flex()
                    .h_8()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new(header)),
            )
            .child(
                div()
                    .flex_1()
                    .p_3()
                    .child(Label::new("(conversation rendering — Task 6.3)")),
            )
            .child(
                div()
                    .h_16()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new("(compose box — Task 6.4)")),
            )
    }
}

impl Item for SolutionSessionView {
    type Event = SolutionSessionViewEvent;

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.session.read(cx).title.clone()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::AiClaude))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("solution_agent_session")
    }
}

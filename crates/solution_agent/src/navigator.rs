use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, WeakEntity, Window, div, px,
};
use solutions::SolutionId;
use ui::prelude::*;
use ui::{IconName, Label, LabelSize};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::actions::FocusNavigator;
use crate::store::SolutionAgentStore;

pub struct SolutionSessionsNavigator {
    // Held for future tasks (drag-to-pane, opening session views) so we can
    // reach back into the workspace without re-plumbing it through.
    _workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    width: gpui::Pixels,
    active_solution: Option<SolutionId>,
    _store_subscription: gpui::Subscription,
}

impl SolutionSessionsNavigator {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let store = SolutionAgentStore::global(cx);
        let store_subscription = cx.subscribe(&store, |_, _, _, cx| cx.notify());
        Self {
            _workspace: workspace,
            focus_handle: cx.focus_handle(),
            width: px(280.0),
            active_solution: None,
            _store_subscription: store_subscription,
        }
    }

    pub fn set_active_solution(&mut self, id: Option<SolutionId>, cx: &mut Context<Self>) {
        self.active_solution = id;
        cx.notify();
    }
}

impl Focusable for SolutionSessionsNavigator {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for SolutionSessionsNavigator {}

impl Render for SolutionSessionsNavigator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = SolutionAgentStore::global(cx);
        let sessions = self
            .active_solution
            .as_ref()
            .map(|sid| store.read_with(cx, |s, _| s.sessions_for(sid)))
            .unwrap_or_default();

        let header = div()
            .flex()
            .h_8()
            .px_3()
            .items_center()
            .border_b_1()
            .child(Label::new("Sessions").size(LabelSize::Small));

        let mut list = div()
            .id("solution-sessions-list")
            .flex_1()
            .overflow_y_scroll();
        for session in sessions {
            let s = session.read(cx);
            let row = div()
                .flex()
                .h_8()
                .px_3()
                .items_center()
                .gap_2()
                .id(SharedString::from(s.id.to_string()))
                .child(Label::new(format!("{:?}", s.state)).size(LabelSize::XSmall))
                .child(Label::new(s.title.clone()));
            list = list.child(row);
        }

        div().flex().flex_col().size_full().child(header).child(list)
    }
}

impl Panel for SolutionSessionsNavigator {
    fn persistent_name() -> &'static str {
        "SolutionSessionsNavigator"
    }

    fn panel_key() -> &'static str {
        "solution_agent::SolutionSessionsNavigator"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Right
    }

    fn position_is_valid(&self, p: DockPosition) -> bool {
        matches!(p, DockPosition::Right | DockPosition::Left)
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> gpui::Pixels {
        self.width
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::AiClaude)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Solution sessions")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(FocusNavigator)
    }

    fn activation_priority(&self) -> u32 {
        30
    }
}

//! Splash page shown when an empty solution opens — guides the user to
//! add a project from the catalog instead of staring at a blank workspace.

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString,
    WeakEntity, Window,
};
use solutions::SolutionId;
use ui::ButtonLike;
use ui::prelude::*;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

use crate::add_member_picker::AddMemberPicker;

pub struct EmptySolutionPage {
    solution_id: SolutionId,
    solution_name: SharedString,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
}

impl EmptySolutionPage {
    pub fn new(
        solution_id: SolutionId,
        solution_name: impl Into<SharedString>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            solution_id,
            solution_name: solution_name.into(),
            workspace,
            focus_handle: cx.focus_handle(),
        }
    }

    fn open_picker(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let id = self.solution_id.clone();
        workspace.update(cx, |workspace, cx| {
            AddMemberPicker::open(workspace, id, window, cx);
        });
    }
}

impl Render for EmptySolutionPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.solution_name.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .bg(cx.theme().colors().editor_background)
            .child(
                Label::new(format!("Solution \"{name}\" is empty")).size(LabelSize::Large),
            )
            .child(
                Label::new("Add a project from your catalog to start working in this solution.")
                    .color(Color::Muted)
                    .size(LabelSize::Small),
            )
            .child(
                ButtonLike::new("empty-solution-add-member")
                    .size(ui::ButtonSize::Medium)
                    .child(
                        h_flex()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .child(
                                Icon::new(IconName::Plus)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .child(Label::new("Add Project from Catalog")),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_picker(window, cx);
                    })),
            )
    }
}

impl EventEmitter<ItemEvent> for EmptySolutionPage {}

impl Focusable for EmptySolutionPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for EmptySolutionPage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        format!("{} (empty)", self.solution_name).into()
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

use editor::Editor;
use gpui::{
    AppContext as _, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, WeakEntity, actions,
};
use settings::Settings as _;
use solutions::{SolutionStore, SolutionsSettings};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{ModalView, Workspace};

use crate::actions::NewSolution;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use solutions::SolutionId;

actions!(
    solutions,
    [
        /// Open the modal to add a new project to the catalog.
        AddCatalogProject,
    ]
);

/// Open the AddMember picker for a specific solution by id. Dispatched from
/// the Welcome page when the user clicks an empty solution — the natural
/// next step there is "let me add a member to this solution", not "fail
/// silently because there's no worktree to mount".
#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, gpui::Action)]
#[action(namespace = solutions)]
#[serde(transparent)]
pub struct AddMemberTo {
    pub solution_id: String,
}

pub fn register(workspace: &mut Workspace, _: Option<&mut Window>, _: &mut Context<Workspace>) {
    workspace.register_action(|workspace, _: &NewSolution, window, cx| {
        let weak = cx.weak_entity();
        workspace.toggle_modal(window, cx, |window, cx| {
            NewSolutionModal::new(weak, window, cx)
        });
    });
    workspace.register_action(|workspace, _: &AddCatalogProject, window, cx| {
        let weak = cx.weak_entity();
        workspace.toggle_modal(window, cx, |window, cx| {
            AddCatalogProjectModal::new(weak, window, cx)
        });
    });
    workspace.register_action(|workspace, action: &AddMemberTo, window, cx| {
        crate::add_member_picker::AddMemberPicker::open(
            workspace,
            SolutionId(action.solution_id.clone()),
            window,
            cx,
        );
    });
}

pub struct NewSolutionModal {
    name_editor: Entity<Editor>,
    _workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
}

impl NewSolutionModal {
    fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_editor = cx.new(|cx| Editor::single_line(window, cx));
        name_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text("Solution name", window, cx);
        });
        let focus_handle = cx.focus_handle();
        Self {
            name_editor,
            _workspace: workspace,
            focus_handle,
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_editor.read(cx).text(cx);
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let root = SolutionsSettings::get_global(cx).root.clone();
        let store = SolutionStore::global(cx);
        store
            .update(cx, |s, cx| s.create_solution(name, root, cx))
            .log_err();
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for NewSolutionModal {}

impl Focusable for NewSolutionModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_editor.focus_handle(cx)
    }
}

impl ModalView for NewSolutionModal {
    fn debug_kind(&self) -> &'static str {
        "NewSolution"
    }
}

impl Render for NewSolutionModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("NewSolutionModal")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .track_focus(&self.focus_handle)
            .w(rems(28.))
            .p_4()
            .gap_3()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_md()
            .child(
                Label::new("New Solution")
                    .size(LabelSize::Large),
            )
            .child(self.name_editor.clone())
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel", "Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel(&menu::Cancel, window, cx);
                            })),
                    )
                    .child(
                        Button::new("create", "Create")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm(&menu::Confirm, window, cx);
                            })),
                    ),
            )
    }
}

pub struct AddCatalogProjectModal {
    name_editor: Entity<Editor>,
    url_editor: Entity<Editor>,
    branch_editor: Entity<Editor>,
    _workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
}

impl AddCatalogProjectModal {
    fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_editor = cx.new(|cx| Editor::single_line(window, cx));
        name_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text("Project name (e.g. ECOS Records)", window, cx);
        });
        let url_editor = cx.new(|cx| Editor::single_line(window, cx));
        url_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text(
                "Remote URL (e.g. git@example.com:org/repo.git)",
                window,
                cx,
            );
        });
        let branch_editor = cx.new(|cx| Editor::single_line(window, cx));
        branch_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text("Default branch (optional)", window, cx);
        });
        let focus_handle = cx.focus_handle();
        Self {
            name_editor,
            url_editor,
            branch_editor,
            _workspace: workspace,
            focus_handle,
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_editor.read(cx).text(cx).trim().to_string();
        let url = self.url_editor.read(cx).text(cx).trim().to_string();
        let branch = self.branch_editor.read(cx).text(cx).trim().to_string();
        if name.is_empty() || url.is_empty() {
            return;
        }
        let default_branch = if branch.is_empty() { None } else { Some(branch) };
        let store = SolutionStore::global(cx);
        store
            .update(cx, |s, cx| s.add_catalog_project(&name, &url, default_branch, cx))
            .log_err();
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for AddCatalogProjectModal {}

impl Focusable for AddCatalogProjectModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_editor.focus_handle(cx)
    }
}

impl ModalView for AddCatalogProjectModal {
    fn debug_kind(&self) -> &'static str {
        "AddCatalogProject"
    }
}

impl Render for AddCatalogProjectModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("AddCatalogProjectModal")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .track_focus(&self.focus_handle)
            .w(rems(32.))
            .p_4()
            .gap_3()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_md()
            .child(Label::new("Add Project to Catalog").size(LabelSize::Large))
            .child(
                Label::new("Name")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(self.name_editor.clone())
            .child(
                Label::new("Remote URL")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(self.url_editor.clone())
            .child(
                Label::new("Default branch")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(self.branch_editor.clone())
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel", "Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel(&menu::Cancel, window, cx);
                            })),
                    )
                    .child(
                        Button::new("add", "Add")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm(&menu::Confirm, window, cx);
                            })),
                    ),
            )
    }
}

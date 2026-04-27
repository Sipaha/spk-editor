use editor::Editor;
use gpui::{
    AppContext as _, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, WeakEntity, actions,
};
use settings::Settings as _;
use solutions::{SolutionId, SolutionStore, SolutionsSettings};
use std::path::PathBuf;
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{ModalView, Workspace};

use crate::actions::{DeleteSolution, NewSolution};

actions!(
    solutions,
    [
        /// Open the modal to add a new project to the catalog.
        AddCatalogProject,
    ]
);

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
    workspace.register_action(|workspace, action: &DeleteSolution, window, cx| {
        let id = SolutionId(action.id.clone());
        let store = SolutionStore::global(cx);
        // Look up the solution's display name + root for the modal copy.
        // If the id is unknown (stale action / already-deleted), do nothing.
        let Some((name, root)) = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|sol| sol.id == id)
                .map(|sol| (sol.name.clone(), sol.root.clone()))
        }) else {
            return;
        };
        workspace.toggle_modal(window, cx, |_window, cx| {
            DeleteSolutionModal::new(id, name, root, cx)
        });
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

pub struct DeleteSolutionModal {
    id: SolutionId,
    name: String,
    root: PathBuf,
    focus_handle: FocusHandle,
}

impl DeleteSolutionModal {
    fn new(id: SolutionId, name: String, root: PathBuf, cx: &mut Context<Self>) -> Self {
        Self {
            id,
            name,
            root,
            focus_handle: cx.focus_handle(),
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        let store = SolutionStore::global(cx);
        let id = self.id.clone();
        store
            .update(cx, |s, cx| s.delete_solution(&id, cx))
            .log_err();
        // Disk cleanup is best-effort and async — the directory can be huge
        // (worktrees with full git histories), so we don't want to block the
        // UI thread. Failures are logged but not surfaced: by this point the
        // metadata entry is gone, so the user has effectively forgotten the
        // solution either way.
        let root = self.root.clone();
        cx.background_spawn(async move {
            let result: std::io::Result<()> =
                smol::unblock(move || std::fs::remove_dir_all(&root)).await;
            if let Err(err) = result {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(
                        "delete_solution: removing directory failed: {err} (orphaned files left in place)"
                    );
                }
            }
        })
        .detach();
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for DeleteSolutionModal {}

impl Focusable for DeleteSolutionModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for DeleteSolutionModal {
    fn debug_kind(&self) -> &'static str {
        "DeleteSolution"
    }
}

impl Render for DeleteSolutionModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path_str = self.root.display().to_string();
        v_flex()
            .key_context("DeleteSolutionModal")
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
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(IconName::Warning)
                            .color(Color::Warning)
                            .size(IconSize::Medium),
                    )
                    .child(Label::new("Delete Solution").size(LabelSize::Large)),
            )
            .child(
                Label::new(format!("\"{}\" will be removed from the launcher.", self.name))
                    .color(Color::Default),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new("All files under this directory will be permanently deleted from disk:")
                            .color(Color::Muted)
                            .size(LabelSize::Small),
                    )
                    .child(
                        Label::new(path_str)
                            .color(Color::Muted)
                            .size(LabelSize::XSmall),
                    ),
            )
            .child(
                Label::new("This action cannot be undone.")
                    .color(Color::Warning)
                    .size(LabelSize::Small),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel", "Cancel").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.cancel(&menu::Cancel, window, cx);
                            },
                        )),
                    )
                    .child(
                        Button::new("delete", "Delete")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm(&menu::Confirm, window, cx);
                            })),
                    ),
            )
    }
}

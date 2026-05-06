use anyhow::Result;
use editor::Editor;
use gpui::{
    App, AppContext as _, AsyncWindowContext, Entity, EventEmitter, FocusHandle, Focusable,
    MouseButton, Pixels, Render, WeakEntity, Window, px,
};
use solution_agent::store::{SolutionAgentStore, SolutionAgentStoreEvent};
use solutions::{
    CatalogId, CatalogProject, PendingAddView, Solution, SolutionId, SolutionStore,
    SolutionStoreEvent, default_cache_root,
};
use ui::{Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    MultiWorkspace, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::actions::{DeleteCatalogProject, EditCatalogProject, NewSolution, ToggleSolutionsPanel};
use crate::open::{OpenIntent, open_solution, workspace_has_solution};

struct RenameState {
    sol_id: SolutionId,
    editor: Entity<Editor>,
}

pub struct SolutionsPanel {
    focus_handle: FocusHandle,
    _workspace: WeakEntity<Workspace>,
    width: Option<Pixels>,
    _store_subscription: gpui::Subscription,
    _agent_store_subscription: Option<gpui::Subscription>,
    rename_state: Option<RenameState>,
}

impl SolutionsPanel {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let store = SolutionStore::global(cx);
        let store_subscription = cx.subscribe(&store, |_, _, _event: &SolutionStoreEvent, cx| {
            cx.notify();
        });
        // Re-render when agent sessions change so the running indicator
        // and session count badge stay in sync without requiring the
        // user to click the panel.
        let agent_store_subscription = SolutionAgentStore::try_global(cx).map(|agent_store| {
            cx.subscribe(
                &agent_store,
                |_, _, _event: &SolutionAgentStoreEvent, cx| {
                    cx.notify();
                },
            )
        });
        Self {
            focus_handle: cx.focus_handle(),
            _workspace: workspace,
            width: None,
            _store_subscription: store_subscription,
            _agent_store_subscription: agent_store_subscription,
            rename_state: None,
        }
    }

    fn start_rename(
        &mut self,
        sol_id: SolutionId,
        current_name: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(current_name, window, cx);
            editor.select_all(&editor::actions::SelectAll, window, cx);
            editor
        });
        let focus = editor.focus_handle(cx);
        self.rename_state = Some(RenameState { sol_id, editor });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(state) = self.rename_state.take() else {
            return;
        };
        let new_name = state.editor.read(cx).text(cx).trim().to_string();
        if !new_name.is_empty() {
            SolutionStore::global(cx)
                .update(cx, |s, cx| s.rename_solution(&state.sol_id, &new_name, cx))
                .log_err();
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.rename_state.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn render_section_header(
        label: &'static str,
        add_button_id: &'static str,
        add_action: Box<dyn gpui::Action>,
        add_tooltip: &'static str,
    ) -> impl IntoElement {
        h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(div().flex_1())
            .child(
                IconButton::new(add_button_id, IconName::Plus)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text(add_tooltip))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(add_action.boxed_clone(), cx);
                    }),
            )
    }

    fn render_catalog_row(project: &CatalogProject) -> impl IntoElement {
        let edit_id = project.id.as_str().to_string();
        let delete_id = edit_id.clone();
        let row_group = SharedString::from(format!("catalog-row-{}", project.id.as_str()));
        h_flex()
            .id(SharedString::from(format!(
                "catalog-{}",
                project.id.as_str()
            )))
            .group(row_group.clone())
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .child(Icon::new(IconName::GitBranch).size(IconSize::Small))
            .child(
                div()
                    .flex_1()
                    .child(Label::new(project.name.clone()).truncate()),
            )
            .child(
                IconButton::new(
                    SharedString::from(format!("catalog-edit-{}", project.id.as_str())),
                    IconName::Pencil,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .visible_on_hover(row_group.clone())
                .tooltip(Tooltip::text("Edit project settings"))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(
                        Box::new(EditCatalogProject {
                            id: edit_id.clone(),
                        }),
                        cx,
                    );
                }),
            )
            .child(
                IconButton::new(
                    SharedString::from(format!("catalog-delete-{}", project.id.as_str())),
                    IconName::Trash,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .visible_on_hover(row_group)
                .tooltip(Tooltip::text("Delete from catalog"))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(
                        Box::new(DeleteCatalogProject {
                            id: delete_id.clone(),
                        }),
                        cx,
                    );
                }),
            )
    }

    /// Ghost row rendered directly under a Solution row while an
    /// `add_member` is in flight, or while a failed add is waiting for the
    /// user to Retry / Edit / Dismiss. The row state is read from
    /// `SolutionStore::pending_adds_for` and refreshes via the existing
    /// `SolutionStoreEvent` subscription installed in `new`.
    fn render_pending_add_row(
        &self,
        sol_id: &SolutionId,
        pending: &PendingAddView,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let sol_id = sol_id.clone();
        let cat_id = pending.catalog_id.clone();
        let is_failed = pending.error.is_some();
        let row_id = SharedString::from(format!("pending-{}-{}", sol_id.as_str(), cat_id.as_str()));
        let group = SharedString::from(format!(
            "pending-row-{}-{}",
            sol_id.as_str(),
            cat_id.as_str()
        ));

        // Status icon: error glyph for the failure state, neutral spinner-ish
        // glyph (not animated) for in-progress. The percent counter is the
        // load-bearing motion cue here, not the icon.
        let status_icon = if is_failed {
            Icon::new(IconName::Warning)
                .size(IconSize::Small)
                .color(Color::Error)
        } else {
            Icon::new(IconName::ArrowCircle)
                .size(IconSize::Small)
                .color(Color::Muted)
        };

        let secondary_text = if let Some(err) = pending.error.as_ref() {
            truncate_one_line(err, 120)
        } else if let Some(pct) = pending.percent {
            format!("{} {}%", pending.stage, pct)
        } else {
            pending.stage.clone()
        };

        let mut row = h_flex()
            .id(row_id)
            .group(group)
            .pl_6()
            .pr_2()
            .py_1()
            .gap_2()
            .items_center()
            .child(status_icon)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(Label::new(pending.catalog_name.clone()).truncate())
                    .child(
                        Label::new(secondary_text)
                            .color(if is_failed {
                                Color::Error
                            } else {
                                Color::Muted
                            })
                            .size(LabelSize::XSmall),
                    ),
            );

        if is_failed {
            let retry_sol = sol_id.clone();
            let retry_cat = cat_id.clone();
            let edit_cat = cat_id.clone();
            let dismiss_sol = sol_id.clone();
            let dismiss_cat = cat_id.clone();
            row = row
                .child(
                    IconButton::new(
                        SharedString::from(format!(
                            "retry-{}-{}",
                            sol_id.as_str(),
                            cat_id.as_str()
                        )),
                        IconName::ArrowCircle,
                    )
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Retry"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        retry_pending_add(&retry_sol, &retry_cat, cx);
                    })),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("edit-{}-{}", sol_id.as_str(), cat_id.as_str())),
                        IconName::Pencil,
                    )
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Edit project settings"))
                    .on_click(cx.listener(move |_, _, window, cx| {
                        window.dispatch_action(
                            Box::new(EditCatalogProject {
                                id: edit_cat.0.clone(),
                            }),
                            cx,
                        );
                    })),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!(
                            "dismiss-{}-{}",
                            sol_id.as_str(),
                            cat_id.as_str()
                        )),
                        IconName::Close,
                    )
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Dismiss"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        SolutionStore::global(cx).update(cx, |s, cx| {
                            s.clear_failed_add(&dismiss_sol, &dismiss_cat, cx);
                        });
                    })),
                );
        } else {
            let cancel_sol = sol_id.clone();
            let cancel_cat = cat_id.clone();
            row = row.child(
                IconButton::new(
                    SharedString::from(format!("cancel-{}-{}", sol_id.as_str(), cat_id.as_str())),
                    IconName::Close,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text("Cancel"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    SolutionStore::global(cx).update(cx, |s, cx| {
                        s.cancel_add_member(&cancel_sol, &cancel_cat, cx);
                    });
                })),
            );
        }

        row
    }

    fn render_solution_row(
        &self,
        s: &Solution,
        window: &Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let sol_id_open = s.id.clone();
        let sol_id_middle = s.id.clone();
        let sol_id_add = s.id.clone();
        let sol_id_close = s.id.clone();
        let row_group = SharedString::from(format!("sol-row-{}", s.id.as_str()));

        let is_open = self.is_solution_open(&s.id, window, cx);
        let session_count = SolutionAgentStore::try_global(cx)
            .map(|store| store.read(cx).sessions_for(&s.id).len())
            .unwrap_or(0);

        // Coloured dot acting as the open/closed indicator. We always
        // reserve the slot so names line up whether or not the solution
        // is open.
        let status_dot = {
            let mut dot = div().w_1p5().h_1p5().rounded_full().flex_none();
            if is_open {
                dot = dot.bg(cx.theme().status().created);
            } else {
                dot = dot.bg(cx.theme().colors().border);
            }
            dot
        };

        let renaming = self
            .rename_state
            .as_ref()
            .is_some_and(|st| st.sol_id == s.id);
        let rename_editor = self
            .rename_state
            .as_ref()
            .filter(|st| st.sol_id == s.id)
            .map(|st| st.editor.clone());

        let mut row = h_flex()
            .id(SharedString::from(format!("sol-{}", s.id.as_str())))
            .group(row_group.clone())
            .px_2()
            .py_1p5()
            .gap_2()
            .items_center()
            .when(!renaming, |this| this.cursor_pointer())
            .hover(|s| s.bg(cx.theme().colors().element_hover))
            .when(renaming, |this| {
                this.key_context("SolutionsPanelRenamePrompt")
                    .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                        this.commit_rename(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                        this.cancel_rename(window, cx);
                    }))
            })
            .child(status_dot)
            .child(Icon::new(IconName::Folder).size(IconSize::Small))
            .child(v_flex().flex_1().min_w_0().gap_0p5().map(|col| {
                if let Some(editor) = rename_editor.clone() {
                    col.child(div().flex_1().child(editor))
                } else {
                    col.child(Label::new(s.name.clone()).truncate()).child(
                        Label::new(format!("{} projects", s.members.len()))
                            .color(Color::Muted)
                            .size(LabelSize::XSmall),
                    )
                }
            }));

        if session_count > 0 {
            let count_label = SharedString::from(session_count.to_string());
            let badge_tooltip: SharedString = if session_count == 1 {
                "1 AI session running".into()
            } else {
                format!("{session_count} AI sessions running").into()
            };
            row = row.child(
                h_flex()
                    .id(SharedString::from(format!(
                        "sol-sessions-{}",
                        s.id.as_str()
                    )))
                    .flex_none()
                    .gap_0p5()
                    .px_1()
                    .rounded_sm()
                    .bg(cx.theme().colors().element_selected)
                    .tooltip(Tooltip::text(badge_tooltip))
                    .child(
                        Icon::new(IconName::Sparkle)
                            .size(IconSize::XSmall)
                            .color(Color::Accent),
                    )
                    .child(
                        Label::new(count_label)
                            .size(LabelSize::XSmall)
                            .color(Color::Accent),
                    ),
            );
        }

        if renaming {
            let row_id = s.id.clone();
            return row
                .child(
                    IconButton::new(
                        SharedString::from(format!("rename-confirm-{}", s.id.as_str())),
                        IconName::Check,
                    )
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Save"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.commit_rename(window, cx);
                    })),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("rename-cancel-{}", row_id.as_str())),
                        IconName::Close,
                    )
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Cancel"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.cancel_rename(window, cx);
                    })),
                );
        }

        let sol_id_rename = s.id.clone();
        let current_name = s.name.clone();

        if is_open {
            row = row.child(
                IconButton::new(
                    SharedString::from(format!("close-solution-{}", s.id.as_str())),
                    IconName::Close,
                )
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .visible_on_hover(row_group.clone())
                .tooltip(Tooltip::text(if session_count > 0 {
                    "Close solution and stop its AI sessions"
                } else {
                    "Close solution"
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_solution(sol_id_close.clone(), window, cx);
                })),
            );
        }

        row.child(
            IconButton::new(
                SharedString::from(format!("rename-solution-{}", s.id.as_str())),
                IconName::Pencil,
            )
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .visible_on_hover(row_group.clone())
            .tooltip(Tooltip::text("Rename solution"))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_rename(sol_id_rename.clone(), &current_name, window, cx);
            })),
        )
        .child(
            IconButton::new(
                SharedString::from(format!("add-member-{}", s.id.as_str())),
                IconName::Plus,
            )
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .visible_on_hover(row_group)
            .tooltip(Tooltip::text("Add project to solution"))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_add_member_picker(sol_id_add.clone(), window, cx);
            })),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event, window, cx| {
                this.open_solution(sol_id_open.clone(), OpenIntent::SameWindow, window, cx);
            }),
        )
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this, _event, window, cx| {
                this.open_solution(sol_id_middle.clone(), OpenIntent::NewWindow, window, cx);
            }),
        )
    }

    /// True when any window has the given Solution loaded as either an
    /// active or retained workspace. We query our own window through
    /// the `Workspace` weak handle (since `cx.read_window` panics for
    /// the rendering window — the one that's currently on the stack)
    /// and other windows through their `MultiWorkspace` root view.
    fn is_solution_open(&self, sol_id: &SolutionId, window: &Window, cx: &App) -> bool {
        if let Some(workspace) = self._workspace.upgrade()
            && let Some(mw) = workspace
                .read(cx)
                .multi_workspace()
                .and_then(|w| w.upgrade())
            && mw
                .read(cx)
                .workspaces()
                .any(|ws| workspace_has_solution(ws, sol_id, cx))
        {
            return true;
        }
        // Skip the window currently being rendered (its root view is
        // borrowed out of the registry, so `read_with` would panic).
        // `cx.active_window()` returns the OS-focused window, which is
        // not necessarily the one we're rendering for, so use the
        // explicit `&Window` instead.
        let skip = window.window_handle().window_id();
        cx.windows().iter().any(|handle| {
            if handle.window_id() == skip {
                return false;
            }
            let Some(mw_handle) = handle.downcast::<MultiWorkspace>() else {
                return false;
            };
            mw_handle
                .read_with(cx, |mw, cx| {
                    mw.workspaces()
                        .any(|ws| workspace_has_solution(ws, sol_id, cx))
                })
                .unwrap_or(false)
        })
    }

    fn close_solution(
        &self,
        sol_id: SolutionId,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // 1. Stop and discard all live agent sessions for this solution.
        if let Some(agent_store) = SolutionAgentStore::try_global(cx) {
            agent_store.update(cx, |store, cx| {
                let session_ids: Vec<_> = store
                    .sessions_for(&sol_id)
                    .into_iter()
                    .map(|session| session.read(cx).id)
                    .collect();
                for id in session_ids {
                    store.close_session(id, cx).log_err();
                }
            });
        }
        // 2. Close any retained / active workspaces hosting this Solution.
        //    Mirror the search in `is_solution_open`: own window first
        //    (via the WeakEntity navigation), then iterate the rest.
        if let Some(workspace) = self._workspace.upgrade()
            && let Some(mw_weak) = workspace.read(cx).multi_workspace().cloned()
            && let Some(mw) = mw_weak.upgrade()
        {
            mw.update(cx, |mw, cx| {
                close_solution_workspaces_in(mw, &sol_id, window, cx);
            });
        }
        let skip = cx.active_window().map(|w| w.window_id());
        let other_windows: Vec<_> = cx
            .windows()
            .into_iter()
            .filter(|handle| Some(handle.window_id()) != skip)
            .filter_map(|handle| handle.downcast::<MultiWorkspace>())
            .collect();
        for handle in other_windows {
            let sol_id = sol_id.clone();
            handle
                .update(cx, move |mw, window, cx| {
                    close_solution_workspaces_in(mw, &sol_id, window, cx);
                })
                .log_err();
        }
    }

    #[allow(dead_code)]
    fn _placeholder(&self) {}
}

fn close_solution_workspaces_in(
    mw: &mut MultiWorkspace,
    sol_id: &SolutionId,
    window: &mut Window,
    cx: &mut gpui::Context<MultiWorkspace>,
) {
    let to_close: Vec<_> = mw
        .workspaces()
        .filter(|ws| workspace_has_solution(ws, sol_id, cx))
        .cloned()
        .collect();
    for ws in to_close {
        mw.close_workspace(&ws, window, cx).detach_and_log_err(cx);
    }
}

impl SolutionsPanel {
    fn open_add_member_picker(
        &self,
        sol_id: SolutionId,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(workspace) = self._workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            crate::add_member_picker::AddMemberPicker::open(workspace, sol_id, window, cx);
        });
    }

    fn open_solution(
        &self,
        sol_id: SolutionId,
        intent: OpenIntent,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let source = window.window_handle().downcast();
        open_solution(sol_id, source, intent, cx);
    }
}

impl EventEmitter<PanelEvent> for SolutionsPanel {}

impl Focusable for SolutionsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for SolutionsPanel {
    fn persistent_name() -> &'static str {
        "SolutionsPanel"
    }

    fn panel_key() -> &'static str {
        "SolutionsPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _: &mut Window,
        _: &mut gpui::Context<Self>,
    ) {
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        self.width.unwrap_or(px(280.))
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Folder)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Solutions")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleSolutionsPanel)
    }

    fn activation_priority(&self) -> u32 {
        9
    }
}

impl Render for SolutionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let store = SolutionStore::global(cx);
        let (catalog, solutions, pending_per_solution) = store.read_with(cx, |s, _| {
            let solutions = s.solutions().to_vec();
            let pending: Vec<Vec<PendingAddView>> = solutions
                .iter()
                .map(|sol| s.pending_adds_for(&sol.id))
                .collect();
            (s.catalog().to_vec(), solutions, pending)
        });

        let mut solution_rows: Vec<gpui::AnyElement> = Vec::new();
        for (sol, pending) in solutions.iter().zip(pending_per_solution.iter()) {
            solution_rows.push(self.render_solution_row(sol, window, cx).into_any_element());
            for p in pending {
                solution_rows.push(
                    self.render_pending_add_row(&sol.id, p, cx)
                        .into_any_element(),
                );
            }
        }

        v_flex()
            .key_context("SolutionsPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(Self::render_section_header(
                "Solutions",
                "header-new-solution",
                Box::new(NewSolution),
                "New solution",
            ))
            .children(solution_rows)
            .child(div().h(px(8.)))
            .child(Self::render_section_header(
                "Catalog",
                "header-add-project",
                Box::new(crate::modals::AddCatalogProject),
                "Add project to catalog",
            ))
            .children(catalog.iter().map(Self::render_catalog_row))
    }
}

/// Re-runs `add_member` for a previously-failed (sol, cat) pair, after
/// clearing the failed in-flight entry. The store wipes any partial
/// directory before cloning, so this is safe even if the previous attempt
/// half-cloned into the target.
fn retry_pending_add(sol: &SolutionId, cat: &CatalogId, cx: &mut App) {
    let store = SolutionStore::global(cx);
    let task = store.update(cx, |s, cx| {
        s.clear_failed_add(sol, cat, cx);
        s.add_member(sol.clone(), cat.clone(), default_cache_root(), cx)
    });
    task.detach_and_log_err(cx);
}

/// Collapse multi-line errors and clip overly long ones for the
/// pending-add row's secondary line. Git emits useful error text on the
/// last stderr line (already handled in `git.rs`), so the first line is
/// almost always the right one to surface.
fn truncate_one_line(s: &str, max_chars: usize) -> String {
    let head = s.lines().next().unwrap_or("").trim();
    if head.chars().count() > max_chars {
        let mut out: String = head.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        head.to_string()
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleSolutionsPanel, window, cx| {
            workspace.toggle_panel_focus::<SolutionsPanel>(window, cx);
        });
        workspace.register_action(
            |workspace, _: &crate::actions::AddProjectToActiveSolution, window, cx| {
                let Some(sol_id) = active_solution_id_for_workspace(workspace, cx) else {
                    return;
                };
                crate::add_member_picker::AddMemberPicker::open(workspace, sol_id, window, cx);
            },
        );
    })
    .detach();
}

/// Returns the `SolutionId` whose root is among the active workspace's
/// worktrees (visible OR hidden — empty solutions attach `solution.root`
/// as a hidden worktree so the project panel stays clean while the
/// workspace still knows which solution it belongs to). Returns `None`
/// when this workspace has no solution association.
fn active_solution_id_for_workspace(workspace: &Workspace, cx: &App) -> Option<SolutionId> {
    let store = SolutionStore::try_global(cx)?;
    let project = workspace.project().clone();
    for tree in project.read(cx).worktrees(cx) {
        let path = tree.read(cx).abs_path();
        if let Some(sol) = store.read(cx).solution_for_path(&path) {
            return Some(sol.id.clone());
        }
    }
    None
}

pub async fn load(
    workspace: WeakEntity<Workspace>,
    mut cx: AsyncWindowContext,
) -> Result<Entity<SolutionsPanel>> {
    workspace.update_in(&mut cx, |_, window, cx| {
        cx.new(|cx2| SolutionsPanel::new(workspace.clone(), window, cx2))
    })
}

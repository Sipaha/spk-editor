//! Right-dock chat panel for Solution-scoped AI sessions.
//!
//! Hosts ALL session UI: tab strip across the top, active session view in the
//! body, "+ New <Adapter> Session" buttons in the footer. Sessions are NOT
//! workspace pane items — overrides FORK.md decision #7 in favour of the
//! flagship-AI-editor pattern (Cursor / Cody / Copilot Chat / upstream Zed
//! AgentPanel) where chat lives in its own dedicated docked panel rather than
//! competing with code for the main editor area.

use std::collections::HashMap;

use anyhow::Context as _;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div, px,
};
use solutions::{SolutionId, SolutionStore, SolutionStoreEvent};
use ui::ButtonLike;
use ui::prelude::*;
use ui::{IconName, Label, LabelSize};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::actions::FocusNavigator;
use crate::model::{AgentServerId, SolutionSession};
use crate::session_view::SolutionSessionView;
use crate::store::SolutionAgentStore;

pub struct SolutionSessionsNavigator {
    workspace: WeakEntity<Workspace>,
    project: WeakEntity<project::Project>,
    focus_handle: FocusHandle,
    width: gpui::Pixels,
    active_solution: Option<SolutionId>,
    /// Sessions opened in this panel, in tab order.
    open_sessions: Vec<crate::model::SolutionSessionId>,
    /// Index into `open_sessions` of the visible session.
    selected_index: Option<usize>,
    /// One `SolutionSessionView` entity per opened session, kept in this
    /// HashMap so re-selecting a tab does not reset its compose-box state.
    views: HashMap<crate::model::SolutionSessionId, Entity<SolutionSessionView>>,
    _store_subscription: Subscription,
    _solutions_subscription: Option<Subscription>,
}

impl SolutionSessionsNavigator {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        project: WeakEntity<project::Project>,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SolutionAgentStore::global(cx);
        let store_subscription = cx.subscribe(&store, |_, _, _, cx| cx.notify());
        let solutions_subscription = SolutionStore::try_global(cx).map(|sol_store| {
            cx.subscribe(&sol_store, |this, _, _: &SolutionStoreEvent, cx| {
                this.refresh_active_solution(cx);
            })
        });
        Self {
            workspace,
            project,
            focus_handle: cx.focus_handle(),
            // Bottom dock — see Panel::position. Height (not width) for the
            // bottom orientation; chat dialogs often produce wide outputs
            // (long bash command lines, file paths, code blocks) and a
            // narrow right-dock truncates them awkwardly.
            width: px(380.0),
            active_solution: None,
            open_sessions: Vec::new(),
            selected_index: None,
            views: HashMap::default(),
            _store_subscription: store_subscription,
            _solutions_subscription: solutions_subscription,
        }
    }

    pub fn refresh_active_solution(&mut self, cx: &mut Context<Self>) {
        let new_id = self.derive_active_solution(cx);
        if new_id != self.active_solution {
            self.active_solution = new_id;
            // Different solution → wipe panel-local tabs. Sessions themselves
            // stay alive in the store so they reappear when the user comes
            // back to that solution.
            self.open_sessions.clear();
            self.selected_index = None;
            self.views.clear();
            cx.notify();
        }
    }

    fn derive_active_solution(&self, cx: &App) -> Option<SolutionId> {
        let project = self.project.upgrade()?;
        let store = SolutionStore::try_global(cx)?;
        let worktrees = project.read(cx).visible_worktrees(cx).collect::<Vec<_>>();
        for worktree in worktrees {
            let path = worktree.read(cx).abs_path();
            let id = store
                .read_with(cx, |s, _| s.solution_for_path(&path).map(|sol| sol.id.clone()));
            if id.is_some() {
                return id;
            }
        }
        None
    }

    fn open_session(
        &mut self,
        session_id: crate::model::SolutionSessionId,
        session: Entity<SolutionSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(idx) = self.open_sessions.iter().position(|s| *s == session_id) {
            self.selected_index = Some(idx);
            cx.notify();
            return;
        }
        let view = cx.new(|cx| {
            SolutionSessionView::new(
                session_id,
                session,
                self.workspace.clone(),
                window,
                cx,
            )
        });
        self.open_sessions.push(session_id);
        self.selected_index = Some(self.open_sessions.len() - 1);
        self.views.insert(session_id, view);
        cx.notify();
    }

    fn close_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.open_sessions.len() {
            return;
        }
        let session_id = self.open_sessions.remove(idx);
        self.views.remove(&session_id);
        if let Some(sel) = self.selected_index {
            self.selected_index = if self.open_sessions.is_empty() {
                None
            } else if sel == idx {
                Some(idx.min(self.open_sessions.len() - 1))
            } else if sel > idx {
                Some(sel - 1)
            } else {
                Some(sel)
            };
        }
        cx.notify();
    }

    fn create_and_open_session(
        &self,
        agent_id: AgentServerId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(solution_id) = self.active_solution.clone() else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = workspace.read(cx).project().clone();
        let store = SolutionAgentStore::global(cx);
        let task = store.update(cx, |store, cx| {
            store.create_session(solution_id, agent_id, project, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let session_id = task
                .await
                .context("create_session failed")
                .log_err()
                .ok_or(())?;
            this.update_in(cx, |this, window, cx| {
                let session = SolutionAgentStore::global(cx)
                    .read_with(cx, |s, _| s.session(session_id));
                if let Some(session) = session {
                    this.open_session(session_id, session, window, cx);
                }
            })
            .ok();
            Ok::<_, ()>(())
        })
        .detach();
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut strip = div()
            .id("solution-sessions-tab-strip")
            .flex()
            .h_8()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .overflow_x_scroll();
        for (idx, session_id) in self.open_sessions.iter().enumerate() {
            let selected = self.selected_index == Some(idx);
            let session_id_for_select = *session_id;
            let title = SolutionAgentStore::global(cx)
                .read_with(cx, |s, _| s.session(session_id_for_select))
                .map(|entity| entity.read(cx).title.clone())
                .unwrap_or_else(|| SharedString::from("Session"));
            let bg = if selected {
                cx.theme().colors().tab_active_background
            } else {
                cx.theme().colors().tab_inactive_background
            };
            let tab = div()
                .id(SharedString::from(format!("tab-{}", session_id_for_select)))
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .bg(bg)
                .border_r_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new(title).size(LabelSize::Small))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "close-{}",
                            session_id_for_select
                        )))
                        .px_1()
                        .child(Label::new("×").size(LabelSize::Small))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.close_tab(idx, cx);
                            }),
                        ),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_index = Some(idx);
                        cx.notify();
                    }),
                );
            strip = strip.child(tab);
        }
        strip
    }

    fn render_status_row(
        &self,
        active_view: Option<&Entity<SolutionSessionView>>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let session = active_view.and_then(|v| {
            let session_id = self.selected_index.and_then(|i| self.open_sessions.get(i).copied())?;
            let _ = v;
            SolutionAgentStore::global(cx).read_with(cx, |s, _| s.session(session_id))
        })?;
        let s = session.read(cx);
        let agent_id = s.agent_id.clone();
        let state_text = SharedString::from(s.state.short_label());
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h_6()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Label::new(agent_id)
                        .color(Color::Muted)
                        .size(LabelSize::XSmall),
                )
                .child(Label::new("·").color(Color::Muted).size(LabelSize::XSmall))
                .child(Label::new(state_text).size(LabelSize::XSmall))
                .into_any_element(),
        )
    }

    fn render_footer(
        &self,
        has_active_solution: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut footer = div()
            .px_2()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().colors().border_variant);
        if !has_active_solution {
            return footer;
        }
        let store = SolutionAgentStore::global(cx);
        let adapter_buttons = store.read_with(cx, |s, _| {
            s.adapters
                .supported_ids()
                .iter()
                .filter_map(|id| {
                    let adapter = s.adapters.get(id)?;
                    Some((id.clone(), adapter.display_name(), adapter.icon()))
                })
                .collect::<Vec<_>>()
        });
        for (id, display_name, icon) in adapter_buttons {
            let agent_id = id.clone();
            footer = footer.child(
                ButtonLike::new(SharedString::from(format!("new-session-{id}")))
                    .full_width()
                    .size(ui::ButtonSize::Medium)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.create_and_open_session(agent_id.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .px_2()
                            .items_center()
                            .child(
                                Icon::new(IconName::Plus)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .child(
                                Icon::new(icon)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .child(Label::new(format!("New {display_name} Session"))),
                    ),
            );
        }
        footer
    }
}

impl Focusable for SolutionSessionsNavigator {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if let Some(idx) = self.selected_index
            && let Some(session_id) = self.open_sessions.get(idx)
            && let Some(view) = self.views.get(session_id)
        {
            return view.read(cx).focus_handle(cx);
        }
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for SolutionSessionsNavigator {}

impl Render for SolutionSessionsNavigator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_active_solution = self.active_solution.is_some();
        let active_view = self
            .selected_index
            .and_then(|i| self.open_sessions.get(i).copied())
            .and_then(|sid| self.views.get(&sid).cloned());
        let body: gpui::AnyElement = if !has_active_solution {
            div()
                .flex_1()
                .px_3()
                .py_4()
                .child(
                    Label::new("Open a Solution to start AI sessions.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .into_any_element()
        } else if let Some(view) = active_view.clone() {
            div().flex_1().child(view).into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Label::new("No session selected. Click + below.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .into_any_element()
        };
        let mut root = div()
            .key_context("SolutionSessionsNavigator")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full();
        if !self.open_sessions.is_empty() {
            root = root.child(self.render_tab_strip(cx));
            if let Some(status) = self.render_status_row(active_view.as_ref(), cx) {
                root = root.child(status);
            }
        }
        root.child(body).child(self.render_footer(has_active_solution, cx))
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
        // Bottom by default — chat dialogs frequently include wide content
        // (long command lines, file paths, code blocks) that the typical
        // right-dock width truncates. Users can drag it to left/right if
        // they prefer the side layout.
        DockPosition::Bottom
    }

    fn position_is_valid(&self, p: DockPosition) -> bool {
        matches!(
            p,
            DockPosition::Right | DockPosition::Left | DockPosition::Bottom
        )
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> gpui::Pixels {
        self.width
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Sparkle)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("AI sessions")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(FocusNavigator)
    }

    fn activation_priority(&self) -> u32 {
        30
    }
}


use anyhow::Context as _;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, WeakEntity, Window, div, px,
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
    _store_subscription: gpui::Subscription,
    _solutions_subscription: Option<gpui::Subscription>,
}

impl SolutionSessionsNavigator {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        project: WeakEntity<project::Project>,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SolutionAgentStore::global(cx);
        let store_subscription = cx.subscribe(&store, |_, _, _, cx| cx.notify());
        // Re-derive the active Solution whenever the SolutionStore changes —
        // covers add/remove/rename so the navigator does not stick to a
        // stale ID after the user edits Solutions from the dock panel.
        let solutions_subscription = SolutionStore::try_global(cx).map(|sol_store| {
            cx.subscribe(&sol_store, |this, _, _: &SolutionStoreEvent, cx| {
                this.refresh_active_solution(cx);
            })
        });
        Self {
            workspace,
            project,
            focus_handle: cx.focus_handle(),
            width: px(280.0),
            active_solution: None,
            _store_subscription: store_subscription,
            _solutions_subscription: solutions_subscription,
        }
    }

    /// Look at the workspace's visible worktrees and pick the first one that
    /// maps to a Solution. Multi-Solution workspaces are not a thing today,
    /// and even if they become one, "first match wins" is a reasonable
    /// fallback the user can override later via UI.
    pub fn refresh_active_solution(&mut self, cx: &mut Context<Self>) {
        let new_id = self.derive_active_solution(cx);
        if new_id != self.active_solution {
            self.active_solution = new_id;
            cx.notify();
        }
    }

    fn derive_active_solution(&self, cx: &App) -> Option<SolutionId> {
        // Read straight off the Project entity stored at construction time —
        // never go through Workspace here. Project subscription callbacks
        // already hold a Workspace lease, and reading Workspace from there
        // would double-lease and panic ("cannot read Workspace while it is
        // already being updated").
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
        &self,
        session: Entity<SolutionSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let session_id = session.read(cx).id;
        let weak_workspace = workspace.downgrade();
        workspace.update(cx, |ws, cx| {
            let view = cx.new(|cx| {
                SolutionSessionView::new(session_id, session, weak_workspace, window, cx)
            });
            ws.add_item_to_active_pane(Box::new(view), None, true, window, cx);
        });
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
        let weak_workspace = workspace.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            let session_id = task
                .await
                .context("create_session failed")
                .log_err()
                .ok_or(())?;
            // Resolve the freshly-created session entity from the store and
            // open the view. We dispatch back through `this` (instead of the
            // workspace directly) so the open path stays in one place.
            this.update_in(cx, |this, window, cx| {
                let session = SolutionAgentStore::global(cx)
                    .read_with(cx, |s, _| s.session(session_id));
                let _ = weak_workspace;
                if let Some(session) = session {
                    this.open_session(session, window, cx);
                }
            })
            .ok();
            Ok::<_, ()>(())
        })
        .detach();
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
        let has_active_solution = self.active_solution.is_some();

        let header = div()
            .flex()
            .h_8()
            .px_3()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(Label::new("Sessions").size(LabelSize::Small));

        let mut list = div()
            .id("solution-sessions-list")
            .flex_1()
            .overflow_y_scroll();

        if !has_active_solution {
            list = list.child(
                div().px_3().py_4().child(
                    Label::new("Open a Solution to start AI sessions.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                ),
            );
        } else if sessions.is_empty() {
            list = list.child(
                div().px_3().py_4().child(
                    Label::new("No sessions yet.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                ),
            );
        } else {
            for session in sessions {
                let s = session.read(cx);
                let id = s.id;
                let title = s.title.clone();
                let state_label = format!("{:?}", s.state);
                // Pick the icon from the session's adapter, not a hard-coded
                // brand — keeps the row honest if a non-Claude session ever
                // shows up.
                let agent_icon = store.read_with(cx, |store, _| {
                    store
                        .adapters
                        .get(&s.agent_id)
                        .map(|a| a.icon())
                        .unwrap_or(IconName::Sparkle)
                });
                let row = ButtonLike::new(SharedString::from(id.to_string()))
                    .full_width()
                    .size(ui::ButtonSize::Medium)
                    .child(
                        div()
                            .flex()
                            .px_2()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::new(agent_icon)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .child(Label::new(title))
                            .child(div().flex_1())
                            .child(
                                Label::new(state_label)
                                    .color(Color::Muted)
                                    .size(LabelSize::XSmall),
                            ),
                    )
                    .on_click(cx.listener({
                        let session = session.clone();
                        move |this, _, window, cx| {
                            this.open_session(session.clone(), window, cx);
                        }
                    }));
                list = list.child(row);
            }
        }

        // Render one button per registered adapter. The fork ships with
        // Claude today, but the panel deliberately enumerates the registry
        // so adding (or swapping) adapters does not require touching this
        // file — and so the button label/icon stay in sync with whatever
        // SolutionAgentAdapter::display_name returns.
        let mut footer = div().px_2().py_2().flex().flex_col().gap_1();
        if has_active_solution {
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
        }

        div()
            .key_context("SolutionSessionsNavigator")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(list)
            .child(footer)
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
        // Sparkle keeps the sidebar entry agent-agnostic — using AiClaude
        // would imply this dock only hosts Claude sessions, which is not
        // structurally true (the AdapterRegistry can hold any number).
        Some(IconName::Sparkle)
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

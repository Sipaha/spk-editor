//! Right-dock chat panel for Solution-scoped AI sessions.
//!
//! Hosts ALL session UI: tab strip across the top, active session view in the
//! body, "+ New Session" button in the strip. Sessions are NOT workspace pane
//! items — overrides FORK.md decision #7 in favour of the flagship-AI-editor
//! pattern (Cursor / Cody / Copilot Chat / upstream Zed AgentPanel) where
//! chat lives in its own dedicated docked panel rather than competing with
//! code for the main editor area.

use std::collections::HashMap;

use anyhow::Context as _;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div, px,
};
use solutions::{SolutionId, SolutionStore, SolutionStoreEvent};
use ui::prelude::*;
use ui::{
    Button, ButtonStyle, CommonAnimationExt, ContextMenu, Icon, IconName, Label, LabelSize,
    PopoverMenu,
};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

use crate::actions::FocusNavigator;
use crate::model::{AgentServerId, SolutionSession, SolutionSessionMetadata};
use crate::session_view::SolutionSessionView;
use crate::store::SolutionAgentStore;

/// In-flight `create_session` task. Rendered as a placeholder tab with a
/// spinner so the user gets immediate feedback (the real session takes
/// 3-4s to start because we have to spawn the agent subprocess + handshake
/// over ACP before the conversation thread exists).
struct PendingCreation {
    id: u64,
    display_name: SharedString,
    icon: IconName,
}

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
    /// In-flight session creations. Each entry renders as a spinner-tab and
    /// is removed when its `create_session` task resolves (success or
    /// error). Multiple entries can coexist if the user clicks "+" again
    /// before the first finishes.
    pending: Vec<PendingCreation>,
    next_pending_id: u64,
    /// Snapshot of persisted sessions for `active_solution`, sorted by
    /// `last_activity_at` desc. Loaded async from the DB whenever the
    /// active solution changes; populates the History popover and the
    /// "Continue last session" empty-state CTA.
    historic_sessions: Vec<SolutionSessionMetadata>,
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
        // Keep the historic-sessions snapshot in sync with the DB —
        // create/close persists rows, so the history popover would otherwise
        // get stale until the user switched solutions.
        let store_subscription = cx.subscribe(&store, |this, _, _, cx| {
            this.refresh_historic_sessions(cx);
            cx.notify();
        });
        let solutions_subscription = SolutionStore::try_global(cx).map(|sol_store| {
            cx.subscribe(&sol_store, |this, _, _: &SolutionStoreEvent, cx| {
                this.refresh_active_solution(cx);
            })
        });
        let mut this = Self {
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
            pending: Vec::new(),
            next_pending_id: 0,
            historic_sessions: Vec::new(),
            _store_subscription: store_subscription,
            _solutions_subscription: solutions_subscription,
        };
        this.refresh_active_solution(cx);
        this
    }

    pub fn refresh_active_solution(&mut self, cx: &mut Context<Self>) {
        let new_id = self.derive_active_solution(cx);
        if new_id != self.active_solution {
            self.active_solution = new_id;
            // Different solution → wipe panel-local tabs. Sessions themselves
            // stay alive in the store so they reappear when the user comes
            // back to that solution. Drop any in-flight pending creations
            // too — they were started against the previous solution.
            self.open_sessions.clear();
            self.selected_index = None;
            self.views.clear();
            self.pending.clear();
            self.historic_sessions.clear();
            cx.notify();
        }
        // Always refresh DB metadata when called — sessions get persisted
        // mid-conversation, so the "history" list needs to update on every
        // store event, not just on solution changes.
        self.refresh_historic_sessions(cx);
    }

    fn refresh_historic_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(solution_id) = self.active_solution.clone() else {
            self.historic_sessions.clear();
            return;
        };
        let store = SolutionAgentStore::global(cx);
        let Some(db) = store.read_with(cx, |s, _| s.db()) else {
            return;
        };
        let task = db.list_for_solution(solution_id.clone());
        cx.spawn(async move |this, cx| {
            let Ok(mut metas) = task.await else {
                return;
            };
            // last_activity_at desc — newest first.
            metas.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
            // Dedup by acp_session_id keeping the freshest row. Old fork-local
            // bug: `resume_session` used to mint a new internal id every time,
            // leaving multiple rows with the same agent-side session pointer.
            // The mint-fresh-id behaviour is gone now (commit message TBD)
            // but legacy rows might still be in the local DB.
            let mut seen: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            metas.retain(|m| seen.insert(m.acp_session_id.0.to_string()));
            this.update(cx, |this, cx| {
                if this.active_solution.as_ref() == Some(&solution_id) {
                    this.historic_sessions = metas;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
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
        &mut self,
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
        // Resolve adapter metadata for the placeholder tab. If the adapter
        // is somehow unregistered between click and dispatch we still go
        // ahead with the create — the placeholder just shows the raw id.
        let (display_name, icon) = store.read_with(cx, |s, _| {
            s.adapters
                .get(&agent_id)
                .map(|a| (a.display_name(), a.icon()))
                .unwrap_or_else(|| (SharedString::from(agent_id.to_string()), IconName::Sparkle))
        });
        let pending_id = self.next_pending_id;
        self.next_pending_id += 1;
        self.pending.push(PendingCreation {
            id: pending_id,
            display_name,
            icon,
        });
        cx.notify();

        let task = store.update(cx, |store, cx| {
            store.create_session(solution_id, agent_id, project, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.context("create_session failed");
            this.update_in(cx, |this, window, cx| {
                this.pending.retain(|p| p.id != pending_id);
                match result {
                    Ok(session_id) => {
                        let session = SolutionAgentStore::global(cx)
                            .read_with(cx, |s, _| s.session(session_id));
                        if let Some(session) = session {
                            this.open_session(session_id, session, window, cx);
                        } else {
                            cx.notify();
                        }
                    }
                    Err(err) => {
                        log::error!("create_session failed: {err:?}");
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Resume a persisted session: asks the store to reattach via ACP
    /// `load_session` (or `resume_session`), then opens its tab. Renders a
    /// pending placeholder while the ACP handshake completes — same pattern
    /// as `create_and_open_session`.
    fn resume_and_open(
        &mut self,
        meta: SolutionSessionMetadata,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = workspace.read(cx).project().clone();
        let store = SolutionAgentStore::global(cx);
        let (display_name, icon) = store.read_with(cx, |s, _| {
            s.adapters
                .get(&meta.agent_id)
                .map(|a| (a.display_name(), a.icon()))
                .unwrap_or_else(|| (meta.title.clone(), IconName::Sparkle))
        });
        let pending_id = self.next_pending_id;
        self.next_pending_id += 1;
        self.pending.push(PendingCreation {
            id: pending_id,
            display_name,
            icon,
        });
        cx.notify();

        let solution_session_id = meta.id;
        let session_title = meta.title.clone();
        let task = store.update(cx, |store, cx| {
            store.resume_session(meta, project, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.pending.retain(|p| p.id != pending_id);
                match result {
                    Ok(session_id) => {
                        let session = SolutionAgentStore::global(cx)
                            .read_with(cx, |s, _| s.session(session_id));
                        if let Some(session) = session {
                            this.open_session(session_id, session, window, cx);
                        } else {
                            cx.notify();
                        }
                    }
                    Err(err) => {
                        let err_str = format!("{err:#}");
                        log::error!("resume_session failed: {err:?}");
                        // claude-acp returns JSON-RPC "Resource not found"
                        // (-32002) when the agent has no record of the
                        // session — happens for sessions that never sent a
                        // message (claude only flushes to ~/.claude/projects
                        // on first turn) or after manual purges. The DB row
                        // is unrecoverable in that case, so drop it so the
                        // History popover stops offering it.
                        let resource_gone = err_str.contains("Resource not found")
                            || err_str.contains("-32002");
                        if resource_gone {
                            let db = SolutionAgentStore::global(cx)
                                .read_with(cx, |s, _| s.db());
                            if let Some(db) = db {
                                cx.background_spawn(async move {
                                    db.delete(solution_session_id).await
                                })
                                .detach_and_log_err(cx);
                            }
                            this.refresh_historic_sessions(cx);
                        }
                        let user_msg: SharedString = if resource_gone {
                            format!(
                                "\"{session_title}\" can't be resumed — the agent no longer has it (empty session, or the agent's storage was cleared). Removed from history."
                            ).into()
                        } else {
                            format!("Resuming \"{session_title}\" failed: {err_str}").into()
                        };
                        if let Some(workspace) = this.workspace.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                struct ResumeFailedNotification;
                                workspace.show_notification(
                                    NotificationId::unique::<ResumeFailedNotification>(),
                                    cx,
                                    move |cx| {
                                        cx.new(|cx| MessageNotification::new(user_msg, cx))
                                    },
                                );
                            });
                        }
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_new_session_button(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.active_solution.is_none() {
            return None;
        }
        let store = SolutionAgentStore::global(cx);
        let adapters = store.read_with(cx, |s, _| {
            s.adapters
                .supported_ids()
                .iter()
                .filter_map(|id| {
                    let adapter = s.adapters.get(id)?;
                    Some((id.clone(), adapter.display_name(), adapter.icon()))
                })
                .collect::<Vec<_>>()
        });
        if adapters.is_empty() {
            return None;
        }
        // The label stays adapter-agnostic on purpose — never hardcode a
        // specific neural network ("Claude", "Gemini", …) into the chrome.
        // The single-vs-multi-adapter distinction only changes whether the
        // click creates immediately or opens a chooser; the user-facing
        // string is the same in both cases.
        let label = SharedString::from("New Session");
        let trigger = Button::new("solution-sessions-new", label)
            .style(ButtonStyle::Subtle)
            .label_size(LabelSize::Small)
            .start_icon(
                Icon::new(IconName::Plus)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            );
        let element = if adapters.len() == 1 {
            // Skip the popover on the single-adapter path; one click creates.
            let (agent_id, _name, _icon) = adapters.into_iter().next().expect("adapters is non-empty");
            trigger
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.create_and_open_session(agent_id.clone(), window, cx);
                }))
                .into_any_element()
        } else {
            PopoverMenu::new("solution-sessions-new-popover")
                .trigger(trigger)
                .menu({
                    let weak = cx.entity().downgrade();
                    move |window, cx| {
                        let adapters = adapters.clone();
                        let weak = weak.clone();
                        Some(ContextMenu::build(window, cx, move |mut menu, _window, _cx| {
                            for (agent_id, name, icon) in adapters {
                                let weak = weak.clone();
                                let agent_id_for_action = agent_id.clone();
                                menu = menu.entry(name, None, {
                                    let _ = icon;
                                    move |window, cx| {
                                        if let Some(this) = weak.upgrade() {
                                            this.update(cx, |this, cx| {
                                                this.create_and_open_session(
                                                    agent_id_for_action.clone(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    }
                                });
                            }
                            menu
                        }))
                    }
                })
                .into_any_element()
        };
        Some(element)
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Tab strip is a single horizontal flex row that scrolls
        // horizontally as a whole. Tabs, spinner placeholders for in-flight
        // creations, and the "+ New Session" button are all siblings — the
        // button appears right after the last tab (Chrome-style) so it is
        // always visible and discoverable, instead of being pinned to the
        // far right where it visually blended into the dock background.
        let mut strip = div()
            .id("solution-sessions-tab-strip")
            .flex()
            .flex_none()
            .items_center()
            .h_8()
            .bg(cx.theme().colors().tab_bar_background)
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
        for pending in &self.pending {
            strip = strip.child(
                div()
                    .id(SharedString::from(format!("pending-tab-{}", pending.id)))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .bg(cx.theme().colors().tab_inactive_background)
                    .border_r_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Icon::new(pending.icon)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Icon::new(IconName::ArrowCircle)
                            .size(IconSize::Small)
                            .color(Color::Muted)
                            .with_rotate_animation(2),
                    )
                    .child(
                        Label::new(format!("Starting {}…", pending.display_name))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            );
        }
        if let Some(new_btn) = self.render_new_session_button(cx) {
            // px_2 = breathing room around the button so it doesn't bump
            // straight against the right edge of the last tab.
            strip = strip.child(div().px_2().flex_none().child(new_btn));
        }
        if let Some(history_btn) = self.render_history_button(cx) {
            strip = strip.child(div().pr_2().flex_none().child(history_btn));
        }
        strip
    }

    /// History popover trigger (clock icon). Lists the last 12 persisted
    /// sessions for the active solution; clicking a row resumes that
    /// session through `SolutionAgentStore::resume_session`.
    ///
    /// Hidden when there's nothing in the DB yet — no point rendering an
    /// always-empty popover.
    fn render_history_button(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.active_solution.is_none() || self.historic_sessions.is_empty() {
            return None;
        }
        let metas: Vec<SolutionSessionMetadata> = self
            .historic_sessions
            .iter()
            .take(12)
            .cloned()
            .collect();
        let trigger = ui::IconButton::new("solution-sessions-history", IconName::HistoryRerun)
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .tooltip(ui::Tooltip::text("Recent sessions"));
        let weak = cx.entity().downgrade();
        Some(
            PopoverMenu::new("solution-sessions-history-popover")
                .trigger(trigger)
                .menu(move |window, cx| {
                    let metas = metas.clone();
                    let weak = weak.clone();
                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                        for meta in metas {
                            let weak = weak.clone();
                            let meta_for_action = meta.clone();
                            // Compose: "<preview-or-title>  ·  <time>  ·  <Ntok>"
                            // Preview takes precedence over the placeholder
                            // "Session <uuid>" title because identical titles
                            // are exactly the case the user wanted to fix.
                            let primary = meta
                                .preview
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .unwrap_or(meta.title.as_ref());
                            let mut label = format!(
                                "{}  ·  {}",
                                primary,
                                relative_time_short(meta.last_activity_at, chrono::Utc::now()),
                            );
                            if let Some(tokens) = meta.total_tokens {
                                label.push_str(&format!("  ·  {}", format_tokens(tokens)));
                            }
                            menu = menu.entry(
                                SharedString::from(label),
                                None,
                                move |window, cx| {
                                    if let Some(this) = weak.upgrade() {
                                        let meta = meta_for_action.clone();
                                        this.update(cx, |this, cx| {
                                            this.resume_and_open(meta, window, cx);
                                        });
                                    }
                                },
                            );
                        }
                        menu
                    }))
                })
                .anchor(gpui::Anchor::TopRight)
                .into_any_element(),
        )
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
        // `min_h_0` on every body branch + on the wrapper that hosts the
        // session view: without it, the inner conversation grows past the
        // panel's allocated height (no scroll, compose row pushed below
        // the visible area). flex_1 alone is not enough — flex children
        // default to `min-height: auto` which equals content height.
        let body: gpui::AnyElement = if !has_active_solution {
            div()
                .flex_1()
                .min_h_0()
                .px_3()
                .py_4()
                .child(
                    Label::new("Open a Solution to start AI sessions.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .into_any_element()
        } else if let Some(view) = active_view.clone() {
            div().flex_1().min_h_0().child(view).into_any_element()
        } else {
            // No tab open. Offer "Continue last session" as the primary CTA
            // when the DB has at least one persisted session for this
            // solution; falls back to a plain hint pointing at the "+"
            // button when there's nothing to resume.
            let last_meta = self.historic_sessions.first().cloned();
            let mut empty = div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .items_center()
                .justify_center()
                .px_3();
            if let Some(meta) = last_meta {
                let primary = meta
                    .preview
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(meta.title.as_ref())
                    .to_string();
                let activity = relative_time_short(meta.last_activity_at, chrono::Utc::now());
                let mut header = format!("Last session: {primary}  ·  {activity}");
                if let Some(tokens) = meta.total_tokens {
                    header.push_str(&format!("  ·  {}", format_tokens(tokens)));
                }
                empty = empty
                    .child(
                        Label::new(header)
                            .color(Color::Muted)
                            .size(LabelSize::XSmall),
                    )
                    .child(
                        Button::new("solution-sessions-continue-last", "Continue last session")
                            .style(ButtonStyle::Filled)
                            .label_size(LabelSize::Small)
                            .start_icon(
                                Icon::new(IconName::HistoryRerun)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let meta = meta.clone();
                                this.resume_and_open(meta, window, cx);
                            })),
                    )
                    .child(
                        Label::new("…or start a fresh one with + above.")
                            .color(Color::Muted)
                            .size(LabelSize::XSmall),
                    );
            } else {
                empty = empty.child(
                    Label::new("No session selected. Click + above to start one.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                );
            }
            empty.into_any_element()
        };
        let mut root = div()
            .key_context("SolutionSessionsNavigator")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full();
        // Always render the tab strip when a solution is active, even with
        // zero open tabs — the strip hosts the "+" button which is the
        // entrypoint for creating the first session. Previously the strip
        // was hidden until at least one tab existed and the "+" lived in
        // a footer; merging them here gives a single, predictable home for
        // session controls (matches Cursor / VS Code chat UX).
        if has_active_solution {
            root = root.child(self.render_tab_strip(cx));
            if let Some(status) = self.render_status_row(active_view.as_ref(), cx) {
                root = root.child(status);
            }
        }
        root.child(body)
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


/// Compact token count, "12.3k tok" / "456 tok", for the History popover.
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M tok", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k tok", tokens as f64 / 1_000.0)
    } else {
        format!("{} tok", tokens)
    }
}

/// Compact "X ago" formatter mirroring `solutions_ui::welcome::relative_time_label`
/// but kept local to avoid a fork-internal cross-crate dep cycle.
fn relative_time_short(ts: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let secs = now.signed_duration_since(ts).num_seconds();
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 7 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else if secs < 30 * 86_400 {
        format!("{}w ago", secs / (7 * 86_400))
    } else if secs < 365 * 86_400 {
        format!("{}mo ago", secs / (30 * 86_400))
    } else {
        format!("{}y ago", secs / (365 * 86_400))
    }
}

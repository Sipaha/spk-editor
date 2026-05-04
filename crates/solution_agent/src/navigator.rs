//! Right-dock chat panel for Solution-scoped AI sessions.
//!
//! Hosts ALL session UI: tab strip across the top, active session view in the
//! body, "+ New Session" button in the strip. Sessions are NOT workspace pane
//! items — overrides FORK.md decision #7 in favour of the flagship-AI-editor
//! pattern (Cursor / Cody / Copilot Chat / upstream Zed AgentPanel) where
//! chat lives in its own dedicated docked panel rather than competing with
//! code for the main editor area.

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use util::ResultExt as _;
use gpui::{
    Animation, AnimationExt, App, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window,
    div, px, pulsating_between,
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
use crate::model::{AgentServerId, SessionState, SolutionSession, SolutionSessionMetadata};
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

/// Visual state shown next to a session tab title. Drives both the colour
/// of the dot and whether it pulses, so glance-reading the tab strip
/// answers "which sessions need me / are still working / are stuck."
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SessionStatusIndicator {
    /// `Idle` — nothing in flight, last turn finished cleanly. Muted
    /// dot so every tab has a visual marker but a quiet session
    /// doesn't compete with active ones.
    Idle,
    /// `Running` — agent is actively processing. Pulses to make a
    /// running tab clearly distinct from a static "needs attention"
    /// dot.
    Working,
    /// `AwaitingInput` — agent is parked waiting for the user. Static
    /// warning-coloured dot so the user notices on return.
    AwaitingUser,
    /// `Errored` — last turn failed; user must read the error and
    /// decide what to do.
    Errored,
}

fn session_status_indicator(state: &SessionState) -> SessionStatusIndicator {
    match state {
        SessionState::Idle => SessionStatusIndicator::Idle,
        SessionState::Running { .. } => SessionStatusIndicator::Working,
        SessionState::AwaitingInput => SessionStatusIndicator::AwaitingUser,
        SessionState::Errored(_) => SessionStatusIndicator::Errored,
    }
}

fn render_status_dot(status: SessionStatusIndicator, cx: &App) -> gpui::AnyElement {
    let (color, tooltip): (Color, &'static str) = match status {
        SessionStatusIndicator::Idle => (Color::Muted, "Idle"),
        SessionStatusIndicator::Working => (Color::Info, "Agent is working"),
        SessionStatusIndicator::AwaitingUser => (Color::Warning, "Awaiting your input"),
        SessionStatusIndicator::Errored => (Color::Error, "Session errored"),
    };
    let dot = div()
        .flex_none()
        .size(px(8.0))
        .rounded_full()
        .bg(color.color(cx));
    // Pulse the "Working" dot so a glance distinguishes "in progress" from
    // a static "needs attention" marker. The opacity sweep matches the
    // upstream pattern in `ai_setting_item.rs`.
    let dot: gpui::AnyElement = if matches!(status, SessionStatusIndicator::Working) {
        dot.with_animation(
            ElementId::Name(format!("solution-tab-status-pulse-{:?}", status).into()),
            Animation::new(std::time::Duration::from_secs(2))
                .repeat()
                .with_easing(pulsating_between(0.4, 1.0)),
            |element: gpui::Div, delta| element.opacity(delta),
        )
        .into_any_element()
    } else {
        dot.into_any_element()
    };
    div()
        .id(SharedString::from(format!(
            "solution-tab-status-{:?}",
            status
        )))
        .flex_none()
        .pr_1()
        .tooltip(ui::Tooltip::text(tooltip))
        .child(dot)
        .into_any_element()
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
    /// In-progress tab-rename. `None` means no tab is being renamed.
    /// While `Some`, the targeted tab swaps its label for the inline
    /// editor and the pencil button is replaced with a checkmark.
    /// Cleared on commit / cancel / tab-switch.
    renaming: Option<RenamingTab>,
    /// Per-session model name cache for the status row. Filled lazily
    /// on the first render that asks for a session's model — the ACP
    /// `selected_model` accessor is async (round-trip to the agent), so
    /// we kick off a fetch and store the result here for synchronous
    /// reads on subsequent frames.
    cached_models: HashMap<crate::model::SolutionSessionId, SharedString>,
    /// Sessions for which a model fetch is in-flight, used to dedupe
    /// the spawn so the status row doesn't fire a fresh request every
    /// time the agent emits a token-update event.
    pending_model_fetches: HashSet<crate::model::SolutionSessionId>,
    _store_subscription: Subscription,
    _solutions_subscription: Option<Subscription>,
}

/// Per-rename mutable state. The editor entity is owned here so the
/// inline editor's text persists across re-renders triggered by
/// unrelated store events (new entries arriving in another tab, etc.).
/// In-flight tab rename. `prior_selected_index` is captured when the
/// rename starts so we can restore the selection state on commit /
/// cancel — without it, force-selecting the renaming tab would leave
/// it permanently active even when the user was renaming an inactive
/// tab and never wanted to switch focus.
struct RenamingTab {
    id: crate::model::SolutionSessionId,
    editor: Entity<editor::Editor>,
    prior_selected_index: Option<usize>,
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
            renaming: None,
            cached_models: HashMap::default(),
            pending_model_fetches: HashSet::default(),
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
        // Iterate ALL worktrees, not just visible ones — an "empty"
        // solution attaches its `solution.root` as a hidden worktree
        // (so the project panel stays clean for the EmptySolutionPage)
        // but we still need to recognise the solution as active so
        // the agent navigator shows up.
        let worktrees = project.read(cx).worktrees(cx).collect::<Vec<_>>();
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
        // If the closed tab was being renamed, the rename slot points
        // at a session that no longer exists in this strip — drop it.
        if self
            .renaming
            .as_ref()
            .is_some_and(|r| r.id == session_id)
        {
            self.renaming = None;
        }
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

    /// Enter inline-rename mode for `id`. Spawns a single-line editor
    /// pre-filled with the current title and grabs focus so the user
    /// can immediately start typing. Idempotent — re-clicking the
    /// pencil while already renaming the same tab is a no-op.
    fn start_rename(
        &mut self,
        id: crate::model::SolutionSessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.renaming.as_ref().is_some_and(|r| r.id == id) {
            return;
        }
        let current_title = SolutionAgentStore::global(cx)
            .read_with(cx, |s, _| s.session(id))
            .map(|entity| entity.read(cx).title.to_string())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut e = editor::Editor::single_line(window, cx);
            e.set_text(current_title, window, cx);
            // Pre-select the whole title so a fresh keystroke
            // overwrites it (Chrome-rename behavior). Without
            // select_all the cursor lands at the end of the title and
            // typing appends instead.
            e.select_all(&editor::actions::SelectAll, window, cx);
            e
        });
        let focus = editor.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        // Force-select the renaming tab so it doesn't read as
        // `tab_inactive_background` (near-black in most themes) for the
        // duration of the edit. Capture the previous selection so we
        // can put it back when the rename ends — otherwise renaming an
        // inactive tab silently switches focus to it.
        let prior_selected_index = self.selected_index;
        if let Some(idx) = self.open_sessions.iter().position(|sid| *sid == id) {
            self.selected_index = Some(idx);
        }
        self.renaming = Some(RenamingTab {
            id,
            editor,
            prior_selected_index,
        });
        cx.notify();
    }

    /// Commit the in-progress rename. Empty / whitespace-only input
    /// is treated as Cancel — leaves the existing title alone instead
    /// of blanking the tab.
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.renaming.take() else {
            return;
        };
        let new_title = state.editor.read(cx).text(cx);
        let new_title = new_title.trim();
        if !new_title.is_empty() {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                let _ = store.rename_session(state.id, SharedString::from(new_title), cx);
            });
        }
        // Restore selection so renaming an inactive tab doesn't silently
        // become a "switch to this tab" gesture.
        self.selected_index = state.prior_selected_index;
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.renaming.take() {
            self.selected_index = state.prior_selected_index;
            cx.notify();
        }
    }

    fn create_and_open_session(
        &mut self,
        agent_id: AgentServerId,
        cwd: Option<std::path::PathBuf>,
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
        let pending_label_for_err = display_name.clone();
        self.pending.push(PendingCreation {
            id: pending_id,
            display_name,
            icon,
        });
        cx.notify();

        let task = store.update(cx, |store, cx| {
            store.create_session_with_cwd(solution_id, agent_id, project, cwd, cx)
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
                        let err_str = format!("{err:#}");
                        log::error!("create_session failed: {err:?}");
                        // Mirror the resume-side toast — without it the
                        // body lloader just disappears with no clue why.
                        let user_msg: SharedString = format!(
                            "Couldn't start a new {pending_label_for_err} session: {err_str}"
                        )
                        .into();
                        if let Some(workspace) = this.workspace.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                struct CreateFailedNotification;
                                workspace.show_notification(
                                    NotificationId::unique::<CreateFailedNotification>(),
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
        let solution_id = self.active_solution.clone()?;
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
        // Resolve the solution's root + member project paths/names. The
        // root entry is always offered so users who want a top-level
        // session don't have to pick a specific project.
        struct CwdChoice {
            label: SharedString,
            path: Option<std::path::PathBuf>,
        }
        let solutions_store = solutions::SolutionStore::try_global(cx);
        let cwd_choices: Vec<CwdChoice> = solutions_store
            .as_ref()
            .map(|store| {
                store.read_with(cx, |store, _| {
                    let mut choices = vec![CwdChoice {
                        label: "Solution root".into(),
                        path: None,
                    }];
                    if let Some(solution) =
                        store.solutions().iter().find(|s| s.id == solution_id)
                    {
                        for member in &solution.members {
                            let name = store
                                .catalog()
                                .iter()
                                .find(|c| c.id == member.catalog_id)
                                .map(|c| c.name.clone())
                                .unwrap_or_else(|| member.catalog_id.0.clone());
                            choices.push(CwdChoice {
                                label: name.into(),
                                path: Some(member.local_path.clone()),
                            });
                        }
                    }
                    choices
                })
            })
            .unwrap_or_else(|| {
                vec![CwdChoice {
                    label: "Solution root".into(),
                    path: None,
                }]
            });

        // The label stays adapter-agnostic on purpose — never hardcode a
        // specific neural network ("Claude", "Gemini", …) into the chrome.
        let label = SharedString::from("New Session");
        let trigger = Button::new("solution-sessions-new", label)
            .style(ButtonStyle::Subtle)
            .label_size(LabelSize::Default)
            .start_icon(
                Icon::new(IconName::Plus)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            );

        // When there's only one project root choice AND a single
        // adapter, skip the popover — one click creates the session.
        if cwd_choices.len() == 1 && adapters.len() == 1 {
            let (agent_id, _, _) = adapters.into_iter().next().expect("non-empty");
            return Some(
                trigger
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.create_and_open_session(agent_id.clone(), None, window, cx);
                    }))
                    .into_any_element(),
            );
        }

        let element = PopoverMenu::new("solution-sessions-new-popover")
            .trigger(trigger)
            .menu({
                let weak = cx.entity().downgrade();
                let adapters = adapters.clone();
                move |window, cx| {
                    let adapters = adapters.clone();
                    let weak = weak.clone();
                    let cwd_choices: Vec<(SharedString, Option<std::path::PathBuf>)> =
                        cwd_choices
                            .iter()
                            .map(|c| (c.label.clone(), c.path.clone()))
                            .collect();
                    Some(ContextMenu::build(
                        window,
                        cx,
                        move |mut menu, _window, _cx| {
                            // Pick the project root. Adapter selection is
                            // implicit: we pass the first registered
                            // adapter, since the fork ships with a single
                            // ACP adapter (Claude). If a future build
                            // registers more, we'd extend this with a
                            // submenu — but until then, an extra layer
                            // of clicks for an irrelevant choice is
                            // worse UX than this default.
                            let primary_agent =
                                adapters.first().map(|(id, _, _)| id.clone());
                            for (label, path) in cwd_choices {
                                let weak = weak.clone();
                                let agent = primary_agent.clone();
                                menu = menu.entry(label, None, move |window, cx| {
                                    let Some(this) = weak.upgrade() else {
                                        return;
                                    };
                                    let Some(agent) = agent.clone() else {
                                        return;
                                    };
                                    let path = path.clone();
                                    this.update(cx, move |this, cx| {
                                        this.create_and_open_session(
                                            agent, path, window, cx,
                                        );
                                    });
                                });
                            }
                            menu
                        },
                    ))
                }
            })
            .into_any_element();
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
            let session_entity = SolutionAgentStore::global(cx)
                .read_with(cx, |s, _| s.session(session_id_for_select));
            let title = session_entity
                .as_ref()
                .map(|entity| entity.read(cx).title.clone())
                .unwrap_or_else(|| SharedString::from(session_id_for_select.to_string()));
            let status = session_entity
                .as_ref()
                .map(|entity| session_status_indicator(&entity.read(cx).state))
                .unwrap_or(SessionStatusIndicator::Idle);
            let bg = if selected {
                cx.theme().colors().tab_active_background
            } else {
                cx.theme().colors().tab_inactive_background
            };
            let is_renaming = self
                .renaming
                .as_ref()
                .is_some_and(|r| r.id == session_id_for_select);
            // Per-tab hover group so the pencil shows only when the
            // user is mousing over THIS tab — keeps the strip clean
            // when scanning multiple sessions at once.
            let tab_group =
                SharedString::from(format!("tab-group-{session_id_for_select}"));
            let tab = div()
                .id(SharedString::from(format!("tab-{}", session_id_for_select)))
                .group(tab_group.clone())
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .bg(bg)
                .border_r_1()
                .border_color(cx.theme().colors().border_variant)
                .child(render_status_dot(status, cx));
            let tab = if is_renaming {
                // Inline edit mode: replace label with single-line
                // editor + checkmark / cancel. Click-through on the
                // tab body during rename would commit-by-accident, so
                // we don't attach the select handler in this branch.
                let editor = self
                    .renaming
                    .as_ref()
                    .map(|r| r.editor.clone())
                    .expect("is_renaming guarded by self.renaming.is_some()");
                tab.child(div().w(px(160.0)).child(editor))
                    .child(
                        IconButton::new(
                            SharedString::from(format!("rename-ok-{session_id_for_select}")),
                            IconName::Check,
                        )
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Success)
                        .tooltip(ui::Tooltip::text("Save (Enter)"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.commit_rename(cx);
                        })),
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("rename-cancel-{session_id_for_select}")),
                            IconName::Close,
                        )
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        .tooltip(ui::Tooltip::text("Cancel (Esc)"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_rename(cx);
                        })),
                    )
            } else {
                // Normal mode: label + pencil + close.
                let pencil_id =
                    SharedString::from(format!("rename-{session_id_for_select}"));
                tab.child(Label::new(title).size(LabelSize::Default))
                    .child(
                        IconButton::new(pencil_id, IconName::Pencil)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .tooltip(ui::Tooltip::text("Rename session"))
                            .visible_on_hover(tab_group.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_rename(session_id_for_select, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "close-{}",
                                session_id_for_select
                            )))
                            .px_1()
                            .child(Label::new("×").size(LabelSize::Default))
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
                    )
            };
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

    /// Clickable card for the empty-state "Recent sessions" list. Two-line
    /// layout: preview as the visual anchor (Default size, truncated), then
    /// "<time ago>  ·  <Ntok>" as a muted Small subline. Each card resumes
    /// its session on left-click via `resume_and_open`.
    fn render_history_card(
        &self,
        meta: SolutionSessionMetadata,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let primary = meta
            .preview
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(meta.title.as_ref())
            .to_string();
        let activity = relative_time_short(meta.last_activity_at, chrono::Utc::now());
        let mut subline = activity;
        if let Some(tokens) = meta.total_tokens {
            subline.push_str(&format!("  ·  {}", format_tokens(tokens)));
        }
        let id = SharedString::from(format!("history-card-{}", meta.id));
        let meta_for_action = meta;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().elevated_surface_background)
            .hover(|s| s.bg(cx.theme().colors().element_hover))
            .cursor_pointer()
            .child(
                Icon::new(IconName::HistoryRerun)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(Label::new(primary).size(LabelSize::Default).truncate())
                    .child(
                        Label::new(subline)
                            .color(Color::Muted)
                            .size(LabelSize::Small),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let meta = meta_for_action.clone();
                    this.resume_and_open(meta, window, cx);
                }),
            )
    }

    /// Resolves the agent's currently-selected model name asynchronously
    /// and stores it in `cached_models`. The status row reads this cache
    /// on subsequent renders. We dedupe in-flight fetches via
    /// `pending_model_fetches` so the row doesn't fire a fresh request
    /// every frame.
    fn ensure_model_loaded(
        &mut self,
        session_id: crate::model::SolutionSessionId,
        cx: &mut Context<Self>,
    ) {
        if self.cached_models.contains_key(&session_id)
            || self.pending_model_fetches.contains(&session_id)
        {
            return;
        }
        let store = SolutionAgentStore::global(cx);
        let Some(thread) = store
            .read(cx)
            .session(session_id)
            .and_then(|s| s.read(cx).acp_thread.clone())
        else {
            return;
        };
        let acp_session_id = thread.read(cx).session_id().clone();
        let connection = thread.read(cx).connection().clone();
        let Some(selector) = connection.model_selector(&acp_session_id) else {
            return;
        };
        let task = selector.selected_model(cx);
        self.pending_model_fetches.insert(session_id);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.pending_model_fetches.remove(&session_id);
                if let Ok(info) = result {
                    this.cached_models.insert(session_id, info.name);
                    cx.notify();
                }
            })
            .log_err();
        })
        .detach();
    }

    fn render_status_row(
        &mut self,
        active_view: Option<&Entity<SolutionSessionView>>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let session_id = self.selected_index.and_then(|i| self.open_sessions.get(i).copied())?;
        let session = active_view.and_then(|v| {
            let _ = v;
            SolutionAgentStore::global(cx).read_with(cx, |s, _| s.session(session_id))
        })?;
        let s = session.read(cx);
        let agent_id = s.agent_id.clone();
        let state_text = SharedString::from(s.state.short_label());
        let is_idle = matches!(s.state, SessionState::Idle);
        let usage = s
            .acp_thread
            .as_ref()
            .and_then(|thread| thread.read(cx).token_usage().cloned());
        // Synchronous read of the agent's current session mode
        // ("default", "plan", …). Claude exposes this via ACP — when
        // the connection doesn't implement modes (e.g. mock test
        // adapter) we just hide the segment.
        let mode_text: Option<SharedString> = s.acp_thread.as_ref().and_then(|thread| {
            let thread = thread.read(cx);
            let modes = thread.connection().session_modes(thread.session_id(), cx)?;
            let current = modes.current_mode();
            modes
                .all_modes()
                .into_iter()
                .find(|m| m.id == current)
                .map(|m| SharedString::from(m.name))
                .or_else(|| Some(SharedString::from(current.0.to_string())))
        });
        let _ = s;
        // Kick off a model lookup if we don't have one cached yet.
        // Stored in `cached_models` for synchronous reads on later
        // frames; the spawn de-dupes via `pending_model_fetches`.
        self.ensure_model_loaded(session_id, cx);
        let model_text = self.cached_models.get(&session_id).cloned();

        let used = usage.as_ref().map(|u| u.used_tokens).unwrap_or(0);
        // claude-acp doesn't always populate `max_tokens` (it's gated by an
        // upstream beta flag). Fall back to the Claude Opus 4 context
        // window so the meter and the compact button stay meaningful.
        let max = usage
            .as_ref()
            .map(|u| u.max_tokens)
            .filter(|m| *m > 0)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let pct = if max == 0 {
            0.0
        } else {
            (used as f64 / max as f64).clamp(0.0, 1.0)
        };
        let meter_text = SharedString::from(format!(
            "{} / {} · {:.1}%",
            format_tokens_compact(used),
            format_tokens_compact(max),
            pct * 100.0
        ));
        let bar_color = if pct >= 0.8 {
            cx.theme().status().error
        } else if pct >= 0.5 {
            cx.theme().status().warning
        } else {
            cx.theme().colors().text_accent
        };

        // The compact prompt + the agent's dump need real headroom (~3k
        // for the prompt, ~10–20k for state.md / decisions.md / next.md
        // / continue.md combined). A percentage gate misbehaves across
        // model sizes — 10 % of a 200 k window is only 20 k tokens
        // (tight) while 10 % of a 1 M window is 100 k (more than
        // enough). Tie the disable threshold to absolute remaining
        // tokens instead so the button stays usable on long-context
        // models even past 90 %.
        let remaining = max.saturating_sub(used);
        let too_full = remaining < COMPACT_HEADROOM_MIN_TOKENS;
        let compact_enabled = is_idle && pct >= COMPACT_BUTTON_MIN_PCT && !too_full;
        let compact_warning = pct >= COMPACT_BUTTON_WARN_PCT && !too_full;
        let compact_tooltip: SharedString = if !is_idle {
            "Wait for the current turn to finish before compacting".into()
        } else if too_full {
            format!(
                "Only {} of headroom left — start a fresh session manually",
                format_tokens(remaining)
            )
            .into()
        } else if !compact_enabled {
            "Conversation is short — compact later".into()
        } else if compact_warning {
            "Context is filling up — compact recommended".into()
        } else {
            "Compact context: agent dumps a summary, then a fresh session continues".into()
        };

        let compact_button = {
            // `Archive` reads as "stash the current conversation away
            // and start a fresh context" — a much closer fit for the
            // compact action than `Sparkle`, which carries an
            // AI/magic connotation we don't want here.
            let mut btn = IconButton::new("solution-status-compact", IconName::Archive)
                .icon_size(IconSize::Small)
                .icon_color(if compact_warning {
                    Color::Warning
                } else {
                    Color::Muted
                })
                .tooltip(ui::Tooltip::text(compact_tooltip));
            if compact_enabled {
                btn = btn.on_click(cx.listener(move |this, _, _, cx| {
                    this.start_compact(session_id, cx);
                }));
            } else {
                btn = btn.disabled(true);
            }
            btn.into_any_element()
        };

        // Token meter sits on the LEFT so the user's eye doesn't have
        // to chase across the whole status row to read it. Width is
        // pinned (`flex_none` on each piece) so a state transition
        // ("Idle" → "Awaiting input" — different chars) re-flows the
        // *right-hand* tail of the row but never nudges the meter
        // sideways. The visual "% used" anchor stays put as the
        // conversation breathes.
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h_7()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    div()
                        .flex_none()
                        .child(
                            Label::new(meter_text)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(72.0))
                        .h(px(4.0))
                        .rounded_full()
                        .bg(cx.theme().colors().border)
                        .child(
                            div()
                                .h_full()
                                .w(relative((pct as f32).clamp(0.0, 1.0)))
                                .rounded_full()
                                .bg(bar_color),
                        ),
                )
                .child(div().flex_none().child(compact_button))
                .child(
                    Label::new(agent_id)
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .when_some(model_text, |this, model| {
                    this.child(Label::new("·").color(Color::Muted).size(LabelSize::Small))
                        .child(
                            Label::new(model)
                                .color(Color::Muted)
                                .size(LabelSize::Small),
                        )
                })
                .when_some(mode_text, |this, mode| {
                    this.child(Label::new("·").color(Color::Muted).size(LabelSize::Small))
                        .child(Label::new(mode).color(Color::Muted).size(LabelSize::Small))
                })
                .child(Label::new("·").color(Color::Muted).size(LabelSize::Small))
                .child(Label::new(state_text).size(LabelSize::Small))
                .into_any_element(),
        )
    }

    /// Renders the current compact-instruction template, creates the
    /// per-rotation handoff directory, and ships the rendered prompt as
    /// a regular user message. The agent then writes its summary files
    /// into that directory and (after we've handed it `compact_dir`)
    /// calls back via `solution_agent.compact_session`.
    fn start_compact(&self, session_id: crate::model::SolutionSessionId, cx: &mut Context<Self>) {
        let store = SolutionAgentStore::global(cx);
        let Some(session_entity) = store.read_with(cx, |s, _| s.session(session_id)) else {
            return;
        };
        let s = session_entity.read(cx);
        if !matches!(s.state, SessionState::Idle) {
            return;
        }
        let solution_id = s.solution_id.clone();
        let agent_id = s.agent_id.clone();
        let started_at = s.created_at;
        // Snapshot the count *before* rotation: the dump dir captures
        // the context being closed (`c01` for the first compact, `c02`
        // for the second, …). After the agent finishes writing files
        // and `compact_session` runs, the session's context_count
        // increments to count + 1 for the next round.
        let context_count = s.context_count;
        let usage = s
            .acp_thread
            .as_ref()
            .and_then(|thread| thread.read(cx).token_usage().cloned());
        let used = usage.as_ref().map(|u| u.used_tokens).unwrap_or(0);
        let max = usage
            .as_ref()
            .map(|u| u.max_tokens)
            .filter(|m| *m > 0)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let _ = s;

        let solution_root = match SolutionStore::try_global(cx).and_then(|store| {
            store.read_with(cx, |s, _| {
                s.solutions()
                    .iter()
                    .find(|sol| sol.id == solution_id)
                    .map(|sol| sol.root.clone())
            })
        }) {
            Some(root) => root,
            None => {
                self.toast_error(
                    SharedString::from(format!(
                        "Compact failed: solution {:?} not registered",
                        solution_id.0
                    )),
                    cx,
                );
                return;
            }
        };

        // `<root>/.agents/<sid>/c<count>/` — `c01`, `c02`, … so a
        // single `<sid>` directory groups every rotation of one
        // logical conversation. The leading `c` keeps the names from
        // accidentally colliding with the legacy timestamp scheme.
        let context_label = format!("c{context_count:02}");
        let compact_dir = solution_root
            .join(".agents")
            .join(session_id.to_string())
            .join(&context_label);
        if let Err(err) = std::fs::create_dir_all(&compact_dir) {
            self.toast_error(
                SharedString::from(format!(
                    "Compact failed: cannot create {}: {err}",
                    compact_dir.display()
                )),
                cx,
            );
            return;
        }

        let mut compact_dir_str = compact_dir.to_string_lossy().to_string();
        if !compact_dir_str.ends_with(std::path::MAIN_SEPARATOR) {
            compact_dir_str.push(std::path::MAIN_SEPARATOR);
        }

        let rendered = COMPACT_INSTRUCTIONS_TEMPLATE
            .replace("{{session_id}}", &session_id.to_string())
            .replace("{{compact_dir}}", &compact_dir_str)
            .replace("{{solution_id}}", solution_id.0.as_str())
            .replace("{{agent_id}}", agent_id.as_ref())
            .replace("{{started_at_iso}}", &started_at.to_rfc3339())
            .replace("{{tokens_used}}", &used.to_string())
            .replace("{{tokens_max}}", &max.to_string());

        store.update(cx, |store, cx| {
            store.send_message(session_id, rendered, cx).detach_and_log_err(cx);
        });
    }

    fn toast_error(&self, message: SharedString, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            log::warn!("solution_agent toast (no workspace): {message}");
            return;
        };
        workspace.update(cx, |workspace, cx| {
            struct CompactFailed;
            workspace.show_notification(
                NotificationId::unique::<CompactFailed>(),
                cx,
                move |cx| cx.new(|cx| MessageNotification::new(message, cx)),
            );
        });
    }
}

/// Hardcoded fallback when claude-acp doesn't advertise the model's
/// context-window size (the field is gated by an upstream beta flag).
/// 1M matches Claude Opus 4 with the long-context flag enabled, which
/// is the default for this fork.
const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;

/// Compact button activation threshold. Below this the conversation is
/// too short for a compact to be worth the round-trip.
const COMPACT_BUTTON_MIN_PCT: f64 = 0.20;

/// Threshold at which the compact button paints in warning colour.
/// Past this, the user should rotate before the model starts dropping
/// context off the back of the window.
const COMPACT_BUTTON_WARN_PCT: f64 = 0.50;

/// Minimum free tokens we require before allowing a compact: enough
/// for the instruction prompt (~3 k) and the agent's dump (state.md +
/// decisions.md + next.md + continue.md, typically ~10–20 k combined),
/// plus a buffer for tool-call traces. Below this, refuse the button —
/// a half-truncated compact loses more than just starting over does.
const COMPACT_HEADROOM_MIN_TOKENS: u64 = 30_000;

/// Markdown template fed to the agent on compact. `{{var}}` placeholders
/// are filled from session state at click time. Source-of-truth lives in
/// the resources file so the prose can be reviewed without recompiling.
const COMPACT_INSTRUCTIONS_TEMPLATE: &str =
    include_str!("../resources/compact_context_instructions.md");

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
        } else if let Some(pending) = self.pending.first() {
            // The strip already shows a spinner-tab for in-flight starts,
            // but at 12px tall it's easy to miss — especially on resume,
            // where the user clicked a big body card and expects the body
            // to react. Mirror the same icon + label centred in the panel
            // body so the click gets unambiguous feedback.
            let label = SharedString::from(format!("Starting {}…", pending.display_name));
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .items_center()
                .justify_center()
                .px_4()
                .child(
                    Icon::new(IconName::ArrowCircle)
                        .size(IconSize::Medium)
                        .color(Color::Muted)
                        .with_rotate_animation(2),
                )
                .child(Label::new(label).size(LabelSize::Default))
                .child(
                    Label::new("Reattaching to the agent — this can take a few seconds.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .into_any_element()
        } else {
            // No tab open. Offer up to 3 recent sessions as clickable cards
            // when the DB has anything persisted for this solution; falls
            // back to a plain hint pointing at the "+" button otherwise.
            // Three cards (vs the previous single CTA) lets users land
            // directly on the right session when they alternate between a
            // few parallel threads — common with Claude where you keep one
            // session per coarse task.
            let last_metas: Vec<_> =
                self.historic_sessions.iter().take(3).cloned().collect();
            let mut empty = div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .items_center()
                .justify_center()
                .px_4();
            if !last_metas.is_empty() {
                let heading = if last_metas.len() == 1 {
                    "Recent session"
                } else {
                    "Recent sessions"
                };
                empty = empty.child(Label::new(heading).size(LabelSize::Large));
                let mut list = div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w_full()
                    .max_w(px(440.0));
                for meta in last_metas {
                    list = list.child(self.render_history_card(meta, cx));
                }
                empty = empty.child(list).child(
                    Label::new("Or start a fresh one with + above.")
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                );
            } else {
                empty = empty.child(
                    Label::new("No session selected. Click + above to start one.")
                        .color(Color::Muted)
                        .size(LabelSize::Default),
                );
            }
            empty.into_any_element()
        };
        let mut root = div()
            .key_context("SolutionSessionsNavigator")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            // Inline tab-rename uses the standard menu actions: Enter
            // to commit, Esc to cancel. We listen on the panel root
            // (not the tab strip) so the actions still reach us when
            // the rename editor swallows focus.
            .on_action(cx.listener(|this, _: &menu::Confirm, _, cx| {
                if this.renaming.is_some() {
                    this.commit_rename(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &menu::Cancel, _, cx| {
                if this.renaming.is_some() {
                    this.cancel_rename(cx);
                }
            }));
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

/// Short token count, "12.3k" / "456", with no unit suffix. Used in the
/// status row where the magnitudes of the two operands ("used / max")
/// already make their meaning unambiguous.
fn format_tokens_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
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

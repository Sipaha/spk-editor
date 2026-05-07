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
use gpui::{
    Animation, AnimationExt, App, AppContext as _, Context, DismissEvent, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, WeakEntity, Window, anchored, deferred, div, pulsating_between, px,
};
use solutions::{SolutionId, SolutionStore, SolutionStoreEvent};
use ui::prelude::*;
use ui::{
    CommonAnimationExt, ContextMenu, Icon, IconButtonShape, IconName, Label, LabelSize,
    PopoverMenu,
};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

use crate::actions::FocusNavigator;
use crate::model::{AgentServerId, SessionState, SolutionSession, SolutionSessionMetadata};
use crate::rename_session_modal::RenameSessionModal;
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

/// Drag-source payload for tab reordering inside the strip. Carries the
/// origin index so the drop handler can call `apply_reorder` directly,
/// and the title for the ghost preview that follows the cursor.
#[derive(Clone)]
struct DraggedSolutionTab {
    from_index: usize,
    title: SharedString,
}

impl Render for DraggedSolutionTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Compact ghost preview matching the inactive tab visuals so the
        // dragged thing reads as "this tab being moved" — same recipe
        // upstream uses for `impl Render for DraggedTab`.
        div()
            .flex()
            .items_center()
            .px_2()
            .h_8()
            .min_w(px(120.0))
            .max_w(px(220.0))
            .bg(cx.theme().colors().tab_inactive_background)
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                Label::new(self.title.clone())
                    .size(LabelSize::Default)
                    .truncate(),
            )
    }
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
    // No `pr_1()` here — `gap_1p5` on the parent tab body already
    // separates the dot from the label. Stacking `pr_1` on top
    // produced an asymmetric 10px gap (4 + 6) that read as
    // "the label is misaligned" against the symmetrical
    // 6px gap the other tab elements use.
    div()
        .id(SharedString::from(format!(
            "solution-tab-status-{:?}",
            status
        )))
        .flex_none()
        .flex()
        .items_center()
        .tooltip(ui::Tooltip::text(tooltip))
        .child(dot)
        .into_any_element()
}

pub struct SolutionSessionsNavigator {
    pub(crate) workspace: WeakEntity<Workspace>,
    project: WeakEntity<project::Project>,
    focus_handle: FocusHandle,
    width: gpui::Pixels,
    pub(crate) active_solution: Option<SolutionId>,
    /// Sessions opened in this panel, in tab order.
    pub(crate) open_sessions: Vec<crate::model::SolutionSessionId>,
    /// Index into `open_sessions` of the visible session.
    pub(crate) selected_index: Option<usize>,
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
    pub(crate) historic_sessions: Vec<SolutionSessionMetadata>,
    /// Tab right-click context menu. `Some` while the menu is open;
    /// stores the click position so the menu renders anchored at the
    /// cursor, plus a `DismissEvent` subscription that clears the slot
    /// when the user dismisses the menu (click outside / Esc / item
    /// activation). The previous inline pencil + × icons that lived in
    /// the tab body itself were noisy on multi-tab strips and were
    /// replaced by this menu — it's the only way to rename or close a
    /// tab from the strip now.
    tab_context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    /// Per-session model name cache for the status row. Filled lazily
    /// on the first render that asks for a session's model — the ACP
    /// `selected_model` accessor is async (round-trip to the agent), so
    /// we kick off a fetch and store the result here for synchronous
    /// reads on subsequent frames.
    pub(crate) cached_models: HashMap<crate::model::SolutionSessionId, SharedString>,
    /// Sessions for which a model fetch is in-flight, used to dedupe
    /// the spawn so the status row doesn't fire a fresh request every
    /// time the agent emits a token-update event.
    pub(crate) pending_model_fetches: HashSet<crate::model::SolutionSessionId>,
    /// 1-second tick that re-renders the status row so the
    /// "Thinking… Ns" elapsed counter advances even when no
    /// AcpThreadEvents fire (long pauses between tool calls etc.).
    /// Kicked off by `render_status_row` when it observes the active
    /// session in `Running` state; dropped (and so cancelled) when the
    /// next render observes the session is no longer running.
    pub(crate) thinking_tick: Option<gpui::Task<()>>,
    /// In-flight `restore_open_tabs` task for the current solution. Held
    /// so we can ignore reconcile-from-store while restoration is mid-
    /// flight (otherwise the cold-session inserts the restore task
    /// performs would reach `reconcile_open_sessions_with_store`
    /// out-of-order — the cold sessions get a `created_at` from their
    /// DB metadata, which doesn't necessarily match `tab_order` so the
    /// strip would land in `created_at` order instead of the user's
    /// preserved drag-drop order). Cleared by the task itself when it
    /// finishes applying the ordered ids.
    pending_restore: Option<gpui::Task<()>>,
    _store_subscription: Subscription,
    _solutions_subscription: Option<Subscription>,
}

impl SolutionSessionsNavigator {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        project: WeakEntity<project::Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SolutionAgentStore::global(cx);
        // Keep the historic-sessions snapshot in sync with the DB —
        // create/close persists rows, so the history popover would otherwise
        // get stale until the user switched solutions. Also reconcile the
        // tab strip: sessions can land in the store via MCP / cross-window
        // create / etc. without going through this panel's own create-tab
        // path, and the dock-panel badge would drift out of sync with the
        // visible tabs.
        let store_subscription = cx.subscribe_in(&store, window, |this, _, _, window, cx| {
            this.refresh_historic_sessions(cx);
            this.reconcile_open_sessions_with_store(window, cx);
            cx.notify();
        });
        let solutions_subscription = SolutionStore::try_global(cx).map(|sol_store| {
            cx.subscribe_in(
                &sol_store,
                window,
                |this, _, _: &SolutionStoreEvent, window, cx| {
                    this.refresh_active_solution(window, cx);
                },
            )
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
            tab_context_menu: None,
            cached_models: HashMap::default(),
            pending_model_fetches: HashSet::default(),
            thinking_tick: None,
            pending_restore: None,
            _store_subscription: store_subscription,
            _solutions_subscription: solutions_subscription,
        };
        this.refresh_active_solution(window, cx);
        this
    }

    pub fn refresh_active_solution(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_id = self.derive_active_solution(cx);
        if new_id != self.active_solution {
            self.active_solution = new_id.clone();
            // Different solution → wipe panel-local tabs. Sessions themselves
            // stay alive in the store so they reappear when the user comes
            // back to that solution. Drop any in-flight pending creations
            // too — they were started against the previous solution.
            self.open_sessions.clear();
            self.selected_index = None;
            self.views.clear();
            self.pending.clear();
            self.historic_sessions.clear();
            // Cancel any restore that was running for the previous
            // solution — its update closure would no-op via the
            // active_solution guard, but dropping the task is cheaper.
            self.pending_restore = None;
            if let Some(sid) = new_id {
                self.kick_off_restore(sid, window, cx);
            }
            cx.notify();
        }
        // Always refresh DB metadata when called — sessions get persisted
        // mid-conversation, so the "history" list needs to update on every
        // store event, not just on solution changes.
        self.refresh_historic_sessions(cx);
        // Repopulate panel tabs from the store so the dock-panel "✨N"
        // badge stays in sync with the visible tab strip across solution
        // switches and after sessions land in the store via other code
        // paths (MCP, cross-window create). Skipped while a restore is
        // mid-flight: the restore task is the source of truth for the
        // initial strip ordering, and reconcile would otherwise insert
        // the cold sessions in `created_at` order.
        if self.pending_restore.is_none() {
            self.reconcile_open_sessions_with_store(window, cx);
        }
    }

    fn kick_off_restore(
        &mut self,
        solution_id: SolutionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = SolutionAgentStore::global(cx);
        let task = store.update(cx, |store, cx| {
            store.restore_open_tabs(solution_id.clone(), cx)
        });
        let restore_task = cx.spawn_in(window, async move |this, cx| {
            let ordered_ids = match task.await {
                Ok(ids) => ids,
                Err(err) => {
                    log::warn!("restore_open_tabs failed for {solution_id:?}: {err:?}");
                    let _ = this.update(cx, |this, _| this.pending_restore = None);
                    return;
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.active_solution.as_ref() != Some(&solution_id) {
                    this.pending_restore = None;
                    return;
                }
                let store = SolutionAgentStore::global(cx);
                for id in &ordered_ids {
                    if this.open_sessions.contains(id) {
                        continue;
                    }
                    let Some(session) = store.read(cx).session(*id) else {
                        continue;
                    };
                    this.open_session(*id, session, window, cx);
                }
                // Pick the most recently active restored tab as
                // selected — matches the user's mental model of
                // "where was I." Falls through to whatever
                // open_session set if there are no restored tabs.
                if !ordered_ids.is_empty() {
                    let restored: HashSet<crate::model::SolutionSessionId> =
                        ordered_ids.iter().copied().collect();
                    let mut best: Option<(usize, chrono::DateTime<chrono::Utc>)> = None;
                    for (idx, id) in this.open_sessions.iter().enumerate() {
                        if !restored.contains(id) {
                            continue;
                        }
                        if let Some(session) = store.read(cx).session(*id) {
                            let activity = session.read(cx).last_activity_at;
                            if best.map(|(_, ts)| activity > ts).unwrap_or(true) {
                                best = Some((idx, activity));
                            }
                        }
                    }
                    if let Some((idx, _)) = best {
                        this.selected_index = Some(idx);
                    }
                }
                this.pending_restore = None;
                // Now that the restore-supplied ordering is in place,
                // pick up any extra sessions that landed in the store
                // mid-restore (e.g. MCP create_session in another
                // window). They append after the restored block.
                this.reconcile_open_sessions_with_store(window, cx);
                cx.notify();
            });
        });
        self.pending_restore = Some(restore_task);
    }

    /// Walk the store's live sessions for `active_solution` and add any
    /// that aren't already in this panel's tab strip. Keeps the dock-
    /// panel badge ("✨N") in sync with the visible tabs — without this,
    /// sessions created via MCP, opened in another window, or carried
    /// over from a previous tab-strip clear (solution-switch round-trip)
    /// inflate the badge silently.
    fn reconcile_open_sessions_with_store(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(solution_id) = self.active_solution.clone() else {
            return;
        };
        let store = SolutionAgentStore::global(cx);
        let mut live: Vec<(crate::model::SolutionSessionId, Entity<SolutionSession>)> = store
            .read(cx)
            .sessions_for(&solution_id)
            .into_iter()
            .map(|entity| (entity.read(cx).id, entity))
            .collect();
        // Stable ordering by created_at so newly-attached tabs land after
        // older ones rather than jittering on every store event.
        live.sort_by_key(|(_, entity)| entity.read(cx).created_at);
        for (session_id, entity) in live {
            if !self.open_sessions.contains(&session_id) {
                self.open_session(session_id, entity, window, cx);
            }
        }
    }

    pub(crate) fn refresh_historic_sessions(&mut self, cx: &mut Context<Self>) {
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
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
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
            let id = store.read_with(cx, |s, _| {
                s.solution_for_path(&path).map(|sol| sol.id.clone())
            });
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
        let navigator = cx.entity().downgrade();
        let view = cx.new(|cx| {
            SolutionSessionView::new(
                session_id,
                session,
                self.workspace.clone(),
                navigator,
                window,
                cx,
            )
        });
        self.open_sessions.push(session_id);
        self.selected_index = Some(self.open_sessions.len() - 1);
        self.views.insert(session_id, view);
        self.persist_open_sessions(cx);
        cx.notify();
    }

    /// Move tab `from` to position `to` in the strip. Used by the
    /// drag-drop handler in `render_tab_strip`. Index arithmetic and
    /// `selected_index` adjustment live in the pure `apply_reorder`
    /// helper so the math is unit-testable without GPUI.
    fn reorder_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let before = self.open_sessions.clone();
        apply_reorder(&mut self.open_sessions, &mut self.selected_index, from, to);
        if self.open_sessions != before {
            self.persist_open_sessions(cx);
            cx.notify();
        }
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
        // Also release the store-side session so the dock-panel "✨N
        // sessions" badge counts down — without this the agent stays
        // alive in the pool and the badge keeps showing the closed tab.
        // Soft-close keeps the row + transcript in the DB so the user
        // can still resume from History.
        SolutionAgentStore::global(cx).update(cx, |store, cx| {
            if let Err(err) = store.close_session(session_id, cx) {
                log::warn!("close_tab: store.close_session({session_id}) failed: {err:?}");
            }
        });
        self.persist_open_sessions(cx);
        cx.notify();
    }

    /// Push the current `open_sessions` order to the store so it's
    /// written to `solution_sessions.tab_order`. No-op when no solution
    /// is active. Sessions removed from the strip (closed) become
    /// `tab_order = NULL` on the next call, since `update_tab_orders`
    /// clears anything not in the slice.
    ///
    /// Suppressed while a restore is in flight: `kick_off_restore`
    /// calls `open_session` once per restored id and we'd otherwise
    /// fire N redundant `update_tab_orders` writes whose final state
    /// is the order we just read FROM the DB. The single post-restore
    /// `reconcile_open_sessions_with_store` is enough to capture any
    /// ordering changes from sessions that landed mid-restore.
    fn persist_open_sessions(&self, cx: &mut Context<Self>) {
        if self.pending_restore.is_some() {
            return;
        }
        let Some(solution_id) = self.active_solution.clone() else {
            return;
        };
        let order = self.open_sessions.clone();
        SolutionAgentStore::global(cx).update(cx, |store, cx| {
            store.persist_tab_order(solution_id, order, cx);
        });
    }

    /// Open the rename popup for `id`. Pre-fills the input with the
    /// current title; on confirm the modal calls
    /// `SolutionAgentStore::rename_session` directly. Replaces the
    /// previous in-tab inline editor so the strip stays compact and
    /// keyboard interactions don't have to cross the focus boundary
    /// between strip and editor.
    fn open_rename_modal(
        &mut self,
        id: crate::model::SolutionSessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let current_title = SolutionAgentStore::global(cx)
            .read_with(cx, |s, _| s.session(id))
            .map(|entity| entity.read(cx).title.to_string())
            .unwrap_or_default();
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                RenameSessionModal::new(id, current_title, window, cx)
            });
        });
    }

    /// Build and show the right-click context menu for tab `idx` at the
    /// click position. The menu currently exposes Rename and Close — the
    /// only two destructive/edit actions for a tab. Pinned by
    /// `self.tab_context_menu` so the panel render-side `deferred(...)`
    /// overlay can position it; the slot self-clears when the menu
    /// emits `DismissEvent`.
    fn deploy_tab_context_menu(
        &mut self,
        idx: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.open_sessions.get(idx).copied() else {
            return;
        };
        let weak = cx.weak_entity();
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            let weak_rename = weak.clone();
            let weak_close = weak.clone();
            menu.entry("Rename...", None, move |window, cx| {
                if let Some(this) = weak_rename.upgrade() {
                    this.update(cx, |this, cx| {
                        this.open_rename_modal(session_id, window, cx)
                    });
                }
            })
            .entry("Close", None, move |_, cx| {
                if let Some(this) = weak_close.upgrade() {
                    this.update(cx, |this, cx| this.close_tab(idx, cx));
                }
            })
        });
        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.tab_context_menu.take();
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.tab_context_menu = Some((menu, position, subscription));
        cx.notify();
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
                                    move |cx| cx.new(|cx| MessageNotification::new(user_msg, cx)),
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
    pub(crate) fn resume_and_open(
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

        let session_title = meta.title.clone();
        let task = store.update(cx, |store, cx| store.resume_session(meta, project, cx));
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
                        // (-32002) when its in-process registry has no
                        // record of this session id. That can happen for
                        // genuinely-empty sessions (claude flushes to
                        // ~/.claude/projects only after the first turn),
                        // but it also fires transiently when the agent
                        // process restarts or its storage hasn't loaded
                        // yet — and we cannot tell those cases apart
                        // from this side. We used to silently delete the
                        // DB row, which destroyed user state on a hit-
                        // your-limit + restart cycle. Now we leave the
                        // history row in place so the user can retry
                        // (and explicitly delete via the History
                        // popover's trash icon if they really want to
                        // discard it).
                        let resource_gone = err_str.contains("Resource not found")
                            || err_str.contains("-32002");
                        let user_msg: SharedString = if resource_gone {
                            format!(
                                "\"{session_title}\" can't be resumed right now — the agent has no record of this session. Try again in a moment, or delete it from History if it's permanently broken."
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
                    if let Some(solution) = store.solutions().iter().find(|s| s.id == solution_id) {
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

        // `Wide` + `ButtonSize::Large` because `Square` auto-sizes from
        // `IconSize` and `IconSize::Custom` has a known padding bug that
        // produces a 72px square for a 24px icon. Wide + Large gives a
        // deterministic 32px-tall hit area; the natural Base08 horizontal
        // padding around the 24px glyph lands at ~40px width.
        let trigger = ui::IconButton::new("solution-sessions-new", IconName::Plus)
            .shape(IconButtonShape::Wide)
            .size(ButtonSize::Large)
            .icon_size(IconSize::Custom(rems_from_px(20.)))
            .icon_color(Color::Muted)
            .tooltip(ui::Tooltip::text("New session"));

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
                move |window, cx| {
                    let adapters = adapters.clone();
                    let weak = weak.clone();
                    let cwd_choices: Vec<(SharedString, Option<std::path::PathBuf>)> = cwd_choices
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
                            let primary_agent = adapters.first().map(|(id, _, _)| id.clone());
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
                                        this.create_and_open_session(agent, path, window, cx);
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
        // Strip height bumped from `h_8` (32 px) to `h_9` (36 px) and
        // each tab below sets `h_full()` so the entire strip-height
        // column is clickable (without `h_full` the tab body sized
        // itself to its label and `items_center` left dead vertical
        // strips above + below). Net effect: bigger hit-target that
        // matches the upstream editor tab strip's perceived size.
        let mut strip = div()
            .id("solution-sessions-tab-strip")
            .flex()
            .flex_none()
            .items_stretch()
            .h_9()
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
            // Compact tab body: status dot + label only. Rename / Close
            // moved to the right-click context menu (see
            // `deploy_tab_context_menu`) — the prior pencil + × icons
            // accumulated visual noise on multi-session strips.
            //
            // `min_w(120)` keeps short titles like "ROOT" or "ecos-records
            // 2" from collapsing into a sliver — without it the tab body
            // shrunk to barely fit the dot + label, hard to click and
            // visually noisy on a strip of mixed lengths. `max_w(220)`
            // caps very long titles so they don't push the rest of the
            // strip off-screen; the label inside truncates with an
            // ellipsis past that point.
            let drag_payload = DraggedSolutionTab {
                from_index: idx,
                title: title.clone(),
            };
            let tab = div()
                .id(SharedString::from(format!("tab-{}", session_id_for_select)))
                .flex()
                .flex_none()
                .items_center()
                .h_full()
                .gap_1p5()
                .px_3()
                .min_w(px(140.0))
                .max_w(px(220.0))
                .bg(bg)
                .border_r_1()
                .border_color(cx.theme().colors().border_variant)
                .child(render_status_dot(status, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .h_full()
                        .child(
                            Label::new(title)
                                .size(LabelSize::Default)
                                .line_height_style(LineHeightStyle::UiLabel)
                                .truncate(),
                        ),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_index = Some(idx);
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| {
                        this.deploy_tab_context_menu(idx, ev.position, window, cx);
                    }),
                )
                .on_drag(drag_payload, |payload, _, _, cx| {
                    cx.new(|_| payload.clone())
                })
                .drag_over::<DraggedSolutionTab>(|tab, _, _, cx| {
                    tab.bg(cx.theme().colors().drop_target_background)
                })
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedSolutionTab, _, cx| {
                        this.reorder_tab(dragged.from_index, idx, cx);
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
                    .h_full()
                    .gap_1p5()
                    .px_3()
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
                            .line_height_style(LineHeightStyle::UiLabel)
                            .color(Color::Muted),
                    ),
            );
        }
        // Right-side controls (`+` new-session, history popover)
        // share a single `h_flex` row with `items_center` + `gap_1`
        // so they sit on the same baseline as a unit and the gap
        // between them is symmetric. Earlier shape had each button
        // in its own `h_full` wrapper with separate `px_2`/`pr_2`
        // padding — that produced a visually uneven step between
        // the last tab, the `+`, and the history clock and made
        // the icons look like they were drifting vertically against
        // the tab labels.
        let new_btn = self.render_new_session_button(cx);
        let history_btn = self.render_history_button(cx);
        if new_btn.is_some() || history_btn.is_some() {
            strip = strip.child(
                h_flex()
                    .flex_none()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .pr_2()
                    .when_some(new_btn, |this, btn| this.child(btn))
                    .when_some(history_btn, |this, btn| this.child(btn)),
            );
        }
        strip
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
        } else if let Some(view) = active_view {
            // `overflow_hidden` clips the session view's content to
            // its flex allocation. Without it, an unusually tall
            // entry inside the conversation list (e.g. a multi-screen
            // assistant message containing a stack trace) overflows
            // past the wrapper despite `flex_1 + min_h_0` — those
            // tell the *flex layout* not to inflate the box, but they
            // don't clip painting. The visual symptom is the message
            // bubble bleeding up onto the tab strip and down onto the
            // status row / compose box.
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(view)
                .into_any_element()
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
            let last_metas: Vec<_> = self.historic_sessions.iter().take(3).cloned().collect();
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
                let mut list = div().flex().flex_col().gap_2().w_full().max_w(px(440.0));
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
            .size_full();
        // Always render the tab strip when a solution is active, even with
        // zero open tabs — the strip hosts the "+" button which is the
        // entrypoint for creating the first session. Previously the strip
        // was hidden until at least one tab existed and the "+" lived in
        // a footer; merging them here gives a single, predictable home for
        // session controls (matches Cursor / VS Code chat UX).
        if has_active_solution {
            root = root.child(self.render_tab_strip(cx));
            // Status row used to live here, directly under the tab strip
            // — but the meter / state / model labels are most useful
            // near the compose box where the user's eye already is when
            // sending a message. Rendered inside `SolutionSessionView`
            // now (between the conversation list and the compose row)
            // via `nav.render_status_row` invoked through the view's
            // `WeakEntity<SolutionSessionsNavigator>` handle. The
            // method itself stays on the navigator because its buttons
            // (compact, history popover) need `cx.listener` bindings
            // against the navigator's `Self`.
        }
        // Tab right-click menu floats above the body, anchored at the
        // click position. `with_priority(1)` keeps it above other
        // deferred surfaces (e.g. tooltips) so a hover doesn't paint
        // through the menu.
        let menu_overlay = self.tab_context_menu.as_ref().map(|(menu, position, _)| {
            deferred(
                anchored()
                    .position(*position)
                    .anchor(gpui::Anchor::TopLeft)
                    .child(menu.clone()),
            )
            .with_priority(1)
        });
        root.child(body).children(menu_overlay)
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

/// Pure helper for `reorder_tab`. Moves `sessions[from]` to position `to`
/// and adjusts `selected` so it keeps pointing at the same session. Both
/// indices are interpreted against the original `sessions` (pre-removal)
/// — see the design spec for the index-arithmetic derivation.
fn apply_reorder(
    sessions: &mut Vec<crate::model::SolutionSessionId>,
    selected: &mut Option<usize>,
    from: usize,
    to: usize,
) {
    let len = sessions.len();
    if from >= len || to >= len || from == to {
        return;
    }
    let id = sessions.remove(from);
    sessions.insert(to, id);
    if let Some(idx) = selected.as_mut() {
        if *idx == from {
            *idx = to;
        } else if from < *idx && *idx <= to {
            *idx -= 1;
        } else if to <= *idx && *idx < from {
            *idx += 1;
        }
    }
}

#[cfg(test)]
impl SolutionSessionsNavigator {
    /// Minimal navigator for unit tests — all fields zero-filled; no
    /// subscriptions that need `window`. Callers that need `workspace`
    /// to resolve (e.g. `toast_error`) must upgrade it separately, but
    /// `start_compact_from_cold` only needs `workspace` on failure paths
    /// (when `render_compact_prompt` returns `None`). The test drives the
    /// success path, so `WeakEntity::new_invalid()` is safe here.
    pub(crate) fn for_test(cx: &mut Context<Self>) -> Self {
        let store = crate::store::SolutionAgentStore::global(cx);
        let store_subscription = cx.subscribe(
            &store,
            |_this, _store, _event: &crate::store::SolutionAgentStoreEvent, _cx| {},
        );
        Self {
            workspace: gpui::WeakEntity::new_invalid(),
            project: gpui::WeakEntity::new_invalid(),
            focus_handle: cx.focus_handle(),
            width: gpui::px(380.0),
            active_solution: None,
            open_sessions: Vec::new(),
            selected_index: None,
            views: HashMap::default(),
            pending: Vec::new(),
            next_pending_id: 0,
            historic_sessions: Vec::new(),
            tab_context_menu: None,
            cached_models: HashMap::default(),
            pending_model_fetches: HashSet::default(),
            thinking_tick: None,
            pending_restore: None,
            _store_subscription: store_subscription,
            _solutions_subscription: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SolutionSessionId;

    fn ids(n: usize) -> Vec<SolutionSessionId> {
        (0..n).map(|_| SolutionSessionId::new()).collect()
    }

    #[test]
    fn reorder_forward_moves_selection_with_dragged_tab() {
        let mut sessions = ids(4);
        let original = sessions.clone();
        let mut selected = Some(0);
        apply_reorder(&mut sessions, &mut selected, 0, 2);
        assert_eq!(
            sessions,
            vec![original[1], original[2], original[0], original[3]]
        );
        assert_eq!(selected, Some(2));
    }

    #[test]
    fn reorder_backward_moves_selection_with_dragged_tab() {
        let mut sessions = ids(4);
        let original = sessions.clone();
        let mut selected = Some(3);
        apply_reorder(&mut sessions, &mut selected, 3, 1);
        assert_eq!(
            sessions,
            vec![original[0], original[3], original[1], original[2]]
        );
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn reorder_forward_across_selection_shifts_selected_left() {
        let mut sessions = ids(4);
        let original = sessions.clone();
        let mut selected = Some(2);
        apply_reorder(&mut sessions, &mut selected, 0, 3);
        assert_eq!(
            sessions,
            vec![original[1], original[2], original[3], original[0]]
        );
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn reorder_backward_across_selection_shifts_selected_right() {
        let mut sessions = ids(4);
        let original = sessions.clone();
        let mut selected = Some(1);
        apply_reorder(&mut sessions, &mut selected, 3, 0);
        assert_eq!(
            sessions,
            vec![original[3], original[0], original[1], original[2]]
        );
        assert_eq!(selected, Some(2));
    }

    #[test]
    fn reorder_no_op_when_from_equals_to() {
        let mut sessions = ids(3);
        let original = sessions.clone();
        let mut selected = Some(1);
        apply_reorder(&mut sessions, &mut selected, 1, 1);
        assert_eq!(sessions, original);
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn reorder_out_of_bounds_is_no_op() {
        let mut sessions = ids(2);
        let original = sessions.clone();
        let mut selected = Some(0);
        apply_reorder(&mut sessions, &mut selected, 5, 0);
        apply_reorder(&mut sessions, &mut selected, 0, 5);
        assert_eq!(sessions, original);
        assert_eq!(selected, Some(0));
    }
}

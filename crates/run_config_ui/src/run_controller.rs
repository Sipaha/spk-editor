use std::collections::VecDeque;
use std::path::PathBuf;

use anyhow::anyhow;
use collections::HashMap;
use dap::client::SessionId;
use gpui::{
    Action as _, Context, Entity, EventEmitter, SharedString, Subscription, Task, WeakEntity, Window,
};
use project::Project;
use project::debugger::dap_store::{DapStore, DapStoreEvent};
use run_config::{
    BeforeLaunchStep, Executor, RunConfigId, RunConfigStore, RunConfigStoreEvent, RunRequest,
    RunResolveContext,
};
use terminal::Terminal;
use terminal_view::terminal_panel::TerminalPanel;
use workspace::Workspace;

/// Per-`Workspace` coordinator for run configurations: tracks the selected
/// config (what the toolbar dropdown shows), runs / stops configs, and records
/// which configs are currently running.
pub struct RunController {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    /// The config the toolbar dropdown currently shows. In-memory only for v1.
    // TODO: persist to WorkspaceDb so the selection survives a restart.
    selected: Option<RunConfigId>,
    active: HashMap<RunConfigId, ActiveRun>,
    /// Debug runs whose `DapStore` session id we haven't matched yet, oldest
    /// first. `start_debug_session` doesn't hand back a `SessionId`, so we
    /// remember which configs we just launched (paired with the scenario label
    /// we used) and, on each `DebugClientStarted` event, match the started
    /// session against the first pending entry whose label matches the new
    /// session's label. Events for sessions we didn't launch (user-initiated
    /// debug-panel runs) don't match any pending label, so they're ignored
    /// rather than draining the queue. Two of *our* configs sharing a name is
    /// the only remaining ambiguity (rare; IDEA has the same caveat).
    pending_debug_launches: VecDeque<(RunConfigId, SharedString)>,
    _subscriptions: Vec<Subscription>,
}

pub struct ActiveRun {
    pub config_id: RunConfigId,
    pub executor: Executor,
    pub kind: ActiveRunKind,
}

pub enum ActiveRunKind {
    /// A terminal task. The poller task removes the `ActiveRun` once the
    /// spawned process reports completion. `terminal` is filled in once the
    /// terminal panel has created the task terminal, so Stop can kill it; if
    /// the workspace has no terminal panel wired up (headless test harness) it
    /// stays `None` and Stop only drops the tracking entry.
    Terminal {
        terminal: Option<WeakEntity<Terminal>>,
        /// Keeps the completion poller alive while the run is tracked.
        _poller: Option<Task<()>>,
    },
    /// A debug session. `session_id` is filled in from the next
    /// `DapStoreEvent::DebugClientStarted` after launch; Stop shuts down that
    /// specific session, and the entry clears when that session reports it has
    /// shut down.
    Debug { session_id: Option<SessionId> },
}

#[derive(Clone, Debug)]
pub enum RunControllerEvent {
    SelectedChanged,
    ActiveRunsChanged,
}

impl EventEmitter<RunControllerEvent> for RunController {}

impl RunController {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let project = workspace.project().clone();
        let mut subscriptions = Vec::new();

        let mut selected = None;
        if let Some(store) = RunConfigStore::try_global(cx) {
            selected = store.read(cx).configs().first().map(|config| config.id.clone());
            subscriptions.push(cx.subscribe(&store, Self::on_store_event));
        }

        let dap_store = project.read(cx).dap_store();
        subscriptions.push(cx.subscribe(&dap_store, Self::on_dap_store_event));

        // When this controller's workspace window closes, drop our running-set
        // entry from the global store; otherwise configs that were running at
        // close time keep showing as running. `entity_id` values can be reused
        // after release, so this also closes the (tiny) collision window.
        let source = cx.entity_id().as_u64();
        cx.on_release(move |_this, app| {
            if let Some(store) = RunConfigStore::try_global(app) {
                store.update(app, |store, cx| store.clear_running_source(source, cx));
            }
        })
        .detach();

        Self {
            workspace: workspace.weak_handle(),
            project,
            selected,
            active: HashMap::default(),
            pending_debug_launches: VecDeque::new(),
            _subscriptions: subscriptions,
        }
    }

    fn on_store_event(
        &mut self,
        store: Entity<RunConfigStore>,
        event: &RunConfigStoreEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            RunConfigStoreEvent::ConfigsChanged => {
                let selected_still_exists = self
                    .selected
                    .as_ref()
                    .map(|id| store.read(cx).config(id).is_some())
                    .unwrap_or(false);
                if self.selected.is_some() && !selected_still_exists {
                    self.selected = None;
                    cx.emit(RunControllerEvent::SelectedChanged);
                }
                if self.selected.is_none() {
                    let first = store.read(cx).configs().first().map(|config| config.id.clone());
                    if let Some(first) = first {
                        self.selected = Some(first);
                        cx.emit(RunControllerEvent::SelectedChanged);
                    }
                }
                cx.notify();
            }
        }
    }

    fn on_dap_store_event(
        &mut self,
        dap_store: Entity<DapStore>,
        event: &DapStoreEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            DapStoreEvent::DebugClientStarted(session_id) => {
                // Match the started session against our pending launches by
                // scenario label. A session we didn't launch (user-initiated
                // debug-panel run) won't match any pending label — ignore it
                // rather than mis-claiming a pending launch.
                let started_label = dap_store
                    .read(cx)
                    .session_by_id(session_id)
                    .and_then(|session| session.read(cx).label());
                let Some(started_label) = started_label else {
                    return;
                };
                let Some(position) = self
                    .pending_debug_launches
                    .iter()
                    .position(|(_, label)| label == &started_label)
                else {
                    return;
                };
                let Some((config_id, _)) = self.pending_debug_launches.remove(position) else {
                    return;
                };
                if let Some(ActiveRun {
                    kind: ActiveRunKind::Debug { session_id: slot },
                    ..
                }) = self.active.get_mut(&config_id)
                {
                    *slot = Some(*session_id);
                }
            }
            DapStoreEvent::DebugClientShutdown(session_id) => {
                let finished: Option<RunConfigId> = self
                    .active
                    .iter()
                    .find(|(_, run)| {
                        matches!(
                            run.kind,
                            ActiveRunKind::Debug { session_id: Some(id) } if id == *session_id
                        )
                    })
                    .map(|(id, _)| id.clone());
                if let Some(config_id) = finished {
                    self.active.remove(&config_id);
                    cx.emit(RunControllerEvent::ActiveRunsChanged);
                    self.publish_running(cx);
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    // --- selection ---

    pub fn selected_id(&self) -> Option<&RunConfigId> {
        self.selected.as_ref()
    }

    pub fn select(&mut self, id: RunConfigId, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&id) {
            self.selected = Some(id);
            cx.emit(RunControllerEvent::SelectedChanged);
            cx.notify();
        }
    }

    pub fn select_next(&mut self, cx: &mut Context<Self>) {
        let Some(store) = RunConfigStore::try_global(cx) else {
            return;
        };
        let configs = store.read(cx).configs();
        if configs.is_empty() {
            return;
        }
        let next_index = self
            .selected
            .as_ref()
            .and_then(|selected| configs.iter().position(|config| &config.id == selected))
            .map(|index| (index + 1) % configs.len())
            .unwrap_or(0);
        self.select(configs[next_index].id.clone(), cx);
    }

    // --- active runs ---

    /// Push the current set of running config ids into the global store so that
    /// non-UI consumers (the toolbar strip, MCP `run_config.list`) see them.
    /// Keyed by this controller's entity id so multiple workspace windows don't
    /// overwrite each other's running state.
    fn publish_running(&self, cx: &mut Context<Self>) {
        let source = cx.entity_id().as_u64();
        let ids: collections::HashSet<RunConfigId> = self.active.keys().cloned().collect();
        if let Some(store) = RunConfigStore::try_global(cx) {
            store.update(cx, |store, cx| store.set_running(source, ids, cx));
        }
    }

    pub fn is_running(&self, id: &RunConfigId) -> bool {
        self.active.contains_key(id)
    }

    pub fn active_runs(&self) -> impl Iterator<Item = &ActiveRun> + '_ {
        self.active.values()
    }

    // --- run / stop ---

    pub fn run(
        &mut self,
        config_id: RunConfigId,
        executor: Executor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_running(&config_id) {
            self.stop(&config_id, cx);
        }

        let Some(store) = RunConfigStore::try_global(cx) else {
            self.notify_error("Run configurations are not available".into(), cx);
            return;
        };
        let Some(config) = store.read(cx).config(&config_id) else {
            self.notify_error("That run configuration no longer exists".into(), cx);
            return;
        };
        let Some(provider) = store.read(cx).provider(&config.provider_type) else {
            self.notify_error(format!("No provider for type `{}`", config.provider_type), cx);
            return;
        };
        if !config.executors.contains(&executor) {
            self.notify_error(
                format!("`{}` does not support {executor:?}", config.name),
                cx,
            );
            return;
        }

        for step in &config.before_launch {
            match step {
                BeforeLaunchStep::SaveAllFiles => {
                    // `Workspace::save_all` is a private action handler; dispatch
                    // the action so the workspace runs its own save-all logic.
                    // Fire-and-forget: we don't block the run on the save.
                    window.dispatch_action(
                        workspace::SaveAll {
                            save_intent: Some(workspace::SaveIntent::SaveAll),
                        }
                        .boxed_clone(),
                        cx,
                    );
                }
            }
        }

        let worktree = self.project.read(cx).worktrees(cx).next();
        let worktree_root: Option<PathBuf> = worktree
            .as_ref()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
        let worktree_id = worktree.as_ref().map(|worktree| worktree.read(cx).id());

        let mut task_variables = task::TaskVariables::default();
        if let Some(root) = worktree_root.as_ref() {
            task_variables.insert(
                task::VariableName::WorktreeRoot,
                root.to_string_lossy().into_owned(),
            );
            if let Some(name) = root.file_name().and_then(|name| name.to_str()) {
                task_variables.insert(task::VariableName::Dirname, name.to_string());
            }
        }
        let task_context = task::TaskContext {
            cwd: worktree_root.clone(),
            task_variables,
            project_env: HashMap::default(),
        };

        let mut resolve_context = RunResolveContext {
            project: self.project.clone(),
            worktree_id,
            worktree_root,
            task_context: task_context.clone(),
        };

        let request = match provider.resolve(&config, executor, &mut resolve_context, cx) {
            Ok(request) => request,
            Err(err) => {
                self.notify_error(format!("{err:#}"), cx);
                return;
            }
        };

        match request {
            RunRequest::Terminal(spawn) => {
                let Some(workspace) = self.workspace.upgrade() else {
                    return;
                };

                let poller = if let Some(terminal_panel) =
                    workspace.read(cx).panel::<TerminalPanel>(cx)
                {
                    // Real path: the terminal panel hands back the task
                    // terminal so Stop can kill it.
                    let spawn_task = terminal_panel.update(cx, |terminal_panel, cx| {
                        terminal_panel.spawn_task(&spawn, window, cx)
                    });
                    let poller_config_id = config_id.clone();
                    cx.spawn(async move |this, cx| {
                        match spawn_task.await {
                            Ok(terminal) => {
                                this.update(cx, |this, _| {
                                    if let Some(ActiveRun {
                                        kind: ActiveRunKind::Terminal { terminal: slot, .. },
                                        ..
                                    }) = this.active.get_mut(&poller_config_id)
                                    {
                                        *slot = Some(terminal.clone());
                                    }
                                })
                                .ok();
                                let completion = terminal
                                    .read_with(cx, |terminal, cx| {
                                        terminal.wait_for_completed_task(cx)
                                    })
                                    .ok();
                                if let Some(completion) = completion {
                                    completion.await;
                                }
                            }
                            Err(err) => {
                                log::warn!(
                                    "run_config: terminal task `{}` failed to launch: {err:#}",
                                    poller_config_id.as_str()
                                );
                                this.update(cx, |this, cx| {
                                    this.notify_error(
                                        format!("Failed to launch run configuration: {err:#}"),
                                        cx,
                                    );
                                })
                                .ok();
                            }
                        }
                        this.update(cx, |this, cx| {
                            if this.active.remove(&poller_config_id).is_some() {
                                cx.emit(RunControllerEvent::ActiveRunsChanged);
                                this.publish_running(cx);
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                } else {
                    // Fallback (no terminal panel, e.g. headless tests): we get
                    // only an exit-status future, no killable handle.
                    let spawn_task = workspace.update(cx, |workspace, cx| {
                        workspace.spawn_in_terminal(spawn, window, cx)
                    });
                    let poller_config_id = config_id.clone();
                    cx.spawn(async move |this, cx| {
                        // `Some(_)` => the process actually exited or failed to
                        // launch; `None` => the spawn was cancelled / no
                        // terminal provider — leave the run tracked so the user
                        // can Stop it explicitly.
                        let Some(result) = spawn_task.await else {
                            return;
                        };
                        if let Err(err) = &result {
                            log::warn!(
                                "run_config: terminal task `{}` failed to launch: {err:#}",
                                poller_config_id.as_str()
                            );
                            this.update(cx, |this, cx| {
                                this.notify_error(
                                    format!("Failed to launch run configuration: {err:#}"),
                                    cx,
                                );
                            })
                            .ok();
                        }
                        this.update(cx, |this, cx| {
                            if this.active.remove(&poller_config_id).is_some() {
                                cx.emit(RunControllerEvent::ActiveRunsChanged);
                                this.publish_running(cx);
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                };

                self.active.insert(
                    config_id.clone(),
                    ActiveRun {
                        config_id,
                        executor,
                        kind: ActiveRunKind::Terminal {
                            terminal: None,
                            _poller: Some(poller),
                        },
                    },
                );
                cx.emit(RunControllerEvent::ActiveRunsChanged);
                self.publish_running(cx);
                cx.notify();
            }
            RunRequest::Debug(scenario) => {
                let Some(workspace) = self.workspace.upgrade() else {
                    return;
                };
                let scenario_label = scenario.label.clone();
                workspace.update(cx, |workspace, cx| {
                    workspace.start_debug_session(
                        scenario,
                        task_context.into(),
                        None,
                        worktree_id,
                        window,
                        cx,
                    );
                });
                self.pending_debug_launches
                    .push_back((config_id.clone(), scenario_label));
                self.active.insert(
                    config_id.clone(),
                    ActiveRun {
                        config_id,
                        executor,
                        kind: ActiveRunKind::Debug { session_id: None },
                    },
                );
                cx.emit(RunControllerEvent::ActiveRunsChanged);
                self.publish_running(cx);
                cx.notify();
            }
        }
    }

    pub fn rerun(
        &mut self,
        config_id: RunConfigId,
        executor: Executor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop(&config_id, cx);
        self.run(config_id, executor, window, cx);
    }

    pub fn stop(&mut self, id: &RunConfigId, cx: &mut Context<Self>) {
        let Some(run) = self.active.remove(id) else {
            return;
        };
        match run.kind {
            ActiveRunKind::Terminal { terminal, _poller } => {
                // Dropping `_poller` cancels the completion watcher; kill the
                // task terminal so the spawned process actually goes away. If
                // we never got a handle (no terminal panel) there's nothing we
                // can do here — the run already won't appear as active.
                if let Some(terminal) = terminal.and_then(|terminal| terminal.upgrade()) {
                    terminal.update(cx, |terminal, _| terminal.kill_active_task());
                }
            }
            ActiveRunKind::Debug { session_id } => {
                self.pending_debug_launches
                    .retain(|(pending, _)| pending != id);
                if let Some(session_id) = session_id {
                    let dap_store = self.project.read(cx).dap_store();
                    dap_store
                        .update(cx, |dap_store, cx| dap_store.shutdown_session(session_id, cx))
                        .detach_and_log_err(cx);
                }
            }
        }
        cx.emit(RunControllerEvent::ActiveRunsChanged);
        self.publish_running(cx);
        cx.notify();
    }

    pub fn stop_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<RunConfigId> = self.active.keys().cloned().collect();
        for id in ids {
            self.stop(&id, cx);
        }
    }

    fn notify_error(&self, message: String, cx: &mut Context<Self>) {
        log::error!("run configuration error: {message}");
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_error(&anyhow!(message), cx);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use gpui::{App, AppContext as _, TestAppContext};
    use project::Project;
    use run_config::{ConfigScope, RunConfigProvider, RunConfiguration};
    use std::future;
    use std::process::ExitStatus;
    use ui::IconName;
    use workspace::{AppState, Workspace};

    struct MockProvider;

    impl RunConfigProvider for MockProvider {
        fn type_id(&self) -> &'static str {
            "mock"
        }
        fn display_name(&self) -> &'static str {
            "Mock"
        }
        fn icon(&self) -> IconName {
            IconName::Terminal
        }
        fn supported_executors(&self) -> &'static [Executor] {
            &[Executor::Run]
        }
        fn settings_schema(&self) -> schemars::Schema {
            schemars::json_schema!({ "type": "object" })
        }
        fn new_template(&self, _cx: &App) -> serde_json::Value {
            serde_json::json!({})
        }
        fn resolve(
            &self,
            _config: &RunConfiguration,
            _executor: Executor,
            _cx: &mut RunResolveContext,
            _app: &App,
        ) -> Result<RunRequest> {
            Ok(RunRequest::Terminal(task::SpawnInTerminal {
                command: Some("true".into()),
                ..Default::default()
            }))
        }
    }

    /// A `TerminalProvider` whose spawned task never completes, so a `run`
    /// stays in the `active` set until the controller is told to `stop`.
    struct PendingTerminalProvider;

    impl workspace::TerminalProvider for PendingTerminalProvider {
        fn spawn(
            &self,
            _task: task::SpawnInTerminal,
            _window: &mut Window,
            cx: &mut App,
        ) -> Task<Option<Result<ExitStatus>>> {
            cx.background_executor()
                .spawn(async { future::pending::<Option<Result<ExitStatus>>>().await })
        }
    }

    fn mock_config(name: &str) -> RunConfiguration {
        RunConfiguration {
            id: RunConfigId::new("mock", name),
            name: name.into(),
            provider_type: "mock".into(),
            settings: serde_json::json!({}),
            executors: vec![Executor::Run],
            before_launch: vec![],
            folder: None,
            scope: ConfigScope::Global,
        }
    }

    async fn setup(cx: &mut TestAppContext, configs: &[&str]) -> Entity<Workspace> {
        let app_state = cx.update(|cx| {
            let app_state = AppState::test(cx);
            cx.set_global(db::AppDatabase::test_new());
            editor::init(cx);
            RunConfigStore::init_global(cx);
            run_config::register_provider(cx, MockProvider);
            app_state
        });
        let store = cx.update(|cx| RunConfigStore::global(cx));
        for name in configs {
            store.update(cx, |store, cx| store.upsert(mock_config(name), cx));
        }
        let project = Project::test(app_state.fs.clone(), [], cx).await;
        let (workspace, _cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace
    }

    #[gpui::test]
    async fn select_next_cycles(cx: &mut TestAppContext) {
        let workspace = setup(cx, &["a", "b"]).await;
        let controller = workspace
            .update(cx, |workspace, cx| cx.new(|cx| RunController::new(workspace, cx)));
        cx.run_until_parked();

        controller.read_with(cx, |controller, _| {
            assert_eq!(
                controller.selected_id().map(RunConfigId::as_str),
                Some("mock:a"),
                "first config is auto-selected"
            );
        });

        controller.update(cx, |controller, cx| controller.select_next(cx));
        controller.read_with(cx, |controller, _| {
            assert_eq!(controller.selected_id().map(RunConfigId::as_str), Some("mock:b"));
        });

        controller.update(cx, |controller, cx| controller.select_next(cx));
        controller.read_with(cx, |controller, _| {
            assert_eq!(controller.selected_id().map(RunConfigId::as_str), Some("mock:a"));
        });
    }

    #[gpui::test]
    async fn run_then_stop_tracks_state(cx: &mut TestAppContext) {
        let workspace = setup(cx, &["a"]).await;
        workspace.update(cx, |workspace, _| {
            workspace.set_terminal_provider(PendingTerminalProvider)
        });
        let controller = workspace
            .update(cx, |workspace, cx| cx.new(|cx| RunController::new(workspace, cx)));
        cx.run_until_parked();

        let id = RunConfigId::new("mock", "a");
        let window = cx
            .update(|cx| cx.windows().first().copied())
            .expect("a window exists");

        window
            .update(cx, |_, window, cx| {
                controller.update(cx, |controller, cx| {
                    controller.run(id.clone(), Executor::Run, window, cx)
                })
            })
            .unwrap();
        cx.run_until_parked();

        controller.read_with(cx, |controller, _| {
            assert!(controller.is_running(&id), "run should be tracked as active");
        });

        controller.update(cx, |controller, cx| controller.stop(&id, cx));
        controller.read_with(cx, |controller, _| {
            assert!(!controller.is_running(&id), "stop should clear the active run");
        });
    }

    #[gpui::test]
    async fn dropping_controller_clears_running_source(cx: &mut TestAppContext) {
        let workspace = setup(cx, &["a"]).await;
        workspace.update(cx, |workspace, _| {
            workspace.set_terminal_provider(PendingTerminalProvider)
        });
        let controller = workspace
            .update(cx, |workspace, cx| cx.new(|cx| RunController::new(workspace, cx)));
        cx.run_until_parked();

        let id = RunConfigId::new("mock", "a");
        let window = cx
            .update(|cx| cx.windows().first().copied())
            .expect("a window exists");
        window
            .update(cx, |_, window, cx| {
                controller.update(cx, |controller, cx| {
                    controller.run(id.clone(), Executor::Run, window, cx)
                })
            })
            .unwrap();
        cx.run_until_parked();

        let store = cx.update(|cx| RunConfigStore::global(cx));
        store.read_with(cx, |store, _| {
            assert!(store.is_running(&id), "run is published to the global store");
        });

        drop(controller);
        // Entity release (and thus the `on_release` handler) runs during the
        // next effect flush, not from the `Rc` drop itself.
        cx.update(|_| {});
        cx.run_until_parked();
        store.read_with(cx, |store, _| {
            assert!(
                !store.is_running(&id),
                "dropping the controller clears its running set"
            );
        });
    }
}

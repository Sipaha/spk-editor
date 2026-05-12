use std::path::PathBuf;

use anyhow::anyhow;
use collections::HashMap;
use gpui::{Action as _, Context, Entity, EventEmitter, Subscription, Task, WeakEntity, Window};
use project::Project;
use project::debugger::dap_store::{DapStore, DapStoreEvent};
use run_config::{
    BeforeLaunchStep, Executor, RunConfigId, RunConfigStore, RunConfigStoreEvent, RunRequest,
    RunResolveContext,
};
use terminal_view::TerminalView;
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
    _subscriptions: Vec<Subscription>,
}

pub struct ActiveRun {
    pub config_id: RunConfigId,
    pub executor: Executor,
    pub kind: ActiveRunKind,
}

pub enum ActiveRunKind {
    /// A terminal task. The poller task removes the `ActiveRun` once the
    /// spawned process reports completion; if the workspace has no terminal
    /// provider wired up we keep `view: None` and the entry only clears on Stop.
    Terminal {
        view: Option<WeakEntity<TerminalView>>,
        /// Keeps the completion poller alive while the run is tracked.
        _poller: Option<Task<()>>,
    },
    /// A debug session. Tracked coarsely: cleared when the project's `DapStore`
    /// reports its last session shut down (no per-session id matching for v1).
    Debug,
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

        Self {
            workspace: workspace.weak_handle(),
            project,
            selected,
            active: HashMap::default(),
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
        if let DapStoreEvent::DebugClientShutdown(_session_id) = event {
            // Coarse v1 behaviour: once the DapStore has no live sessions left,
            // every debug run we were tracking is considered finished.
            if dap_store.read(cx).sessions().next().is_none() {
                let had_debug = self
                    .active
                    .values()
                    .any(|run| matches!(run.kind, ActiveRunKind::Debug));
                if had_debug {
                    self.active
                        .retain(|_, run| !matches!(run.kind, ActiveRunKind::Debug));
                    cx.emit(RunControllerEvent::ActiveRunsChanged);
                    cx.notify();
                }
            }
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
                let spawn_task = workspace.update(cx, |workspace, cx| {
                    workspace.spawn_in_terminal(spawn, window, cx)
                });
                let poller_config_id = config_id.clone();
                let poller = cx.spawn(async move |this, cx| {
                    let outcome = spawn_task.await;
                    // `Some(_)` => the process actually exited (success or not);
                    // `None` => the spawn was cancelled / no terminal provider —
                    // leave the run tracked so the user can Stop it explicitly.
                    if outcome.is_some() {
                        this.update(cx, |this, cx| {
                            if this.active.remove(&poller_config_id).is_some() {
                                cx.emit(RunControllerEvent::ActiveRunsChanged);
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                });
                // `spawn_in_terminal` doesn't hand back a `TerminalView`, so
                // terminal runs carry `view: None` and Stop is best-effort.
                self.active.insert(
                    config_id.clone(),
                    ActiveRun {
                        config_id,
                        executor,
                        kind: ActiveRunKind::Terminal {
                            view: None,
                            _poller: Some(poller),
                        },
                    },
                );
                cx.emit(RunControllerEvent::ActiveRunsChanged);
                cx.notify();
            }
            RunRequest::Debug(scenario) => {
                let Some(workspace) = self.workspace.upgrade() else {
                    return;
                };
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
                self.active.insert(
                    config_id.clone(),
                    ActiveRun {
                        config_id,
                        executor,
                        kind: ActiveRunKind::Debug,
                    },
                );
                cx.emit(RunControllerEvent::ActiveRunsChanged);
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
            ActiveRunKind::Terminal { view, _poller } => {
                // Best-effort: no terminal-task-kill API is wired for v1.
                // Dropping `_poller` cancels the completion watcher; the process
                // keeps running unless the user closes the terminal tab.
                drop(view);
            }
            ActiveRunKind::Debug => {
                // Best-effort: leave session teardown to the debugger panel.
            }
        }
        cx.emit(RunControllerEvent::ActiveRunsChanged);
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
}

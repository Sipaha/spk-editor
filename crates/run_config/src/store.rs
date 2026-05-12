use std::sync::Arc;

use collections::HashMap;
use futures::StreamExt as _;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Subscription, Task, WeakEntity};
use project::{Project, Worktree, WorktreeId};
use settings::watch_config_file;
use util::ResultExt as _;

use crate::file_format;
use crate::model::{ConfigScope, RunConfigId, RunConfiguration};
use crate::provider::{ArcProvider, RunConfigProvider};

pub struct RunConfigStore {
    providers: HashMap<&'static str, ArcProvider>,
    /// Persisted configs keyed by id. Project + Global scope.
    persisted: HashMap<RunConfigId, RunConfiguration>,
    /// Ephemeral discovered configs keyed by id. Rebuilt by `refresh_discovered`.
    ephemeral: HashMap<RunConfigId, RunConfiguration>,
    /// Insertion order for stable dropdown listing.
    order: Vec<RunConfigId>,
    /// Project handle captured by `watch_project`, for `save_to_disk` later & re-discovery.
    project: Option<WeakEntity<Project>>,
    fs: Option<Arc<dyn fs::Fs>>,
    /// Configs loaded from the global `run-configurations.json`.
    global_configs: Vec<RunConfiguration>,
    /// Configs loaded from each worktree's `.spke/run-configurations.json`.
    worktree_configs: HashMap<WorktreeId, Vec<RunConfiguration>>,
    /// Live FS watcher tasks (dropped → watchers stop).
    _watchers: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Debug)]
pub enum RunConfigStoreEvent {
    ConfigsChanged,
}

impl EventEmitter<RunConfigStoreEvent> for RunConfigStore {}

struct GlobalRunConfigStore(Entity<RunConfigStore>);
impl Global for GlobalRunConfigStore {}

impl RunConfigStore {
    fn empty() -> Self {
        RunConfigStore {
            providers: HashMap::default(),
            persisted: HashMap::default(),
            ephemeral: HashMap::default(),
            order: Vec::new(),
            project: None,
            fs: None,
            global_configs: Vec::new(),
            worktree_configs: HashMap::default(),
            _watchers: Vec::new(),
            _subscriptions: Vec::new(),
        }
    }

    pub fn init_global(cx: &mut App) {
        let store = cx.new(|_| RunConfigStore::empty());
        cx.set_global(GlobalRunConfigStore(store));
    }

    pub fn global(cx: &App) -> Entity<RunConfigStore> {
        cx.global::<GlobalRunConfigStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<RunConfigStore>> {
        cx.try_global::<GlobalRunConfigStore>().map(|g| g.0.clone())
    }

    // --- provider registry ---
    pub fn register_provider(&mut self, provider: impl RunConfigProvider) {
        let provider: ArcProvider = Arc::new(provider);
        self.providers.insert(provider.type_id(), provider);
    }

    pub fn provider(&self, type_id: &str) -> Option<ArcProvider> {
        self.providers.get(type_id).cloned()
    }

    pub fn providers(&self) -> impl Iterator<Item = &ArcProvider> + '_ {
        self.providers.values()
    }

    // --- config set ---
    /// All configs (persisted in insertion order, then ephemeral sorted by name).
    pub fn configs(&self) -> Vec<RunConfiguration> {
        let mut out: Vec<RunConfiguration> = self
            .order
            .iter()
            .filter_map(|id| self.persisted.get(id).cloned())
            .collect();
        let mut ephemeral: Vec<_> = self.ephemeral.values().cloned().collect();
        ephemeral.sort_by(|a, b| a.name.cmp(&b.name));
        out.extend(ephemeral);
        out
    }

    pub fn config(&self, id: &RunConfigId) -> Option<RunConfiguration> {
        self.persisted
            .get(id)
            .or_else(|| self.ephemeral.get(id))
            .cloned()
    }

    /// Replace the full set of persisted configs (called after a file reload).
    pub fn set_persisted(&mut self, configs: Vec<RunConfiguration>, cx: &mut Context<Self>) {
        self.persisted.clear();
        self.order.clear();
        for config in configs {
            self.order.push(config.id.clone());
            self.persisted.insert(config.id.clone(), config);
        }
        cx.emit(RunConfigStoreEvent::ConfigsChanged);
        cx.notify();
    }

    /// Insert/update one persisted config.
    pub fn upsert(&mut self, config: RunConfiguration, cx: &mut Context<Self>) {
        debug_assert!(config.scope.is_persisted());
        if !self.persisted.contains_key(&config.id) {
            self.order.push(config.id.clone());
        }
        self.persisted.insert(config.id.clone(), config);
        cx.emit(RunConfigStoreEvent::ConfigsChanged);
        cx.notify();
    }

    pub fn remove(
        &mut self,
        id: &RunConfigId,
        cx: &mut Context<Self>,
    ) -> Option<RunConfiguration> {
        let removed = self.persisted.remove(id);
        self.order.retain(|existing| existing != id);
        if removed.is_some() {
            cx.emit(RunConfigStoreEvent::ConfigsChanged);
            cx.notify();
        }
        removed
    }

    /// Re-run every provider's `discover` and replace the ephemeral set.
    pub fn refresh_discovered(&mut self, project: &Entity<Project>, cx: &mut Context<Self>) {
        let providers: Vec<ArcProvider> = self.providers.values().cloned().collect();
        let mut next = HashMap::default();
        for provider in providers {
            for config in provider.discover(project, cx) {
                debug_assert!(matches!(config.scope, ConfigScope::Ephemeral));
                next.insert(config.id.clone(), config);
            }
        }
        if next != self.ephemeral {
            self.ephemeral = next;
            cx.emit(RunConfigStoreEvent::ConfigsChanged);
            cx.notify();
        }
    }

    // --- file watching ---

    /// Rebuild `persisted` + `order` from the per-source buckets, then notify.
    fn rebuild_persisted(&mut self, cx: &mut Context<Self>) {
        self.persisted.clear();
        self.order.clear();
        let mut insert = |config: &RunConfiguration| {
            if !self.persisted.contains_key(&config.id) {
                self.order.push(config.id.clone());
            }
            self.persisted.insert(config.id.clone(), config.clone());
        };
        for config in &self.global_configs {
            insert(config);
        }
        // Iterate worktree buckets in a stable order (by id) for deterministic listing.
        let mut worktree_ids: Vec<WorktreeId> = self.worktree_configs.keys().copied().collect();
        worktree_ids.sort_by_key(|id| id.to_usize());
        for worktree_id in worktree_ids {
            if let Some(configs) = self.worktree_configs.get(&worktree_id) {
                for config in configs {
                    insert(config);
                }
            }
        }
        cx.emit(RunConfigStoreEvent::ConfigsChanged);
        cx.notify();
    }

    fn spawn_global_watch(&mut self, cx: &mut Context<Self>) {
        let Some(fs) = self.fs.clone() else {
            return;
        };
        let path = paths::run_configurations_file().clone();
        let task = cx.spawn(async move |this, cx| {
            let (mut contents_rx, _watcher) =
                watch_config_file(cx.background_executor(), fs, path);
            while let Some(text) = contents_rx.next().await {
                let parsed = parse_text(&text, ConfigScope::Global);
                if this
                    .update(cx, |this, cx| {
                        this.global_configs = parsed;
                        this.rebuild_persisted(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self._watchers.push(task);
    }

    fn spawn_worktree_watch(&mut self, worktree: Entity<Worktree>, cx: &mut Context<Self>) {
        let Some(fs) = self.fs.clone() else {
            return;
        };
        let worktree = worktree.read(cx);
        let worktree_id = worktree.id();
        let path = worktree
            .abs_path()
            .join(paths::local_run_configurations_file_relative_path().as_std_path());
        let task = cx.spawn(async move |this, cx| {
            let (mut contents_rx, _watcher) =
                watch_config_file(cx.background_executor(), fs, path);
            while let Some(text) = contents_rx.next().await {
                let parsed = parse_text(&text, ConfigScope::Project { worktree: worktree_id });
                if this
                    .update(cx, |this, cx| {
                        this.worktree_configs.insert(worktree_id, parsed);
                        this.rebuild_persisted(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self._watchers.push(task);
    }

    fn drop_worktree_configs(&mut self, worktree_id: WorktreeId, cx: &mut Context<Self>) {
        if self.worktree_configs.remove(&worktree_id).is_some() {
            self.rebuild_persisted(cx);
        }
    }

    /// Load and live-watch the global + per-worktree `run-configurations.json`
    /// files for `project`, and keep the ephemeral (discovered) set in sync with
    /// the project. Idempotent: a second call for any project is a no-op.
    pub fn watch_project(
        &mut self,
        project: Entity<Project>,
        fs: Arc<dyn fs::Fs>,
        cx: &mut Context<Self>,
    ) {
        if self.project.is_some() {
            return;
        }
        self.project = Some(project.downgrade());
        self.fs = Some(fs);

        self.spawn_global_watch(cx);
        for worktree in project.read(cx).worktrees(cx).collect::<Vec<_>>() {
            self.spawn_worktree_watch(worktree, cx);
        }

        let task_store = project.read(cx).task_store().clone();
        let task_store_subscription = cx.subscribe(&task_store, |this, _task_store, _event, cx| {
            if let Some(project) = this.project.clone().and_then(|p| p.upgrade()) {
                this.refresh_discovered(&project, cx);
            }
        });
        let project_subscription = cx.subscribe(&project, |this, project, event, cx| match event {
            project::Event::WorktreeAdded(worktree_id) => {
                if let Some(worktree) = project.read(cx).worktree_for_id(*worktree_id, cx) {
                    this.spawn_worktree_watch(worktree, cx);
                }
            }
            project::Event::WorktreeRemoved(worktree_id) => {
                this.drop_worktree_configs(*worktree_id, cx);
            }
            _ => {}
        });
        self._subscriptions.push(project_subscription);
        self._subscriptions.push(task_store_subscription);

        self.refresh_discovered(&project, cx);
    }
}

fn parse_text(text: &str, scope: ConfigScope) -> Vec<RunConfiguration> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    file_format::parse_document(text, scope).log_err().unwrap_or_default()
}

/// Register a provider on the global store. Call from `init` / extension setup.
pub fn register_provider(cx: &mut App, provider: impl RunConfigProvider) {
    if let Some(store) = RunConfigStore::try_global(cx) {
        store.update(cx, |store, _| store.register_provider(provider));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Executor, RunConfigId};
    use fs::Fs as _;
    use gpui::TestAppContext;
    use std::path::Path;

    fn cfg(name: &str) -> RunConfiguration {
        RunConfiguration {
            id: RunConfigId::new("shell", name),
            name: name.into(),
            provider_type: "shell".into(),
            settings: serde_json::json!({}),
            executors: vec![Executor::Run],
            before_launch: vec![],
            folder: None,
            scope: ConfigScope::Global,
        }
    }

    #[gpui::test]
    fn upsert_remove_order(cx: &mut TestAppContext) {
        let store = cx.new(|_| RunConfigStore::empty());
        store.update(cx, |s, cx| {
            s.upsert(cfg("a"), cx);
            s.upsert(cfg("b"), cx);
        });
        store.read_with(cx, |s, _| {
            let names: Vec<_> = s.configs().iter().map(|c| c.name.to_string()).collect();
            assert_eq!(names, vec!["a", "b"]);
        });
        store.update(cx, |s, cx| {
            s.remove(&RunConfigId::new("shell", "a"), cx);
        });
        store.read_with(cx, |s, _| {
            assert_eq!(s.configs().len(), 1);
            assert_eq!(s.configs()[0].name.as_ref(), "b");
        });
    }

    #[gpui::test]
    async fn loads_and_reloads_project_file(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree(
            "/proj",
            serde_json::json!({
                ".spke": {
                    "run-configurations.json": r#"{ "configurations": [ { "name": "X", "type": "shell", "command": "echo" } ] }"#
                }
            }),
        )
        .await;
        let project = project::Project::test(fs.clone(), [Path::new("/proj")], cx).await;
        cx.update(|cx| RunConfigStore::init_global(cx));
        let store = cx.update(|cx| RunConfigStore::global(cx));
        store.update(cx, |s, cx| s.watch_project(project.clone(), fs.clone(), cx));
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            assert!(
                s.configs().iter().any(|c| c.name.as_ref() == "X"),
                "configs: {:?}",
                s.configs()
            );
        });

        fs.write(
            Path::new("/proj/.spke/run-configurations.json"),
            br#"{ "configurations": [ { "name": "Y", "type": "shell", "command": "echo" } ] }"#,
        )
        .await
        .unwrap();
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            let names: Vec<_> = s.configs().iter().map(|c| c.name.to_string()).collect();
            assert!(names.contains(&"Y".to_string()), "got {names:?}");
            assert!(!names.contains(&"X".to_string()), "X should be gone, got {names:?}");
        });
    }
}

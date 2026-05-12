use std::sync::Arc;

use collections::HashMap;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global};
use project::Project;

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
}

#[derive(Clone, Debug)]
pub enum RunConfigStoreEvent {
    ConfigsChanged,
}

impl EventEmitter<RunConfigStoreEvent> for RunConfigStore {}

struct GlobalRunConfigStore(Entity<RunConfigStore>);
impl Global for GlobalRunConfigStore {}

impl RunConfigStore {
    pub fn init_global(cx: &mut App) {
        let store = cx.new(|_| RunConfigStore {
            providers: HashMap::default(),
            persisted: HashMap::default(),
            ephemeral: HashMap::default(),
            order: Vec::new(),
        });
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
    use gpui::TestAppContext;

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
        let store = cx.new(|_| RunConfigStore {
            providers: Default::default(),
            persisted: Default::default(),
            ephemeral: Default::default(),
            order: vec![],
        });
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
}

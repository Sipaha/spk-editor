use crate::cache;
use crate::git;
use crate::model::{CatalogId, CatalogProject, Solution, SolutionId, SolutionMember};
use crate::persistence::{CURRENT_VERSION, SolutionsConfig, load_or_default, save_atomic};
use crate::slug::unique_slug;
use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use gpui::{App, AppContext as _, AsyncApp, Entity, EventEmitter, Global, Task};
use std::path::PathBuf;
use std::sync::Arc;
use util::ResultExt as _;

pub struct SolutionStore {
    config_path: PathBuf,
    config: SolutionsConfig,
    fs_lock: Arc<smol::lock::Mutex<()>>,
}

#[derive(Clone, Debug)]
pub enum SolutionStoreEvent {
    Changed,
}

impl EventEmitter<SolutionStoreEvent> for SolutionStore {}

impl SolutionStore {
    pub fn init_global(cx: &mut App) {
        let config_path = paths::config_dir().join("solutions.json");
        let config = match load_or_default(&config_path) {
            Ok(cfg) => cfg,
            Err(err) => {
                log::error!("solutions::store: failed to load solutions.json: {err}");
                SolutionsConfig {
                    version: CURRENT_VERSION,
                    ..Default::default()
                }
            }
        };
        let store = cx.new(|_| SolutionStore {
            config_path,
            config,
            fs_lock: Arc::new(smol::lock::Mutex::new(())),
        });
        cx.set_global(GlobalSolutionStore(store));
    }

    pub fn global(cx: &App) -> Entity<SolutionStore> {
        cx.global::<GlobalSolutionStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<SolutionStore>> {
        cx.try_global::<GlobalSolutionStore>().map(|g| g.0.clone())
    }

    #[cfg(test)]
    pub fn for_test(config_path: PathBuf, cx: &mut App) -> Entity<SolutionStore> {
        cx.new(|_| SolutionStore {
            config_path,
            config: SolutionsConfig {
                version: CURRENT_VERSION,
                ..Default::default()
            },
            fs_lock: Arc::new(smol::lock::Mutex::new(())),
        })
    }

    pub fn catalog(&self) -> &[CatalogProject] {
        &self.config.catalog
    }

    pub fn solutions(&self) -> &[Solution] {
        &self.config.solutions
    }

    pub fn fs_lock(&self) -> Arc<smol::lock::Mutex<()>> {
        Arc::clone(&self.fs_lock)
    }

    pub fn add_catalog_project(
        &mut self,
        name: &str,
        remote_url: &str,
        default_branch: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) -> Result<CatalogId> {
        let taken: Vec<String> = self.config.catalog.iter().map(|c| c.id.0.clone()).collect();
        let slug = unique_slug(name, &taken);
        let id = CatalogId(slug);
        self.config.catalog.push(CatalogProject {
            id: id.clone(),
            name: name.into(),
            remote_url: remote_url.into(),
            default_branch,
        });
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(id)
    }

    pub fn remove_catalog_project(
        &mut self,
        id: &CatalogId,
        cx: &mut gpui::Context<Self>,
    ) -> Result<()> {
        let referenced_by: Vec<String> = self
            .config
            .solutions
            .iter()
            .filter(|s| s.members.iter().any(|m| m.catalog_id == *id))
            .map(|s| s.name.clone())
            .collect();
        if !referenced_by.is_empty() {
            bail!(
                "catalog project {} is used by solution(s): {}",
                id.0,
                referenced_by.join(", ")
            );
        }
        self.config.catalog.retain(|c| c.id != *id);
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(())
    }

    pub fn create_solution(
        &mut self,
        name: &str,
        root_base: PathBuf,
        cx: &mut gpui::Context<Self>,
    ) -> Result<SolutionId> {
        let taken: Vec<String> = self.config.solutions.iter().map(|s| s.id.0.clone()).collect();
        let slug = unique_slug(name, &taken);
        let id = SolutionId(slug.clone());
        let root = root_base.join(&slug);
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        self.config.solutions.push(Solution {
            id: id.clone(),
            name: name.into(),
            root,
            members: vec![],
            last_opened_at: None,
        });
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(id)
    }

    pub fn rename_solution(
        &mut self,
        id: &SolutionId,
        new_name: &str,
        cx: &mut gpui::Context<Self>,
    ) -> Result<()> {
        let sol = self.find_solution_mut(id)?;
        sol.name = new_name.into();
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(())
    }

    pub fn delete_solution(
        &mut self,
        id: &SolutionId,
        cx: &mut gpui::Context<Self>,
    ) -> Result<()> {
        let before = self.config.solutions.len();
        self.config.solutions.retain(|s| s.id != *id);
        if self.config.solutions.len() == before {
            bail!("solution not found: {}", id.0);
        }
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(())
    }

    pub fn touch_last_opened(
        &mut self,
        id: &SolutionId,
        cx: &mut gpui::Context<Self>,
    ) -> Result<()> {
        let sol = self.find_solution_mut(id)?;
        sol.last_opened_at = Some(Utc::now());
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(())
    }

    pub fn add_member(
        &mut self,
        solution_id: SolutionId,
        catalog_id: CatalogId,
        cache_root: PathBuf,
        cx: &mut gpui::Context<Self>,
    ) -> Task<Result<()>> {
        let sol = match self.config.solutions.iter().find(|s| s.id == solution_id) {
            Some(s) => s.clone(),
            None => {
                let id = solution_id.0.clone();
                return cx.background_spawn(async move { bail!("solution not found: {id}") });
            }
        };
        let cat = match self.config.catalog.iter().find(|c| c.id == catalog_id) {
            Some(c) => c.clone(),
            None => {
                let id = catalog_id.0.clone();
                return cx.background_spawn(async move { bail!("catalog project not found: {id}") });
            }
        };
        if sol.members.iter().any(|m| m.catalog_id == catalog_id) {
            let sol_name = sol.name;
            let cat_name = cat.name;
            return cx.background_spawn(async move {
                bail!("solution {sol_name} already contains {cat_name}")
            });
        }
        let target = sol.root.join(&catalog_id.0);
        let remote_url = cat.remote_url;
        let default_branch = cat.default_branch;
        let lock = Arc::clone(&self.fs_lock);

        cx.spawn(async move |weak: gpui::WeakEntity<Self>, cx: &mut AsyncApp| {
            let _guard = lock.lock().await;
            let cache_path = cache::ensure_cache(&cache_root, &remote_url, |_| {}).await?;
            git::clone_local(&cache_path, &target, |_| {}).await?;
            git::set_remote_url(&target, "origin", &remote_url).await?;
            if let Some(branch) = default_branch.as_deref() {
                git::checkout(&target, branch).await.ok();
            }
            weak.update(cx, |store, cx| {
                if let Some(sol) = store
                    .config
                    .solutions
                    .iter_mut()
                    .find(|s| s.id == solution_id)
                {
                    sol.members.push(SolutionMember {
                        catalog_id: catalog_id.clone(),
                        local_path: target.clone(),
                    });
                    store.persist().log_err();
                    cx.emit(SolutionStoreEvent::Changed);
                    cx.notify();
                }
                Ok::<(), anyhow::Error>(())
            })??;
            Ok(())
        })
    }

    pub fn remove_member(
        &mut self,
        solution_id: &SolutionId,
        catalog_id: &CatalogId,
        cx: &mut gpui::Context<Self>,
    ) -> Result<()> {
        let sol = self.find_solution_mut(solution_id)?;
        let before = sol.members.len();
        sol.members.retain(|m| m.catalog_id != *catalog_id);
        if sol.members.len() == before {
            bail!("member not in solution");
        }
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(())
    }

    pub fn reorder_members(
        &mut self,
        solution_id: &SolutionId,
        new_order: Vec<CatalogId>,
        cx: &mut gpui::Context<Self>,
    ) -> Result<()> {
        let sol = self.find_solution_mut(solution_id)?;
        let mut by_id: collections::HashMap<CatalogId, SolutionMember> = sol
            .members
            .drain(..)
            .map(|m| (m.catalog_id.clone(), m))
            .collect();
        for id in &new_order {
            if let Some(m) = by_id.remove(id) {
                sol.members.push(m);
            }
        }
        for (_, m) in by_id {
            sol.members.push(m);
        }
        self.persist()?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(())
    }

    pub fn paths_for_open(&self, id: &SolutionId) -> Result<Vec<PathBuf>> {
        let sol = self
            .config
            .solutions
            .iter()
            .find(|s| s.id == *id)
            .with_context(|| format!("solution not found: {}", id.0))?;
        Ok(sol.members.iter().map(|m| m.local_path.clone()).collect())
    }

    fn find_solution_mut(&mut self, id: &SolutionId) -> Result<&mut Solution> {
        self.config
            .solutions
            .iter_mut()
            .find(|s| s.id == *id)
            .with_context(|| format!("solution not found: {}", id.0))
    }

    fn persist(&self) -> Result<()> {
        save_atomic(&self.config_path, &self.config)
            .with_context(|| format!("writing {}", self.config_path.display()))
    }

    #[cfg(test)]
    pub fn test_force_add_member(&mut self, sid: &SolutionId, cid: &CatalogId) {
        let sol = self
            .config
            .solutions
            .iter_mut()
            .find(|s| s.id == *sid)
            .expect("test_force_add_member: solution not found");
        sol.members.push(SolutionMember {
            catalog_id: cid.clone(),
            local_path: sol.root.join(&cid.0),
        });
    }
}

struct GlobalSolutionStore(Entity<SolutionStore>);

impl Global for GlobalSolutionStore {}

#[cfg(test)]
pub(crate) fn install_global_for_test(entity: Entity<SolutionStore>, cx: &mut App) {
    cx.set_global(GlobalSolutionStore(entity));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support;
    use gpui::TestAppContext;
    use tempfile::tempdir;

    #[gpui::test]
    async fn add_catalog_project_persists(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let cfg_path = dir.path().join("solutions.json");
        let store = cx.update(|cx| SolutionStore::for_test(cfg_path.clone(), cx));

        store
            .update(cx, |store, cx| {
                store.add_catalog_project(
                    "ECOS Base",
                    "git@example.com:ecos/ecos-base.git",
                    None,
                    cx,
                )
            })
            .expect("add_catalog_project");

        let raw = std::fs::read_to_string(&cfg_path).expect("read config");
        assert!(raw.contains("ecos-base"));
        assert!(raw.contains("git@example.com:ecos/ecos-base.git"));
    }

    #[gpui::test]
    async fn add_catalog_project_dedupes_slug(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let cfg_path = dir.path().join("solutions.json");
        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));

        let id1 = store
            .update(cx, |s, cx| s.add_catalog_project("Foo", "git@x:foo.git", None, cx))
            .expect("first add");
        let id2 = store
            .update(cx, |s, cx| {
                s.add_catalog_project("Foo", "git@x:other-foo.git", None, cx)
            })
            .expect("second add");
        assert_eq!(id1.as_str(), "foo");
        assert_eq!(id2.as_str(), "foo-2");
    }

    #[gpui::test]
    async fn remove_catalog_refuses_when_referenced(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let cfg_path = dir.path().join("solutions.json");
        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));

        let cat_id = store
            .update(cx, |s, cx| s.add_catalog_project("Foo", "git@x:foo.git", None, cx))
            .expect("add catalog");
        let solutions_root = std::env::temp_dir().join("spke-test-solutions");
        let sol_id = store
            .update(cx, |s, cx| s.create_solution("Sol", solutions_root, cx))
            .expect("create solution");
        store.update(cx, |s, _| {
            s.test_force_add_member(&sol_id, &cat_id);
        });

        let result = store.update(cx, |s, cx| s.remove_catalog_project(&cat_id, cx));
        assert!(result.is_err(), "expected refusal");
    }

    #[gpui::test]
    async fn add_member_clones_and_records(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempdir().expect("tempdir");
        let bare = test_support::make_bare_with_one_commit(dir.path()).await;
        let cache_root = dir.path().join("cache");
        let cfg_path = dir.path().join("solutions.json");
        let solutions_root = dir.path().join("solutions");
        std::fs::create_dir_all(&solutions_root).expect("mkdir solutions");

        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));
        let cat_id = store
            .update(cx, |s, cx| {
                s.add_catalog_project(
                    "Bare",
                    bare.to_str().expect("path str"),
                    Some("master".into()),
                    cx,
                )
            })
            .expect("add catalog");
        let sol_id = store
            .update(cx, |s, cx| s.create_solution("S", solutions_root, cx))
            .expect("create solution");

        let task = store.update(cx, |s, cx| {
            s.add_member(sol_id.clone(), cat_id.clone(), cache_root, cx)
        });
        task.await.expect("add_member");

        let target = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|x| x.id == sol_id)
                .expect("solution exists")
                .members[0]
                .local_path
                .clone()
        });
        assert!(target.join(".git").exists());
    }

    #[gpui::test]
    async fn paths_for_open_returns_member_paths_in_order(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store =
            cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        let sol_id = store
            .update(cx, |s, cx| s.create_solution("S", dir.path().to_path_buf(), cx))
            .expect("create solution");
        let cat_a = store
            .update(cx, |s, cx| s.add_catalog_project("A", "git@x:a", None, cx))
            .expect("add A");
        let cat_b = store
            .update(cx, |s, cx| s.add_catalog_project("B", "git@x:b", None, cx))
            .expect("add B");
        store.update(cx, |s, _| {
            s.test_force_add_member(&sol_id, &cat_a);
            s.test_force_add_member(&sol_id, &cat_b);
        });
        let paths =
            store.read_with(cx, |s, _| s.paths_for_open(&sol_id).expect("paths"));
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("a"));
        assert!(paths[1].ends_with("b"));
    }
}

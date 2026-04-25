use crate::model::{CatalogId, CatalogProject, Solution, SolutionId};
#[cfg(test)]
use crate::model::SolutionMember;
use crate::persistence::{CURRENT_VERSION, SolutionsConfig, load_or_default, save_atomic};
use crate::slug::unique_slug;
use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use gpui::{App, AppContext as _, Entity, EventEmitter, Global};
use std::path::PathBuf;
use std::sync::Arc;

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
mod tests {
    use super::*;
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
}

use crate::add_member::InFlightAdd;
use crate::db::SolutionsDb;
use crate::git;
use crate::model::{CatalogId, CatalogProject, Solution, SolutionId, SolutionMember};
use crate::persistence::{CURRENT_VERSION, SolutionsConfig, save_atomic};
use crate::slug::unique_slug;
use crate::tabs_snapshot::{SolutionTabsSnapshot, TabSnapshots};
use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use collections::HashMap;
use gpui::{App, AppContext as _, Entity, EventEmitter, Global};
use std::path::PathBuf;
use std::sync::Arc;

pub struct SolutionStore {
    config_path: PathBuf,
    pub(crate) config: SolutionsConfig,
    /// `Some` for stores hydrated from `SolutionsDb` (production via
    /// `init_global` and tests via `init_global_for_test`); `None` for
    /// `for_test` stores that exercise mutations through the legacy
    /// JSON path without a DB. Tasks 7-9 will start writing through
    /// this connection in addition to (then instead of) `persist()`.
    pub(crate) db: Option<SolutionsDb>,
    pub(crate) fs_lock: Arc<smol::lock::Mutex<()>>,
    pub(crate) in_flight_adds: HashMap<(SolutionId, CatalogId), InFlightAdd>,
    /// Per-Solution open-tab snapshots, populated by the in-place
    /// switch orchestrator on switch-out and replayed on switch-in.
    /// Runtime-only; not persisted to disk — losing the snapshot
    /// after an editor restart is acceptable (user can re-open the
    /// tabs themselves), and persistence would mean keeping
    /// `solutions.json` in sync with potentially-stale path lists.
    pub(crate) tab_snapshots: TabSnapshots,
}

#[derive(Clone, Debug)]
pub enum SolutionStoreEvent {
    Changed,
    /// Emitted by `touch_last_opened` whenever a Solution is opened
    /// (or switched to). Subscribers that only need to react to "the
    /// active Solution flipped" — e.g. fork panels refreshing their
    /// content for a new Solution scope — should listen to this
    /// instead of `Changed`, which fires on every store mutation
    /// including non-active edits (catalog adds, member moves, …).
    /// The id IS always Some — `None` would mean "no Solution is
    /// active", which we model as "no event fired".
    ActiveSolutionChanged(SolutionId),
    MemberAddProgress {
        solution: SolutionId,
        catalog: CatalogId,
        stage: String,
        percent: Option<u8>,
    },
    MemberAddCompleted {
        solution: SolutionId,
        catalog: CatalogId,
        /// `None` on success; `Some(msg)` on failure or cancellation.
        error: Option<String>,
    },
}

impl EventEmitter<SolutionStoreEvent> for SolutionStore {}

impl SolutionStore {
    pub fn init_global(cx: &mut App) {
        let db = SolutionsDb::global(cx);
        Self::init_with_db(db, cx);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn init_global_for_test(db: SolutionsDb, cx: &mut App) {
        Self::init_with_db(db, cx);
    }

    fn init_with_db(db: SolutionsDb, cx: &mut App) {
        let config = match Self::load_from_db_blocking(&db) {
            Ok(cfg) => cfg,
            Err(err) => {
                log::error!("solutions::store: failed to hydrate from DB: {err}");
                SolutionsConfig {
                    version: CURRENT_VERSION,
                    ..Default::default()
                }
            }
        };
        let store = cx.new(|_| SolutionStore {
            config_path: paths::config_dir().join("solutions.json"),
            config,
            db: Some(db),
            fs_lock: Arc::new(smol::lock::Mutex::new(())),
            in_flight_adds: HashMap::default(),
            tab_snapshots: TabSnapshots::default(),
        });
        cx.set_global(GlobalSolutionStore(store));
    }

    fn load_from_db_blocking(db: &SolutionsDb) -> anyhow::Result<SolutionsConfig> {
        let catalog_rows = gpui::block_on(db.load_all_catalog_projects())?;
        let catalog: Vec<CatalogProject> = catalog_rows
            .into_iter()
            .map(|(id, name, remote_url, default_branch)| CatalogProject {
                id: CatalogId(id),
                name,
                remote_url,
                default_branch,
            })
            .collect();

        let solution_rows = gpui::block_on(db.load_all_solutions_with_members())?;
        let mut by_id: collections::HashMap<String, Solution> = collections::HashMap::default();
        let mut order: Vec<String> = Vec::new();
        for (sid, sname, sroot, last_opened_at, catalog_id, local_path, _position) in solution_rows
        {
            let entry = by_id.entry(sid.clone()).or_insert_with(|| {
                order.push(sid.clone());
                Solution {
                    id: SolutionId(sid.clone()),
                    name: sname,
                    root: PathBuf::from(sroot),
                    members: vec![],
                    last_opened_at: last_opened_at
                        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis),
                }
            });
            if !catalog_id.is_empty() {
                entry.members.push(SolutionMember {
                    catalog_id: CatalogId(catalog_id),
                    local_path: PathBuf::from(local_path),
                });
            }
        }
        let solutions: Vec<Solution> =
            order.into_iter().filter_map(|k| by_id.remove(&k)).collect();

        Ok(SolutionsConfig {
            version: CURRENT_VERSION,
            catalog,
            solutions,
        })
    }

    pub fn global(cx: &App) -> Entity<SolutionStore> {
        cx.global::<GlobalSolutionStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<SolutionStore>> {
        cx.try_global::<GlobalSolutionStore>().map(|g| g.0.clone())
    }

    pub fn for_test(config_path: PathBuf, cx: &mut App) -> Entity<SolutionStore> {
        cx.new(|_| SolutionStore {
            config_path,
            config: SolutionsConfig {
                version: CURRENT_VERSION,
                ..Default::default()
            },
            db: None,
            fs_lock: Arc::new(smol::lock::Mutex::new(())),
            in_flight_adds: HashMap::default(),
            tab_snapshots: TabSnapshots::default(),
        })
    }

    /// Read the open-tab snapshot (if any) for a given Solution.
    /// Empty / missing entries return `None`. Used by the in-place
    /// switch orchestrator after worktrees have been swapped to find
    /// out which buffers to re-open.
    pub fn tab_snapshot(&self, id: &SolutionId) -> Option<&SolutionTabsSnapshot> {
        self.tab_snapshots.get(id)
    }

    /// Write the open-tab snapshot for a given Solution. An empty
    /// `snapshot` (no paths and no active path) **evicts** the entry
    /// instead of storing an empty record — keeps the in-memory map
    /// trim and matches the contract that `tab_snapshot` only ever
    /// returns `Some` when there's something worth restoring.
    /// Emits `Changed` (not `ActiveSolutionChanged` — the active id
    /// itself didn't move, only the saved shape).
    pub fn store_tab_snapshot(
        &mut self,
        id: SolutionId,
        snapshot: SolutionTabsSnapshot,
        cx: &mut gpui::Context<Self>,
    ) {
        if snapshot.is_empty() {
            self.tab_snapshots.remove(&id);
        } else {
            self.tab_snapshots.insert(id, snapshot);
        }
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
    }

    pub fn catalog(&self) -> &[CatalogProject] {
        &self.config.catalog
    }

    pub fn solutions(&self) -> &[Solution] {
        &self.config.solutions
    }

    /// First Solution whose `root` is an ancestor of (or equal to) `path`.
    /// Used by the title bar to determine which Solution segment to render
    /// for the active worktree, and by tests to assert the same matching
    /// without going through the rendered UI.
    pub fn solution_for_path(&self, path: &std::path::Path) -> Option<&Solution> {
        self.config
            .solutions
            .iter()
            .find(|sol| path.starts_with(&sol.root))
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
        self.db_save_catalog(self.config.catalog.last().expect("just pushed"))?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(id)
    }

    pub fn edit_catalog_project(
        &mut self,
        id: &CatalogId,
        new_name: Option<String>,
        new_default_branch: Option<String>,
        new_remote_url: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) -> Result<()> {
        let proj = self
            .config
            .catalog
            .iter_mut()
            .find(|p| p.id == *id)
            .with_context(|| format!("catalog_not_found: {}", id.0))?;
        if let Some(name) = new_name {
            proj.name = name;
        }
        if let Some(branch) = new_default_branch {
            proj.default_branch = Some(branch);
        }
        // Track whether the URL actually changed so we can propagate the
        // new value to existing solution-member clones below. Comparing
        // before reassigning avoids both no-op `git remote set-url`
        // round-trips and re-reading the freshly-written value out of
        // `proj` after the assignment.
        let url_change: Option<String> = new_remote_url.and_then(|url| {
            if proj.remote_url == url {
                None
            } else {
                proj.remote_url = url.clone();
                Some(url)
            }
        });
        let updated = self
            .config
            .catalog
            .iter()
            .find(|c| c.id == *id)
            .expect("just edited")
            .clone();
        self.db_save_catalog(&updated)?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();

        if let Some(new_url) = url_change {
            // For every existing clone that points at this catalog entry,
            // rewrite `.git/config`'s `origin` so the next pull / fetch
            // hits the new URL. The warm-cache key is hashed from the URL
            // (see `cache.rs`), so a stale `origin` plus a fresh cache
            // would eventually diverge — better to fix both halves
            // atomically. Fire-and-forget on the foreground executor (so
            // the GPUI test scheduler can pump it deterministically); a
            // failed `git remote set-url` is logged but not surfaced to
            // the user, since the worst case is "next fetch fails with
            // the old error" which is the pre-edit state anyway.
            let targets: Vec<PathBuf> = self
                .config
                .solutions
                .iter()
                .flat_map(|sol| sol.members.iter())
                .filter(|m| m.catalog_id == *id)
                .map(|m| m.local_path.clone())
                .collect();
            if !targets.is_empty() {
                cx.spawn(async move |_, _| {
                    for target in targets {
                        if let Err(err) = git::set_remote_url(&target, "origin", &new_url).await {
                            log::warn!(
                                "edit_catalog_project: git remote set-url failed for {}: {err}",
                                target.display(),
                            );
                        }
                    }
                })
                .detach();
            }
        }
        Ok(())
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
        self.db_delete_catalog(id)?;
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(())
    }

    /// Snapshot of which solutions reference a given catalog entry. Used
    /// by the delete-confirmation modal to render "this will be removed
    /// from N solution(s):" before the user pulls the trigger.
    pub fn solutions_referencing(&self, id: &CatalogId) -> Vec<(SolutionId, String)> {
        self.config
            .solutions
            .iter()
            .filter(|s| s.members.iter().any(|m| m.catalog_id == *id))
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect()
    }

    /// Remove a catalog entry, cascading the deletion to every solution
    /// that references it (drops the matching `SolutionMember` from
    /// each). Returns the list of clone directories that the caller
    /// should remove from disk — disk cleanup is the caller's
    /// responsibility (mirrors `delete_solution` which expects the
    /// `DeleteSolutionModal` to wipe `solution.root`). No-op + `bail!`
    /// if the id is not in the catalog.
    pub fn remove_catalog_project_cascade(
        &mut self,
        id: &CatalogId,
        cx: &mut gpui::Context<Self>,
    ) -> Result<Vec<PathBuf>> {
        if !self.config.catalog.iter().any(|c| c.id == *id) {
            bail!("catalog_not_found: {}", id.0);
        }
        let mut clone_paths: Vec<PathBuf> = Vec::new();
        for sol in self.config.solutions.iter_mut() {
            sol.members.retain(|m| {
                if m.catalog_id == *id {
                    clone_paths.push(m.local_path.clone());
                    false
                } else {
                    true
                }
            });
        }
        // Also drop any in-flight or failed `add_member` rows for this
        // catalog id so the panel doesn't paint orphan "Adding…" /
        // "Failed: …" rows after the catalog entry itself is gone.
        self.in_flight_adds.retain(|(_, cat), _| cat != id);
        self.config.catalog.retain(|c| c.id != *id);
        if let Some(db) = self.db.as_ref() {
            gpui::block_on(async {
                for sol in self.config.solutions.iter() {
                    db.delete_solution_member(sol.id.0.clone(), id.0.clone())
                        .await
                        .ok();
                }
                db.delete_catalog_project(id.0.clone()).await
            })?;
        }
        cx.emit(SolutionStoreEvent::Changed);
        cx.notify();
        Ok(clone_paths)
    }

    pub fn create_solution(
        &mut self,
        name: &str,
        root_base: PathBuf,
        cx: &mut gpui::Context<Self>,
    ) -> Result<SolutionId> {
        let taken: Vec<String> = self
            .config
            .solutions
            .iter()
            .map(|s| s.id.0.clone())
            .collect();
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

    pub fn delete_solution(&mut self, id: &SolutionId, cx: &mut gpui::Context<Self>) -> Result<()> {
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
        // `Changed` first so listeners that watch the broad signal
        // see the broadcast in chronological order; the more specific
        // `ActiveSolutionChanged` follows so subscribers that only
        // care about the active-id-flipped case can ignore `Changed`.
        cx.emit(SolutionStoreEvent::Changed);
        cx.emit(SolutionStoreEvent::ActiveSolutionChanged(id.clone()));
        cx.notify();
        Ok(())
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

    fn db_save_catalog(&self, c: &CatalogProject) -> anyhow::Result<()> {
        let Some(db) = self.db.as_ref() else {
            return Ok(());
        };
        gpui::block_on(db.save_catalog_project(
            c.id.0.clone(),
            c.name.clone(),
            c.remote_url.clone(),
            c.default_branch.clone(),
        ))
    }

    fn db_delete_catalog(&self, id: &CatalogId) -> anyhow::Result<()> {
        let Some(db) = self.db.as_ref() else {
            return Ok(());
        };
        gpui::block_on(db.delete_catalog_project(id.0.clone()))
    }

    fn find_solution_mut(&mut self, id: &SolutionId) -> Result<&mut Solution> {
        self.config
            .solutions
            .iter_mut()
            .find(|s| s.id == *id)
            .with_context(|| format!("solution not found: {}", id.0))
    }

    pub(crate) fn persist(&self) -> Result<()> {
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

pub fn install_global_for_test(entity: Entity<SolutionStore>, cx: &mut App) {
    cx.set_global(GlobalSolutionStore(entity));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support;
    use gpui::TestAppContext;
    use std::time::Duration;
    use tempfile::tempdir;

    #[gpui::test]
    async fn add_catalog_project_dedupes_slug(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let cfg_path = dir.path().join("solutions.json");
        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));

        let id1 = store
            .update(cx, |s, cx| {
                s.add_catalog_project("Foo", "git@x:foo.git", None, cx)
            })
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
            .update(cx, |s, cx| {
                s.add_catalog_project("Foo", "git@x:foo.git", None, cx)
            })
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
    async fn solution_for_path_matches_root_and_descendants(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        let root_base = dir.path().join("alpha-root");
        std::fs::create_dir_all(&root_base).expect("mkdir sol root");
        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("Alpha", root_base.clone(), cx)
            })
            .expect("create solution");
        // create_solution joins the slug onto root_base — fetch the real root.
        let actual_root = store
            .read_with(cx, |s, _| {
                s.solutions()
                    .iter()
                    .find(|x| x.id == sol_id)
                    .map(|x| x.root.clone())
            })
            .expect("solution exists");

        store.read_with(cx, |s, _| {
            // Exact match on the stored root.
            assert_eq!(
                s.solution_for_path(&actual_root).map(|x| x.id.clone()),
                Some(sol_id.clone()),
            );
            // Descendant.
            assert_eq!(
                s.solution_for_path(&actual_root.join("nested/file.rs"))
                    .map(|x| x.id.clone()),
                Some(sol_id.clone()),
            );
            // Sibling at the same parent — not under actual_root.
            let sibling = root_base.join("not-alpha");
            assert!(s.solution_for_path(&sibling).is_none());
            // Path above the root → not contained.
            assert!(s.solution_for_path(&root_base).is_none());
            // Unrelated path.
            assert!(
                s.solution_for_path(std::path::Path::new("/tmp/elsewhere"))
                    .is_none(),
            );
        });
    }

    #[gpui::test]
    async fn solution_for_path_returns_none_when_no_solutions(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        store.read_with(cx, |s, _| {
            assert!(s.solution_for_path(dir.path()).is_none());
        });
    }

    #[gpui::test]
    async fn remove_catalog_project_cascade_drops_from_solutions(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let cfg_path = dir.path().join("solutions.json");
        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));

        let cat_a = store
            .update(cx, |s, cx| s.add_catalog_project("A", "git@x:a", None, cx))
            .expect("add A");
        let cat_b = store
            .update(cx, |s, cx| s.add_catalog_project("B", "git@x:b", None, cx))
            .expect("add B");
        let sol_one = store
            .update(cx, |s, cx| {
                s.create_solution("One", dir.path().to_path_buf(), cx)
            })
            .expect("sol One");
        let sol_two = store
            .update(cx, |s, cx| {
                s.create_solution("Two", dir.path().to_path_buf(), cx)
            })
            .expect("sol Two");
        store.update(cx, |s, _| {
            s.test_force_add_member(&sol_one, &cat_a);
            s.test_force_add_member(&sol_one, &cat_b);
            s.test_force_add_member(&sol_two, &cat_a);
        });

        // Removing A cascades into both solutions; B is untouched.
        let dropped_paths = store
            .update(cx, |s, cx| s.remove_catalog_project_cascade(&cat_a, cx))
            .expect("cascade remove");
        // Cascade returns the local paths the caller now owns the
        // responsibility of wiping. test_force_add_member assigns them
        // synthetically; we just confirm the count is right.
        assert_eq!(dropped_paths.len(), 2, "two members of A were dropped");

        store.read_with(cx, |s, _| {
            assert!(
                s.catalog().iter().all(|c| c.id != cat_a),
                "catalog entry must be gone"
            );
            assert!(s.catalog().iter().any(|c| c.id == cat_b), "B preserved");
            let one = s.solutions().iter().find(|x| x.id == sol_one).unwrap();
            assert_eq!(one.members.len(), 1, "One keeps only B");
            assert_eq!(one.members[0].catalog_id, cat_b);
            let two = s.solutions().iter().find(|x| x.id == sol_two).unwrap();
            assert!(two.members.is_empty(), "Two had only A so it ends up empty");
        });
    }

    #[gpui::test]
    async fn remove_catalog_project_cascade_errors_for_unknown_id(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("solutions.json"), cx));
        let result = store.update(cx, |s, cx| {
            s.remove_catalog_project_cascade(&CatalogId("ghost".into()), cx)
        });
        assert!(result.is_err());
    }

    #[gpui::test]
    async fn solutions_referencing_lists_consumers(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("solutions.json"), cx));
        let cat = store
            .update(cx, |s, cx| s.add_catalog_project("X", "git@x:x", None, cx))
            .expect("add X");
        let sol_a = store
            .update(cx, |s, cx| {
                s.create_solution("A", dir.path().to_path_buf(), cx)
            })
            .expect("sol A");
        let sol_b = store
            .update(cx, |s, cx| {
                s.create_solution("B", dir.path().to_path_buf(), cx)
            })
            .expect("sol B");
        store.update(cx, |s, _| {
            s.test_force_add_member(&sol_a, &cat);
            s.test_force_add_member(&sol_b, &cat);
        });
        let mut refs = store.read_with(cx, |s, _| s.solutions_referencing(&cat));
        refs.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, sol_a);
        assert_eq!(refs[1].0, sol_b);
    }

    #[gpui::test]
    async fn edit_catalog_url_rewrites_member_origin(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempdir().expect("tempdir");
        let bare = test_support::make_bare_with_one_commit(dir.path()).await;
        let cache_root = dir.path().join("cache");
        let cfg_path = dir.path().join("solutions.json");
        let solutions_root = dir.path().join("solutions");
        std::fs::create_dir_all(&solutions_root).expect("mkdir solutions");

        let store = cx.update(|cx| SolutionStore::for_test(cfg_path, cx));
        let original_url = bare.to_str().expect("path str").to_string();
        let cat_id = store
            .update(cx, |s, cx| {
                s.add_catalog_project("Bare", &original_url, Some("master".into()), cx)
            })
            .expect("add catalog");
        let sol_id = store
            .update(cx, |s, cx| s.create_solution("S", solutions_root, cx))
            .expect("create solution");
        let task = store.update(cx, |s, cx| {
            s.add_member(sol_id.clone(), cat_id.clone(), cache_root, cx)
        });
        task.await.expect("add_member success");

        let new_url = format!("{original_url}-renamed");
        store
            .update(cx, |s, cx| {
                s.edit_catalog_project(&cat_id, None, None, Some(new_url.clone()), cx)
            })
            .expect("edit catalog");

        let local_path = store.read_with(cx, |s, _| {
            s.solutions()
                .iter()
                .find(|x| x.id == sol_id)
                .unwrap()
                .members[0]
                .local_path
                .clone()
        });
        // The URL update spawns a foreground task that shells out to
        // `git remote set-url`. Drive the executor until the new URL
        // shows up in `.git/config`. We poll instead of asserting after
        // a single `run_until_parked` because the spawned `git` child
        // process awaits real I/O outside the GPUI scheduler — one
        // pump cycle isn't enough.
        let config_path = local_path.join(".git/config");
        let mut attempts = 0u32;
        let observed = loop {
            cx.run_until_parked();
            cx.background_executor
                .timer(Duration::from_millis(50))
                .await;
            let text = std::fs::read_to_string(&config_path).expect("read .git/config");
            let url = text
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("url ="))
                .map(|line| line.trim_start_matches("url =").trim().to_string());
            if url.as_deref() == Some(new_url.as_str()) {
                break url.unwrap();
            }
            attempts += 1;
            assert!(
                attempts < 100,
                "origin URL never updated; last seen {:?}",
                url
            );
        };
        assert_eq!(observed, new_url);
    }

    #[gpui::test]
    async fn paths_for_open_returns_member_paths_in_order(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("c.json"), cx));
        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("S", dir.path().to_path_buf(), cx)
            })
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
        let paths = store.read_with(cx, |s, _| s.paths_for_open(&sol_id).expect("paths"));
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("a"));
        assert!(paths[1].ends_with("b"));
    }

    #[gpui::test]
    async fn store_tab_snapshot_round_trips(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("s.json"), cx));
        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("S", dir.path().to_path_buf(), cx)
            })
            .expect("create solution");
        let snapshot = SolutionTabsSnapshot {
            open_paths: vec![PathBuf::from("/x"), PathBuf::from("/y")],
            active_path: Some(PathBuf::from("/y")),
        };
        store.update(cx, |s, cx| {
            s.store_tab_snapshot(sol_id.clone(), snapshot.clone(), cx);
        });
        let recovered = store.read_with(cx, |s, _| s.tab_snapshot(&sol_id).cloned());
        assert_eq!(recovered, Some(snapshot));
    }

    #[gpui::test]
    async fn store_tab_snapshot_empty_evicts(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("s.json"), cx));
        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("S", dir.path().to_path_buf(), cx)
            })
            .expect("create solution");
        store.update(cx, |s, cx| {
            s.store_tab_snapshot(
                sol_id.clone(),
                SolutionTabsSnapshot {
                    open_paths: vec![PathBuf::from("/x")],
                    active_path: None,
                },
                cx,
            );
            s.store_tab_snapshot(sol_id.clone(), SolutionTabsSnapshot::default(), cx);
        });
        let still = store.read_with(cx, |s, _| s.tab_snapshot(&sol_id).cloned());
        assert!(
            still.is_none(),
            "default (empty) snapshot must evict the entry; got {still:?}"
        );
    }

    #[gpui::test]
    async fn touch_last_opened_emits_active_solution_changed(cx: &mut TestAppContext) {
        let dir = tempdir().expect("tempdir");
        let store = cx.update(|cx| SolutionStore::for_test(dir.path().join("s.json"), cx));
        let sol_id = store
            .update(cx, |s, cx| {
                s.create_solution("S", dir.path().to_path_buf(), cx)
            })
            .expect("create solution");
        let events: std::sync::Arc<std::sync::Mutex<Vec<SolutionStoreEvent>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let _sub = cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&store, move |_store, ev: &SolutionStoreEvent, _cx| {
                events.lock().expect("events lock").push(ev.clone());
            })
        });
        store
            .update(cx, |s, cx| s.touch_last_opened(&sol_id, cx))
            .expect("touch");
        cx.run_until_parked();
        let events = events.lock().expect("events lock");
        let active_changes: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                SolutionStoreEvent::ActiveSolutionChanged(id) => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            active_changes,
            vec![sol_id],
            "expected exactly one ActiveSolutionChanged for the touched id; got events: {:?}",
            events
        );
    }
}

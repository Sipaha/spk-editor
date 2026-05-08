//! Persistent per-repo favorites + recent-checkouts store for the S-BRP
//! Branches popup. Backed by `<config_dir>/git_branch_favorites.json`,
//! shared across all editor instances. Concurrent writers serialize
//! through an `fs2` advisory lock + write-to-tmp + atomic-rename
//! (mirrors the `git::undo_registry` pattern).
//!
//! The repo is keyed by a stable hash of its work-tree absolute path so
//! the file format doesn't pin the on-disk path verbatim — moving a repo
//! preserves favorites only by best-effort (the user re-favorites after
//! the move). Trade-off: avoids leaking the user's filesystem layout into
//! the JSON.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

const STORE_FILE: &str = "git_branch_favorites.json";
const RECENT_CAP: usize = 50;

/// Stable identifier for a repository, derived from the absolute path of
/// its working directory. Returned as a hex-encoded `u64` so it survives
/// JSON round-trips without serialization quirks.
pub fn repo_hash(work_dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    work_dir.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub branch: String,
    pub last_checkout_unix: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    favorites: HashMap<String, Vec<String>>,
    #[serde(default)]
    recent: HashMap<String, Vec<RecentEntry>>,
}

/// Snapshot for one repo as exposed to the UI. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct RepoFavoritesSnapshot {
    pub favorites: Vec<String>,
    pub recent: Vec<RecentEntry>,
}

/// Read favorites + recent for a single repository.
pub fn load_for_repo(work_dir: &Path) -> Result<RepoFavoritesSnapshot> {
    let key = repo_hash(work_dir);
    let store = read_store()?;
    Ok(RepoFavoritesSnapshot {
        favorites: store.favorites.get(&key).cloned().unwrap_or_default(),
        recent: store.recent.get(&key).cloned().unwrap_or_default(),
    })
}

/// Toggle `branch` in the favorites set for the given repository.
/// Returns the post-toggle membership (`true` = is now a favorite).
pub fn toggle_favorite(work_dir: &Path, branch: &str) -> Result<bool> {
    let key = repo_hash(work_dir);
    let branch = branch.to_string();
    let mut now_favorite = false;
    with_store(|store| {
        let entry = store.favorites.entry(key.clone()).or_default();
        if let Some(pos) = entry.iter().position(|b| b == &branch) {
            entry.remove(pos);
        } else {
            entry.push(branch.clone());
            now_favorite = true;
        }
        Ok(())
    })?;
    Ok(now_favorite)
}

/// Record `branch` as the most-recent checkout in `work_dir`. Truncates
/// the recent list at [`RECENT_CAP`] entries to keep the file bounded.
pub fn record_checkout(work_dir: &Path, branch: &str) -> Result<()> {
    let key = repo_hash(work_dir);
    let branch = branch.to_string();
    let now = current_unix_seconds();
    with_store(|store| {
        let entry = store.recent.entry(key.clone()).or_default();
        entry.retain(|e| e.branch != branch);
        entry.insert(
            0,
            RecentEntry {
                branch,
                last_checkout_unix: now,
            },
        );
        if entry.len() > RECENT_CAP {
            entry.truncate(RECENT_CAP);
        }
        Ok(())
    })
}

fn store_path() -> PathBuf {
    if let Some(custom) = test_override::current() {
        return custom.join(STORE_FILE);
    }
    paths::config_dir().join(STORE_FILE)
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_override {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub fn set(path: PathBuf) {
        OVERRIDE.with(|cell| *cell.borrow_mut() = Some(path));
    }

    pub fn clear() {
        OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    }

    pub fn current() -> Option<PathBuf> {
        OVERRIDE.with(|cell| cell.borrow().clone())
    }
}

#[cfg(not(any(test, feature = "test-support")))]
mod test_override {
    use std::path::PathBuf;
    pub fn current() -> Option<PathBuf> {
        None
    }
}

fn read_store() -> Result<StoreFile> {
    let path = store_path();
    if !path.exists() {
        return Ok(StoreFile::default());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    fs2::FileExt::lock_shared(&file)
        .with_context(|| format!("locking {}", path.display()))?;
    let mut body = String::new();
    file.read_to_string(&mut body)
        .with_context(|| format!("reading {}", path.display()))?;
    fs2::FileExt::unlock(&file).ok();
    if body.trim().is_empty() {
        return Ok(StoreFile::default());
    }
    serde_json::from_str(&body).with_context(|| format!("parsing {}", path.display()))
}

fn with_store<R>(f: impl FnOnce(&mut StoreFile) -> Result<R>) -> Result<R> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    fs2::FileExt::lock_exclusive(&lock_file)
        .with_context(|| format!("locking {}", path.display()))?;

    let mut body = String::new();
    lock_file
        .read_to_string(&mut body)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut store: StoreFile = if body.trim().is_empty() {
        StoreFile::default()
    } else {
        serde_json::from_str(&body)
            .with_context(|| format!("parsing {}", path.display()))?
    };

    let result = f(&mut store)?;

    let serialized = serde_json::to_vec_pretty(&store)
        .context("serializing branch favorites store")?;
    let tmp_path = path.with_extension("json.tmp");
    {
        let mut tmp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .with_context(|| format!("opening {}", tmp_path.display()))?;
        tmp.write_all(&serialized)
            .with_context(|| format!("writing {}", tmp_path.display()))?;
        tmp.sync_all().ok();
    }
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("renaming {} -> {}", tmp_path.display(), path.display()))?;
    lock_file.seek(SeekFrom::Start(0)).ok();
    fs2::FileExt::unlock(&lock_file).ok();
    Ok(result)
}

fn current_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn pin(dir: &Path) {
        test_override::set(dir.to_path_buf());
    }

    #[test]
    fn toggle_favorite_round_trips() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let repo = Path::new("/tmp/r1");
        let added = toggle_favorite(repo, "main").expect("toggle");
        assert!(added);
        let snap = load_for_repo(repo).expect("load");
        assert_eq!(snap.favorites, vec!["main".to_string()]);
        let removed = toggle_favorite(repo, "main").expect("toggle");
        assert!(!removed);
        let snap = load_for_repo(repo).expect("load");
        assert!(snap.favorites.is_empty());
        test_override::clear();
    }

    #[test]
    fn recent_caps_at_50_and_dedupes() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let repo = Path::new("/tmp/r2");
        for ix in 0..60 {
            record_checkout(repo, &format!("b{ix}")).expect("record");
        }
        let snap = load_for_repo(repo).expect("load");
        assert_eq!(snap.recent.len(), RECENT_CAP);
        // Most recent first.
        assert_eq!(snap.recent[0].branch, "b59");
        // Re-checking an existing branch moves it to the front, doesn't dupe.
        record_checkout(repo, "b30").expect("record");
        let snap = load_for_repo(repo).expect("load");
        assert_eq!(snap.recent[0].branch, "b30");
        assert!(snap.recent.iter().filter(|e| e.branch == "b30").count() == 1);
        test_override::clear();
    }

    #[test]
    fn repo_hash_stable_per_path() {
        let h1 = repo_hash(Path::new("/a/b"));
        let h2 = repo_hash(Path::new("/a/b"));
        let h3 = repo_hash(Path::new("/a/c"));
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }
}

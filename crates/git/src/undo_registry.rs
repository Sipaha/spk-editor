//! Persistent undo registry — every destructive operation records a row so
//! the user can rewind with `editor.git.undo_last`.
//!
//! Persistence lives at `<config_dir>/git_undo.json` as a JSON array of
//! [`UndoEntry`]. Concurrent writers from multiple processes are serialized
//! via an exclusive `fs2` advisory lock on the file plus the
//! write-to-tmp + atomic-rename pattern, so a kill -9 mid-write can never
//! produce a half-written JSON file (the rename is atomic on POSIX and
//! Windows replaces the destination).

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

const REGISTRY_FILE: &str = "git_undo.json";

/// One undo row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub id: u64,
    pub repo_path: PathBuf,
    pub op: String,
    pub timestamp_unix: i64,
    pub branch: String,
    pub before_sha: String,
    pub after_sha: Option<String>,
    pub failed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RegistryFile {
    /// Monotonic counter for the next-assigned `id`. Outlives entry deletion
    /// so re-using IDs is impossible even if entries are pruned.
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    entries: Vec<UndoEntry>,
}

/// Append a fresh entry. Returns the assigned id.
pub fn record(repo_path: &Path, op: &str, branch: &str, before_sha: &str) -> Result<u64> {
    with_registry(|registry| {
        registry.next_id = registry.next_id.saturating_add(1);
        let id = registry.next_id;
        registry.entries.push(UndoEntry {
            id,
            repo_path: repo_path.to_path_buf(),
            op: op.to_string(),
            timestamp_unix: current_unix_seconds(),
            branch: branch.to_string(),
            before_sha: before_sha.to_string(),
            after_sha: None,
            failed: false,
        });
        Ok(id)
    })
}

/// Mark `id` as completed with the resulting `after_sha`.
pub fn complete(id: u64, after_sha: &str) -> Result<()> {
    with_registry(|registry| {
        if let Some(entry) = registry.entries.iter_mut().find(|e| e.id == id) {
            entry.after_sha = Some(after_sha.to_string());
        }
        Ok(())
    })
}

/// Mark `id` as failed.
pub fn mark_failed(id: u64) -> Result<()> {
    with_registry(|registry| {
        if let Some(entry) = registry.entries.iter_mut().find(|e| e.id == id) {
            entry.failed = true;
        }
        Ok(())
    })
}

/// List entries newer than `since_unix` (inclusive lower bound), most recent first.
pub fn list(since_unix: i64) -> Result<Vec<UndoEntry>> {
    let registry = read_registry()?;
    let mut entries: Vec<UndoEntry> = registry
        .entries
        .into_iter()
        .filter(|e| e.timestamp_unix >= since_unix)
        .collect();
    entries.sort_by(|a, b| b.timestamp_unix.cmp(&a.timestamp_unix));
    Ok(entries)
}

/// Remove the entry with `id` from the registry. The corresponding
/// backup-ref (if any) is **not** touched — it follows its own retention.
pub fn forget(id: u64) -> Result<()> {
    with_registry(|registry| {
        registry.entries.retain(|e| e.id != id);
        Ok(())
    })
}

fn registry_path() -> PathBuf {
    if let Some(custom) = test_override::current() {
        return custom.join(REGISTRY_FILE);
    }
    paths::config_dir().join(REGISTRY_FILE)
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

fn read_registry() -> Result<RegistryFile> {
    let path = registry_path();
    if !path.exists() {
        return Ok(RegistryFile::default());
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
        return Ok(RegistryFile::default());
    }
    serde_json::from_str(&body)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Open the registry under exclusive lock, run `f`, write back atomically.
/// `f` mutates the in-memory representation; persistence is handled here so
/// callers can't forget to flush.
fn with_registry<R>(f: impl FnOnce(&mut RegistryFile) -> Result<R>) -> Result<R> {
    let path = registry_path();
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

    let mut registry: RegistryFile = if body.trim().is_empty() {
        RegistryFile::default()
    } else {
        serde_json::from_str(&body)
            .with_context(|| format!("parsing {}", path.display()))?
    };

    let result = f(&mut registry)?;

    let serialized =
        serde_json::to_vec_pretty(&registry).context("serializing undo registry")?;

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

    // Truncate the lock-file handle (which now points at the *replaced* inode
    // on Unix anyway) so subsequent reads via this fd return nothing — but
    // the actual persisted file is the renamed one.
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

    /// Each test pins the registry path to a private tempdir.
    fn pin(dir: &Path) {
        test_override::set(dir.to_path_buf());
    }

    #[test]
    fn record_assigns_monotonic_ids() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let id1 = record(Path::new("/repo"), "drop", "main", "deadbeef").expect("record");
        let id2 = record(Path::new("/repo"), "squash", "main", "feedface").expect("record");
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        test_override::clear();
    }

    #[test]
    fn complete_sets_after_sha() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let id = record(Path::new("/r"), "rebase", "main", "aaaa").expect("record");
        complete(id, "bbbb").expect("complete");
        let entries = list(0).expect("list");
        let entry = entries.iter().find(|e| e.id == id).expect("entry exists");
        assert_eq!(entry.after_sha.as_deref(), Some("bbbb"));
        assert!(!entry.failed);
        test_override::clear();
    }

    #[test]
    fn mark_failed_flips_flag() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let id = record(Path::new("/r"), "drop", "main", "ccc").expect("record");
        mark_failed(id).expect("mark_failed");
        let entries = list(0).expect("list");
        let entry = entries.iter().find(|e| e.id == id).expect("entry exists");
        assert!(entry.failed);
        test_override::clear();
    }

    #[test]
    fn list_filters_by_timestamp() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let id = record(Path::new("/r"), "drop", "main", "aa").expect("record");
        let entries = list(0).expect("list");
        assert!(entries.iter().any(|e| e.id == id));
        // Cutoff in the future: empty.
        let future = current_unix_seconds() + 10_000;
        let entries = list(future).expect("list");
        assert!(entries.is_empty());
        test_override::clear();
    }

    #[test]
    fn forget_removes_entry() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let id = record(Path::new("/r"), "drop", "main", "aa").expect("record");
        forget(id).expect("forget");
        let entries = list(0).expect("list");
        assert!(entries.iter().all(|e| e.id != id));
        test_override::clear();
    }

    #[test]
    fn persistence_roundtrip_through_fs() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let _ = record(Path::new("/r"), "drop", "main", "aa").expect("record");
        // Read raw file and parse — confirms file shape is round-trippable.
        let body = std::fs::read_to_string(dir.path().join(REGISTRY_FILE)).expect("read");
        let parsed: RegistryFile = serde_json::from_str(&body).expect("parse");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.next_id, 1);
        test_override::clear();
    }
}

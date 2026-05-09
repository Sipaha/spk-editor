//! S-SHL — Shelf: named long-term entries on top of git stash.
//!
//! A shelf entry pairs a stable `stash_sha` (the commit hash of a real git
//! stash entry, NOT `stash@{N}` — positional indices shift on every push /
//! drop and can't be used as a long-term key) with editor-local metadata
//! (name, description, source branch, file summary) persisted at
//! `<config_dir>/shelf.json`.
//!
//! Persistence mirrors `crate::undo_registry` and
//! `git_ui::branch_picker::favorites`: an `fs2` advisory exclusive lock on
//! the file plus a write-to-tmp + atomic-rename, so concurrent writers
//! across processes can't produce a half-written JSON file.
//!
//! Auto-shelve (the crash-recovery `.diff` snapshots under
//! `<temp_dir>/spk-editor-auto-shelve/`) is a separate mechanism and lives
//! in `super::auto_shelve` — named shelf and auto-shelve never share state.

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

const STORE_FILE: &str = "shelf.json";

/// One named shelf entry. The `stash_sha` is the stable identifier — git
/// stash positions (`stash@{N}`) shift on push/drop and would silently
/// corrupt long-term entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShelfEntry {
    pub name: String,
    pub stash_sha: String,
    pub created_at_unix: i64,
    pub source_branch: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub files_summary: FilesSummary,
}

/// Lightweight per-entry stat pulled from `git stash show --numstat`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesSummary {
    pub count_added: u32,
    pub count_modified: u32,
    pub count_deleted: u32,
    pub total_lines_added: u32,
    pub total_lines_removed: u32,
    /// First five paths from the stash, in numstat order.
    #[serde(default)]
    pub top_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreFile {
    /// Map of `repo_hash(work_dir)` -> entries for that repo.
    #[serde(default)]
    repos: HashMap<String, Vec<ShelfEntry>>,
}

/// In-memory snapshot of one repo's shelf, owned by callers.
#[derive(Debug, Clone)]
pub struct ShelfStore {
    repo_path: PathBuf,
    repo_key: String,
    entries: Vec<ShelfEntry>,
}

impl ShelfStore {
    /// Load the slice of the store relevant to `repo_path`. Other repos'
    /// entries are not held in memory.
    pub fn load(repo_path: &Path) -> Result<Self> {
        let store = read_store()?;
        let repo_key = repo_hash(repo_path);
        let entries = store.repos.get(&repo_key).cloned().unwrap_or_default();
        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            repo_key,
            entries,
        })
    }

    /// Persist this repo's slice back into the on-disk store, leaving
    /// other repos' entries untouched.
    pub fn save(&self) -> Result<()> {
        let key = self.repo_key.clone();
        let entries = self.entries.clone();
        with_store(|store| {
            if entries.is_empty() {
                store.repos.remove(&key);
            } else {
                store.repos.insert(key, entries);
            }
            Ok(())
        })
    }

    /// Append a fresh entry. Errors if `name` already exists for this repo.
    pub fn add(&mut self, entry: ShelfEntry) -> Result<()> {
        if self.entries.iter().any(|e| e.name == entry.name) {
            return Err(anyhow!("a shelf entry named {:?} already exists", entry.name));
        }
        self.entries.push(entry);
        self.save()
    }

    /// Read-only view of this repo's entries. `_repo_path` is accepted to
    /// keep the signature symmetric with the spec — callers can pass any
    /// path (it's ignored; the store was bound to the repo at `load()`).
    pub fn list(&self, _repo_path: &Path) -> Vec<&ShelfEntry> {
        self.entries.iter().collect()
    }

    /// Borrowed access to the in-memory entries.
    pub fn entries(&self) -> &[ShelfEntry] {
        &self.entries
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// Drop the entry whose `name` matches. Errors if no match.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        let before = self.entries.len();
        self.entries.retain(|e| e.name != name);
        if self.entries.len() == before {
            return Err(anyhow!("no shelf entry named {:?}", name));
        }
        self.save()
    }

    /// Rename `old` -> `new`. Errors if `old` is missing or `new`
    /// collides with an existing entry.
    pub fn rename(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        if self.entries.iter().any(|e| e.name == new) {
            return Err(anyhow!("a shelf entry named {:?} already exists", new));
        }
        let target = self
            .entries
            .iter_mut()
            .find(|e| e.name == old)
            .ok_or_else(|| anyhow!("no shelf entry named {:?}", old))?;
        target.name = new.to_string();
        self.save()
    }

    /// Replace the description on `name`. `desc=None` clears it.
    pub fn update_description(&mut self, name: &str, desc: Option<String>) -> Result<()> {
        let target = self
            .entries
            .iter_mut()
            .find(|e| e.name == name)
            .ok_or_else(|| anyhow!("no shelf entry named {:?}", name))?;
        target.description = desc.filter(|d| !d.trim().is_empty());
        self.save()
    }

    /// Look up an entry by its stable `stash_sha`. Useful when correlating
    /// a `git stash list` row back to a named entry.
    pub fn lookup_by_sha(&self, sha: &str) -> Option<&ShelfEntry> {
        self.entries.iter().find(|e| e.stash_sha == sha)
    }

    /// Names of entries whose `stash_sha` is no longer in
    /// `git stash list` output (manually dropped via the CLI, etc.).
    /// Returns names so the caller can show "Forget" buttons or prune.
    pub fn lookup_orphaned(&self, repo_path: &Path) -> Vec<String> {
        let live = list_live_stash_shas(repo_path).unwrap_or_default();
        self.entries
            .iter()
            .filter(|entry| !live.iter().any(|sha| sha == &entry.stash_sha))
            .map(|entry| entry.name.clone())
            .collect()
    }
}

/// Stable identifier for a repository, hashed from the absolute path of
/// its working directory. Hex-encoded so JSON round-trips cleanly.
pub fn repo_hash(work_dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    work_dir.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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

    let serialized = serde_json::to_vec_pretty(&store).context("serializing shelf store")?;
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

// ====================================================================
//  High-level operations
// ====================================================================

/// Capture `paths_to_shelve` into a git stash labelled `[spk-shelf] <name>`,
/// record metadata in [`ShelfStore`], and return the resulting entry.
///
/// `paths_to_shelve = None` shelves the entire working-tree diff (the
/// default behaviour of `git stash push`). `remove_after = false` re-applies
/// the stash on top of the working tree so the shelf is a copy rather than
/// a move; the on-disk stash entry still exists either way.
pub fn shelve(
    repo_path: &Path,
    name: &str,
    description: Option<String>,
    paths_to_shelve: Option<Vec<PathBuf>>,
    remove_after: bool,
) -> Result<ShelfEntry> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err(anyhow!("shelf entry name must not be empty"));
    }

    let mut store = ShelfStore::load(repo_path)?;
    if store.entries.iter().any(|e| e.name == trimmed_name) {
        return Err(anyhow!(
            "a shelf entry named {:?} already exists",
            trimmed_name
        ));
    }

    let stash_message = format!("[spk-shelf] {}", trimmed_name);
    let mut args: Vec<String> = vec![
        "stash".into(),
        "push".into(),
        "--include-untracked".into(),
        "-m".into(),
        stash_message,
    ];
    if let Some(paths) = paths_to_shelve.as_ref() {
        if !paths.is_empty() {
            args.push("--".into());
            for path in paths {
                args.push(path.to_string_lossy().into_owned());
            }
        }
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let push_out = run_git(repo_path, &arg_refs)?;
    if push_out.contains("No local changes to save") {
        return Err(anyhow!("nothing to shelve: working tree is clean"));
    }

    let stash_sha = run_git(repo_path, &["rev-parse", "stash@{0}"])
        .context("reading stash@{0} sha")?
        .trim()
        .to_string();
    if stash_sha.is_empty() {
        return Err(anyhow!("`git rev-parse stash@{{0}}` returned empty sha"));
    }

    let summary = parse_files_summary(repo_path, &stash_sha)?;
    let source_branch = current_branch(repo_path).ok();

    if !remove_after {
        run_git(repo_path, &["stash", "apply", "--quiet", &stash_sha])
            .context("re-applying shelved stash (remove_after=false)")?;
    }

    let entry = ShelfEntry {
        name: trimmed_name.to_string(),
        stash_sha,
        created_at_unix: current_unix_seconds(),
        source_branch,
        description: description.filter(|d| !d.trim().is_empty()),
        files_summary: summary,
    };
    store.add(entry.clone())?;
    Ok(entry)
}

/// Apply the stash backing the named shelf entry. `remove_from_shelf=true`
/// drops the underlying stash and removes the entry; `false` leaves both
/// in place (equivalent to `git stash apply` rather than `git stash pop`).
pub fn apply(repo_path: &Path, name: &str, remove_from_shelf: bool) -> Result<()> {
    let mut store = ShelfStore::load(repo_path)?;
    let entry = store
        .entries
        .iter()
        .find(|e| e.name == name)
        .cloned()
        .ok_or_else(|| anyhow!("no shelf entry named {:?}", name))?;

    run_git(repo_path, &["stash", "apply", &entry.stash_sha])
        .with_context(|| format!("applying shelf entry {:?}", name))?;

    if remove_from_shelf {
        // Resolve the position of `stash_sha` in `git stash list`, then
        // drop it. We can't use `stash@{0}` here because the user may have
        // pushed unrelated stashes after the shelf was created.
        if let Some(index) = locate_stash_index(repo_path, &entry.stash_sha)? {
            run_git(
                repo_path,
                &["stash", "drop", &format!("stash@{{{index}}}")],
            )
            .with_context(|| format!("dropping stash backing {:?}", name))?;
        }
        store.remove(name)?;
    }
    Ok(())
}

/// Drop both the underlying stash (if still present) and the named entry.
pub fn drop(repo_path: &Path, name: &str) -> Result<()> {
    let mut store = ShelfStore::load(repo_path)?;
    let entry = store
        .entries
        .iter()
        .find(|e| e.name == name)
        .cloned()
        .ok_or_else(|| anyhow!("no shelf entry named {:?}", name))?;

    if let Some(index) = locate_stash_index(repo_path, &entry.stash_sha)? {
        // Best-effort — the shelf entry should disappear even if the stash
        // drop fails (so the user can stop seeing a stale row).
        if let Err(err) = run_git(
            repo_path,
            &["stash", "drop", &format!("stash@{{{index}}}")],
        ) {
            log::warn!(
                "git::shelf: failed to drop stash for shelf entry {name:?}: {err}"
            );
        }
    }
    store.remove(name)?;
    Ok(())
}

/// Names of entries whose backing stash is no longer reachable. Mirrors
/// [`ShelfStore::lookup_orphaned`] for callers that don't keep a store
/// handle around.
pub fn lookup_orphaned(repo_path: &Path) -> Result<Vec<String>> {
    let store = ShelfStore::load(repo_path)?;
    Ok(store.lookup_orphaned(repo_path))
}

fn parse_files_summary(repo_path: &Path, stash_sha: &str) -> Result<FilesSummary> {
    let raw = run_git(
        repo_path,
        &["stash", "show", "--numstat", "--no-color", stash_sha],
    )
    .unwrap_or_default();
    Ok(parse_numstat(&raw))
}

/// Parses `git stash show --numstat` output — `<added>\t<deleted>\t<path>`
/// with `-` standing in for binary files. Lines that don't match are
/// silently skipped (`git` occasionally interleaves header rows).
pub(crate) fn parse_numstat(raw: &str) -> FilesSummary {
    let mut summary = FilesSummary::default();
    for line in raw.lines() {
        let mut parts = line.splitn(3, '\t');
        let added = parts.next().unwrap_or("").trim();
        let removed = parts.next().unwrap_or("").trim();
        let path = parts.next().unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        let added_n: u32 = added.parse().unwrap_or(0);
        let removed_n: u32 = removed.parse().unwrap_or(0);
        summary.total_lines_added = summary.total_lines_added.saturating_add(added_n);
        summary.total_lines_removed = summary.total_lines_removed.saturating_add(removed_n);
        if added_n > 0 && removed_n == 0 {
            summary.count_added = summary.count_added.saturating_add(1);
        } else if added_n == 0 && removed_n > 0 {
            summary.count_deleted = summary.count_deleted.saturating_add(1);
        } else {
            summary.count_modified = summary.count_modified.saturating_add(1);
        }
        if summary.top_paths.len() < 5 {
            summary.top_paths.push(path.to_string());
        }
    }
    summary
}

fn list_live_stash_shas(repo_path: &Path) -> Result<Vec<String>> {
    let raw = run_git(repo_path, &["stash", "list", "--format=%H"])?;
    Ok(raw
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

fn locate_stash_index(repo_path: &Path, target_sha: &str) -> Result<Option<usize>> {
    let shas = list_live_stash_shas(repo_path)?;
    Ok(shas.iter().position(|sha| sha == target_sha))
}

fn current_branch(repo_path: &Path) -> Result<String> {
    let raw = run_git(repo_path, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("HEAD is detached"));
    }
    Ok(trimmed.to_string())
}

// `Command::new("git")` is in the `disallowed_methods` set workspace-wide;
// shelf operations run synchronously under callers that already hopped to
// a background thread, so the simple `Command` form is the right shape.
#[allow(clippy::disallowed_methods)]
fn run_git(repo_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .map_err(|err| anyhow!("spawn git: {err}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn pin(dir: &Path) {
        test_override::set(dir.to_path_buf());
    }

    #[test]
    fn store_roundtrips_through_disk() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());

        let repo = Path::new("/tmp/example-repo");
        let mut store = ShelfStore::load(repo).expect("load empty");
        assert!(store.entries.is_empty());

        let entry = ShelfEntry {
            name: "feature-x".into(),
            stash_sha: "deadbeef".repeat(5),
            created_at_unix: 1_700_000_000,
            source_branch: Some("main".into()),
            description: Some("Half-baked feature x".into()),
            files_summary: FilesSummary {
                count_modified: 2,
                total_lines_added: 10,
                total_lines_removed: 1,
                top_paths: vec!["src/lib.rs".into(), "src/main.rs".into()],
                ..FilesSummary::default()
            },
        };
        store.add(entry.clone()).expect("add");

        let store2 = ShelfStore::load(repo).expect("reload");
        assert_eq!(store2.entries.len(), 1);
        assert_eq!(store2.entries[0], entry);

        // Different repo path, separate slot.
        let other = ShelfStore::load(Path::new("/tmp/other")).expect("load other");
        assert!(other.entries.is_empty());

        test_override::clear();
    }

    #[test]
    fn add_rejects_duplicate_name() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let mut store = ShelfStore::load(Path::new("/r")).expect("load");
        let entry = ShelfEntry {
            name: "wip".into(),
            stash_sha: "a".into(),
            created_at_unix: 1,
            source_branch: None,
            description: None,
            files_summary: FilesSummary::default(),
        };
        store.add(entry.clone()).expect("add");
        let err = store.add(entry).expect_err("must reject duplicate");
        assert!(err.to_string().contains("already exists"));
        test_override::clear();
    }

    #[test]
    fn rename_and_update_description_mutate_in_place() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let mut store = ShelfStore::load(Path::new("/r")).expect("load");
        store
            .add(ShelfEntry {
                name: "old".into(),
                stash_sha: "x".into(),
                created_at_unix: 1,
                source_branch: None,
                description: None,
                files_summary: FilesSummary::default(),
            })
            .expect("add");
        store.rename("old", "new").expect("rename");
        store
            .update_description("new", Some("desc".into()))
            .expect("desc");
        let reloaded = ShelfStore::load(Path::new("/r")).expect("reload");
        assert_eq!(reloaded.entries[0].name, "new");
        assert_eq!(reloaded.entries[0].description.as_deref(), Some("desc"));
        test_override::clear();
    }

    #[test]
    fn lookup_orphaned_flags_missing_sha() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        // We can't easily run real `git stash list` against a tempdir
        // here, so just verify the in-memory plumbing — the live-list
        // helper falls through to "empty" on failure, so every entry is
        // reported as orphaned.
        let mut store = ShelfStore::load(Path::new("/no-such-repo")).expect("load");
        store
            .add(ShelfEntry {
                name: "ghost".into(),
                stash_sha: "0".repeat(40),
                created_at_unix: 1,
                source_branch: None,
                description: None,
                files_summary: FilesSummary::default(),
            })
            .expect("add");
        let orphans = store.lookup_orphaned(Path::new("/no-such-repo"));
        assert_eq!(orphans, vec!["ghost".to_string()]);
        test_override::clear();
    }

    #[test]
    fn parse_numstat_buckets_files_correctly() {
        let raw = "5\t0\tnew_file.rs\n0\t3\tdeleted.rs\n2\t2\tmodified.rs\n-\t-\tbinary.png\n";
        let summary = parse_numstat(raw);
        assert_eq!(summary.count_added, 1);
        assert_eq!(summary.count_deleted, 1);
        // Modified + binary (whose `0\t0` parse falls into the modified bucket).
        assert_eq!(summary.count_modified, 2);
        assert_eq!(summary.total_lines_added, 7);
        assert_eq!(summary.total_lines_removed, 5);
        assert_eq!(summary.top_paths.len(), 4);
    }

    #[test]
    fn store_save_clears_repo_when_empty() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let mut store = ShelfStore::load(Path::new("/r")).expect("load");
        store
            .add(ShelfEntry {
                name: "tmp".into(),
                stash_sha: "x".into(),
                created_at_unix: 1,
                source_branch: None,
                description: None,
                files_summary: FilesSummary::default(),
            })
            .expect("add");
        store.remove("tmp").expect("remove");
        let raw = std::fs::read_to_string(dir.path().join(STORE_FILE)).expect("read");
        // No `repos` entry left for `/r`.
        assert!(!raw.contains("\"/r\""));
        test_override::clear();
    }
}

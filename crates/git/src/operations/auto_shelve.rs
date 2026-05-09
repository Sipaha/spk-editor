//! S-SHL — Auto-shelve crash-recovery snapshots.
//!
//! Independent of [`super::shelf`] — this module periodically writes
//! `git diff HEAD` to a `.diff` file under
//! `<temp_dir>/spk-editor-auto-shelve/<repo-hash>/<timestamp>.diff` so a
//! later editor run can offer to recover the working-tree state if the
//! editor died with uncommitted changes. We deliberately do **not** use
//! `git stash` here — that would noise up the user's reflog and
//! `git stash list` for what's meant to be a transparent safety net.
//!
//! Public surface:
//! * [`take_snapshot`] writes a fresh snapshot and trims the directory to
//!   the latest `max_snapshots` files (older entries removed).
//! * [`latest_snapshot`] returns the newest `.diff` for a repo, or `None`.
//! * [`is_recovery_needed`] → true when the working tree is dirty AND
//!   there's a snapshot newer than the last commit on `HEAD`.
//! * [`apply_snapshot`] runs `git apply <diff>`.
//! * [`clear_for_repo`] removes every snapshot for a repo (called on
//!   successful commit and from a manual "discard" action).
//!
//! Override the storage directory in tests via [`test_override`] — pinned
//! per-thread so unit tests can run in parallel without colliding with
//! the shared cache directory.

use anyhow::{Context as _, Result, anyhow};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

const ROOT_SUBDIR: &str = "spk-editor-auto-shelve";
const SNAPSHOT_EXT: &str = "diff";

/// Hex-encoded stable hash of the working-directory absolute path, used
/// as the per-repo bucket name. Mirrors `super::shelf::repo_hash`.
pub fn repo_hash(work_dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    work_dir.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn root_dir() -> PathBuf {
    if let Some(custom) = test_override::current() {
        return custom;
    }
    paths::temp_dir().join(ROOT_SUBDIR)
}

fn repo_dir(work_dir: &Path) -> PathBuf {
    root_dir().join(repo_hash(work_dir))
}

/// Capture `git diff HEAD` for `work_dir` and write it to a fresh
/// timestamped file. Older snapshots beyond `max_snapshots` are deleted.
/// Returns the path to the newly-written snapshot, or `Ok(None)` when the
/// working tree was clean.
pub fn take_snapshot(work_dir: &Path, max_snapshots: u32) -> Result<Option<PathBuf>> {
    if max_snapshots == 0 {
        // 0 = disabled; never write, but proactively clear leftovers.
        clear_for_repo(work_dir).ok();
        return Ok(None);
    }

    let diff = run_git(work_dir, &["diff", "HEAD", "--binary", "--no-color"])
        .context("running `git diff HEAD` for auto-shelve")?;
    if diff.trim().is_empty() {
        return Ok(None);
    }

    let dir = repo_dir(work_dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    apply_dir_perms(&dir).ok();

    let unix = current_unix_seconds();
    let target = dir.join(format!("{unix}.{SNAPSHOT_EXT}"));
    write_snapshot(&target, &diff)?;
    trim_to(&dir, max_snapshots);
    Ok(Some(target))
}

/// Newest snapshot for `work_dir`, or `None` when no snapshots exist.
pub fn latest_snapshot(work_dir: &Path) -> Option<PathBuf> {
    let mut snapshots = list_snapshots(work_dir).ok()?;
    snapshots.pop()
}

/// True when the working tree has uncommitted changes AND the latest
/// auto-shelve snapshot is newer than the most recent `HEAD` commit's
/// timestamp. Use as the gate for showing the "Recover?" modal.
pub fn is_recovery_needed(work_dir: &Path) -> bool {
    let Some(snapshot) = latest_snapshot(work_dir) else {
        return false;
    };
    let working_tree_dirty = run_git(work_dir, &["status", "--porcelain"])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(false);
    if !working_tree_dirty {
        return false;
    }
    let head_unix = run_git(work_dir, &["log", "-1", "--format=%ct"])
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let snapshot_unix = snapshot_timestamp(&snapshot).unwrap_or(0);
    snapshot_unix > head_unix
}

/// Apply the diff at `snapshot` to `work_dir` via `git apply`. The caller
/// is responsible for prompting the user first.
pub fn apply_snapshot(work_dir: &Path, snapshot: &Path) -> Result<()> {
    let snap_str = snapshot
        .to_str()
        .ok_or_else(|| anyhow!("snapshot path is not valid UTF-8: {}", snapshot.display()))?;
    run_git_void(work_dir, &["apply", "--whitespace=nowarn", snap_str])
        .with_context(|| format!("git apply {}", snapshot.display()))
}

/// Remove every auto-shelve snapshot for this repo. Best-effort — a
/// failure to remove the directory is logged and swallowed so the caller
/// (commit completion handler) doesn't surface noise to the user.
pub fn clear_for_repo(work_dir: &Path) -> Result<()> {
    let dir = repo_dir(work_dir);
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))
}

/// Snapshots in *ascending* `<unix>.diff` filename order — the last
/// element is therefore the freshest snapshot.
pub fn list_snapshots(work_dir: &Path) -> Result<Vec<PathBuf>> {
    let dir = repo_dir(work_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e == SNAPSHOT_EXT)
            != Some(true)
        {
            continue;
        }
        if stem.parse::<i64>().is_err() {
            continue;
        }
        out.push(path);
    }
    out.sort();
    Ok(out)
}

fn trim_to(dir: &Path, max_snapshots: u32) {
    let cap = max_snapshots as usize;
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut snapshots: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e == SNAPSHOT_EXT)
                .unwrap_or(false)
        })
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<i64>().ok())
                .is_some()
        })
        .collect();
    snapshots.sort();
    if snapshots.len() <= cap {
        return;
    }
    let excess = snapshots.len() - cap;
    for path in snapshots.into_iter().take(excess) {
        if let Err(err) = fs::remove_file(&path) {
            log::warn!(
                "git::auto_shelve: failed to remove old snapshot {}: {err}",
                path.display()
            );
        }
    }
}

fn snapshot_timestamp(path: &Path) -> Option<i64> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<i64>().ok())
}

fn write_snapshot(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    apply_file_perms(path).ok();
    Ok(())
}

#[cfg(unix)]
fn apply_dir_perms(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn apply_dir_perms(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn apply_file_perms(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn apply_file_perms(_path: &Path) -> Result<()> {
    Ok(())
}

fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[allow(clippy::disallowed_methods)]
fn run_git(work_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(work_dir)
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

fn run_git_void(work_dir: &Path, args: &[&str]) -> Result<()> {
    run_git(work_dir, args).map(|_| ())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn pin(dir: &Path) {
        test_override::set(dir.to_path_buf());
    }

    fn touch_snapshot(dir: &Path, repo_hash: &str, unix: i64, body: &str) -> PathBuf {
        let bucket = dir.join(repo_hash);
        fs::create_dir_all(&bucket).expect("mkdir");
        let path = bucket.join(format!("{unix}.{SNAPSHOT_EXT}"));
        fs::write(&path, body).expect("write");
        path
    }

    #[test]
    fn list_snapshots_orders_by_filename_unix() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let repo = Path::new("/r1");
        let key = repo_hash(repo);
        touch_snapshot(dir.path(), &key, 100, "a");
        touch_snapshot(dir.path(), &key, 300, "b");
        touch_snapshot(dir.path(), &key, 200, "c");
        let snapshots = list_snapshots(repo).expect("list");
        let names: Vec<_> = snapshots
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["100.diff", "200.diff", "300.diff"]);
        let latest = latest_snapshot(repo).expect("latest");
        assert!(latest.ends_with("300.diff"));
        test_override::clear();
    }

    #[test]
    fn trim_keeps_only_latest_n() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let repo = Path::new("/r2");
        let key = repo_hash(repo);
        for unix in &[100, 200, 300, 400, 500, 600, 700] {
            touch_snapshot(dir.path(), &key, *unix, "x");
        }
        let bucket = dir.path().join(&key);
        trim_to(&bucket, 3);
        let snapshots = list_snapshots(repo).expect("list");
        let names: Vec<_> = snapshots
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["500.diff", "600.diff", "700.diff"]);
        test_override::clear();
    }

    #[test]
    fn clear_for_repo_removes_bucket() {
        let dir = tempdir().expect("tempdir");
        pin(dir.path());
        let repo = Path::new("/r3");
        let key = repo_hash(repo);
        touch_snapshot(dir.path(), &key, 1, "x");
        clear_for_repo(repo).expect("clear");
        assert!(list_snapshots(repo).expect("list").is_empty());
        test_override::clear();
    }

    #[test]
    fn snapshot_timestamp_parses_filename() {
        let path = PathBuf::from("/whatever/123456.diff");
        assert_eq!(snapshot_timestamp(&path), Some(123_456));
        let bad = PathBuf::from("/whatever/not-a-number.diff");
        assert_eq!(snapshot_timestamp(&bad), None);
    }

    #[allow(clippy::disallowed_methods)]
    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn init_dirty_repo() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        run(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("README"), "x").expect("write");
        run(dir.path(), &["add", "README"]);
        run(dir.path(), &["-c", "user.name=T", "-c", "user.email=t@x", "commit", "-qm", "init"]);
        std::fs::write(dir.path().join("README"), "y").expect("dirty");
        dir
    }

    /// A fake snapshot whose filename timestamp is well after the HEAD
    /// commit time should make `is_recovery_needed` return true on a
    /// dirty working tree.
    #[test]
    fn recovery_probe_fires_on_dirty_repo_with_newer_snapshot() {
        let cache = tempdir().expect("cache tempdir");
        pin(cache.path());
        let repo = init_dirty_repo();
        let key = repo_hash(repo.path());
        let future = current_unix_seconds() + 3_600;
        touch_snapshot(cache.path(), &key, future, "fake diff");
        assert!(is_recovery_needed(repo.path()));
        test_override::clear();
    }

    /// A clean working tree must always return false, even if a snapshot
    /// exists (the user committed and the runner should clean up).
    #[test]
    fn recovery_probe_silent_on_clean_repo() {
        let cache = tempdir().expect("cache tempdir");
        pin(cache.path());
        let dir = tempdir().expect("repo tempdir");
        run(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("README"), "x").expect("write");
        run(dir.path(), &["add", "README"]);
        run(dir.path(), &["-c", "user.name=T", "-c", "user.email=t@x", "commit", "-qm", "init"]);
        let key = repo_hash(dir.path());
        let future = current_unix_seconds() + 3_600;
        touch_snapshot(cache.path(), &key, future, "fake");
        assert!(!is_recovery_needed(dir.path()));
        test_override::clear();
    }
}

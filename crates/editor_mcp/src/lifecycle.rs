//! MCP server lifecycle: lock acquisition, server bind, graceful shutdown.
use anyhow::Result;
use fs2::FileExt;
use gpui::App;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug)]
pub struct SingleInstanceLock {
    file: File,
}

#[derive(Debug)]
pub enum LockError {
    Busy { holder_pid: Option<u32> },
    Io(std::io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Busy { holder_pid: Some(pid) } => {
                write!(f, "another spk-editor instance holds the lock (PID {pid})")
            }
            LockError::Busy { holder_pid: None } => {
                write!(f, "another spk-editor instance holds the lock")
            }
            LockError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for LockError {}

impl SingleInstanceLock {
    pub fn acquire(path: &Path) -> std::result::Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LockError::Io)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(LockError::Io)?;

        if FileExt::try_lock_exclusive(&file).is_err() {
            let mut body = String::new();
            file.read_to_string(&mut body).ok();
            let holder_pid = body.trim().parse::<u32>().ok();
            return Err(LockError::Busy { holder_pid });
        }
        file.set_len(0).map_err(LockError::Io)?;
        let pid = std::process::id();
        writeln!(file, "{pid}").map_err(LockError::Io)?;
        file.sync_all().map_err(LockError::Io)?;
        Ok(SingleInstanceLock { file })
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        FileExt::unlock(&self.file).ok();
    }
}

pub fn start_server(_cx: &mut App) -> Result<()> {
    // Stub — implemented in Task 1.4.
    Ok(())
}

#[cfg(test)]
pub fn start_server_for_test(_cx: &mut App) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn acquire_lock_writes_pid() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("mcp.lock");
        let lock = SingleInstanceLock::acquire(&lock_path).expect("acquire");
        let body = std::fs::read_to_string(&lock_path).expect("read");
        let pid: u32 = body.trim().parse().expect("pid is u32");
        assert_eq!(pid, std::process::id());
        drop(lock);
    }

    #[test]
    fn second_acquire_fails_while_held() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("mcp.lock");
        let lock = SingleInstanceLock::acquire(&lock_path).expect("first");
        match SingleInstanceLock::acquire(&lock_path) {
            Err(LockError::Busy { holder_pid }) => {
                assert_eq!(holder_pid, Some(std::process::id()));
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        drop(lock);
    }

    #[test]
    fn release_then_reacquire_works() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("mcp.lock");
        {
            let _lock = SingleInstanceLock::acquire(&lock_path).expect("first");
        }
        let _lock = SingleInstanceLock::acquire(&lock_path).expect("second");
    }
}

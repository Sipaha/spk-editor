//! Chunked upload manager for the WebSocket binary-frame attachment path.
//!
//! Mobile clients open an upload via `solution_agent.upload_init`, push raw
//! bytes as WS binary frames (16-byte header `u64 upload_id BE | u64 offset
//! BE` + payload) handled by the listener in `remote_control::listener`, then
//! `solution_agent.upload_finish` to commit. The resulting handle string
//! `spk-upload://<id>` is embedded as a `ResourceLink` in
//! `send_message_blocks`, which resolves it to an inline `Image` (or text)
//! block immediately before forwarding to ACP.
//!
//! Storage is per-process: a `OnceLock<Arc<Mutex<UploadManager>>>` lives in
//! this module and is shared between MCP tools (GPUI context) and the WS
//! listener (pure tokio context). The mutex is a `std::sync::Mutex` — every
//! method is short, all I/O is synchronous file writes, and the contention is
//! single-digit ops per second per active upload.
//!
//! Tmp files are kept under `<editor_mcp runtime_dir>/uploads/<id>.bin` so
//! they share the lifetime of the editor's runtime dir (cleaned by GC on the
//! editor side, plus the OS-level tempdir conventions if the runtime_dir
//! itself is a tempdir during tests).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};

/// Public id for an in-flight upload. u64 so the wire encoding maps 1:1 to
/// 8 BE bytes inside the binary-frame header.
pub type UploadId = u64;

/// Per-session cap. With four active uploads (e.g. four images) the total tmp
/// budget per session is bounded by `4 * max_total_size` — high enough for
/// realistic multi-image messages, low enough to prevent runaway state.
const MAX_CONCURRENT_PER_SESSION: usize = 4;

/// 1 hour TTL — uploads that aren't finished within this window get GCed.
pub const UPLOAD_TTL: Duration = Duration::from_secs(60 * 60);

/// State for a single in-flight upload. `received_bytes` is sequential — see
/// [`UploadManager::write_chunk`].
pub struct UploadState {
    pub id: UploadId,
    pub session_id: String,
    pub mime: String,
    pub display_name: String,
    pub expected_size: u64,
    pub tmp_path: PathBuf,
    pub received_bytes: u64,
    pub created_at: Instant,
    pub sha256: Option<String>,
    file: File,
}

/// Successful `finish` result. The tmp file lives until the caller either
/// reads it (via [`UploadManager::resolve`]) and explicitly aborts the entry,
/// or until the GC reaps it.
#[derive(Debug)]
pub struct UploadHandle {
    pub id: UploadId,
    pub tmp_path: PathBuf,
    pub mime: String,
    pub display_name: String,
}

/// Ack event drained on the GPUI side so `editor_mcp::emit_notification` can
/// be called with a real `&App`. The listener (tokio context) only ever
/// pushes; the consumer side lives in `solution_agent::init`.
#[derive(Clone, Debug)]
pub struct ChunkAck {
    pub upload_id: UploadId,
    pub received_bytes: u64,
}

pub struct UploadManager {
    state: HashMap<UploadId, UploadState>,
    next_id: AtomicU64,
    tmp_root: PathBuf,
    /// Buffer of acked chunk events the GPUI-side drainer turns into
    /// `editor_mcp::emit_notification` calls. Bounded by the rate of
    /// inbound chunks; the drainer empties it on every tick.
    ack_queue: Vec<ChunkAck>,
}

impl UploadManager {
    pub fn new(tmp_root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&tmp_root)
            .map_err(|err| anyhow!("creating upload tmp_root {tmp_root:?}: {err}"))?;
        // Seed `next_id` from wall clock so an editor restart that lands a
        // burst of fresh uploads doesn't reuse low ids that an in-flight
        // client may still be talking to. Wall-clock seed is monotonic
        // enough for the purpose (uniqueness within a process).
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1);
        Ok(Self {
            state: HashMap::new(),
            next_id: AtomicU64::new(seed.max(1)),
            tmp_root,
            ack_queue: Vec::new(),
        })
    }

    /// Allocate an upload id, open its tmp file, and register the slot.
    /// Validates the per-session concurrency cap to bound disk footprint.
    pub fn init(
        &mut self,
        session_id: String,
        mime: String,
        display_name: String,
        total_size: u64,
        sha256: Option<String>,
    ) -> Result<UploadId> {
        if session_id.is_empty() {
            bail!("upload_init: session_id is required");
        }
        let active_for_session = self
            .state
            .values()
            .filter(|s| s.session_id == session_id)
            .count();
        if active_for_session >= MAX_CONCURRENT_PER_SESSION {
            bail!(
                "upload_init: session {session_id} already has {MAX_CONCURRENT_PER_SESSION} concurrent uploads — finish or abort one first"
            );
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let tmp_path = self.tmp_root.join(format!("{id}.bin"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|err| anyhow!("creating tmp file {tmp_path:?}: {err}"))?;
        self.state.insert(
            id,
            UploadState {
                id,
                session_id,
                mime,
                display_name,
                expected_size: total_size,
                tmp_path,
                received_bytes: 0,
                created_at: Instant::now(),
                sha256,
                file,
            },
        );
        Ok(id)
    }

    /// Append `data` at `offset`. Refuses non-sequential writes (offset must
    /// equal current `received_bytes`) — V1 wire protocol is in-order to
    /// avoid sparse-file handling. Returns the new cumulative `received_bytes`.
    pub fn write_chunk(&mut self, id: UploadId, offset: u64, data: &[u8]) -> Result<u64> {
        let entry = self
            .state
            .get_mut(&id)
            .ok_or_else(|| anyhow!("write_chunk: unknown upload_id {id}"))?;
        if offset != entry.received_bytes {
            bail!(
                "write_chunk: out-of-order chunk for upload {id} — got offset {offset}, expected {}",
                entry.received_bytes
            );
        }
        let new_total = entry
            .received_bytes
            .saturating_add(data.len() as u64);
        if new_total > entry.expected_size {
            bail!(
                "write_chunk: upload {id} overrun — total {new_total} > expected {expected}",
                expected = entry.expected_size,
            );
        }
        entry
            .file
            .seek(SeekFrom::Start(offset))
            .map_err(|err| anyhow!("seek({offset}) on upload {id}: {err}"))?;
        entry
            .file
            .write_all(data)
            .map_err(|err| anyhow!("write_all on upload {id}: {err}"))?;
        // No `flush()`: the file is `O_RDWR`, kernel page cache is fine for
        // an upload that lives a few seconds and is read back via
        // `std::fs::read` on `finish`. If we ever need crash-safety across
        // editor restarts (we don't — TTL drops dangling uploads anyway),
        // promote this to `sync_all()`.
        entry.received_bytes = new_total;
        self.ack_queue.push(ChunkAck {
            upload_id: id,
            received_bytes: new_total,
        });
        Ok(new_total)
    }

    pub fn status(&self, id: UploadId) -> Option<(u64, u64)> {
        self.state
            .get(&id)
            .map(|s| (s.received_bytes, s.expected_size))
    }

    /// Verify the upload is complete (and optionally matches the caller-
    /// supplied sha256), then return a handle to the tmp file. The entry
    /// stays in the map so `resolve` callers (e.g. `send_message_blocks`)
    /// can find it.
    pub fn finish(
        &mut self,
        id: UploadId,
        expected_sha256: Option<&str>,
    ) -> Result<UploadHandle> {
        let entry = self
            .state
            .get(&id)
            .ok_or_else(|| anyhow!("finish: unknown upload_id {id}"))?;
        if entry.received_bytes != entry.expected_size {
            bail!(
                "finish: upload {id} incomplete — received {} of {} bytes",
                entry.received_bytes,
                entry.expected_size
            );
        }
        // Sha256 verification: prefer the per-call argument, fall back to the
        // hash supplied at `init` if any. Mismatch → don't expose the handle,
        // but DON'T abort the entry either (caller can inspect status and
        // decide whether to retry).
        let expected = expected_sha256.or(entry.sha256.as_deref());
        if let Some(want) = expected {
            let actual = sha256_of_path(&entry.tmp_path)?;
            if !want.eq_ignore_ascii_case(&actual) {
                bail!(
                    "finish: upload {id} sha256 mismatch — expected {want}, computed {actual}",
                );
            }
        }
        Ok(UploadHandle {
            id: entry.id,
            tmp_path: entry.tmp_path.clone(),
            mime: entry.mime.clone(),
            display_name: entry.display_name.clone(),
        })
    }

    /// Drop the upload's state and delete the tmp file. Safe to call after
    /// `finish` (typical: `resolve` reads the file, then aborts) or to
    /// cancel an in-flight upload.
    pub fn abort(&mut self, id: UploadId) -> Result<()> {
        let entry = self
            .state
            .remove(&id)
            .ok_or_else(|| anyhow!("abort: unknown upload_id {id}"))?;
        if let Err(err) = std::fs::remove_file(&entry.tmp_path) {
            // tmp file may already be gone (e.g. test cleanup); log and
            // continue rather than failing the caller.
            log::debug!(
                "upload::abort: remove_file({}) failed: {err}",
                entry.tmp_path.display()
            );
        }
        Ok(())
    }

    /// Prune entries older than `ttl`. Returns the number reaped. Caller
    /// passes `Instant::now()` so tests can shift the clock.
    pub fn gc(&mut self, now: Instant, ttl: Duration) -> usize {
        let mut expired = Vec::new();
        for (id, entry) in self.state.iter() {
            if now.saturating_duration_since(entry.created_at) > ttl {
                expired.push((*id, entry.tmp_path.clone()));
            }
        }
        let count = expired.len();
        for (id, path) in expired {
            self.state.remove(&id);
            if let Err(err) = std::fs::remove_file(&path) {
                log::debug!("upload::gc: remove_file({}) failed: {err}", path.display());
            }
        }
        count
    }

    /// Inspect an entry without consuming it — used by
    /// `send_message_blocks` to map `spk-upload://<id>` ResourceLinks to
    /// inline content.
    pub fn resolve(&self, id: UploadId) -> Option<&UploadState> {
        self.state.get(&id)
    }

    /// Drain queued chunk acks. Called from the GPUI-side drainer task in
    /// `solution_agent::init` which then fans each one out as a
    /// `upload_chunk_acked` notification. Returns an owned Vec so the
    /// drainer can release the mutex before emitting.
    pub fn drain_acks(&mut self) -> Vec<ChunkAck> {
        std::mem::take(&mut self.ack_queue)
    }
}

fn sha256_of_path(path: &std::path::Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|err| anyhow!("opening {path:?} for sha256: {err}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|err| anyhow!("hashing {path:?}: {err}"))?;
    let digest = hasher.finalize();
    Ok(hex_encode(&digest))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Process-wide handle. `solution_agent::init` calls
/// [`install`] once; the WS listener (tokio context) and MCP tool
/// callbacks (GPUI context) both reach in via [`with_manager`] or the
/// raw [`get`] accessor.
static MANAGER: OnceLock<Arc<Mutex<UploadManager>>> = OnceLock::new();

/// Set the global manager. Idempotent only with the same instance — a
/// second call with a different `Arc` is silently ignored (`OnceLock`).
pub fn install(manager: Arc<Mutex<UploadManager>>) {
    let _ = MANAGER.set(manager);
}

/// Borrow the global manager. `None` if [`install`] hasn't been called yet
/// (i.e. `solution_agent::init` hasn't run — should only happen in
/// pre-init test harnesses).
pub fn get() -> Option<Arc<Mutex<UploadManager>>> {
    MANAGER.get().cloned()
}

/// Convenience wrapper: lock and call `f`. Returns `None` if there is no
/// installed manager OR if the mutex is poisoned (logged at warn).
pub fn with_manager<R, F: FnOnce(&mut UploadManager) -> R>(f: F) -> Option<R> {
    let arc = get()?;
    match arc.lock() {
        Ok(mut guard) => Some(f(&mut guard)),
        Err(err) => {
            log::warn!("upload::with_manager: mutex poisoned: {err}");
            None
        }
    }
}

/// Reserved scheme for upload handle URIs. Mobile clients embed strings
/// like `spk-upload://42` as `ResourceLink.uri`; the
/// `send_message_blocks` resolver swaps them for inline content.
pub const HANDLE_SCHEME: &str = "spk-upload://";

/// Mirror of the mobile encoder's text-mime allow-list
/// (`mobile/.../UserMessageBlocks.kt`). Anything matching is rendered as a
/// fenced code block; everything else (and non-text non-image mimes) is
/// rejected by `send_message_blocks` resolution.
pub fn is_text_like(mime: &str) -> bool {
    if mime.starts_with("text/") {
        return true;
    }
    matches!(
        mime,
        "application/json"
            | "application/xml"
            | "application/x-yaml"
            | "application/yaml"
            | "application/javascript"
            | "application/typescript"
            | "application/sql"
            | "application/x-sh"
    )
}

/// Best-effort markdown fence hint for a text-like upload. Falls back to
/// the empty string when we don't have a better suggestion — markdown
/// renderers handle empty fences fine.
pub fn extension_from_mime(mime: &str) -> &'static str {
    match mime {
        "text/plain" => "",
        "text/markdown" => "markdown",
        "text/html" => "html",
        "text/css" => "css",
        "text/csv" => "csv",
        "text/x-rust" | "text/rust" => "rust",
        "text/x-python" => "python",
        "text/x-go" => "go",
        "text/x-kotlin" => "kotlin",
        "text/x-java" => "java",
        "text/x-c" | "text/x-csrc" => "c",
        "text/x-c++" | "text/x-c++src" => "cpp",
        "text/x-script.shell" => "bash",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "application/x-yaml" | "application/yaml" | "text/yaml" | "text/x-yaml" => "yaml",
        "application/javascript" | "text/javascript" => "javascript",
        "application/typescript" => "typescript",
        "application/sql" => "sql",
        "application/x-sh" => "bash",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use tempfile::tempdir;

    fn mgr() -> (UploadManager, tempfile::TempDir) {
        let dir = tempdir().expect("tempdir");
        let m = UploadManager::new(dir.path().to_path_buf()).expect("new");
        (m, dir)
    }

    #[test]
    fn init_then_write_sequential_chunks() {
        let (mut m, _dir) = mgr();
        let id = m
            .init(
                "session-1".into(),
                "image/png".into(),
                "pic.png".into(),
                10,
                None,
            )
            .expect("init");
        let r1 = m.write_chunk(id, 0, &[1, 2, 3, 4]).expect("c1");
        assert_eq!(r1, 4);
        assert_eq!(m.status(id), Some((4, 10)));
        let r2 = m
            .write_chunk(id, 4, &[5, 6, 7, 8, 9, 10])
            .expect("c2");
        assert_eq!(r2, 10);
        let acks = m.drain_acks();
        assert_eq!(acks.len(), 2);
        assert_eq!(acks[1].received_bytes, 10);
    }

    #[test]
    fn write_chunk_rejects_wrong_offset() {
        let (mut m, _dir) = mgr();
        let id = m
            .init("s".into(), "image/png".into(), "a".into(), 8, None)
            .expect("init");
        m.write_chunk(id, 0, &[1, 2, 3]).expect("c1");
        let err = m.write_chunk(id, 10, &[9, 9]).unwrap_err().to_string();
        assert!(
            err.contains("out-of-order"),
            "expected out-of-order error, got: {err}"
        );
    }

    #[test]
    fn write_chunk_rejects_overrun() {
        let (mut m, _dir) = mgr();
        let id = m
            .init("s".into(), "image/png".into(), "a".into(), 4, None)
            .expect("init");
        let err = m
            .write_chunk(id, 0, &[1, 2, 3, 4, 5])
            .unwrap_err()
            .to_string();
        assert!(err.contains("overrun"), "got: {err}");
    }

    #[test]
    fn finish_returns_handle_on_complete() {
        let (mut m, _dir) = mgr();
        let id = m
            .init("s".into(), "image/png".into(), "pic.png".into(), 4, None)
            .expect("init");
        m.write_chunk(id, 0, &[1, 2, 3, 4]).expect("c");
        let handle = m.finish(id, None).expect("finish");
        assert_eq!(handle.id, id);
        assert_eq!(handle.mime, "image/png");
        assert_eq!(handle.display_name, "pic.png");
        assert!(handle.tmp_path.exists());
    }

    #[test]
    fn finish_rejects_incomplete_upload() {
        let (mut m, _dir) = mgr();
        let id = m
            .init("s".into(), "image/png".into(), "a".into(), 10, None)
            .expect("init");
        m.write_chunk(id, 0, &[1, 2, 3]).expect("c");
        let err = m.finish(id, None).unwrap_err().to_string();
        assert!(err.contains("incomplete"), "got: {err}");
    }

    #[test]
    fn finish_verifies_sha256_match_and_mismatch() {
        let (mut m, _dir) = mgr();
        let data = b"hello world!";
        let mut hasher = Sha256::new();
        hasher.update(data);
        let want = hex_encode(&hasher.finalize());

        let id = m
            .init(
                "s".into(),
                "text/plain".into(),
                "hi.txt".into(),
                data.len() as u64,
                None,
            )
            .expect("init");
        m.write_chunk(id, 0, data).expect("c");
        let _ok = m.finish(id, Some(&want)).expect("sha256 match");

        // Mismatch — `init` still has the entry (finish doesn't drop) so
        // we can re-verify with a wrong hash and assert it fails.
        let err = m
            .finish(id, Some("ff00ff00".repeat(8).as_str()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("sha256 mismatch"),
            "got: {err}"
        );
    }

    #[test]
    fn abort_removes_tmp_file_and_entry() {
        let (mut m, _dir) = mgr();
        let id = m
            .init("s".into(), "image/png".into(), "a".into(), 4, None)
            .expect("init");
        m.write_chunk(id, 0, &[1, 2, 3, 4]).expect("c");
        let tmp = m.resolve(id).map(|s| s.tmp_path.clone()).expect("resolve");
        assert!(tmp.exists());
        m.abort(id).expect("abort");
        assert!(m.resolve(id).is_none());
        assert!(!tmp.exists());
    }

    #[test]
    fn gc_prunes_expired_entries() {
        let (mut m, _dir) = mgr();
        let id = m
            .init("s".into(), "image/png".into(), "a".into(), 4, None)
            .expect("init");
        m.write_chunk(id, 0, &[1, 2, 3, 4]).expect("c");
        // Need real elapsed time for `created_at` — sleep 10ms, then call
        // gc with a 1ms TTL.
        sleep(Duration::from_millis(10));
        let reaped = m.gc(Instant::now(), Duration::from_millis(1));
        assert_eq!(reaped, 1);
        assert!(m.resolve(id).is_none());
    }

    #[test]
    fn per_session_cap_enforced() {
        let (mut m, _dir) = mgr();
        for _ in 0..MAX_CONCURRENT_PER_SESSION {
            m.init("s".into(), "image/png".into(), "a".into(), 1, None)
                .expect("init within cap");
        }
        let err = m
            .init("s".into(), "image/png".into(), "a".into(), 1, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("concurrent uploads"), "got: {err}");
    }

    #[test]
    fn is_text_like_covers_mobile_mimes() {
        assert!(is_text_like("text/plain"));
        assert!(is_text_like("application/json"));
        assert!(is_text_like("application/x-yaml"));
        assert!(!is_text_like("image/png"));
        assert!(!is_text_like("application/octet-stream"));
    }
}

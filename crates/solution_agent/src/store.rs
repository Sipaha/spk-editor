use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use agent_client_protocol::schema as acp;
use anyhow::{Result, anyhow};
use chrono::Utc;
use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, SharedString, Subscription,
    Task,
};
use solutions::{Solution, SolutionId, SolutionStore, SolutionStoreEvent};
use util::ResultExt;

use crate::adapter::AdapterRegistry;
use crate::db::SolutionAgentDb;
use crate::model::{
    AgentServerId, SessionState, SolutionSession, SolutionSessionId, SolutionSessionMetadata,
};
use crate::notifier;
use crate::pool::SubprocessPool;

mod connection_pool;
mod queue;
#[cfg(test)]
mod tests;

pub(crate) use queue::{QUEUE_MARKER_BODY_SEP, QUEUE_MARKER_PREFIX};

pub struct SolutionAgentStore {
    sessions: HashMap<SolutionSessionId, Entity<SolutionSession>>,
    by_solution: HashMap<SolutionId, Vec<SolutionSessionId>>,
    pool: parking_lot::Mutex<SubprocessPool>,
    persistence: Option<Arc<SolutionAgentDb>>,
    pub(crate) adapters: Arc<AdapterRegistry>,
    /// Map of `AgentServerId -> Rc<dyn AgentServer>`. Real `agent_servers`
    /// instances live per-Project (via `Project::agent_server_store`), but
    /// `SolutionAgentStore` is global-scoped — so we keep a fork-local lookup
    /// table that production wiring will populate at app init and tests
    /// populate manually. Held in an `Rc` because `dyn AgentServer` is `!Sync`.
    server_registry: HashMap<AgentServerId, Rc<dyn agent_servers::AgentServer>>,
    /// Set by the navigator (Phase 6) so `mutate_state` can ask "is this
    /// session currently focused in the UI?" before deciding whether to
    /// fire an OS notification. Stored as `Fn(&App) -> bool` rather than
    /// `Fn(&Context<Self>) -> bool` because `Context` is parameterised on
    /// `Self`, which makes the trait object generic and unstorable. `&App`
    /// is the strict supertype the resolver actually needs.
    pub focus_resolver: Option<Arc<dyn Fn(SolutionSessionId, &gpui::App) -> bool + Send + Sync>>,
    _solution_subscription: Option<Subscription>,
}

#[derive(Debug)]
pub enum SolutionAgentStoreEvent {
    SessionCreated(SolutionSessionId),
    SessionClosed(SolutionSessionId),
    SessionStateChanged(SolutionSessionId),
    SessionTitleChanged(SolutionSessionId),
    SessionMessageAppended(SolutionSessionId),
    SessionNotified(SolutionSessionId, notifier::NotifyKind),
}

impl EventEmitter<SolutionAgentStoreEvent> for SolutionAgentStore {}

struct GlobalSolutionAgentStore(Entity<SolutionAgentStore>);
impl Global for GlobalSolutionAgentStore {}

/// On-disk snapshot of a session. Persisted as a JSON blob in the
/// `acp_thread_blob` column so MCP / future archive UIs can rehydrate
/// the conversation transcript even after the session was closed.
///
/// Public so downstream tools (`solution_agent.read_session_history`)
/// can deserialize the same blob the store wrote.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct PersistedSession {
    pub title: String,
    /// Structured per-entry record used by the navigator's cold-tab
    /// renderer to display the dialog without spawning the agent. New
    /// builds always populate this; blobs written by older builds
    /// decode with `entries: vec![]` and the cold-renderer falls back
    /// to flat-rendering `entry_summaries`.
    #[serde(default)]
    pub entries: Vec<PersistedEntry>,
    /// Legacy flat markdown summaries — one string per thread entry.
    /// Kept populated alongside `entries` for backwards compat with the
    /// `solution_agent.read_session_history` MCP tool, which slices
    /// this list directly.
    pub entry_summaries: Vec<String>,
}

pub use crate::model::{PersistedEntry, PersistedRole};

/// First user prompt, normalised to a single line and truncated, for the
/// History popover label. Returns `None` if the thread has no user message
/// yet — caller's COALESCE keeps the previously-stored preview in that case.
fn extract_preview(entries: &[acp_thread::AgentThreadEntry]) -> Option<gpui::SharedString> {
    let first_user = entries.iter().find_map(|entry| match entry {
        acp_thread::AgentThreadEntry::UserMessage(msg) => Some(msg),
        _ => None,
    })?;
    // `chunks` is the raw ACP payload from the agent and contains the user's
    // typed text verbatim; `content` is the same data wrapped in a render-
    // ready `Markdown` entity that requires `&App` to read. We don't have
    // `cx` here (called from event-handler contexts that already hold a
    // mutable borrow of the store), so we walk chunks instead.
    let mut text = String::new();
    for chunk in &first_user.chunks {
        let chunk_text = match chunk {
            acp::ContentBlock::Text(t) => t.text.as_str(),
            _ => continue,
        };
        if !text.is_empty() && !text.ends_with(' ') {
            text.push(' ');
        }
        text.push_str(chunk_text);
        if text.len() >= 200 {
            break;
        }
    }
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let truncated = if collapsed.chars().count() > 80 {
        let mut s: String = collapsed.chars().take(77).collect();
        s.push('…');
        s
    } else {
        collapsed
    };
    Some(gpui::SharedString::from(truncated))
}

/// Placeholder title for a brand-new session, before claude-acp emits a
/// `TitleUpdated` describing the actual conversation. Keeps the tab
/// readable: 5 hex chars of the UUID is enough to disambiguate adjacent
/// tabs without smearing the entire UUID across the strip.
#[allow(dead_code)]
fn short_session_title(session_id: SolutionSessionId) -> SharedString {
    // SolutionSessionId is already 8 chars — no trimming needed; the
    // raw form is short enough to read at a glance and uniquely
    // identifies the session in `.agents/<id>/` paths.
    SharedString::from(session_id.to_string())
}

/// Resolve the catalog project name for `cwd` if `cwd` matches one of
/// `solution.members`'s `local_path`s. Returns `None` for `solution.root`
/// (the "Solution root" choice in the New Session popover) and for any
/// path that doesn't map to a registered member — caller decides how to
/// label those (status row says "ROOT", title default uses
/// `solution.name`).
pub(crate) fn project_name_for_cwd(
    solution: &Solution,
    cwd: &std::path::Path,
    cx: &App,
) -> Option<SharedString> {
    if cwd.as_os_str().is_empty() || cwd == solution.root {
        return None;
    }
    let member = solution.members.iter().find(|m| m.local_path == cwd)?;
    let store = SolutionStore::try_global(cx)?;
    store.read_with(cx, |s, _| {
        s.catalog()
            .iter()
            .find(|c| c.id == member.catalog_id)
            .map(|c| SharedString::from(c.name.clone()))
    })
}

/// Pick a tab title that doesn't collide with any existing session in
/// the same Solution. First call returns `base`; subsequent collisions
/// get ` 2`, ` 3`, … appended (matching the "Untitled 2 / 3" convention
/// the rest of the editor uses for duplicate names). Caps at 1000 just
/// to avoid an infinite loop on a pathological state — practically
/// nobody opens 1000 sessions of the same project in one Solution.
fn unique_session_title(
    base: &str,
    store: &SolutionAgentStore,
    solution_id: &SolutionId,
    cx: &App,
) -> SharedString {
    let existing: std::collections::HashSet<String> = store
        .by_solution
        .get(solution_id)
        .into_iter()
        .flatten()
        .filter_map(|sid| store.sessions.get(sid))
        .map(|s| s.read(cx).title.to_string())
        .collect();
    if !existing.contains(base) {
        return SharedString::from(base.to_string());
    }
    for n in 2..1000 {
        let candidate = format!("{base} {n}");
        if !existing.contains(&candidate) {
            return SharedString::from(candidate);
        }
    }
    SharedString::from(base.to_string())
}

fn serializable_snapshot(session: &SolutionSession, cx: &App) -> Vec<u8> {
    let entries = session
        .acp_thread
        .as_ref()
        .map(|thread| {
            thread
                .read(cx)
                .entries()
                .iter()
                .map(|entry| PersistedEntry {
                    role: persisted_role_for(entry),
                    markdown: entry.to_markdown(cx),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let entry_summaries = entries.iter().map(|e| e.markdown.clone()).collect();
    let snapshot = PersistedSession {
        title: session.title.to_string(),
        entries,
        entry_summaries,
    };
    serde_json::to_vec(&snapshot).unwrap_or_default()
}

fn persisted_role_for(entry: &acp_thread::AgentThreadEntry) -> PersistedRole {
    match entry {
        acp_thread::AgentThreadEntry::UserMessage(_) => PersistedRole::User,
        acp_thread::AgentThreadEntry::AssistantMessage(_) => PersistedRole::Assistant,
        acp_thread::AgentThreadEntry::ToolCall(_) => PersistedRole::Tool,
        acp_thread::AgentThreadEntry::CompletedPlan(_) => PersistedRole::Plan,
    }
}

impl SolutionAgentStore {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalSolutionAgentStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalSolutionAgentStore>()
            .map(|g| g.0.clone())
    }

    pub fn init_global(cx: &mut App, adapters: Arc<AdapterRegistry>) {
        let entity = cx.new(|cx| Self::new_in_app(adapters, cx));
        cx.set_global(GlobalSolutionAgentStore(entity));
    }

    fn new_in_app(adapters: Arc<AdapterRegistry>, cx: &mut Context<Self>) -> Self {
        // SolutionStore subscription is opt-in here: in tests SolutionStore
        // may not be initialised, so we tolerate its absence by checking
        // `try_global` (the public sentinel for "is solutions::init done?").
        let solution_subscription = SolutionStore::try_global(cx)
            .map(|store| cx.subscribe(&store, Self::on_solution_event));
        Self {
            sessions: HashMap::new(),
            by_solution: HashMap::new(),
            pool: parking_lot::Mutex::new(SubprocessPool::new()),
            persistence: None,
            adapters,
            server_registry: HashMap::new(),
            focus_resolver: None,
            _solution_subscription: solution_subscription,
        }
    }

    /// Register an `AgentServer` instance under the given id so that
    /// `create_session` can look it up. Production wiring registers
    /// `CustomAgentServer::new(...)` for each known agent at app init;
    /// tests register a `MockAgentServer`.
    pub fn register_agent_server(
        &mut self,
        agent_id: AgentServerId,
        server: Rc<dyn agent_servers::AgentServer>,
    ) {
        self.server_registry.insert(agent_id, server);
    }

    pub fn registered_agent_server(
        &self,
        agent_id: &AgentServerId,
    ) -> Option<Rc<dyn agent_servers::AgentServer>> {
        self.server_registry.get(agent_id).cloned()
    }

    pub fn set_persistence(&mut self, db: Arc<SolutionAgentDb>) {
        self.persistence = Some(db);
    }

    /// Returns the database handle if set. Used by the navigator to list
    /// historic sessions (those persisted across editor restarts) for the
    /// "Resume" / "Continue last session" affordances.
    pub fn db(&self) -> Option<Arc<SolutionAgentDb>> {
        self.persistence.clone()
    }

    /// Create a new ACP session for `(solution_id, agent_id)`, multiplexed
    /// onto a shared subprocess via the pool. The caller passes the `project`
    /// to use for the session: production callers pass the active workspace's
    /// `Entity<Project>`; tests pass a `Project::test`-built entity.
    ///
    /// Synthetic single-worktree projects per session were considered (see
    /// `pool::make_production_project_for_solution`) but defer to a follow-up
    /// — the AgentServer's `connect()` path is tightly coupled to a
    /// per-Project `AgentServerStore`, so re-using the workspace project is
    /// the diff-minimal choice today.
    pub fn create_session(
        &mut self,
        solution_id: SolutionId,
        agent_id: AgentServerId,
        project: Entity<project::Project>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        self.create_session_with_cwd(solution_id, agent_id, project, None, cx)
    }

    /// Same as `create_session`, but lets the caller pin the session's
    /// working directory to a specific path inside the solution (e.g.
    /// a member project root) instead of defaulting to `solution.root`.
    /// Pass `None` for the default behavior.
    pub fn create_session_with_cwd(
        &mut self,
        solution_id: SolutionId,
        agent_id: AgentServerId,
        project: Entity<project::Project>,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        let pair = (solution_id.clone(), agent_id.clone());

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // 1. Resolve the solution. Cloned out so we don't hold the store
            //    borrow across the connection await.
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", solution_id))
                    })
            })?;

            // 2. Get-or-spawn the pooled connection for (solution, agent).
            let connection_task = this.update(cx, |store, cx| {
                store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx)
            })?;
            let connection = connection_task.await?;

            // 3. Create an ACP session on that connection.
            let work_dir = cwd.unwrap_or_else(|| solution.root.clone());
            let work_dirs =
                util::path_list::PathList::new(&[work_dir.to_string_lossy().into_owned()]);
            let session_cwd = work_dir.clone();
            let acp_thread_task = cx.update(|cx| {
                connection
                    .clone()
                    .new_session(project.clone(), work_dirs, cx)
            });
            let acp_thread = match acp_thread_task.await {
                Ok(thread) => thread,
                Err(err) => {
                    // Spawn succeeded but new_session failed — release our
                    // refcount on the pooled connection so it can debounce-
                    // close if no other sessions are active.
                    this.update(cx, |store, cx| {
                        store.pool_release_session(pair.clone(), cx);
                    })
                    .ok();
                    return Err(err);
                }
            };

            // 4. Register the session and emit `SessionCreated`.
            let session_id = this.update(cx, |store, cx| {
                let acp_session_id = acp_thread.read(cx).session_id().clone();
                let session_id = SolutionSessionId::new();
                // Default tab title = name of the project that's the
                // session's cwd: catalog name for a member, else the
                // Solution name (covers the "Solution root" choice).
                // Dedup'd against existing sessions in the same Solution
                // so successive same-cwd opens land as `name`, `name 2`,
                // `name 3`, …
                let title_base: SharedString = project_name_for_cwd(&solution, &session_cwd, cx)
                    .unwrap_or_else(|| SharedString::from(solution.name.clone()));
                let title = unique_session_title(&title_base, store, &solution_id, cx);
                let session = SolutionSession {
                    id: session_id,
                    solution_id: solution_id.clone(),
                    agent_id: agent_id.clone(),
                    acp_session_id,
                    acp_thread: Some(acp_thread.clone()),
                    title,
                    created_at: Utc::now(),
                    last_activity_at: Utc::now(),
                    state: SessionState::Idle,
                    context_count: 1,
                    project: Some(project.clone()),
                    _acp_subscription: None,
                    pending_messages: std::collections::VecDeque::new(),
                    flush_after_cancel: false,
                    cwd: session_cwd.clone(),
                    cold_entries: Vec::new(),
                };
                let entity = cx.new(|_| session);
                store.sessions.insert(session_id, entity);
                let by_sol = store.by_solution.entry(solution_id.clone()).or_default();
                if !by_sol.contains(&session_id) {
                    by_sol.push(session_id);
                }
                let sub = store.subscribe_to_session(session_id, acp_thread, cx);
                store
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("session vanished after insert"))?
                    .update(cx, |s, _| s._acp_subscription = Some(sub));
                store.persist_session_row(session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionCreated(session_id));
                cx.notify();
                anyhow::Ok(session_id)
            })??;

            Ok(session_id)
        })
    }

    /// Persist the row for `session_id` to the DB so the History popover and
    /// "Continue last session" CTA pick it up across editor restarts. No-op
    /// when persistence is disabled (test contexts).
    fn persist_session_row(&self, session_id: SolutionSessionId, cx: &mut Context<Self>) {
        let Some(db) = self.persistence.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(&session_id) else {
            return;
        };
        let s = session.read(cx);
        // Pull the dialog preview + total token count from the live thread.
        // Both are `None` until the user sends the first prompt and the agent
        // emits a usage update, respectively. The DB write uses COALESCE so a
        // None on a follow-up insert never clobbers a previously-stored value.
        let (preview, total_tokens) = s
            .acp_thread
            .as_ref()
            .map(|thread| {
                let thread = thread.read(cx);
                // `used_tokens` is the cumulative context usage that
                // claude-acp reports via `SessionUpdate::UsageUpdate`
                // — same number the status-row meter shows live. We
                // used to persist `input_tokens + output_tokens`,
                // which only covers the LAST turn (gated by the
                // ACP-beta response.usage path), so a 33k-token
                // session resumed as 700 tokens. Saving used_tokens
                // keeps the persisted value aligned with the meter.
                (
                    extract_preview(thread.entries()),
                    thread.token_usage().map(|u| u.used_tokens),
                )
            })
            .unwrap_or((None, None));
        let meta = SolutionSessionMetadata {
            id: session_id,
            solution_id: s.solution_id.clone(),
            agent_id: s.agent_id.clone(),
            acp_session_id: s.acp_session_id.clone(),
            title: s.title.clone(),
            created_at: s.created_at,
            last_activity_at: s.last_activity_at,
            preview,
            total_tokens,
            context_count: s.context_count,
            cwd: s.cwd.clone(),
        };
        db.save_metadata(meta).detach_and_log_err(cx);
    }

    /// Resume a session from its persisted metadata: spawns / reuses the
    /// pooled connection and asks the agent to attach to the saved
    /// `acp_session_id`. Falls back to `resume_session` (history-less
    /// reattach) if `load_session` (full replay) isn't supported. If the
    /// metadata is already in-memory the existing session is returned.
    ///
    /// Returns the live `SolutionSessionId`. The caller can then look up
    /// the entity via `session(id)` and open it in the navigator.
    pub fn resume_session(
        &mut self,
        meta: SolutionSessionMetadata,
        project: Entity<project::Project>,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        // Already hot (`acp_thread` attached)? Return the existing
        // session id directly. A cold session — registered by
        // `restore_open_tabs` with `acp_thread: None` — falls through
        // and triggers the real spawn path so the user's pending Send
        // makes it to a live agent.
        if let Some(existing) = self
            .by_solution
            .get(&meta.solution_id)
            .into_iter()
            .flatten()
            .find(|sid| {
                self.sessions
                    .get(sid)
                    .map(|s| {
                        let s = s.read(cx);
                        s.acp_session_id == meta.acp_session_id && s.acp_thread.is_some()
                    })
                    .unwrap_or(false)
            })
            .cloned()
        {
            return Task::ready(Ok(existing));
        }

        let pair = (meta.solution_id.clone(), meta.agent_id.clone());

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == meta.solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", meta.solution_id))
                    })
            })?;

            let connection_task = this.update(cx, |store, cx| {
                store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx)
            })?;
            let connection = connection_task.await?;

            // Empty `cwd` = legacy row written before the column existed —
            // fall back to `solution.root` (matches the pre-fix resume
            // behaviour, so already-broken sessions don't get any worse).
            let resume_cwd = if meta.cwd.as_os_str().is_empty() {
                solution.root.clone()
            } else {
                meta.cwd.clone()
            };
            let work_dirs =
                util::path_list::PathList::new(&[resume_cwd.to_string_lossy().into_owned()]);
            let acp_session_id = meta.acp_session_id.clone();
            let title_for_load = Some(meta.title.clone());

            let acp_thread_task: Task<Result<Entity<acp_thread::AcpThread>>> = cx.update(|cx| {
                if connection.supports_load_session() {
                    Ok(connection.clone().load_session(
                        acp_session_id.clone(),
                        project.clone(),
                        work_dirs.clone(),
                        title_for_load.clone(),
                        cx,
                    ))
                } else if connection.supports_resume_session() {
                    Ok(connection.clone().resume_session(
                        acp_session_id.clone(),
                        project.clone(),
                        work_dirs.clone(),
                        title_for_load.clone(),
                        cx,
                    ))
                } else {
                    Err(anyhow!(
                        "agent {:?} does not support loading or resuming sessions",
                        meta.agent_id,
                    ))
                }
            })?;
            let acp_thread = match acp_thread_task.await {
                Ok(thread) => thread,
                Err(err) => {
                    this.update(cx, |store, cx| {
                        store.pool_release_session(pair.clone(), cx);
                    })
                    .ok();
                    return Err(err);
                }
            };

            let session_id = this.update(cx, |store, cx| {
                // Reuse the metadata's existing internal id — minting a fresh
                // SolutionSessionId on every resume duplicated the row in the
                // History popover (each restart added another "Session
                // <new-uuid>" pointing at the same `acp_session_id`).
                let session_id = meta.id;
                let new_thread_session_id = acp_thread.read(cx).session_id().clone();
                if let Some(existing) = store.sessions.get(&session_id).cloned() {
                    // Cold-session path: this id was hydrated by
                    // `restore_open_tabs` with `acp_thread: None` and
                    // populated `cold_entries`. Update the existing
                    // `Entity` in place instead of replacing it — the
                    // navigator's `SolutionSessionView` already holds
                    // this handle, so a swap would leave the UI bound
                    // to a stale entity.
                    existing.update(cx, |session, _| {
                        session.acp_session_id = new_thread_session_id;
                        session.acp_thread = Some(acp_thread.clone());
                        session.last_activity_at = Utc::now();
                        session.state = SessionState::Idle;
                        session.context_count = meta.context_count;
                        session.project = Some(project.clone());
                        session.pending_messages.clear();
                        session.flush_after_cancel = false;
                        session.cwd = resume_cwd.clone();
                        session.cold_entries.clear();
                    });
                } else {
                    let session = SolutionSession {
                        id: session_id,
                        solution_id: meta.solution_id.clone(),
                        agent_id: meta.agent_id.clone(),
                        acp_session_id: new_thread_session_id,
                        acp_thread: Some(acp_thread.clone()),
                        title: meta.title.clone(),
                        created_at: meta.created_at,
                        last_activity_at: Utc::now(),
                        state: SessionState::Idle,
                        context_count: meta.context_count,
                        project: Some(project.clone()),
                        _acp_subscription: None,
                        pending_messages: std::collections::VecDeque::new(),
                        flush_after_cancel: false,
                        // Persist the same cwd we resumed against so the next
                        // restart finds the row aligned with the agent state.
                        cwd: resume_cwd.clone(),
                        cold_entries: Vec::new(),
                    };
                    let entity = cx.new(|_| session);
                    store.sessions.insert(session_id, entity);
                }
                let by_sol = store
                    .by_solution
                    .entry(meta.solution_id.clone())
                    .or_default();
                if !by_sol.contains(&session_id) {
                    by_sol.push(session_id);
                }
                // Re-seed token usage from the persisted metadata so the
                // status-row meter doesn't claim "0 tokens" for a long
                // resumed conversation. We only have a coarse aggregate
                // (`total_tokens`); the model will fill in the
                // input/output split + max_tokens on the next turn via
                // session_update events.
                if let Some(total) = meta.total_tokens {
                    acp_thread.update(cx, |thread, cx| {
                        thread.update_token_usage(
                            Some(acp_thread::TokenUsage {
                                used_tokens: total,
                                ..Default::default()
                            }),
                            cx,
                        );
                    });
                }
                let sub = store.subscribe_to_session(session_id, acp_thread, cx);
                store
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("session vanished after insert"))?
                    .update(cx, |s, _| s._acp_subscription = Some(sub));
                store.persist_session_row(session_id, cx);
                // Resume re-livens a previously soft-closed row. Clear
                // the marker so MCP `read_session_history` (and any
                // future "Archived sessions" UI) reports it as live
                // again until the user closes the tab next time.
                if let Some(db) = &store.persistence {
                    db.mark_closed(session_id, None).detach_and_log_err(cx);
                }
                cx.emit(SolutionAgentStoreEvent::SessionCreated(session_id));
                cx.notify();
                anyhow::Ok(session_id)
            })??;

            Ok(session_id)
        })
    }

    pub fn sessions_for(&self, solution_id: &SolutionId) -> Vec<Entity<SolutionSession>> {
        self.by_solution
            .get(solution_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.sessions.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn session(&self, id: SolutionSessionId) -> Option<Entity<SolutionSession>> {
        self.sessions.get(&id).cloned()
    }

    pub fn all_sessions(&self) -> impl Iterator<Item = Entity<SolutionSession>> + '_ {
        self.sessions.values().cloned()
    }

    /// Test-only helper: register a session whose `acp_thread` was constructed
    /// elsewhere (or left `None`). Real `create_session` (Task 3.3) replaces
    /// this for production use.
    #[cfg(any(feature = "test-support", test))]
    pub fn register_prebuilt_session(
        &mut self,
        session: SolutionSession,
        cx: &mut Context<Self>,
    ) -> SolutionSessionId {
        let id = session.id;
        let solution_id = session.solution_id.clone();
        let entity = cx.new(|_| session);
        self.sessions.insert(id, entity);
        self.by_solution.entry(solution_id).or_default().push(id);
        cx.emit(SolutionAgentStoreEvent::SessionCreated(id));
        cx.notify();
        id
    }

    /// Restore tabs the user had open the last time they closed this
    /// Solution, **without spawning the agent subprocess**. For each
    /// session id where `tab_order IS NOT NULL`, hydrate a
    /// `SolutionSession` with `acp_thread: None` and `cold_entries`
    /// populated from the persisted JSON blob. The session view will
    /// render those entries as a read-only conversation; the live
    /// `AcpThread` is only attached if/when the user submits a new
    /// message via `resume_session`.
    ///
    /// Sessions that already exist in `self.sessions` (created earlier
    /// in this process — e.g. via MCP from another window) are left
    /// untouched: they keep their live `acp_thread` and the navigator
    /// will pick them up via the normal reconcile path.
    ///
    /// Returns the ordered ids matching `tab_order ASC`. Caller (the
    /// navigator) uses that order directly to populate the strip,
    /// instead of relying on `created_at` sort.
    pub fn restore_open_tabs(
        &self,
        solution_id: SolutionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<SolutionSessionId>>> {
        let Some(db) = self.persistence.clone() else {
            return Task::ready(Ok(Vec::new()));
        };
        let already_open: std::collections::HashSet<SolutionSessionId> =
            self.sessions.keys().copied().collect();
        cx.spawn(async move |this, cx| {
            let ordered_ids = db.list_open_tabs(solution_id.clone()).await?;
            if ordered_ids.is_empty() {
                return Ok(Vec::new());
            }
            // Pull metadata for the whole solution once (single query) and
            // index by id. Cheaper than N round-trips when the user had
            // five-plus tabs open.
            let metas = db.list_for_solution(solution_id.clone()).await?;
            let by_id: std::collections::HashMap<SolutionSessionId, SolutionSessionMetadata> =
                metas.into_iter().map(|m| (m.id, m)).collect();
            // Load blobs for all rows we'll hydrate (skip ones already in
            // the in-memory store — they're already live or being
            // resumed).
            let mut blobs: std::collections::HashMap<SolutionSessionId, Vec<u8>> =
                std::collections::HashMap::new();
            for id in &ordered_ids {
                if already_open.contains(id) {
                    continue;
                }
                let blob = db.load_blob(*id).await?;
                if let Some(bytes) = blob {
                    blobs.insert(*id, bytes);
                }
            }
            // Apply on the foreground thread so the cx.new + emit
            // observe-callbacks all happen in the GPUI scheduler.
            // Collect the ids that survive into a result vec — orphans
            // (tab_order pointing at deleted metadata) and
            // hydration failures must NOT appear in the navigator's
            // restored strip, so the returned Vec only contains ids
            // that are now backed by a live `Entity<SolutionSession>`.
            let result_ids: Vec<SolutionSessionId> = this.update(cx, |this, cx| {
                let mut hydrated: Vec<SolutionSessionId> = Vec::with_capacity(ordered_ids.len());
                for id in &ordered_ids {
                    if this.sessions.contains_key(id) {
                        hydrated.push(*id);
                        continue;
                    }
                    let Some(meta) = by_id.get(id) else {
                        // tab_order pointed at a session whose metadata
                        // was deleted out from under it. Skip — the
                        // navigator never sees this id in the
                        // returned slice.
                        log::warn!("restore_open_tabs: orphaned tab_order for {id}");
                        continue;
                    };
                    let cold_entries = blobs
                        .remove(id)
                        .and_then(|bytes| serde_json::from_slice::<PersistedSession>(&bytes).ok())
                        .map(|persisted| {
                            if !persisted.entries.is_empty() {
                                persisted.entries
                            } else {
                                // Legacy blob: per-entry role wasn't
                                // recorded, so we tag every entry as
                                // `Archived`. Idx-based User/Assistant
                                // synthesis mis-rolled tool calls in
                                // mixed conversations — a neutral
                                // label is less misleading than a
                                // confidently-wrong one.
                                persisted
                                    .entry_summaries
                                    .into_iter()
                                    .map(|markdown| PersistedEntry {
                                        role: PersistedRole::Archived,
                                        markdown,
                                    })
                                    .collect()
                            }
                        })
                        .unwrap_or_default();
                    let cold_session = SolutionSession {
                        id: meta.id,
                        solution_id: meta.solution_id.clone(),
                        agent_id: meta.agent_id.clone(),
                        acp_session_id: meta.acp_session_id.clone(),
                        acp_thread: None,
                        title: meta.title.clone(),
                        created_at: meta.created_at,
                        last_activity_at: meta.last_activity_at,
                        state: SessionState::Idle,
                        context_count: meta.context_count,
                        project: None,
                        _acp_subscription: None,
                        pending_messages: std::collections::VecDeque::new(),
                        flush_after_cancel: false,
                        cwd: meta.cwd.clone(),
                        cold_entries,
                    };
                    let entity = cx.new(|_| cold_session);
                    this.sessions.insert(meta.id, entity);
                    this.by_solution
                        .entry(solution_id.clone())
                        .or_default()
                        .push(meta.id);
                    cx.emit(SolutionAgentStoreEvent::SessionCreated(meta.id));
                    hydrated.push(meta.id);
                }
                cx.notify();
                hydrated
            })?;
            Ok(result_ids)
        })
    }

    pub fn close_session(&mut self, id: SolutionSessionId, cx: &mut Context<Self>) -> Result<()> {
        let removed = self
            .sessions
            .remove(&id)
            .ok_or_else(|| anyhow!("unknown session {id}"))?;
        let solution_id = removed.read(cx).solution_id.clone();
        if let Some(list) = self.by_solution.get_mut(&solution_id) {
            list.retain(|sid| *sid != id);
        }
        // Soft-close: keep the persisted blob so downstream tooling
        // (MCP read_session_history, future "View archived sessions"
        // UI, etc.) can still read the transcript. Hard-delete only
        // happens when the whole solution is removed via
        // `delete_for_solution`.
        if let Some(db) = &self.persistence {
            db.mark_closed(id, Some(Utc::now())).detach_and_log_err(cx);
        }
        cx.emit(SolutionAgentStoreEvent::SessionClosed(id));
        cx.notify();
        Ok(())
    }

    /// Update the user-visible title of a session and persist the change
    /// (best-effort). Emits `SessionTitleChanged` so the navigator
    /// re-renders the row immediately.
    pub fn rename_session(
        &mut self,
        session_id: SolutionSessionId,
        title: SharedString,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let session = self
            .sessions
            .get(&session_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        session.update(cx, |s, _| s.title = title.clone());
        // Reuse `persist_session_row` so preview + token columns get
        // populated from the live thread instead of being NULL'd by this
        // title-only write path.
        self.persist_session_row(session_id, cx);
        cx.emit(SolutionAgentStoreEvent::SessionTitleChanged(session_id));
        cx.notify();
        Ok(())
    }

    /// Restart the agent backing `session_id`: drop the pool entry so the
    /// next `create_session` call forces a fresh subprocess spawn, close
    /// the existing session, and open a new one against the cached project.
    /// v1 does not replay history — the new session starts empty (deferred
    /// per Phase-5 spec "Open implementation questions" item 5).
    ///
    /// Returns the freshly minted `SolutionSessionId` so callers can
    /// reattach navigator focus to it.
    pub fn restart_agent(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        let Some(session) = self.sessions.get(&session_id).cloned() else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };
        let (solution_id, agent_id, project) = {
            let s = session.read(cx);
            let project = match s.project.clone() {
                Some(project) => project,
                None => {
                    return Task::ready(Err(anyhow!(
                        "session {session_id} has no cached project — was it created via \
                         register_prebuilt_session?"
                    )));
                }
            };
            (s.solution_id.clone(), s.agent_id.clone(), project)
        };
        let pair = (solution_id.clone(), agent_id.clone());
        {
            let mut pool = self.pool.lock();
            pool.remove(&pair);
        }
        // Mark the old session as restarting so the UI can show feedback
        // before the new session is registered.
        session.update(cx, |s, _| {
            s.state = SessionState::Errored(SharedString::from("restarting…"));
        });
        cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
        // Best-effort close of the old session; we still spawn the new
        // one even if removal fails so the user isn't stranded.
        if let Err(err) = self.close_session(session_id, cx) {
            log::warn!("restart_agent: close_session({session_id}) failed: {err:?}");
        }
        let create_task = self.create_session(solution_id, agent_id, project, cx);
        cx.spawn(async move |_this, _cx: &mut AsyncApp| create_task.await)
    }

    /// In-place context rotation: drop the current AcpThread, spawn a
    /// fresh ACP-level session against the SAME pooled connection, and
    /// graft it onto the existing `SolutionSession`. The user-facing
    /// `SolutionSessionId` and tab identity stay stable so dump
    /// directories from successive compacts cluster under one
    /// `<root>/.agents/<sid>/` tree, distinguishable only by the
    /// `context_count` (= which rotation).
    ///
    /// Different from `restart_agent` in two ways:
    ///   1. Keeps `SolutionSessionId` (restart_agent mints a fresh
    ///      one because its goal is "this session is broken — please
    ///      give me a clean slate" while rotate's goal is "same
    ///      conversation, just freed up the context window").
    ///   2. Reuses the same pooled subprocess (restart_agent drops
    ///      the pool entry to force a subprocess respawn).
    pub fn rotate_context(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<u32>> {
        let Some(session_entity) = self.sessions.get(&session_id).cloned() else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };
        let (solution_id, agent_id, project, current_count) = {
            let s = session_entity.read(cx);
            let project = match s.project.clone() {
                Some(project) => project,
                None => {
                    return Task::ready(Err(anyhow!(
                        "session {session_id} has no cached project — rotate_context not supported \
                         for prebuilt test sessions"
                    )));
                }
            };
            (
                s.solution_id.clone(),
                s.agent_id.clone(),
                project,
                s.context_count,
            )
        };
        let pair = (solution_id.clone(), agent_id);

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // Resolve the live Solution so `connection.new_session`
            // gets a real cwd.
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", solution_id))
                    })
            })?;
            let connection_task = this.update(cx, |store, cx| {
                store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx)
            })?;
            let connection = connection_task.await?;
            let work_dirs =
                util::path_list::PathList::new(&[solution.root.to_string_lossy().into_owned()]);
            let new_thread_task = cx.update(|cx| {
                connection
                    .clone()
                    .new_session(project.clone(), work_dirs, cx)
            });
            let new_thread = new_thread_task.await?;

            let new_count = this.update(cx, |store, cx| {
                let new_acp_session_id = new_thread.read(cx).session_id().clone();
                let new_count = current_count.saturating_add(1);
                session_entity.update(cx, |s, _| {
                    s.acp_thread = Some(new_thread.clone());
                    s.acp_session_id = new_acp_session_id;
                    s.context_count = new_count;
                    s.state = SessionState::Idle;
                    s.last_activity_at = Utc::now();
                });
                // Re-subscribe to the new AcpThread's event stream.
                // Dropping the old subscription unhooks us from the
                // dead thread automatically.
                let new_sub = store.subscribe_to_session(session_id, new_thread, cx);
                session_entity.update(cx, |s, _| s._acp_subscription = Some(new_sub));
                store.persist_session_row(session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
                cx.notify();
                new_count
            })?;

            Ok(new_count)
        })
    }

    /// Reset the session's conversation context: drop the current
    /// `AcpThread` and spawn a fresh ACP-level session under the same
    /// `SolutionSessionId` and pooled subprocess.
    ///
    /// Different from [`rotate_context`](Self::rotate_context) in that
    /// `context_count` is left untouched (no `c<N>` directory bump) —
    /// this is the path wired to the user-facing `/clear` slash command,
    /// where the intent is "wipe this conversation, keep the tab"
    /// rather than "archive a long-running conversation as a numbered
    /// rotation". Agent-agnostic: nothing is forwarded to the agent
    /// subprocess; the new ACP session has zero history by construction.
    ///
    /// Returns the same `SolutionSessionId` for caller convenience (so
    /// the call site can chain "reset then dispatch follow-up" without
    /// re-plumbing the id).
    pub fn reset_context(
        &mut self,
        session_id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SolutionSessionId>> {
        let Some(session_entity) = self.sessions.get(&session_id).cloned() else {
            return Task::ready(Err(anyhow!("unknown session {session_id}")));
        };
        let (solution_id, agent_id, project) = {
            let s = session_entity.read(cx);
            let project = match s.project.clone() {
                Some(project) => project,
                None => {
                    return Task::ready(Err(anyhow!(
                        "session {session_id} has no cached project — reset_context not supported \
                         for prebuilt test sessions"
                    )));
                }
            };
            (s.solution_id.clone(), s.agent_id.clone(), project)
        };
        let pair = (solution_id.clone(), agent_id);

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let solution = cx.update(|cx| {
                SolutionStore::try_global(cx)
                    .ok_or_else(|| anyhow!("SolutionStore global is not initialised"))
                    .and_then(|store| {
                        store
                            .read(cx)
                            .solutions()
                            .iter()
                            .find(|s| s.id == solution_id)
                            .cloned()
                            .ok_or_else(|| anyhow!("solution {:?} not found", solution_id))
                    })
            })?;
            let connection_task = this.update(cx, |store, cx| {
                store.get_or_spawn_connection(pair.clone(), &solution, project.clone(), cx)
            })?;
            let connection = connection_task.await?;
            let work_dirs =
                util::path_list::PathList::new(&[solution.root.to_string_lossy().into_owned()]);
            let new_thread_task = cx.update(|cx| {
                connection
                    .clone()
                    .new_session(project.clone(), work_dirs, cx)
            });
            let new_thread = new_thread_task.await?;

            this.update(cx, |store, cx| {
                let new_acp_session_id = new_thread.read(cx).session_id().clone();
                session_entity.update(cx, |s, _| {
                    s.acp_thread = Some(new_thread.clone());
                    s.acp_session_id = new_acp_session_id;
                    s.state = SessionState::Idle;
                    s.last_activity_at = Utc::now();
                    s.pending_messages.clear();
                    s.flush_after_cancel = false;
                });
                let new_sub = store.subscribe_to_session(session_id, new_thread, cx);
                session_entity.update(cx, |s, _| s._acp_subscription = Some(new_sub));
                store.persist_session_row(session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
                cx.notify();
            })?;

            Ok(session_id)
        })
    }

    /// Returns a clone of the persistence handle if one was configured
    /// (i.e. the editor is running with a real on-disk DB, not the test
    /// in-memory mode). Used by MCP tools that need to read archived
    /// session blobs without re-hydrating the full session.
    pub fn persistence(&self) -> Option<Arc<crate::db::SolutionAgentDb>> {
        self.persistence.clone()
    }

    /// Persists the tab strip's open-session order for `solution_id`.
    /// Sessions in `ordered_ids` get `tab_order = 0..N`; everything else
    /// for the solution is set to `tab_order = NULL`. Called from the
    /// navigator on reorder, open, and close so the strip survives an
    /// editor restart.
    pub fn persist_tab_order(
        &self,
        solution_id: SolutionId,
        ordered_ids: Vec<SolutionSessionId>,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.persistence.clone() else {
            return;
        };
        cx.background_spawn(async move {
            db.update_tab_orders(solution_id, ordered_ids)
                .await
                .log_err();
        })
        .detach();
    }

    /// Schedule a debounce-friendly write of the session's serialised snapshot
    /// to the persistence backend (if configured). The serialisation runs on
    /// the foreground thread because it must read the `AcpThread` entity; the
    /// SQLite write itself is dispatched to the background executor by
    /// `SolutionAgentDb::save_blob`.
    pub fn persist_session_blob(&self, session_id: SolutionSessionId, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(&session_id).cloned() else {
            return;
        };
        let Some(db) = self.persistence.clone() else {
            return;
        };
        cx.spawn(async move |_this, cx: &mut AsyncApp| {
            let blob: Vec<u8> = cx.update(|cx| {
                let s = session.read(cx);
                serializable_snapshot(s, cx)
            });
            if !blob.is_empty() {
                db.save_blob(session_id, blob).await.log_err();
            }
        })
        .detach();
    }

    /// Subscribe to a session's `AcpThread` event stream so that ACP-level
    /// state changes (turn completion, tool authorization, errors, etc.)
    /// translate into `SessionState` transitions on `SolutionSession`.
    /// Returns the `Subscription` — caller must store it on the session
    /// (in `_acp_subscription`) or it will drop and unsubscribe immediately.
    fn subscribe_to_session(
        &mut self,
        session_id: SolutionSessionId,
        acp_thread: Entity<acp_thread::AcpThread>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(&acp_thread, move |store, _thread, event, cx| {
            store.handle_acp_event(session_id, event, cx);
        })
    }

    fn handle_acp_event(
        &mut self,
        session_id: SolutionSessionId,
        event: &acp_thread::AcpThreadEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(session_entity) = self.sessions.get(&session_id).cloned() else {
            return;
        };
        match event {
            acp_thread::AcpThreadEvent::NewEntry => {
                self.mutate_state(
                    session_id,
                    |state| {
                        if matches!(state, SessionState::Idle | SessionState::AwaitingInput) {
                            *state = SessionState::Running {
                                started_at: std::time::Instant::now(),
                                notified: false,
                            };
                        }
                    },
                    cx,
                );
                if let Some(s) = self.sessions.get(&session_id).cloned() {
                    s.update(cx, |s, _| s.last_activity_at = Utc::now());
                }
                // First user message appends a NewEntry — refresh DB so the
                // History popover preview stops being NULL.
                self.persist_session_row(session_id, cx);
                cx.emit(SolutionAgentStoreEvent::SessionMessageAppended(session_id));
            }
            acp_thread::AcpThreadEvent::Stopped(_) => {
                self.mutate_state(session_id, |state| *state = SessionState::Idle, cx);
                if let Some(s) = self.sessions.get(&session_id).cloned() {
                    s.update(cx, |s, _| s.last_activity_at = Utc::now());
                }
                // Token usage is finalised on turn completion — refresh DB
                // so the History popover token column reflects the latest.
                self.persist_session_row(session_id, cx);
                // Flush queued follow-ups (if any). All pending entries
                // are drained and concatenated into ONE send — the user
                // typed them as a fast-fire stream while the agent was
                // working, so it's their joint intent for the next turn
                // rather than N independent prompts. A Cancelled stop
                // (user pressed Stop) is treated as "abandon what I
                // queued too": the queue is cleared without sending.
                if let acp_thread::AcpThreadEvent::Stopped(reason) = event {
                    // `flush_after_cancel` (set by `interrupt_and_flush_pending`)
                    // flips Cancelled's default semantics from "abandon the
                    // queue too" to "cancel the current turn but immediately
                    // start the next one with the queued follow-ups". One-
                    // shot — clear the flag whether or not the queue had
                    // anything left to send.
                    let flush_after_cancel = self
                        .sessions
                        .get(&session_id)
                        .map(|s| {
                            s.update(cx, |s, _| {
                                let was = s.flush_after_cancel;
                                s.flush_after_cancel = false;
                                was
                            })
                        })
                        .unwrap_or(false);
                    let cancelled =
                        matches!(reason, agent_client_protocol::schema::StopReason::Cancelled);
                    if cancelled && !flush_after_cancel {
                        if let Some(s) = self.sessions.get(&session_id).cloned() {
                            s.update(cx, |s, _| s.pending_messages.clear());
                        }
                    } else {
                        let drained: Vec<_> = self
                            .sessions
                            .get(&session_id)
                            .cloned()
                            .map(|s| {
                                s.update(cx, |s, _| {
                                    s.pending_messages.drain(..).collect::<Vec<_>>()
                                })
                            })
                            .unwrap_or_default();
                        if !drained.is_empty() {
                            // Flatten N queued messages into one Vec.
                            // Each was its own send-press, but we coalesce
                            // them so the agent gets a single prompt.
                            let combined: Vec<_> = drained.into_iter().flatten().collect();
                            if !combined.is_empty() {
                                self.send_message_blocks(session_id, combined, cx).detach();
                            }
                        }
                    }
                }
            }
            acp_thread::AcpThreadEvent::TokenUsageUpdated => {
                // claude-acp ships incremental usage during a turn, not
                // just at the end. Persist on every update so a session
                // closed mid-turn (or right before `Stopped` fires)
                // resumes with the correct meter — without this the DB
                // value lags behind the live meter and a resume drops
                // back to whatever the previous Stopped wrote.
                self.persist_session_row(session_id, cx);
            }
            acp_thread::AcpThreadEvent::Error | acp_thread::AcpThreadEvent::LoadError(_) => {
                self.mutate_state(
                    session_id,
                    |state| *state = SessionState::Errored(SharedString::from("agent error")),
                    cx,
                );
            }
            acp_thread::AcpThreadEvent::ToolAuthorizationRequested(_) => {
                self.mutate_state(session_id, |state| *state = SessionState::AwaitingInput, cx);
            }
            acp_thread::AcpThreadEvent::ToolAuthorizationReceived(_) => {
                self.mutate_state(
                    session_id,
                    |state| {
                        if matches!(state, SessionState::AwaitingInput) {
                            *state = SessionState::Running {
                                started_at: std::time::Instant::now(),
                                notified: false,
                            };
                        }
                    },
                    cx,
                );
            }
            acp_thread::AcpThreadEvent::TitleUpdated => {
                let new_title = session_entity
                    .read(cx)
                    .acp_thread
                    .as_ref()
                    .and_then(|t| t.read(cx).title())
                    .unwrap_or_default();
                session_entity.update(cx, |s, _| s.title = new_title);
                cx.emit(SolutionAgentStoreEvent::SessionTitleChanged(session_id));
            }
            _ => {}
        }
        cx.notify();
    }

    /// Wraps a `SessionState` mutation so notifier hooks fire uniformly:
    ///   1. Snapshot previous state.
    ///   2. Apply `f` to mutate state.
    ///   3. Emit `SessionStateChanged` only when the discriminant changed.
    ///   4. Ask the notifier whether the transition warrants a desktop
    ///      notification, dispatch it, emit `SessionNotified`, and mark
    ///      the session's `Running { notified: true }` to suppress dupes.
    ///
    /// Side-channel updates (e.g. `last_activity_at`) stay outside `f` so
    /// they don't accidentally affect the notification decision.
    fn mutate_state<F: FnOnce(&mut SessionState)>(
        &mut self,
        session_id: SolutionSessionId,
        f: F,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.sessions.get(&session_id).cloned() else {
            return;
        };
        let previous = session.read(cx).state.clone();
        session.update(cx, |s, _| f(&mut s.state));
        let next = session.read(cx).state.clone();
        if std::mem::discriminant(&previous) != std::mem::discriminant(&next) {
            cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
        }
        let now = std::time::Instant::now();
        let is_focused = self
            .focus_resolver
            .as_ref()
            .map(|f| f(session_id, cx))
            .unwrap_or(false);
        let has_pending_messages = !session.read(cx).pending_messages.is_empty();
        if let Some(decision) = notifier::decide_notification(
            session_id,
            &previous,
            &next,
            now,
            is_focused,
            has_pending_messages,
        ) {
            let (title, body) = {
                let s = session.read(cx);
                let title = format!("SPK Editor — {} ({})", s.agent_id, s.title);
                let body = match decision.kind {
                    notifier::NotifyKind::Completed => {
                        format!("Done after {} min", decision.elapsed.as_secs() / 60)
                    }
                    notifier::NotifyKind::AwaitingInput => format!(
                        "Awaiting your input after {} min",
                        decision.elapsed.as_secs() / 60
                    ),
                    notifier::NotifyKind::Errored => match &next {
                        SessionState::Errored(msg) => format!("Failed: {msg}"),
                        _ => "Failed".to_string(),
                    },
                };
                (title, body)
            };
            notifier::dispatch(&decision, &title, &body, cx);
            cx.emit(SolutionAgentStoreEvent::SessionNotified(
                session_id,
                decision.kind,
            ));
            session.update(cx, |s, _| {
                if let SessionState::Running { notified, .. } = &mut s.state {
                    *notified = true;
                }
            });
        }
    }

    fn on_solution_event(
        &mut self,
        _: Entity<SolutionStore>,
        event: &SolutionStoreEvent,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, SolutionStoreEvent::Changed) {
            self.gc_orphan_solutions(cx);
        }
    }

    fn gc_orphan_solutions(&mut self, cx: &mut Context<Self>) {
        let Some(store) = SolutionStore::try_global(cx) else {
            return;
        };
        let alive: std::collections::HashSet<SolutionId> = store
            .read(cx)
            .solutions()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let orphan_ids: Vec<SolutionId> = self
            .by_solution
            .keys()
            .filter(|sid| !alive.contains(*sid))
            .cloned()
            .collect();
        for sid in orphan_ids {
            if let Some(session_ids) = self.by_solution.remove(&sid) {
                for session_id in session_ids {
                    self.sessions.remove(&session_id);
                    if let Some(db) = &self.persistence {
                        db.delete(session_id).detach_and_log_err(cx);
                    }
                    cx.emit(SolutionAgentStoreEvent::SessionClosed(session_id));
                }
            }
            if let Some(db) = &self.persistence {
                db.delete_for_solution(sid).detach_and_log_err(cx);
            }
        }
        cx.notify();
    }
}

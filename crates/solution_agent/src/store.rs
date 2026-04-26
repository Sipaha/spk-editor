use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::Utc;
use futures::FutureExt;
use futures::future::Shared;
use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, SharedString, Subscription,
    Task,
};
use solutions::{Solution, SolutionId, SolutionStore, SolutionStoreEvent};

use crate::adapter::AdapterRegistry;
use crate::db::SolutionAgentDb;
use crate::model::{AgentServerId, SessionState, SolutionSession, SolutionSessionId};
use crate::pool::{PooledConnection, SHUTDOWN_DEBOUNCE, SpawnState, SubprocessPool};

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
    _solution_subscription: Option<Subscription>,
}

#[derive(Debug)]
pub enum SolutionAgentStoreEvent {
    SessionCreated(SolutionSessionId),
    SessionClosed(SolutionSessionId),
    SessionStateChanged(SolutionSessionId),
    SessionTitleChanged(SolutionSessionId),
    SessionMessageAppended(SolutionSessionId),
}

impl EventEmitter<SolutionAgentStoreEvent> for SolutionAgentStore {}

struct GlobalSolutionAgentStore(Entity<SolutionAgentStore>);
impl Global for GlobalSolutionAgentStore {}

impl SolutionAgentStore {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalSolutionAgentStore>().0.clone()
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
            let work_dirs = util::path_list::PathList::new(&[
                solution.root.to_string_lossy().into_owned()
            ]);
            let acp_thread_task = cx.update(|cx| {
                connection.clone().new_session(project.clone(), work_dirs, cx)
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
                let session = SolutionSession {
                    id: session_id,
                    solution_id: solution_id.clone(),
                    agent_id: agent_id.clone(),
                    acp_session_id,
                    acp_thread: Some(acp_thread.clone()),
                    title: SharedString::from(format!("Session {}", session_id)),
                    created_at: Utc::now(),
                    last_activity_at: Utc::now(),
                    state: SessionState::Idle,
                    _acp_subscription: None,
                };
                let entity = cx.new(|_| session);
                store.sessions.insert(session_id, entity);
                store
                    .by_solution
                    .entry(solution_id.clone())
                    .or_default()
                    .push(session_id);
                let sub = store.subscribe_to_session(session_id, acp_thread, cx);
                store
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("session vanished after insert"))?
                    .update(cx, |s, _| s._acp_subscription = Some(sub));
                cx.emit(SolutionAgentStoreEvent::SessionCreated(session_id));
                cx.notify();
                anyhow::Ok(session_id)
            })??;

            Ok(session_id)
        })
    }

    /// Pool-aware lookup: returns the existing connection, awaits an
    /// in-flight spawn, drops a previously failed entry and retries, or
    /// kicks off a new spawn. Always increments `live_session_count` so
    /// callers must pair this with `pool_release_session` on session close.
    fn get_or_spawn_connection(
        &mut self,
        pair: (SolutionId, AgentServerId),
        _solution: &Solution,
        project: Entity<project::Project>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Rc<dyn acp_thread::AgentConnection>>> {
        // Phase 1: short critical section over the pool. Either we observe
        // an existing entry (Ready / Pending / Failed) or we hold no entry
        // and proceed to spawn.
        {
            let mut pool = self.pool.lock();
            if let Some(entry) = pool.entry_mut(&pair) {
                entry.shutdown_task = None;
                entry.live_session_count += 1;
                match &entry.state {
                    SpawnState::Ready(connection) => {
                        return Task::ready(Ok(connection.clone()));
                    }
                    SpawnState::Pending(shared) => {
                        let shared = shared.clone();
                        return cx.foreground_executor().spawn(async move {
                            shared.await.map_err(|e| anyhow!("{e}"))
                        });
                    }
                    SpawnState::Failed(_) => {
                        // Drop the failed entry and fall through to a fresh spawn.
                        // We've already bumped live_session_count; reset it so
                        // remove() leaves a clean slate.
                        entry.live_session_count = 0;
                        pool.remove(&pair);
                    }
                }
            }
        }

        // Phase 2: look up the registered AgentServer. If absent, return
        // an error without inserting a Failed pool entry — callers should be
        // able to retry once the server is registered.
        let server = match self.server_registry.get(&pair.1).cloned() {
            Some(server) => server,
            None => {
                return Task::ready(Err(anyhow!(
                    "no AgentServer registered for id {:?}",
                    pair.1
                )));
            }
        };

        // Phase 3: kick off connect() on the foreground executor (AgentServer
        // is `!Send` and so are its returned `Rc<dyn AgentConnection>`s; the
        // pool lives on the foreground thread).
        let pair_for_task = pair.clone();
        // AgentServerDelegate requires an `Entity<AgentServerStore>` — we
        // get one from the project. This is the same coupling documented on
        // `create_session`.
        let agent_server_store = project.read(cx).agent_server_store().clone();
        let delegate = agent_servers::AgentServerDelegate::new(agent_server_store, None);
        let project_for_connect = project;
        let server_for_connect = server;

        let task: Shared<
            Task<Result<Rc<dyn acp_thread::AgentConnection>, std::sync::Arc<anyhow::Error>>>,
        > = cx
            .spawn(async move |this, cx: &mut AsyncApp| {
                let connect_task = cx.update(|cx| {
                    server_for_connect.connect(delegate, project_for_connect, cx)
                });
                let result_for_pool: Result<
                    Rc<dyn acp_thread::AgentConnection>,
                    std::sync::Arc<anyhow::Error>,
                > = connect_task.await.map_err(std::sync::Arc::new);

                // Promote pool state to Ready/Failed once the spawn resolves.
                let _ = this.update(cx, |store, _| {
                    let mut pool = store.pool.lock();
                    if let Some(entry) = pool.entry_mut(&pair_for_task) {
                        entry.state = match &result_for_pool {
                            Ok(connection) => SpawnState::Ready(connection.clone()),
                            Err(err) => SpawnState::Failed(err.clone()),
                        };
                    }
                });
                result_for_pool
            })
            .shared();

        // Phase 4: insert a Pending entry holding the shared task.
        {
            let mut pool = self.pool.lock();
            pool.insert(
                pair.clone(),
                PooledConnection {
                    state: SpawnState::Pending(task.clone()),
                    live_session_count: 1,
                    shutdown_task: None,
                },
            );
        }

        cx.foreground_executor().spawn(async move {
            task.await.map_err(|e| anyhow!("{e}"))
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

    pub fn close_session(
        &mut self,
        id: SolutionSessionId,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let removed = self
            .sessions
            .remove(&id)
            .ok_or_else(|| anyhow!("unknown session {id}"))?;
        let solution_id = removed.read(cx).solution_id.clone();
        if let Some(list) = self.by_solution.get_mut(&solution_id) {
            list.retain(|sid| *sid != id);
        }
        if let Some(db) = &self.persistence {
            db.delete(id).detach_and_log_err(cx);
        }
        cx.emit(SolutionAgentStoreEvent::SessionClosed(id));
        cx.notify();
        Ok(())
    }

    /// Test-only: pretend a session was added against an existing connection.
    #[cfg(any(feature = "test-support", test))]
    pub fn pool_pretend_session_added(
        &mut self,
        key: (SolutionId, AgentServerId),
        connection: std::rc::Rc<dyn acp_thread::AgentConnection>,
    ) {
        let mut pool = self.pool.lock();
        if let Some(entry) = pool.entry_mut(&key) {
            entry.live_session_count += 1;
            entry.shutdown_task = None;
        } else {
            pool.insert(
                key,
                PooledConnection {
                    state: SpawnState::Ready(connection),
                    live_session_count: 1,
                    shutdown_task: None,
                },
            );
        }
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
                session_entity.update(cx, |s, _| {
                    s.last_activity_at = Utc::now();
                    if matches!(s.state, SessionState::Idle | SessionState::AwaitingInput) {
                        s.state = SessionState::Running {
                            started_at: std::time::Instant::now(),
                            notified: false,
                        };
                    }
                });
                cx.emit(SolutionAgentStoreEvent::SessionMessageAppended(session_id));
            }
            acp_thread::AcpThreadEvent::Stopped(_) => {
                session_entity.update(cx, |s, _| {
                    s.state = SessionState::Idle;
                    s.last_activity_at = Utc::now();
                });
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
            }
            acp_thread::AcpThreadEvent::Error
            | acp_thread::AcpThreadEvent::LoadError(_) => {
                session_entity.update(cx, |s, _| {
                    s.state = SessionState::Errored(SharedString::from("agent error"));
                });
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
            }
            acp_thread::AcpThreadEvent::ToolAuthorizationRequested(_) => {
                session_entity.update(cx, |s, _| {
                    s.state = SessionState::AwaitingInput;
                });
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
            }
            acp_thread::AcpThreadEvent::ToolAuthorizationReceived(_) => {
                session_entity.update(cx, |s, _| {
                    if matches!(s.state, SessionState::AwaitingInput) {
                        s.state = SessionState::Running {
                            started_at: std::time::Instant::now(),
                            notified: false,
                        };
                    }
                });
                cx.emit(SolutionAgentStoreEvent::SessionStateChanged(session_id));
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

    pub fn pool_release_session(
        &mut self,
        key: (SolutionId, AgentServerId),
        cx: &mut Context<Self>,
    ) {
        let needs_arm = {
            let mut pool = self.pool.lock();
            let Some(entry) = pool.entry_mut(&key) else {
                return;
            };
            entry.live_session_count = entry.live_session_count.saturating_sub(1);
            entry.live_session_count == 0
        };
        if needs_arm {
            self.arm_shutdown(key, cx);
        }
    }

    fn arm_shutdown(&mut self, key: (SolutionId, AgentServerId), cx: &mut Context<Self>) {
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx: &mut AsyncApp| {
                cx.background_executor().timer(SHUTDOWN_DEBOUNCE).await;
                this.update(cx, |this, _cx| {
                    let mut pool = this.pool.lock();
                    if let Some(entry) = pool.entry_mut(&key) {
                        if entry.live_session_count == 0 {
                            pool.remove(&key);
                        }
                    }
                })
                .ok();
            }
        });
        let mut pool = self.pool.lock();
        if let Some(entry) = pool.entry_mut(&key) {
            entry.shutdown_task = Some(task);
        }
    }

    #[cfg(any(feature = "test-support", test))]
    pub fn pool_size(&self) -> usize {
        self.pool.lock().pair_count()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterRegistry;
    use crate::model::SessionState;
    use chrono::Utc;
    use gpui::{SharedString, Task, TestAppContext};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[gpui::test]
    fn close_session_removes_from_indices(cx: &mut TestAppContext) {
        let registry = Arc::new(AdapterRegistry::new());
        cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                let id = SolutionSessionId::new();
                let entity = cx.new(|_| SolutionSession {
                    id,
                    solution_id: SolutionId("sol-a".into()),
                    agent_id: SharedString::from("claude-acp"),
                    acp_session_id: agent_client_protocol::schema::SessionId::new("acp-1"),
                    acp_thread: None,
                    title: SharedString::from("test"),
                    created_at: Utc::now(),
                    last_activity_at: Utc::now(),
                    state: SessionState::Idle,
                    _acp_subscription: None,
                });
                store.sessions.insert(id, entity);
                store
                    .by_solution
                    .entry(SolutionId("sol-a".into()))
                    .or_default()
                    .push(id);

                assert_eq!(store.sessions_for(&SolutionId("sol-a".into())).len(), 1);
                store.close_session(id, cx).expect("close_session");
                assert_eq!(store.sessions_for(&SolutionId("sol-a".into())).len(), 0);
                assert!(store.session(id).is_none());
            });
        });
    }

    /// AgentConnection mock that returns a real `AcpThread` from `new_session`
    /// so `create_session` can complete without going through a real subprocess.
    struct MockConnection {
        next_session: std::cell::Cell<u64>,
    }

    impl MockConnection {
        fn new() -> Self {
            Self {
                next_session: std::cell::Cell::new(0),
            }
        }
    }

    impl acp_thread::AgentConnection for MockConnection {
        fn agent_id(&self) -> project::AgentId {
            project::AgentId::new("mock-agent")
        }
        fn telemetry_id(&self) -> SharedString {
            SharedString::from("mock")
        }
        fn new_session(
            self: Rc<Self>,
            project: gpui::Entity<project::Project>,
            work_dirs: util::path_list::PathList,
            cx: &mut App,
        ) -> Task<anyhow::Result<gpui::Entity<acp_thread::AcpThread>>> {
            let n = self.next_session.get();
            self.next_session.set(n + 1);
            let session_id = agent_client_protocol::schema::SessionId::new(format!("mock-{n}"));
            let action_log = cx.new(|_| action_log::ActionLog::new(project.clone()));
            let connection: Rc<dyn acp_thread::AgentConnection> = self;
            let thread = cx.new(|cx| {
                acp_thread::AcpThread::new(
                    None,
                    None,
                    Some(work_dirs),
                    connection,
                    project,
                    action_log,
                    session_id,
                    watch::Receiver::constant(
                        agent_client_protocol::schema::PromptCapabilities::new(),
                    ),
                    cx,
                )
            });
            Task::ready(Ok(thread))
        }
        fn auth_methods(&self) -> &[agent_client_protocol::schema::AuthMethod] {
            &[]
        }
        fn authenticate(
            &self,
            _method: agent_client_protocol::schema::AuthMethodId,
            _cx: &mut App,
        ) -> Task<anyhow::Result<()>> {
            Task::ready(Ok(()))
        }
        fn prompt(
            &self,
            _user_message_id: acp_thread::UserMessageId,
            _params: agent_client_protocol::schema::PromptRequest,
            _cx: &mut App,
        ) -> Task<anyhow::Result<agent_client_protocol::schema::PromptResponse>> {
            Task::ready(Err(anyhow::anyhow!("not used in this test")))
        }
        fn cancel(&self, _session_id: &agent_client_protocol::schema::SessionId, _cx: &mut App) {}
        fn into_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
            self
        }
    }

    /// AgentServer mock whose `connect()` counts invocations and lazily
    /// constructs a single shared `MockConnection` on first call. Used to
    /// assert that the pool collapses parallel calls onto one spawn.
    ///
    /// `MockConnection` is `Rc<...>` (and hence `!Send`), but `AgentServer`
    /// is `Send`-bound — so we keep the `Rc` inside a `RefCell` that is
    /// constructed only on the foreground thread inside `connect()`.
    struct MockAgentServer {
        connect_count: Arc<AtomicUsize>,
        // Optional async gate to hold connect() pending until the test releases it.
        gate: parking_lot::Mutex<Option<async_channel::Receiver<()>>>,
    }

    // SAFETY: We only ever touch `gate` from the foreground thread (its
    // contents are `!Send` `Rc`s, but `async_channel::Receiver` itself is
    // `Send`). The `Mutex` guards the option swap.
    unsafe impl Send for MockAgentServer {}

    impl MockAgentServer {
        fn new(connect_count: Arc<AtomicUsize>) -> Self {
            Self {
                connect_count,
                gate: parking_lot::Mutex::new(None),
            }
        }

        fn with_gate(
            connect_count: Arc<AtomicUsize>,
            gate: async_channel::Receiver<()>,
        ) -> Self {
            Self {
                connect_count,
                gate: parking_lot::Mutex::new(Some(gate)),
            }
        }
    }

    impl agent_servers::AgentServer for MockAgentServer {
        fn logo(&self) -> ui::IconName {
            ui::IconName::Sparkle
        }
        fn agent_id(&self) -> project::AgentId {
            project::AgentId::new("mock-agent")
        }
        fn connect(
            &self,
            _delegate: agent_servers::AgentServerDelegate,
            _project: gpui::Entity<project::Project>,
            cx: &mut App,
        ) -> Task<anyhow::Result<Rc<dyn acp_thread::AgentConnection>>> {
            self.connect_count.fetch_add(1, Ordering::SeqCst);
            let gate = self.gate.lock().clone();
            cx.spawn(async move |_| {
                if let Some(gate) = gate {
                    let _ = gate.recv().await;
                }
                let connection: Rc<dyn acp_thread::AgentConnection> =
                    Rc::new(MockConnection::new());
                Ok(connection)
            })
        }
        fn into_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
            self
        }
    }

    /// Set up SolutionStore with one Solution rooted at a tempdir, plus
    /// a `Project::test` whose worktree is that root. Returns
    /// (`SolutionId`, `tempdir`, `Project`). Hold the tempdir for the
    /// lifetime of the test — `create_solution` writes to it.
    async fn setup_solution_and_project(
        cx: &mut TestAppContext,
    ) -> (SolutionId, tempfile::TempDir, gpui::Entity<project::Project>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("solutions.json");
        let solutions_root = dir.path().join("solutions");
        std::fs::create_dir_all(&solutions_root).expect("solutions root");
        let store = cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            let store = solutions::SolutionStore::for_test(cfg_path, cx);
            solutions::install_global_for_test(store.clone(), cx);
            store
        });
        let solution_id = store
            .update(cx, |store, cx| {
                store.create_solution("Sol", solutions_root.clone(), cx)
            })
            .expect("create_solution");
        let solution_root: PathBuf = store.read_with(cx, |store, _| {
            store
                .solutions()
                .iter()
                .find(|s| s.id == solution_id)
                .map(|s| s.root.clone())
                .expect("solution exists")
        });

        let fs = fs::FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            solution_root.clone(),
            serde_json::json!({ ".keep": "" }),
        )
        .await;
        let project = project::Project::test(fs, [solution_root.as_path()], cx).await;

        (solution_id, dir, project)
    }

    #[gpui::test]
    async fn pool_release_arms_60s_shutdown_then_drops(cx: &mut TestAppContext) {
        let registry = Arc::new(AdapterRegistry::new());
        cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

        let key = (SolutionId("sol-a".into()), SharedString::from("mock-agent"));

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection::new()));
                assert_eq!(store.pool_size(), 1);
            });
        });

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.pool_release_session(key.clone(), cx);
            });
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(30));
        cx.executor().run_until_parked();
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| assert_eq!(store.pool_size(), 1));
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(35));
        cx.executor().run_until_parked();
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| assert_eq!(store.pool_size(), 0));
        });
    }

    #[gpui::test]
    async fn shutdown_cancels_when_session_re_added(cx: &mut TestAppContext) {
        let registry = Arc::new(AdapterRegistry::new());
        cx.update(|cx| SolutionAgentStore::init_global(cx, registry));
        let key = (SolutionId("sol-a".into()), SharedString::from("mock-agent"));

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection::new()));
                store.pool_release_session(key.clone(), cx);
            });
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(30));
        cx.executor().run_until_parked();

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection::new()));
            });
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(60));
        cx.executor().run_until_parked();
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| assert_eq!(store.pool_size(), 1));
        });
    }

    #[gpui::test]
    async fn create_session_spawns_subprocess_once_per_pair(cx: &mut TestAppContext) {
        let (solution_id, _tmp, project) = setup_solution_and_project(cx).await;
        let agent_id = SharedString::from("mock-agent");

        let connect_count = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            let registry = Arc::new(AdapterRegistry::new());
            SolutionAgentStore::init_global(cx, registry);
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.register_agent_server(
                    agent_id.clone(),
                    Rc::new(MockAgentServer::new(connect_count.clone())),
                );
            });
        });

        let session_id = cx
            .update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.update(cx, |store, cx| {
                    store.create_session(
                        solution_id.clone(),
                        agent_id.clone(),
                        project.clone(),
                        cx,
                    )
                })
            })
            .await
            .expect("create_session");

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                assert!(store.session(session_id).is_some());
                assert_eq!(store.pool_size(), 1);
            });
        });
        assert_eq!(connect_count.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    async fn parallel_create_session_for_same_pair_spawns_only_once(
        cx: &mut TestAppContext,
    ) {
        let (solution_id, _tmp, project) = setup_solution_and_project(cx).await;
        let agent_id = SharedString::from("mock-agent");

        // Gate `connect()` until both create_session calls have observed the
        // pool entry — this guarantees the second call sees `Pending` and
        // doesn't race past into a fresh spawn before the first one inserts.
        let (gate_tx, gate_rx) = async_channel::bounded(1);
        let connect_count = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            let registry = Arc::new(AdapterRegistry::new());
            SolutionAgentStore::init_global(cx, registry);
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.register_agent_server(
                    agent_id.clone(),
                    Rc::new(MockAgentServer::with_gate(connect_count.clone(), gate_rx)),
                );
            });
        });

        let task1 = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session(
                    solution_id.clone(),
                    agent_id.clone(),
                    project.clone(),
                    cx,
                )
            })
        });
        let task2 = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session(
                    solution_id.clone(),
                    agent_id.clone(),
                    project.clone(),
                    cx,
                )
            })
        });

        // Pump scheduler so both tasks reach the await on `connect_task`.
        cx.executor().run_until_parked();
        // Now release the gate, letting connect() resolve.
        gate_tx.send(()).await.expect("gate send");
        gate_tx.close();

        let id1 = task1.await.expect("task1");
        let id2 = task2.await.expect("task2");
        assert_ne!(id1, id2);

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                assert_eq!(store.pool_size(), 1);
                assert!(store.session(id1).is_some());
                assert!(store.session(id2).is_some());
            });
        });
        assert_eq!(connect_count.load(Ordering::SeqCst), 1);
    }

    /// Create a real session (via `create_session`) backed by `MockAgentServer`/
    /// `MockConnection`, then return both its id and a clone of the underlying
    /// `Entity<AcpThread>` so tests can emit synthetic `AcpThreadEvent`s.
    async fn create_session_with_thread(
        cx: &mut TestAppContext,
    ) -> (
        SolutionSessionId,
        gpui::Entity<acp_thread::AcpThread>,
        tempfile::TempDir,
    ) {
        let (solution_id, tmp, project) = setup_solution_and_project(cx).await;
        let agent_id = SharedString::from("mock-agent");

        let connect_count = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            let registry = Arc::new(AdapterRegistry::new());
            SolutionAgentStore::init_global(cx, registry);
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.register_agent_server(
                    agent_id.clone(),
                    Rc::new(MockAgentServer::new(connect_count.clone())),
                );
            });
        });

        let session_id = cx
            .update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.update(cx, |store, cx| {
                    store.create_session(
                        solution_id.clone(),
                        agent_id.clone(),
                        project.clone(),
                        cx,
                    )
                })
            })
            .await
            .expect("create_session");

        let acp_thread = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store
                .read(cx)
                .session(session_id)
                .expect("session exists")
                .read(cx)
                .acp_thread
                .clone()
                .expect("acp_thread populated")
        });

        (session_id, acp_thread, tmp)
    }

    #[gpui::test]
    async fn turn_complete_event_transitions_running_to_idle(cx: &mut TestAppContext) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                let session = store.session(session_id).expect("session exists");
                session.update(cx, |s, _| {
                    s.state = SessionState::Running {
                        started_at: std::time::Instant::now(),
                        notified: false,
                    };
                });
            });
        });

        cx.update(|cx| {
            acp_thread.update(cx, |_thread, cx| {
                cx.emit(acp_thread::AcpThreadEvent::Stopped(
                    agent_client_protocol::schema::StopReason::EndTurn,
                ));
            });
        });
        cx.executor().run_until_parked();

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                let session = store.session(session_id).expect("session exists");
                let state = session.read(cx).state.clone();
                assert!(
                    matches!(state, SessionState::Idle),
                    "expected Idle, got {:?}",
                    state
                );
            });
        });
    }

    #[gpui::test]
    async fn error_event_transitions_to_errored_state(cx: &mut TestAppContext) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

        cx.update(|cx| {
            acp_thread.update(cx, |_thread, cx| {
                cx.emit(acp_thread::AcpThreadEvent::Error);
            });
        });
        cx.executor().run_until_parked();

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                let session = store.session(session_id).expect("session exists");
                let state = session.read(cx).state.clone();
                assert!(
                    matches!(state, SessionState::Errored(_)),
                    "expected Errored, got {:?}",
                    state
                );
            });
        });
    }

    #[gpui::test]
    async fn tool_authorization_request_transitions_to_awaiting_input(
        cx: &mut TestAppContext,
    ) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

        cx.update(|cx| {
            acp_thread.update(cx, |_thread, cx| {
                cx.emit(acp_thread::AcpThreadEvent::ToolAuthorizationRequested(
                    agent_client_protocol::schema::ToolCallId::new("test-tool"),
                ));
            });
        });
        cx.executor().run_until_parked();

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                let session = store.session(session_id).expect("session exists");
                let state = session.read(cx).state.clone();
                assert!(
                    matches!(state, SessionState::AwaitingInput),
                    "expected AwaitingInput, got {:?}",
                    state
                );
            });
        });
    }
}

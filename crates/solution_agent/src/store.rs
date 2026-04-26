use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, Subscription};
use solutions::{SolutionId, SolutionStore, SolutionStoreEvent};

use crate::adapter::AdapterRegistry;
use crate::db::SolutionAgentDb;
use crate::model::{AgentServerId, SolutionSession, SolutionSessionId};
use crate::pool::{PooledConnection, SHUTDOWN_DEBOUNCE, SpawnState, SubprocessPool};

pub struct SolutionAgentStore {
    sessions: HashMap<SolutionSessionId, Entity<SolutionSession>>,
    by_solution: HashMap<SolutionId, Vec<SolutionSessionId>>,
    pool: parking_lot::Mutex<SubprocessPool>,
    persistence: Option<Arc<SolutionAgentDb>>,
    pub(crate) adapters: Arc<AdapterRegistry>,
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
            _solution_subscription: solution_subscription,
        }
    }

    pub fn set_persistence(&mut self, db: Arc<SolutionAgentDb>) {
        self.persistence = Some(db);
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
    use std::rc::Rc;

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

    /// Trivial AgentConnection mock — every method stubs.
    struct MockConnection;

    impl acp_thread::AgentConnection for MockConnection {
        fn agent_id(&self) -> project::AgentId {
            project::AgentId::new("mock-agent")
        }
        fn telemetry_id(&self) -> SharedString {
            SharedString::from("mock")
        }
        fn new_session(
            self: Rc<Self>,
            _project: gpui::Entity<project::Project>,
            _work_dirs: util::path_list::PathList,
            _cx: &mut App,
        ) -> Task<anyhow::Result<gpui::Entity<acp_thread::AcpThread>>> {
            Task::ready(Err(anyhow::anyhow!("not used in this test")))
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

    #[gpui::test]
    async fn pool_release_arms_60s_shutdown_then_drops(cx: &mut TestAppContext) {
        let registry = Arc::new(AdapterRegistry::new());
        cx.update(|cx| SolutionAgentStore::init_global(cx, registry));

        let key = (SolutionId("sol-a".into()), SharedString::from("mock-agent"));

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection));
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
                store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection));
                store.pool_release_session(key.clone(), cx);
            });
        });

        cx.executor()
            .advance_clock(std::time::Duration::from_secs(30));
        cx.executor().run_until_parked();

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, _| {
                store.pool_pretend_session_added(key.clone(), Rc::new(MockConnection));
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
}

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use futures::future::Shared;
use gpui::Task;

use acp_thread::AgentConnection;
use solutions::SolutionId;

use crate::model::AgentServerId;

pub(crate) const SHUTDOWN_DEBOUNCE: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(crate) enum SpawnState {
    Pending(Shared<Task<Result<Rc<dyn AgentConnection>, std::sync::Arc<anyhow::Error>>>>),
    Ready(Rc<dyn AgentConnection>),
    Failed(std::sync::Arc<anyhow::Error>),
}

pub(crate) struct PooledConnection {
    pub(crate) state: SpawnState,
    pub(crate) live_session_count: usize,
    pub(crate) shutdown_task: Option<Task<()>>,
}

pub(crate) struct SubprocessPool {
    entries: HashMap<(SolutionId, AgentServerId), PooledConnection>,
}

impl SubprocessPool {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn entry_mut(
        &mut self,
        key: &(SolutionId, AgentServerId),
    ) -> Option<&mut PooledConnection> {
        self.entries.get_mut(key)
    }

    pub fn insert(&mut self, key: (SolutionId, AgentServerId), entry: PooledConnection) {
        self.entries.insert(key, entry);
    }

    pub fn remove(&mut self, key: &(SolutionId, AgentServerId)) {
        self.entries.remove(key);
    }

    pub fn keys_for_solution<'a>(
        &'a self,
        solution_id: &'a SolutionId,
    ) -> impl Iterator<Item = (SolutionId, AgentServerId)> + 'a {
        self.entries
            .keys()
            .filter(move |(s, _)| s == solution_id)
            .cloned()
    }

    #[cfg(any(feature = "test-support", test))]
    pub fn pair_count(&self) -> usize {
        self.entries.len()
    }
}

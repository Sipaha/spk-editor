//! Connection + `AgentServer` implementations for the native claude
//! stream-json backend.
//!
//! Both types are stubs at this stage: `ClaudeNativeAgentServer::connect`
//! returns an error until the real session/process machinery lands in later
//! phases. The wiring exists so `solution_agent` can select this backend via
//! the `solution_agent.claude_backend = "native"` setting.

use std::any::Any;
use std::rc::Rc;

use acp_thread::AgentConnection;
use agent_servers::{AgentServer, AgentServerDelegate};
use anyhow::{Result, anyhow};
use gpui::{App, Entity, Task};
use project::{AgentId, Project};
use ui::IconName;

/// Per-session connection to a `claude` subprocess. Filled in during Phase 5.
pub struct ClaudeNativeConnection;

/// `AgentServer` that spawns the `claude` binary directly (no node wrapper).
/// Stub `connect` until the process/session machinery lands.
pub struct ClaudeNativeAgentServer {
    agent_id: AgentId,
}

impl ClaudeNativeAgentServer {
    pub fn new(agent_id: AgentId) -> Self {
        Self { agent_id }
    }
}

impl AgentServer for ClaudeNativeAgentServer {
    fn logo(&self) -> IconName {
        IconName::AiClaude
    }

    fn agent_id(&self) -> AgentId {
        self.agent_id.clone()
    }

    fn connect(
        &self,
        _delegate: AgentServerDelegate,
        _project: Entity<Project>,
        _cx: &mut App,
    ) -> Task<Result<Rc<dyn AgentConnection>>> {
        Task::ready(Err(anyhow!("native claude backend not yet implemented")))
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}

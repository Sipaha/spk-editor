//! Tier-enforcing wrapper for [`McpServerTool`] (S-BAK).
//!
//! [`TierGuardTool<T>`] wraps an inner [`McpServerTool`] and rejects calls
//! whose registered tier exceeds the connecting caller's [`CallerCapabilities`].
//! The caps used for the check come from a process-global cell populated by
//! [`set_process_caps`] (set once during [`crate::lifecycle::start_server`]
//! from the [`crate::tier::BRIDGE_CAPS_ENV_VAR`] env value).
//!
//! Trade-off: the cell is process-global, not per-connection — every connection
//! that lands on this server sees the same caps. That's correct for the common
//! pattern in this fork (one `--nc` subprocess per ACP subagent, env value
//! stamped on the subprocess and propagated to its `nc` child) but it would
//! be wrong if the editor itself accepted multiple concurrent subagent
//! connections with different capability profiles. A future task
//! ([git-panel-plan §S-BAK-PER-CONN]) can either thread caps through
//! `serve_connection` (touches upstream `context_server`) or have `nc` send
//! them as a handshake notification on the socket — for now we accept the
//! coarseness.

use std::sync::OnceLock;

use anyhow::Result;
use context_server::listener::{McpServerTool, ToolResponse};
use gpui::AsyncApp;

use crate::tier::{CallerCapabilities, ToolTier};

static PROCESS_CAPS: OnceLock<CallerCapabilities> = OnceLock::new();

/// Apply the process-global caller capabilities. Called once from
/// [`crate::lifecycle::start_server`] using the [`crate::tier::BRIDGE_CAPS_ENV_VAR`]
/// env value. Subsequent calls are silently ignored (`OnceLock`).
pub(crate) fn set_process_caps(caps: CallerCapabilities) {
    let _ = PROCESS_CAPS.set(caps);
}

/// Read the active capability profile. Defaults to
/// [`CallerCapabilities::SUBAGENT_DEFAULT`] (Write tier) when not yet set —
/// matches the spec rule "missing env = Write".
pub fn current_caps() -> CallerCapabilities {
    PROCESS_CAPS
        .get()
        .copied()
        .unwrap_or(CallerCapabilities::SUBAGENT_DEFAULT)
}

/// Wrap an [`McpServerTool`] with tier-check enforcement. The inner tool's
/// `Input`, `Output`, and `NAME` are passed through unchanged so the wire
/// protocol view is identical to the unwrapped tool.
#[derive(Clone)]
pub struct TierGuardTool<T: McpServerTool + Clone> {
    inner: T,
    tier: ToolTier,
}

impl<T: McpServerTool + Clone> TierGuardTool<T> {
    pub fn new(inner: T, tier: ToolTier) -> Self {
        Self { inner, tier }
    }
}

impl<T> McpServerTool for TierGuardTool<T>
where
    T: McpServerTool + Clone,
{
    type Input = T::Input;
    type Output = T::Output;
    const NAME: &'static str = T::NAME;

    fn annotations(&self) -> context_server::types::ToolAnnotations {
        self.inner.annotations()
    }

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let caps = current_caps();
        if !self.tier.permits(caps.allowed_tier) {
            // Custom error code -32401 per S-BAK plan §P-4: tools rejected
            // for insufficient capability return a recognisable code distinct
            // from the standard JSON-RPC error codes.
            anyhow::bail!(
                "tool {} requires {:?} capability (caller has {:?}) [code=-32401]",
                T::NAME,
                self.tier,
                caps.allowed_tier
            );
        }
        self.inner.run(input, cx).await
    }
}

// Per-tool integration tests for TierGuardTool would require a fresh process
// per test (the caps cell is a `OnceLock`); the tier-permit logic itself is
// covered by unit tests in `tier.rs`. End-to-end enforcement is exercised
// indirectly by any e2e test that connects through the `--nc` bridge with a
// non-default `SPK_EDITOR_MCP_BRIDGE_CAPS` value.

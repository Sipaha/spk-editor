//! Handoff: connect to an existing instance's MCP socket and forward CLI args.
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug)]
pub enum HandoffOutcome {
    /// We acquired the lock — we are the canonical instance.
    BecameCanonical,
    /// Existing instance accepted the handoff. The caller should exit(0).
    HandedOff { focused_window_id: Option<String> },
    /// Lock held but socket unreachable after retries.
    LockBusyButUnreachable { lockholder_pid: Option<u32> },
}

pub fn try_handoff_to_existing_instance(_paths: Vec<PathBuf>) -> Result<HandoffOutcome> {
    // Stub — implemented in Task 1.2 + 2.1.
    Ok(HandoffOutcome::BecameCanonical)
}

//! Branch-protection check stub. Real implementation lands in S-SOL-PRT
//! (see `docs/superpowers/plans/git-panel-plan.md`); for now every
//! query is `Allowed` so callers exercise the integration path without
//! changing behaviour.
//!
//! Call sites in S-DST destructive ops invoke `check(...)` before
//! handing off to git so the future check can refuse with `Denied`
//! and surface the protected-branch reason in the UI without a code
//! change.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    Denied { reason: String },
}

/// Currently always `Allowed`. The signature matches the future
/// S-SOL-PRT API: caller passes the `repo_path`, the `branch` whose tip
/// is being mutated, and the `op_name` (matching
/// `AtomicGitOp::op_name`) for policy lookups.
pub fn check(_repo_path: &Path, _branch: &str, _op_name: &str) -> Decision {
    Decision::Allowed
}

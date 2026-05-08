//! S-CTM operation handlers — non-destructive commit operations dispatched
//! from the Git Graph commit context menu. Each submodule owns one
//! action family: clipboard / branch / tag / checkout / compare. All
//! handlers return `Task<Result<()>>` so the caller can attach
//! `.detach_and_prompt_err(...)` for UI-side error reporting.
//!
//! Destructive operations (cherry-pick / revert / reset / drop / squash /
//! merge / rebase) are out of scope here — they land via the S-DST work
//! along with backup-ref creation and the `OpRunner` framework.

pub mod branch;
pub mod checkout;
pub mod compare;
pub mod copy;
pub mod tag;

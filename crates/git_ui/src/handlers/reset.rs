//! S-DST reset handler.

use anyhow::{Result, anyhow};
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::OpRunner;
use git::operations::RunOutcome;
use git::operations::reset::{ResetMode, ResetOp};

pub fn run(
    repo_path: PathBuf,
    sha: String,
    mode: ResetMode,
    cx: &mut App,
) -> Task<Result<RunOutcome>> {
    cx.background_spawn(async move {
        OpRunner::run(ResetOp { sha, mode }, &repo_path)
            .map_err(|err| anyhow!("reset failed: {err}"))
    })
}

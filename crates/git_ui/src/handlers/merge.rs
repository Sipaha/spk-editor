//! S-DST merge handler.

use anyhow::{Result, anyhow};
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::OpRunner;
use git::operations::RunOutcome;
use git::operations::merge::MergeOp;

pub fn run(
    repo_path: PathBuf,
    target_branch: String,
    no_ff: bool,
    squash: bool,
    message: Option<String>,
    cx: &mut App,
) -> Task<Result<RunOutcome>> {
    cx.background_spawn(async move {
        OpRunner::run(
            MergeOp {
                target_branch,
                no_ff,
                squash,
                message,
            },
            &repo_path,
        )
        .map_err(|err| anyhow!("merge failed: {err}"))
    })
}

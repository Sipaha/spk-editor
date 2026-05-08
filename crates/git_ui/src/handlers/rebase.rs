//! S-DST linear rebase handler ("Rebase Current onto Branch…").

use anyhow::{Result, anyhow};
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::OpRunner;
use git::operations::RunOutcome;
use git::operations::linear_rebase::LinearRebaseOp;

pub fn run(
    repo_path: PathBuf,
    target_branch: String,
    autostash: bool,
    cx: &mut App,
) -> Task<Result<RunOutcome>> {
    cx.background_spawn(async move {
        OpRunner::run(
            LinearRebaseOp {
                target_branch,
                autostash,
            },
            &repo_path,
        )
        .map_err(|err| anyhow!("rebase failed: {err}"))
    })
}

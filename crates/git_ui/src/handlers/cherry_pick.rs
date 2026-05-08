//! S-DST cherry-pick handler. Runs [`git::operations::cherry_pick::CherryPickOp`]
//! through `OpRunner` on a background task. On `PausedForConflict`, the
//! caller is expected to surface the conflict resolver — wired by
//! `git_graph::context_menu` after this returns.

use anyhow::{Result, anyhow};
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::OpRunner;
use git::operations::RunOutcome;
use git::operations::cherry_pick::CherryPickOp;

pub fn run(
    repo_path: PathBuf,
    shas: Vec<String>,
    no_commit: bool,
    mainline: Option<u32>,
    x: bool,
    cx: &mut App,
) -> Task<Result<RunOutcome>> {
    cx.background_spawn(async move {
        OpRunner::run(
            CherryPickOp {
                shas,
                no_commit,
                mainline,
                x,
            },
            &repo_path,
        )
        .map_err(|err| anyhow!("cherry_pick failed: {err}"))
    })
}

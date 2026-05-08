//! S-DST drop-commit handler.

use anyhow::Result;
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::drop_commit::run_drop;
use git::operations::rebase::{RebaseCallbacks, RebaseHandle};

pub fn run(
    repo_path: PathBuf,
    sha: String,
    callbacks: RebaseCallbacks,
    cx: &mut App,
) -> Task<Result<RebaseHandle>> {
    cx.background_spawn(async move { run_drop(&repo_path, &sha, callbacks).await })
}

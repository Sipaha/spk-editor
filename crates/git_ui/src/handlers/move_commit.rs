//! S-DST move-commit handler.

use anyhow::Result;
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::move_commit::{BeforeOrAfter, MoveCommitOp};
use git::operations::rebase::{RebaseCallbacks, RebaseHandle};

pub fn run(
    repo_path: PathBuf,
    source_sha: String,
    target_sha: String,
    position: BeforeOrAfter,
    callbacks: RebaseCallbacks,
    cx: &mut App,
) -> Task<Result<RebaseHandle>> {
    cx.background_spawn(async move {
        MoveCommitOp {
            source_sha,
            target_sha,
            position,
        }
        .run(&repo_path, callbacks)
        .await
    })
}

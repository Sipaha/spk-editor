//! S-DST squash handler.

use anyhow::Result;
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::rebase::{RebaseCallbacks, RebaseHandle};
use git::operations::squash::SquashOp;

pub fn run(
    repo_path: PathBuf,
    shas: Vec<String>,
    final_message: String,
    callbacks: RebaseCallbacks,
    cx: &mut App,
) -> Task<Result<RebaseHandle>> {
    cx.background_spawn(async move {
        SquashOp {
            shas,
            final_message,
        }
        .run(&repo_path, callbacks)
        .await
    })
}

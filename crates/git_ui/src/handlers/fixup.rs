//! S-DST fixup handler.

use anyhow::Result;
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::fixup::FixupOp;
use git::operations::rebase::{RebaseCallbacks, RebaseHandle};

pub fn run(
    repo_path: PathBuf,
    shas: Vec<String>,
    callbacks: RebaseCallbacks,
    cx: &mut App,
) -> Task<Result<RebaseHandle>> {
    cx.background_spawn(async move { FixupOp { shas }.run(&repo_path, callbacks).await })
}

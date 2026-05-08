//! S-DST edit-message handler. Splits the HEAD-amend path (sync) from
//! the past-commit path (async via rebase).

use anyhow::Result;
use gpui::{App, AppContext as _, Task};
use std::path::PathBuf;

use git::operations::edit_commit_message::{EditMessageOp, EditMessageOutcome};
use git::operations::rebase::RebaseCallbacks;

pub fn run(
    repo_path: PathBuf,
    sha: String,
    new_message: String,
    callbacks: RebaseCallbacks,
    cx: &mut App,
) -> Task<Result<EditMessageOutcome>> {
    cx.background_spawn(async move {
        EditMessageOp { sha, new_message }
            .run(&repo_path, callbacks)
            .await
    })
}

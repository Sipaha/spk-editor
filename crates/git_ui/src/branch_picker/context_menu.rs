//! Per-branch context menu for the S-BRP Branches popup. Wires entries
//! to existing infrastructure (S-CTM compare/checkout/copy, S-BAK
//! atomic-op runner, S-PSH stubs). Entries that need a not-yet-landed
//! sibling task (S-DST destructive ops, S-PSH push dialog) are kept in
//! the menu shape but disabled with an explanatory tooltip — this keeps
//! the menu shape stable across releases.

use git::operations::{DeleteBranchOp, OpRunner};
use gpui::{App, ClipboardItem, Entity, IntoElement, SharedString, WeakEntity, Window};
use project::git_store::Repository;
use ui::{ContextMenu, ContextMenuEntry, DocumentationSide, prelude::*};
use util::ResultExt as _;
use workspace::Workspace;

use crate::branch_picker::favorites;
use crate::handlers::compare as compare_handlers;
use crate::project_diff::ProjectDiff;

/// Surrounding context for a branch row's context menu. Cheap to clone.
#[derive(Clone)]
pub struct BranchContext {
    pub workspace: WeakEntity<Workspace>,
    pub repository: Entity<Repository>,
    pub branch_name: SharedString,
    pub is_remote: bool,
    pub is_head: bool,
    pub is_favorite: bool,
}

/// Build the context menu for a regular branch row (Recent / Local /
/// Remote / Favorites tabs). Tag rows use [`build_tag_menu`] instead.
pub fn build_branch_menu(
    ctx: BranchContext,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    ContextMenu::build(window, cx, move |menu, _window, _cx| {
        let mut menu = menu;

        if !ctx.is_head {
            let checkout_ctx = ctx.clone();
            menu = menu.entry("Checkout", None, move |_window, cx| {
                checkout(checkout_ctx.clone(), cx);
            });
        }

        let new_branch_ctx = ctx.clone();
        menu = menu
            .entry("Checkout as New Branch From Here…", None, move |_window, cx| {
                checkout_as_new(new_branch_ctx.clone(), cx);
            })
            .separator();

        let cwc_ctx = ctx.clone();
        menu = menu.entry("Compare with Current", None, move |window, cx| {
            compare_with_current(cwc_ctx.clone(), window, cx);
        });
        let dwt_ctx = ctx.clone();
        menu = menu.entry("Show Diff with Working Tree", None, move |window, cx| {
            compare_with_current(dwt_ctx.clone(), window, cx);
        });

        // S-DST stubs — destructive sub-actions; menu shape locked so the
        // S-DST PR drops in handlers without rearranging entries.
        menu = menu
            .separator()
            .item(disabled_entry("Rebase Current Onto…", "Not yet implemented — S-DST"))
            .item(disabled_entry("Merge Into Current…", "Not yet implemented — S-DST"));

        menu = menu
            .separator()
            .entry("Pull", None, |window, cx| {
                window.dispatch_action(Box::new(git::Pull), cx);
            })
            .entry("Push", None, |window, cx| {
                window.dispatch_action(Box::new(git::Push), cx);
            })
            .item(disabled_entry("Force Push", "Not yet implemented — S-PSH"));

        let upstream_ctx = ctx.clone();
        menu = menu.separator().entry("Set Upstream…", None, move |window, cx| {
            open_set_upstream_modal(upstream_ctx.clone(), window, cx);
        });

        if !ctx.is_remote {
            let rename_ctx = ctx.clone();
            menu = menu.entry("Rename…", None, move |window, cx| {
                open_rename_modal(rename_ctx.clone(), window, cx);
            });
        }

        if !ctx.is_head {
            let delete_ctx = ctx.clone();
            menu = menu.entry("Delete", None, move |_window, cx| {
                delete_branch(delete_ctx.clone(), cx);
            });
        }

        let star_label = if ctx.is_favorite {
            "Unfavorite Branch"
        } else {
            "Favorite Branch"
        };
        let fav_ctx = ctx.clone();
        menu = menu.separator().entry(star_label, None, move |_window, cx| {
            toggle_favorite(fav_ctx.clone(), cx);
        });

        let copy_branch_name = ctx.branch_name.to_string();
        menu = menu.entry("Copy Branch Name", None, move |_window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_branch_name.clone()));
        });
        menu
    })
}

/// Per-tag row context menu (Tags tab).
#[derive(Clone)]
pub struct TagContext {
    pub workspace: WeakEntity<Workspace>,
    pub repository: Entity<Repository>,
    pub tag_name: SharedString,
}

pub fn build_tag_menu(
    ctx: TagContext,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    ContextMenu::build(window, cx, move |menu, _window, _cx| {
        let checkout_ctx = ctx.clone();
        let compare_ctx = ctx.clone();
        let push_ctx = ctx.clone();
        let delete_ctx = ctx.clone();
        let copy_ctx = ctx;
        menu.entry("Checkout", None, move |_window, cx| {
            let tag = checkout_ctx.tag_name.clone();
            let repo = checkout_ctx.repository.clone();
            cx.spawn(async move |cx| {
                let recv = repo
                    .update(cx, |repo, _| repo.checkout_revision(tag.to_string()));
                recv.await??;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        })
        .entry("Compare with Working Tree", None, move |window, cx| {
            if let Some(workspace) = compare_ctx.workspace.upgrade() {
                workspace.update(cx, |workspace, cx| {
                    compare_handlers::compare_with_local_working_tree(
                        workspace,
                        compare_ctx.tag_name.as_ref(),
                        window,
                        cx,
                    );
                });
            }
        })
        .entry("Push Tag", None, move |_window, cx| {
            let tag = push_ctx.tag_name.clone();
            let repo = push_ctx.repository.clone();
            cx.spawn(async move |cx| {
                let recv = repo.update(cx, |repo, _| {
                    repo.push_tag("origin".into(), tag.to_string())
                });
                recv.await??;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        })
        .entry("Delete Tag", None, move |_window, cx| {
            let tag = delete_ctx.tag_name.clone();
            let repo = delete_ctx.repository.clone();
            cx.spawn(async move |cx| {
                let recv = repo.update(cx, |repo, _| repo.delete_tag(tag.to_string()));
                recv.await??;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        })
        .separator()
        .entry("Copy Tag Name", None, move |_window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_ctx.tag_name.to_string()));
        })
    })
}

fn checkout(ctx: BranchContext, cx: &mut App) {
    let branch = ctx.branch_name;
    let repo = ctx.repository;
    let work_dir = repo.read(cx).work_directory_abs_path.clone();
    cx.spawn(async move |cx| {
        let recv = repo.update(cx, |repo, _| repo.change_branch(branch.to_string()));
        recv.await??;
        favorites::record_checkout(&work_dir, branch.as_ref()).log_err();
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn checkout_as_new(ctx: BranchContext, cx: &mut App) {
    // `git switch -c <new> <base>` accepts a branch name as the base, so
    // we don't need to resolve to a sha first. Pick a safe default name
    // — the user will rename via the rename modal afterward if desired.
    let branch = ctx.branch_name;
    let repo = ctx.repository;
    cx.spawn(async move |cx| {
        let new_name = format!("{}-copy", branch);
        let recv = repo.update(cx, |repo, _| {
            repo.create_branch(new_name, Some(branch.to_string()))
        });
        recv.await??;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn compare_with_current(ctx: BranchContext, window: &mut Window, cx: &mut App) {
    let workspace = ctx.workspace;
    let branch = ctx.branch_name;
    if let Some(workspace) = workspace.upgrade() {
        workspace.update(cx, |workspace, cx| {
            ProjectDiff::deploy_at_revision(
                workspace,
                SharedString::from(branch.to_string()),
                window,
                cx,
            );
        });
    }
}

fn open_set_upstream_modal(ctx: BranchContext, window: &mut Window, cx: &mut App) {
    if let Some(workspace) = ctx.workspace.upgrade() {
        let repo = ctx.repository;
        let branch = ctx.branch_name;
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                super::SetUpstreamModal::new(repo, branch, window, cx)
            });
        });
    }
}

fn open_rename_modal(ctx: BranchContext, window: &mut Window, cx: &mut App) {
    if let Some(workspace) = ctx.workspace.upgrade() {
        let repo = ctx.repository;
        let branch = ctx.branch_name;
        let work_dir = repo.read(cx).work_directory_abs_path.clone();
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                super::RenameBranchPopupModal::new(repo, branch, work_dir, window, cx)
            });
        });
    }
}

fn delete_branch(ctx: BranchContext, cx: &mut App) {
    let repo = ctx.repository.clone();
    let work_dir = repo.read(cx).work_directory_abs_path.clone();
    let branch = ctx.branch_name.clone();
    let is_remote = ctx.is_remote;
    if is_remote {
        // Remote-branch delete still goes through the existing
        // `Repository::delete_branch` path — we don't backup-ref remote
        // refs, since the remote retains them.
        cx.spawn(async move |cx| {
            let recv = repo.update(cx, |repo, _| {
                repo.delete_branch(true, branch.to_string())
            });
            recv.await??;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
        return;
    }

    // Local branch: route through `OpRunner` so we get a backup-ref
    // before the delete. Force-delete confirmation modal is owned by
    // S-DST; for now we attempt non-force `-d` and the caller surfaces
    // any "not fully merged" error.
    let work_dir_buf = work_dir.to_path_buf();
    let branch_string = branch.to_string();
    cx.background_spawn(async move {
        OpRunner::run(
            DeleteBranchOp {
                name: branch_string,
                force: false,
            },
            &work_dir_buf,
        )
        .log_err();
    })
    .detach();
}

fn toggle_favorite(ctx: BranchContext, cx: &mut App) {
    let work_dir = ctx.repository.read(cx).work_directory_abs_path.clone();
    let branch = ctx.branch_name.to_string();
    let work_dir_buf = work_dir.to_path_buf();
    cx.background_spawn(async move {
        favorites::toggle_favorite(&work_dir_buf, &branch).log_err();
    })
    .detach();
}

fn disabled_entry(label: &'static str, tooltip: &'static str) -> ContextMenuEntry {
    ContextMenuEntry::new(label).disabled(true).documentation_aside(
        DocumentationSide::Right,
        move |_| Label::new(tooltip).into_any_element(),
    )
}

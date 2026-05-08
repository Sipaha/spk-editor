//! MCP tools backing the S-CTM context menu — branch_create_at,
//! tag_create, checkout_revision, compare_revisions. Each tool is tiered
//! per S-BAK so subagents over the `--nc` bridge get the right capability
//! gate (writes need `--write`, reads need only `--read_only`).
//!
//! Source-of-truth: see the parent module `handlers/` for the in-process
//! handlers; the MCP tools wrap raw `git` invocations on the active
//! repository's working directory rather than going through `Repository`,
//! mirroring the existing `editor.git.commit_show` / `editor.git.list_backups`
//! tools (which avoid pulling `git_store::Repository` access into a
//! background task).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use editor_mcp::{ToolTier, register_typed_tool_with_tier};
use gpui::{App, AsyncApp};
use project::git_store::RepositoryId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use util::command::new_command;

pub(crate) fn register(cx: &mut App) {
    register_typed_tool_with_tier(cx, ToolTier::Write, BranchCreateAtTool);
    register_typed_tool_with_tier(cx, ToolTier::Write, TagCreateTool);
    register_typed_tool_with_tier(cx, ToolTier::Write, CheckoutRevisionTool);
    register_typed_tool_with_tier(cx, ToolTier::ReadOnly, CompareRevisionsTool);
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
/// Input for `editor.git.branch_create_at`. Creates a branch pointing at
/// `sha` without checking it out by default — flip `checkout` to do both.
pub struct BranchCreateAtInput {
    /// New branch name. Errors if a branch with that name already exists.
    pub name: String,
    /// Commit SHA the new branch should point at.
    pub sha: String,
    /// When `true`, additionally check out the new branch.
    pub checkout: bool,
    /// Repository to operate on. Defaults to the focused window's active
    /// repository.
    pub repo_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BranchCreateAtOutput {
    pub branch: String,
    pub sha: String,
    pub checked_out: bool,
}

#[derive(Clone)]
pub struct BranchCreateAtTool;

impl McpServerTool for BranchCreateAtTool {
    type Input = BranchCreateAtInput;
    type Output = BranchCreateAtOutput;
    const NAME: &'static str = "editor.git.branch_create_at";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let work_dir =
            cx.update(|cx| resolve_work_directory(input.repo_id.map(RepositoryId), cx))?;
        run_git_void(&work_dir, &["branch", &input.name, &input.sha]).await?;
        if input.checkout {
            run_git_void(&work_dir, &["checkout", &input.name]).await?;
        }
        let summary = if input.checkout {
            format!("created and checked out {} at {}", input.name, input.sha)
        } else {
            format!("created {} at {}", input.name, input.sha)
        };
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text: summary }],
            structured_content: BranchCreateAtOutput {
                branch: input.name,
                sha: input.sha,
                checked_out: input.checkout,
            },
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
/// Input for `editor.git.tag_create`. Annotated when `message` is `Some`,
/// lightweight otherwise.
pub struct TagCreateInput {
    pub name: String,
    pub sha: String,
    pub message: Option<String>,
    pub repo_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TagCreateOutput {
    pub tag: String,
    pub sha: String,
    pub annotated: bool,
}

#[derive(Clone)]
pub struct TagCreateTool;

impl McpServerTool for TagCreateTool {
    type Input = TagCreateInput;
    type Output = TagCreateOutput;
    const NAME: &'static str = "editor.git.tag_create";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let work_dir =
            cx.update(|cx| resolve_work_directory(input.repo_id.map(RepositoryId), cx))?;
        let annotated = input.message.is_some();
        if let Some(message) = &input.message {
            run_git_void(
                &work_dir,
                &["tag", "-a", "-m", message, &input.name, &input.sha],
            )
            .await?;
        } else {
            run_git_void(&work_dir, &["tag", &input.name, &input.sha]).await?;
        }
        let summary = format!(
            "created {}{} at {}",
            if annotated { "annotated tag " } else { "tag " },
            input.name,
            input.sha
        );
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text: summary }],
            structured_content: TagCreateOutput {
                tag: input.name,
                sha: input.sha,
                annotated,
            },
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
/// Input for `editor.git.checkout_revision`. Leaves HEAD detached on
/// success — the result includes the prior branch name so callers can
/// surface "you left $branch — switch back?" UX.
pub struct CheckoutRevisionInput {
    pub sha: String,
    pub repo_id: Option<u64>,
    /// When `false` (default), errors if the working tree is dirty. Set
    /// to `true` to invoke `git checkout` regardless — git will refuse
    /// when there are conflicting changes anyway.
    pub force_dirty: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CheckoutRevisionOutput {
    pub sha: String,
    pub detached_head: bool,
    pub prior_branch: Option<String>,
}

#[derive(Clone)]
pub struct CheckoutRevisionTool;

impl McpServerTool for CheckoutRevisionTool {
    type Input = CheckoutRevisionInput;
    type Output = CheckoutRevisionOutput;
    const NAME: &'static str = "editor.git.checkout_revision";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let work_dir =
            cx.update(|cx| resolve_work_directory(input.repo_id.map(RepositoryId), cx))?;
        if !input.force_dirty {
            let status = run_git(&work_dir, &["status", "--porcelain"]).await?;
            if !status.trim().is_empty() {
                return Err(anyhow!(
                    "working tree has uncommitted changes — set force_dirty=true to checkout anyway, or stash first"
                ));
            }
        }
        let prior_branch = run_git(&work_dir, &["symbolic-ref", "--short", "-q", "HEAD"])
            .await
            .ok()
            .and_then(|out| {
                let trimmed = out.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            });
        run_git_void(&work_dir, &["checkout", &input.sha]).await?;
        let summary = format!("checked out {} (detached HEAD)", input.sha);
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text: summary }],
            structured_content: CheckoutRevisionOutput {
                sha: input.sha,
                detached_head: true,
                prior_branch,
            },
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
/// Input for `editor.git.compare_revisions`. Read-only diff between two
/// revisions, optionally restricted to a path subset.
pub struct CompareRevisionsInput {
    pub rev_a: String,
    pub rev_b: String,
    /// Restrict the diff to one or more paths (relative to repo root).
    pub paths: Vec<String>,
    pub repo_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CompareRevisionsOutput {
    pub rev_a: String,
    pub rev_b: String,
    pub files: Vec<DiffFile>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DiffFile {
    pub path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    /// Old path when git detected a rename or copy; `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename_from: Option<String>,
}

#[derive(Clone)]
pub struct CompareRevisionsTool;

impl McpServerTool for CompareRevisionsTool {
    type Input = CompareRevisionsInput;
    type Output = CompareRevisionsOutput;
    const NAME: &'static str = "editor.git.compare_revisions";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let work_dir =
            cx.update(|cx| resolve_work_directory(input.repo_id.map(RepositoryId), cx))?;
        let mut numstat_args: Vec<&str> = vec!["diff", "--numstat", "-z", &input.rev_a, &input.rev_b];
        let mut namestatus_args: Vec<&str> = vec!["diff", "--name-status", "-z", &input.rev_a, &input.rev_b];
        if !input.paths.is_empty() {
            numstat_args.push("--");
            for p in &input.paths {
                numstat_args.push(p);
            }
            namestatus_args.push("--");
            for p in &input.paths {
                namestatus_args.push(p);
            }
        }
        let stat_out = run_git(&work_dir, &numstat_args).await?;
        let status_out = run_git(&work_dir, &namestatus_args).await?;
        let files = merge_diff(&stat_out, &status_out);
        let summary = format!(
            "diff {}..{}: {} files changed",
            short(&input.rev_a),
            short(&input.rev_b),
            files.len()
        );
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text: summary }],
            structured_content: CompareRevisionsOutput {
                rev_a: input.rev_a,
                rev_b: input.rev_b,
                files,
            },
        })
    }
}

fn short(rev: &str) -> String {
    rev.chars().take(7).collect()
}

fn merge_diff(numstat_z: &str, namestatus_z: &str) -> Vec<DiffFile> {
    let stats = parse_numstat_z(numstat_z);
    let statuses = parse_namestatus_z(namestatus_z);
    stats
        .into_iter()
        .map(|(path, additions, deletions, rename_from)| {
            let status = statuses
                .iter()
                .find(|(p, _, _)| p == &path)
                .map(|(_, status, _)| status.clone())
                .unwrap_or_else(|| "M".to_string());
            DiffFile {
                path,
                status,
                additions,
                deletions,
                rename_from,
            }
        })
        .collect()
}

fn parse_numstat_z(stdout: &str) -> Vec<(String, u32, u32, Option<String>)> {
    let mut out = Vec::new();
    let mut iter = stdout.split('\0').peekable();
    while let Some(record) = iter.next() {
        if record.is_empty() {
            continue;
        }
        let mut tabs = record.splitn(3, '\t');
        let additions: u32 = tabs.next().unwrap_or("0").parse().unwrap_or(0);
        let deletions: u32 = tabs.next().unwrap_or("0").parse().unwrap_or(0);
        let path_part = tabs.next().unwrap_or("");
        if path_part.is_empty() {
            let old = iter.next().unwrap_or("").to_string();
            let new = iter.next().unwrap_or("").to_string();
            out.push((new, additions, deletions, Some(old)));
        } else {
            out.push((path_part.to_string(), additions, deletions, None));
        }
    }
    out
}

fn parse_namestatus_z(stdout: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    let mut iter = stdout.split('\0').filter(|s| !s.is_empty());
    while let Some(record) = iter.next() {
        let (status, path_part) = match record.split_once('\t') {
            Some((status, rest)) => (status.to_string(), rest.to_string()),
            None => continue,
        };
        if (status.starts_with('R') || status.starts_with('C')) && path_part.is_empty() {
            let old = iter.next().unwrap_or("").to_string();
            let new = iter.next().unwrap_or("").to_string();
            out.push((new, status, Some(old)));
        } else if status.starts_with('R') || status.starts_with('C') {
            let new = iter.next().unwrap_or("").to_string();
            out.push((new, status, Some(path_part)));
        } else {
            out.push((path_part, status, None));
        }
    }
    out
}

async fn run_git(work_dir: &Path, args: &[&str]) -> Result<String> {
    let work_dir_buf: PathBuf = work_dir.to_path_buf();
    let mut command = new_command("git");
    command.current_dir(&work_dir_buf);
    command.args(args);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let output = command.output().await.context("running `git`")?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

async fn run_git_void(work_dir: &Path, args: &[&str]) -> Result<()> {
    run_git(work_dir, args).await.map(|_| ())
}

fn resolve_work_directory(repo_id: Option<RepositoryId>, cx: &mut App) -> Result<Arc<Path>> {
    let active_window_id = cx.active_window().map(|h| h.window_id());

    if let Some(want) = repo_id {
        for handle in cx.windows() {
            let Some(multi) = handle.downcast::<workspace::MultiWorkspace>() else {
                continue;
            };
            let found = multi
                .update(cx, |multi, _window, cx| {
                    for ws in multi.workspaces() {
                        let project = ws.read(cx).project();
                        let git_store = project.read(cx).git_store().clone();
                        let repo = git_store.read(cx).repositories().get(&want).cloned();
                        if let Some(repo) = repo {
                            return Some(repo.read(cx).work_directory_abs_path.clone());
                        }
                    }
                    None
                })
                .ok()
                .flatten();
            if let Some(dir) = found {
                return Ok(dir);
            }
        }
        return Err(anyhow!("repository_not_found: id={}", want.0));
    }

    for handle in cx.windows() {
        if active_window_id != Some(handle.window_id()) {
            continue;
        }
        let Some(multi) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let found = multi
            .update(cx, |multi, _window, cx| {
                for ws in multi.workspaces() {
                    let project = ws.read(cx).project();
                    if let Some(repo) = project.read(cx).active_repository(cx) {
                        return Some(repo.read(cx).work_directory_abs_path.clone());
                    }
                }
                None
            })
            .ok()
            .flatten();
        if let Some(dir) = found {
            return Ok(dir);
        }
    }
    Err(anyhow!("no_active_repository"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numstat_with_renames() {
        let raw = "5\t3\tsrc/foo.rs\x002\t1\t\x00src/old.rs\x00src/new.rs\x00";
        let entries = parse_numstat_z(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "src/foo.rs");
        assert_eq!(entries[0].1, 5);
        assert_eq!(entries[0].2, 3);
        assert!(entries[0].3.is_none());
        assert_eq!(entries[1].0, "src/new.rs");
        assert_eq!(entries[1].3.as_deref(), Some("src/old.rs"));
    }

    #[test]
    fn parses_namestatus_with_rename() {
        let raw = "M\tsrc/foo.rs\x00R100\t\x00src/old.rs\x00src/new.rs\x00";
        let entries = parse_namestatus_z(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("src/foo.rs".into(), "M".into(), None));
        assert_eq!(
            entries[1],
            (
                "src/new.rs".into(),
                "R100".into(),
                Some("src/old.rs".into())
            )
        );
    }
}

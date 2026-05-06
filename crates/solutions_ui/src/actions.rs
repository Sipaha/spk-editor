use gpui::{Action, actions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

actions!(
    solutions,
    [
        /// Open the picker to switch to a Solution.
        OpenSolution,
        /// Toggle the Solutions dock panel.
        ToggleSolutionsPanel,
        /// Create a new Solution.
        NewSolution,
        /// Refresh the cache for every catalog project referenced by the active Solution.
        RefreshCacheForCurrent,
        /// Open the catalog picker to add a project to the active Solution.
        /// Resolves "active Solution" via the workspace's worktrees (a
        /// solution-bound workspace has at least one worktree under
        /// `solution.root`, even when that worktree is hidden).
        AddProjectToActiveSolution,
    ]
);

/// Delete a Solution (with disk cleanup) by id. Triggered from the welcome
/// list's row trash icon; opens a confirmation modal that does the work.
#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, Action)]
#[action(namespace = solutions)]
pub struct DeleteSolution {
    pub id: String,
}

/// Open the edit modal for a catalog project (Name / Remote URL / default
/// branch). Triggered from the failed in-flight add row in the Solutions
/// panel — the most common reason an add fails is a wrong URL, and this
/// is the path the user clicks to fix it before retrying.
#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, Action)]
#[action(namespace = solutions)]
pub struct EditCatalogProject {
    pub id: String,
}

/// Open the delete-confirmation modal for a catalog project. Triggered
/// from the trash icon on a Catalog row. The modal lists every solution
/// that references the project so the user can see the cascade impact
/// before confirming.
#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, Action)]
#[action(namespace = solutions)]
pub struct DeleteCatalogProject {
    pub id: String,
}

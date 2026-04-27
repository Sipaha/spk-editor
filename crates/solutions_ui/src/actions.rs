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
    ]
);

/// Delete a Solution (with disk cleanup) by id. Triggered from the welcome
/// list's row trash icon; opens a confirmation modal that does the work.
#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, Action)]
#[action(namespace = solutions)]
pub struct DeleteSolution {
    pub id: String,
}

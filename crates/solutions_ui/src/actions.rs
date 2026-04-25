use gpui::actions;

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

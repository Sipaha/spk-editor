use gpui::actions;

actions!(
    solution_agent,
    [
        /// Create a new AI session in the current solution.
        NewSession,
        /// Focus the solution sessions navigator panel.
        FocusNavigator,
        /// Focus the active session view.
        FocusActiveSession,
        /// Cycle through sessions in the current solution.
        CycleSession,
        /// Duplicate the active session.
        DuplicateSession,
        /// Close the active session.
        CloseSession,
        /// Restart the agent subprocess for the active session.
        RestartAgent,
    ]
);

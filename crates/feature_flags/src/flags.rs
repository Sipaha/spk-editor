use crate::{EnumFeatureFlag, FeatureFlag, PresenceFlag, register_feature_flag};

pub struct NotebookFeatureFlag;

impl FeatureFlag for NotebookFeatureFlag {
    const NAME: &'static str = "notebooks";
    type Value = PresenceFlag;
}
register_feature_flag!(NotebookFeatureFlag);

pub struct PanicFeatureFlag;

impl FeatureFlag for PanicFeatureFlag {
    const NAME: &'static str = "panic";
    type Value = PresenceFlag;
}
register_feature_flag!(PanicFeatureFlag);

/// A feature flag for granting access to beta ACP features.
///
/// We reuse this feature flag for new betas, so don't delete it if it is not currently in use.
pub struct AcpBetaFeatureFlag;

impl FeatureFlag for AcpBetaFeatureFlag {
    const NAME: &'static str = "acp-beta";
    type Value = PresenceFlag;

    // SPK fork: force-enable. The flag gates `SessionUpdate::UsageUpdate`
    // → `AcpThread::token_usage` plumbing in `acp_thread.rs`, which is
    // what feeds the per-session token meter in the chat status row
    // (`solution_agent::status_row::render_status_row`).
    // Cloud feature flags are unwired in this fork (no telemetry, no
    // `client.zed.dev`), so without this override the meter would be
    // permanently stuck at `0 / 1.0M` even for active conversations.
    fn enabled_for_all() -> bool {
        true
    }
}
register_feature_flag!(AcpBetaFeatureFlag);

pub struct AgentSharingFeatureFlag;

impl FeatureFlag for AgentSharingFeatureFlag {
    const NAME: &'static str = "agent-sharing";
    type Value = PresenceFlag;
}
register_feature_flag!(AgentSharingFeatureFlag);

pub struct DiffReviewFeatureFlag;

impl FeatureFlag for DiffReviewFeatureFlag {
    const NAME: &'static str = "diff-review";
    type Value = PresenceFlag;

    fn enabled_for_staff() -> bool {
        false
    }
}
register_feature_flag!(DiffReviewFeatureFlag);

pub struct StreamingEditFileToolFeatureFlag;

impl FeatureFlag for StreamingEditFileToolFeatureFlag {
    const NAME: &'static str = "streaming-edit-file-tool";
    type Value = PresenceFlag;

    fn enabled_for_staff() -> bool {
        true
    }
}
register_feature_flag!(StreamingEditFileToolFeatureFlag);

pub struct UpdatePlanToolFeatureFlag;

impl FeatureFlag for UpdatePlanToolFeatureFlag {
    const NAME: &'static str = "update-plan-tool";
    type Value = PresenceFlag;

    fn enabled_for_staff() -> bool {
        false
    }
}
register_feature_flag!(UpdatePlanToolFeatureFlag);

pub struct ProjectPanelUndoRedoFeatureFlag;

impl FeatureFlag for ProjectPanelUndoRedoFeatureFlag {
    const NAME: &'static str = "project-panel-undo-redo";
    type Value = PresenceFlag;

    fn enabled_for_staff() -> bool {
        true
    }
}
register_feature_flag!(ProjectPanelUndoRedoFeatureFlag);

/// Controls how agent thread worktree chips are labeled in the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug, EnumFeatureFlag)]
pub enum AgentThreadWorktreeLabel {
    #[default]
    Both,
    Worktree,
    Branch,
}

pub struct AgentThreadWorktreeLabelFlag;

impl FeatureFlag for AgentThreadWorktreeLabelFlag {
    const NAME: &'static str = "agent-thread-worktree-label";
    type Value = AgentThreadWorktreeLabel;

    fn enabled_for_staff() -> bool {
        false
    }
}
register_feature_flag!(AgentThreadWorktreeLabelFlag);

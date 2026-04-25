//! Editor MCP — single-instance MCP server embedded in SPK Editor.
//!
//! Approach C central registry: domain crates register their tools during
//! their own `init()` via [`register_tool`]. After all init is done, the
//! editor calls [`start_server`] which binds the Unix socket and accepts
//! connections.

mod handoff;
mod lifecycle;
mod notifications;
mod operations;
mod registry;
mod subscriptions;
mod tools;
mod window_ids;

pub use handoff::{HandoffOutcome, try_handoff_to_existing_instance};
pub use lifecycle::start_server;
pub use registry::{init, register_tool};
pub use window_ids::format as format_window_id;

pub use operations::{
    OperationProgress, OperationState, OperationStatus,
    complete_cancelled as op_complete_cancelled, complete_err as op_complete_err,
    complete_ok as op_complete_ok, get as op_get, is_cancelled as op_is_cancelled,
    record_progress as op_record_progress, request_cancellation as op_request_cancellation,
    start as op_start,
};
pub use subscriptions::{
    Subscription, create as sub_create, delete as sub_delete, list as sub_list,
};

#[cfg(test)]
pub use lifecycle::start_server_for_test;

#[doc(hidden)]
pub mod tools_for_test {
    pub use crate::tools::capabilities::{CapabilitiesParams, CapabilitiesTool};
    pub use crate::tools::operations::{
        CancelOperationParams, CancelOperationTool, GetOperationParams, GetOperationTool,
    };
    pub use crate::tools::subscribe::{
        ListSubscriptionsParams, ListSubscriptionsTool, SubscribeParams, SubscribeTool,
        UnsubscribeParams, UnsubscribeTool,
    };
}

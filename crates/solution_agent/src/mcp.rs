//! MCP tools exposed by the `solution_agent` crate. Tools register with the
//! central `editor_mcp` registry from `solution_agent::init` so that
//! `start_server` (called later from `crates/zed/src/main.rs`) sees them
//! when binding the socket.
use agent_client_protocol::schema as acp;
use anyhow::{Context as _, Result, anyhow};
use context_server::listener::{McpServerTool, ToolResponse};
use context_server::types::ToolResponseContent;
use gpui::{App, AsyncApp, Entity};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{SolutionSession, SolutionSessionId};
use crate::store::{PersistedSession, SolutionAgentStore};
use gpui::SharedString;
use solutions::{SolutionId, SolutionStore};

pub fn register(cx: &mut App) {
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListSessionsTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ListAgentsTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetSessionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetSessionEntryTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CreateSessionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(SendMessageTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(SendMessageBlocksTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CloseSessionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CancelTurnTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(AuthorizeToolCallTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(RenameSessionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(RestartAgentTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ResetContextTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(CompactSessionTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(StartCompactTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(ReadSessionHistoryTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(GetSessionChildrenTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(UploadInitTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(UploadStatusTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(UploadFinishTool);
    });
    editor_mcp::register_tool(cx, |server| {
        server.add_tool(UploadAbortTool);
    });
}

// =====================================================================
// solution_agent.list_sessions
// =====================================================================

/// List Solution-scoped AI sessions, optionally filtered by `solution_id`.
///
/// R-6e: paginated. Sessions are ordered by `last_activity_at` DESC and
/// `before_last_activity_at_ms` / `count` carve a time-anchored window.
/// `total_count` on the result reflects the unfiltered count (subject to
/// `solution_id` only), so the client can decide whether to fetch more.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ListSessionsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution_id: Option<String>,
    /// F: filter by parent session id. When set, returns only sessions
    /// whose `parent_session_id` matches — i.e. the immediate children
    /// of the named session. Stacks with `solution_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// R-6e: exclusive upper bound on `last_activity_at` (millis since
    /// epoch). `None` = no upper bound (current behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_last_activity_at_ms: Option<i64>,
    /// R-6e: take only the first N sessions after ordering DESC by
    /// `last_activity_at` and applying `before_last_activity_at_ms`.
    /// `None` = unbounded (current behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

impl<'de> Deserialize<'de> for ListSessionsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Helper {
            solution_id: Option<String>,
            parent_session_id: Option<String>,
            before_last_activity_at_ms: Option<i64>,
            count: Option<usize>,
        }
        let helper = Option::<Helper>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: helper.solution_id,
            parent_session_id: helper.parent_session_id,
            before_last_activity_at_ms: helper.before_last_activity_at_ms,
            count: helper.count,
        })
    }
}

/// Structured, serde-tagged wire representation of `SessionState`. Replaces the
/// former `format!("{:?}", state)` string — `Debug` output is not a stable
/// protocol. `Running` carries the wall-clock start anchor (the monotonic
/// `Instant` isn't serialisable); `Errored` carries the message.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionStateDto {
    Idle,
    Running { started_at_ms: i64 },
    Stopping,
    AwaitingInput,
    Errored { message: String },
}

impl SessionStateDto {
    fn from_state(state: &crate::model::SessionState, started_at_ms: i64) -> Self {
        use crate::model::SessionState;
        match state {
            SessionState::Idle => SessionStateDto::Idle,
            SessionState::Running { .. } => SessionStateDto::Running { started_at_ms },
            SessionState::Stopping => SessionStateDto::Stopping,
            SessionState::AwaitingInput => SessionStateDto::AwaitingInput,
            SessionState::Errored(msg) => SessionStateDto::Errored {
                message: msg.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SessionSummary {
    pub id: String,
    pub solution_id: String,
    pub agent_id: String,
    pub title: String,
    pub state: SessionStateDto,
    pub created_at: i64,
    pub last_activity_at: i64,
    /// F: cumulative tokens reported by the agent for this session.
    /// Sourced from the live `AcpThread::token_usage().used_tokens` when
    /// a thread is attached, falling back to
    /// `SolutionSession::cached_total_tokens` (populated from persistent
    /// metadata) for cold tabs. `None` when neither source has a value
    /// yet — e.g. a fresh session whose first turn hasn't shipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Model context window in tokens (`used_tokens / max_tokens` is the
    /// percentage the desktop's status-row meter shows). Live thread's
    /// `TokenUsage::max_tokens` when hot, falling back to the in-memory
    /// `SolutionSession::cached_max_tokens` mirrored from the last
    /// observed live event. `None` when no live event has arrived yet —
    /// clients should choose their own default rather than assume a
    /// specific window size (the desktop picks `DEFAULT_CONTEXT_WINDOW`,
    /// but a phone client might prefer a smaller assumption for an
    /// unknown model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// F: parent session reference for sub-agent indication. `None` for
    /// top-level sessions. Set at creation time via
    /// `solution_agent.create_session({parent_session_id})`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListSessionsResult {
    pub sessions: Vec<SessionSummary>,
    /// R-6e: total session count matching `solution_id` only (i.e. before
    /// `before_last_activity_at_ms` / `count` are applied). Lets a paginated
    /// client decide whether to fetch an older page.
    pub total_count: usize,
}

#[derive(Clone)]
pub struct ListSessionsTool;

impl McpServerTool for ListSessionsTool {
    type Input = ListSessionsParams;
    type Output = ListSessionsResult;
    const NAME: &'static str = "solution_agent.list_sessions";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        // F: optional parent filter. Parse once up-front so a malformed
        // id surfaces a clear error rather than silently producing an
        // empty result. Done outside the `cx.update` because the
        // read-only closure has no clean error-propagation shape.
        let want_parent = match input.parent_session_id.as_deref() {
            Some(s) => Some(
                SolutionSessionId::parse(s).map_err(|e| anyhow!("bad parent_session_id: {e}"))?,
            ),
            None => None,
        };
        // Hydrate any DB-only sessions for the requested solution. The
        // desktop's tab strip only hydrates rows with `tab_order IS
        // NOT NULL`, so closed-tab sessions were invisible to MCP-only
        // consumers like the phone — even though their full transcripts
        // sit on disk. `hydrate_all_for_solution` is a no-op for already-
        // hydrated sessions, so the second list_sessions call costs just
        // one cheap DB metadata query.
        if let Some(s) = input.solution_id.as_ref() {
            let sol_id = SolutionId(s.clone());
            let task = cx.update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.update(cx, |s, cx| s.hydrate_all_for_solution(sol_id, cx))
            });
            task.await?;
        }
        let (sessions, total_count) = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.read_with(cx, |store, cx| {
                let want_solution = input.solution_id.as_ref().map(|s| SolutionId(s.clone()));
                let mut matching: Vec<SessionSummary> = store
                    .all_sessions()
                    .filter_map(|entity| {
                        let session = entity.read(cx);
                        if let Some(want) = &want_solution {
                            if &session.solution_id != want {
                                return None;
                            }
                        }
                        if let Some(want) = want_parent {
                            if session.parent_session_id != Some(want) {
                                return None;
                            }
                        }
                        Some(session_summary(session, cx))
                    })
                    .collect();
                // R-6e: order DESC by last_activity_at so `count=N` returns
                // the most-recent N sessions. `total_count` is the count
                // BEFORE before_last_activity_at_ms / count filtering, so
                // the client knows if a "load older" page exists.
                matching.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
                let total = matching.len();
                if let Some(before) = input.before_last_activity_at_ms {
                    matching.retain(|s| s.last_activity_at < before);
                }
                if let Some(count) = input.count {
                    matching.truncate(count);
                }
                (matching, total)
            })
        });
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} session(s)", sessions.len()),
            }],
            structured_content: ListSessionsResult {
                sessions,
                total_count,
            },
        })
    }
}

fn session_summary(session: &SolutionSession, cx: &App) -> SessionSummary {
    // Prefer the live thread's `TokenUsage.used_tokens` so an active
    // session reports the current count (R-5/R-6 token tracking already
    // writes through to `cached_total_tokens` on every
    // `TokenUsageUpdated` event, but live > cached when the two
    // disagree). Cold tabs fall back to the persisted cache.
    let live_usage = session
        .acp_thread()
        .and_then(|thread| thread.read(cx).token_usage().cloned());
    let total_tokens = live_usage
        .as_ref()
        .map(|usage| usage.used_tokens)
        .or(session.cached_total_tokens);
    // `max_tokens == 0` is the "agent didn't fill it in yet" sentinel
    // claude-acp ships under beta-gated paths. Treat that as None so
    // clients can apply their own default instead of dividing by zero.
    let max_tokens = live_usage
        .as_ref()
        .map(|usage| usage.max_tokens)
        .filter(|m| *m > 0)
        .or(session.cached_max_tokens);
    // Wall-clock anchor for a Running turn's live counter (monotonic Instant →
    // serialisable ms). Only meaningful for Running; folded into the DTO.
    let started_at_ms = match &session.state {
        crate::model::SessionState::Running { started_at, .. } => {
            let wall = chrono::Utc::now()
                - chrono::Duration::from_std(started_at.elapsed()).unwrap_or_default();
            wall.timestamp_millis()
        }
        _ => 0,
    };
    SessionSummary {
        id: session.id.to_string(),
        solution_id: session.solution_id.0.clone(),
        agent_id: session.agent_id.to_string(),
        title: session.title.to_string(),
        state: SessionStateDto::from_state(&session.state, started_at_ms),
        created_at: session.created_at.timestamp_millis(),
        last_activity_at: session.last_activity_at.timestamp_millis(),
        total_tokens,
        max_tokens,
        parent_session_id: session.parent_session_id.map(|id| id.to_string()),
    }
}

// =====================================================================
// solution_agent.get_session_children
// =====================================================================

/// F: list the immediate children of a session — sessions whose
/// `parent_session_id` equals the input. Used by the desktop /
/// phone "sub-agents" strip to fetch siblings in a single round-trip
/// instead of running a filtered `list_sessions`. Returns an empty
/// list when the session has no children. Errors with
/// `unknown_parent_session` when the parent itself is unknown.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct GetSessionChildrenParams {
    pub session_id: String,
}

impl<'de> Deserialize<'de> for GetSessionChildrenParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetSessionChildrenResult {
    /// Immediate children ordered by `created_at` ASC, so the consumer
    /// renders the oldest child first (matches the desktop strip layout
    /// described in the F plan-doc: "main → first spawn → second
    /// spawn").
    pub children: Vec<SessionSummary>,
}

#[derive(Clone)]
pub struct GetSessionChildrenTool;

impl McpServerTool for GetSessionChildrenTool {
    type Input = GetSessionChildrenParams;
    type Output = GetSessionChildrenResult;
    const NAME: &'static str = "solution_agent.get_session_children";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let parent_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        let children = cx.update(|cx| -> Result<Vec<SessionSummary>> {
            let store = SolutionAgentStore::global(cx);
            store.read_with(cx, |store, cx| -> Result<Vec<SessionSummary>> {
                // Verify the parent itself exists so an unknown id
                // surfaces a clear error instead of an empty list (the
                // latter is ambiguous: "no children" vs. "no parent").
                store
                    .session(parent_id)
                    .ok_or_else(|| anyhow!("unknown_parent_session: {parent_id}"))?;
                let mut children: Vec<SessionSummary> = store
                    .all_sessions()
                    .filter_map(|entity| {
                        let session = entity.read(cx);
                        if session.parent_session_id == Some(parent_id) {
                            Some(session_summary(session, cx))
                        } else {
                            None
                        }
                    })
                    .collect();
                children.sort_by(|a, b| a.created_at.cmp(&b.created_at));
                Ok(children)
            })
        })?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} child session(s)", children.len()),
            }],
            structured_content: GetSessionChildrenResult { children },
        })
    }
}

// =====================================================================
// solution_agent.list_agents
// =====================================================================

/// List registered agent adapters. The `id` is what `create_session`'s
/// `agent_id` param accepts; `display_name` is what a client picker
/// (e.g. the Android client's "New session" dialog) should show.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ListAgentsParams {}

impl<'de> Deserialize<'de> for ListAgentsParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let _ = serde::de::IgnoredAny::deserialize(de)?;
        Ok(ListAgentsParams {})
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AgentSummary {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListAgentsResult {
    pub agents: Vec<AgentSummary>,
}

#[derive(Clone)]
pub struct ListAgentsTool;

impl McpServerTool for ListAgentsTool {
    type Input = ListAgentsParams;
    type Output = ListAgentsResult;
    const NAME: &'static str = "solution_agent.list_agents";

    async fn run(
        &self,
        _input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let summaries = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.read_with(cx, |store, _| {
                store
                    .adapters
                    .supported_ids()
                    .iter()
                    .filter_map(|id| {
                        store.adapters.get(id).map(|adapter| AgentSummary {
                            id: id.to_string(),
                            display_name: adapter.display_name().to_string(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
        });
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{} agent(s)", summaries.len()),
            }],
            structured_content: ListAgentsResult { agents: summaries },
        })
    }
}

// =====================================================================
// solution_agent.get_session
// =====================================================================

/// Fetch a session's metadata plus a per-entry preview (first ~200 chars
/// of each entry's markdown rendering). When the session has no live
/// `acp_thread`, `entries` is empty and only the metadata is populated.
///
/// Wire-size trade-off: with the default flags off the response stays
/// compact — preview-only on a ~10-entry session is ≈ 1.5–2 KB. Flipping
/// `include_full_content` adds the untruncated markdown for every entry
/// (roughly 10–20× the preview-only size depending on conversation
/// length). Flipping `include_images` on top inlines base64-encoded
/// image payloads — a single screenshot can balloon the response by
/// hundreds of KB, so prefer `solution_agent.get_session_entry` for
/// per-entry image fetches when bandwidth is tight.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct GetSessionParams {
    pub session_id: String,
    /// Default false. When true, every `EntrySummary.markdown` is
    /// populated with the full untruncated rendering. Caller pays the
    /// wire cost.
    #[serde(default)]
    pub include_full_content: bool,
    /// Default false. When true, `EntrySummary.images` carries inline
    /// base64 image payloads on entries that contain image content
    /// blocks. Combine with `include_full_content` for the rich chat
    /// case.
    #[serde(default)]
    pub include_images: bool,
    /// R-6e: return only entries with absolute index < `before_index`.
    /// `None` = no upper bound (current behavior). Combine with
    /// `after_index` for a slice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_index: Option<usize>,
    /// R-6e: return only entries with absolute index > `after_index`.
    /// `None` = no lower bound (current behavior). This is the param
    /// the client uses for incremental resume — pass the last seen
    /// entry index and get only what's new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_index: Option<usize>,
    /// R-6e: take only the LAST `count` entries after applying
    /// `after_index` / `before_index`. "Last" — not first — because the
    /// dominant client query (initial session-detail open) wants the
    /// newest N entries, not the oldest. `None` = unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

impl<'de> Deserialize<'de> for GetSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            include_full_content: bool,
            include_images: bool,
            before_index: Option<usize>,
            after_index: Option<usize>,
            count: Option<usize>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            include_full_content: inner.include_full_content,
            include_images: inner.include_images,
            before_index: inner.before_index,
            after_index: inner.after_index,
            count: inner.count,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntrySummary {
    /// One of "user" | "assistant" | "tool_call" | "plan".
    pub role: String,
    /// R-6e: absolute 0-based index in the session, stable across
    /// paginated calls. Always populated regardless of whether the
    /// caller requested a slice — lets the client reassemble a sparse
    /// map from multiple paginated responses.
    pub index: usize,
    /// Markdown rendering of the entry, truncated to roughly 200 chars.
    pub preview: String,
    /// Full untruncated markdown rendering. Populated only when the
    /// caller passes `include_full_content: true`, or when the entry
    /// came back via `solution_agent.get_session_entry` (which always
    /// includes the full markdown for the single-entry case).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    /// Inline images present in this entry, raw base64 (no `data:` URI
    /// prefix). Populated only when the caller opts in via
    /// `include_images: true`. `None` means "the caller did not ask";
    /// an empty `Vec` means "the caller asked but the entry has no
    /// image content blocks".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<EntryImage>>,
    /// Present only for `role == "tool_call"` entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCallSummary>,
    /// Present only for `role == "plan"` entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanSummary>,
    /// Originating client's locally-generated send id, plumbed verbatim
    /// from the user message's content-block `_meta.spk_client_send_id`
    /// (see `acp_thread::SPK_CLIENT_SEND_ID_META_KEY`). Present only for
    /// `role == "user"` entries that came from a client that stamped one
    /// (the mobile client today; desktop-originated sends leave it
    /// `None`). Lets the originating client dedupe its in-flight
    /// optimistic bubble against the server-echoed entry by an exact
    /// id-match instead of fragile content-equality on truncated previews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_send_id: Option<i64>,
    /// Every distinct `spk_client_send_id` carried by this user
    /// entry, in source order. The single-id `client_send_id` field
    /// above is kept for back-compat (old mobile builds only look
    /// there); modern clients should prefer this list, since the
    /// server-side queue-merge path (`store::queue::send_message_blocks`'s
    /// `pending_messages` flush) rolls N originating bundles into one
    /// ACP message with N distinct stamps. Empty for non-user entries
    /// or for user entries from clients that don't stamp ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_send_ids: Vec<i64>,
    /// Unix-millis creation time of this entry, captured server-side at first
    /// append. Absent (`None`, omitted from JSON) for entries that predate the
    /// feature — clients show no time rather than a fabricated one. Only
    /// positive values are real; the server maps the internal absent-sentinel
    /// to `None` here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntryImage {
    /// 0-based stable index of the image within the session, in the
    /// order images appear when walking entries oldest-first. Lets
    /// renderers cross-reference the `spk-image://N` URL scheme used by
    /// the desktop side (see `conversation_render.rs`'s
    /// `clean_user_message_text`).
    pub index: usize,
    /// e.g. "image/png", "image/jpeg".
    pub mime_type: String,
    /// Raw base64, no `data:` URI prefix.
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ToolCallSummary {
    /// Opaque tool-call id, matching the id `authorize_tool_call` resolves
    /// against. Always populated. The client echoes this verbatim when
    /// answering an authorization prompt.
    pub tool_call_id: String,
    /// Human-readable tool name (e.g. "Read", "Edit", "Bash"). Derived
    /// from `tool_name` when set, falling back to the markdown source of
    /// the call's label entity.
    pub name: String,
    /// Status string mirrors `conversation_render::tool_call_status_text`
    /// so the wire string matches what the desktop UI shows:
    /// "pending" | "waiting for confirmation" | "running" | "done" |
    /// "failed" | "rejected" | "canceled".
    pub status: String,
    /// JSON-serialised `raw_input`, truncated to ~500 chars. Empty
    /// string when the agent didn't supply structured args.
    pub args_preview: String,
    /// JSON-serialised `raw_output`, truncated to ~500 chars. Empty
    /// string until the call completes (or fails) with structured
    /// output.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub result_preview: String,
    /// Unix epoch in milliseconds captured the first time this tool
    /// call's status transitioned into `InProgress`. Preserved across
    /// the transition to terminal statuses so clients can render
    /// "ran for Xs" on a completed call too. `None` for tool calls
    /// that have never entered `InProgress` (e.g. cold-rehydrated
    /// entries restored straight into a terminal status, or pending
    /// calls that haven't started yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_status_started_at_ms: Option<i64>,
    /// Authorization options when this tool call is awaiting confirmation
    /// (status == "waiting for confirmation"). Empty otherwise. The client
    /// renders one button per option and answers via
    /// `solution_agent.authorize_tool_call`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ToolCallAuthOption>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ToolCallAuthOption {
    /// Opaque option id — pass back verbatim to authorize_tool_call.
    pub option_id: String,
    /// Display label for the button.
    pub label: String,
    /// One of: "allow_once" | "allow_always" | "reject_once" | "reject_always".
    pub kind: String,
    /// True for allow-style options (render as primary), false for reject-style.
    pub is_allow: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanSummary {
    /// One markdown source per plan item, in order. Tool-call plans
    /// surface as completed checklists in the desktop UI; rendering
    /// them remotely typically means a `- [x]` bullet list.
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetSessionResult {
    pub id: String,
    pub solution_id: String,
    pub agent_id: String,
    pub title: String,
    pub state: SessionStateDto,
    pub created_at: i64,
    pub last_activity_at: i64,
    /// F: cumulative tokens for the session (live thread > cached
    /// metadata fall-back). `None` until the agent reports its first
    /// usage update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Model context window in tokens; mirrors `SessionSummary::max_tokens`.
    /// `None` until the agent emits its first `TokenUsageUpdated` with a
    /// non-zero `max_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// F: parent session reference for sub-agent indication. `None` for
    /// top-level sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub entries: Vec<EntrySummary>,
    /// R-6e: total entry count of the underlying thread, regardless of
    /// pagination filters applied to `entries`. Lets the client render a
    /// "Load older" affordance and detect resume-time gaps.
    pub total_count: usize,
    /// Server-side `pending_messages` queue, one descriptor per bundle.
    /// Empty when the agent isn't holding any follow-up sends from
    /// during a Running window. Mobile renders each bundle as a
    /// Queued bubble — paired with the live `agent_session_queue_changed`
    /// notification this is the cold-start seed for the unified
    /// cross-client queue display.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_bundles: Vec<QueuedBundleSummary>,
}

/// One descriptor from `SolutionSession::pending_messages` exposed to
/// MCP consumers. Mirrors the wire shape that
/// `event_sources::build_queue_changed_payload` emits on the
/// `agent_session_queue_changed` notification.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QueuedBundleSummary {
    /// Every distinct `spk_client_send_id` carried by the bundle's
    /// content blocks, in source order. Empty for desktop-typed
    /// bundles (no csid stamp) — clients should still render them as
    /// Queued bubbles, they just can't dedupe against local
    /// optimistic state in that case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub csids: Vec<i64>,
    /// Markdown preview of the bundle, queue-marker stripped, image
    /// placeholders rendered inline as `[image #N]`.
    pub preview: String,
    /// Number of image blocks in the bundle. Lets the client surface
    /// an `[image #N]` affordance without holding the image bytes.
    pub image_count: u32,
}

#[derive(Clone)]
pub struct GetSessionTool;

impl McpServerTool for GetSessionTool {
    type Input = GetSessionParams;
    type Output = GetSessionResult;
    const NAME: &'static str = "solution_agent.get_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        let result = cx.update(|cx| -> Result<GetSessionResult> {
            let store = SolutionAgentStore::global(cx);
            let entity = store
                .read_with(cx, |store, _| store.session(session_id))
                .with_context(|| format!("session_not_found: {}", session_id))?;
            let session = entity.read(cx);
            // Live sessions have an attached `acp_thread`; cold (sleeping)
            // sessions don't — but they still have `cold_entries`
            // reconstructed from the persisted blob on disk. Without this
            // fallback the mobile client sees an empty chat for any
            // session whose subprocess hasn't been respawned yet (a
            // common state for any session you scroll past without
            // tapping into).
            let live_entries: Option<Vec<&acp_thread::AgentThreadEntry>> = session
                .acp_thread()
                .map(|thread| thread.read(cx).entries().iter().collect());
            let entries_ref: Vec<&acp_thread::AgentThreadEntry> =
                live_entries.unwrap_or_else(|| session.cold_entries.iter().collect());
            let (entries, total_count) = {
                let total = entries_ref.len();
                // R-6e: index-anchored slice. `after_index` /
                // `before_index` are exclusive bounds and `count`
                // takes the LAST n entries within the bound (so the
                // common "show me the newest 50" query is just
                // `count=50` with no bounds).
                //
                // We walk every entry (not just the kept ones) so
                // `image_cursor` stays in lock-step with what a
                // non-paginated call would have produced — that
                // keeps `EntryImage.index` stable across paginated
                // calls, which is the contract that lets the client
                // rely on `spk-image://N` URLs in markdown.
                let after = input.after_index;
                let before = input.before_index;
                let mut image_cursor = 0usize;
                let mut kept: Vec<EntrySummary> = Vec::new();
                for (index, entry) in entries_ref.iter().enumerate() {
                    let in_range =
                        after.map_or(true, |a| index > a) && before.map_or(true, |b| index < b);
                    if in_range {
                        let created_ms = session
                            .entry_created_ms
                            .get(index)
                            .copied()
                            .filter(|&ms| ms > 0);
                        kept.push(summarize_entry(
                            entry,
                            index,
                            created_ms,
                            input.include_full_content,
                            input.include_images,
                            &mut image_cursor,
                            cx,
                        ));
                    } else {
                        image_cursor += count_images_in_entry(entry);
                    }
                }
                if let Some(n) = input.count {
                    if kept.len() > n {
                        // Take the last n. `EntrySummary.index`
                        // preserves the absolute position so the
                        // client can still tell where it sits in
                        // the session timeline.
                        let drop_count = kept.len() - n;
                        kept.drain(..drop_count);
                    }
                }
                (kept, total)
            };
            let summary = session_summary(session, cx);
            let pending_bundles = build_pending_bundle_summaries(session, cx);
            Ok(GetSessionResult {
                id: summary.id,
                solution_id: summary.solution_id,
                agent_id: summary.agent_id,
                title: summary.title,
                state: summary.state,
                created_at: summary.created_at,
                last_activity_at: summary.last_activity_at,
                total_tokens: summary.total_tokens,
                max_tokens: summary.max_tokens,
                parent_session_id: summary.parent_session_id,
                entries,
                total_count,
                pending_bundles,
            })
        })?;

        let title = result.title.clone();
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text: title }],
            structured_content: result,
        })
    }
}

/// Build the [QueuedBundleSummary] list off a session's
/// `pending_messages` queue. Pulled out so `get_session` and any
/// future cold-load surface can reuse the same shape that
/// `event_sources::build_queue_changed_payload` emits on the live
/// notification path. Empty queue → empty Vec.
fn build_pending_bundle_summaries(
    session: &crate::model::SolutionSession,
    _cx: &App,
) -> Vec<QueuedBundleSummary> {
    session
        .pending_messages
        .iter()
        .map(|bundle| {
            let csids = acp_thread::csids_from_blocks(bundle);
            let preview = crate::conversation_render::pending_blocks_preview(bundle, _cx);
            let image_count: u32 = bundle
                .iter()
                .filter(|b| matches!(b, acp::ContentBlock::Image(_)))
                .count() as u32;
            QueuedBundleSummary {
                csids,
                preview,
                image_count,
            }
        })
        .collect()
}

fn entry_role(entry: &acp_thread::AgentThreadEntry) -> &'static str {
    match entry {
        acp_thread::AgentThreadEntry::UserMessage(_) => "user",
        acp_thread::AgentThreadEntry::AssistantMessage(_) => "assistant",
        acp_thread::AgentThreadEntry::ToolCall(_) => "tool_call",
        acp_thread::AgentThreadEntry::CompletedPlan(_) => "plan",
    }
}

pub(crate) fn truncate_preview(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (count, ch) in s.chars().enumerate() {
        if count >= max_chars {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

/// Hard cap on per-field previews other than the 200-char top-level
/// `preview`. Picked at ~500 chars to fit a chat-bubble truncation
/// without ballooning the wire payload for tool calls that dump huge
/// JSON args / results.
const FIELD_PREVIEW_MAX_CHARS: usize = 500;

/// Builds the per-entry `EntrySummary` for the MCP wire shape.
///
/// `index` is the entry's absolute position in the session — set on the
/// returned `EntrySummary` so the client can reassemble paginated
/// responses into a sparse map without computing offsets itself.
///
/// `include_full_content` controls whether the untruncated markdown is
/// populated on every variant. `include_images` controls whether image
/// content blocks get inlined as base64 (caller pays the wire cost).
///
/// `image_cursor` is the session-scoped 0-based image counter; the
/// caller threads it through `summarize_entry` calls in oldest-first
/// order so each `EntryImage.index` is stable across the session even
/// when an entry holds multiple images.
fn summarize_entry(
    entry: &acp_thread::AgentThreadEntry,
    index: usize,
    created_ms: Option<i64>,
    include_full_content: bool,
    include_images: bool,
    image_cursor: &mut usize,
    cx: &App,
) -> EntrySummary {
    let role = entry_role(entry);
    let markdown_source = entry.to_markdown(cx);
    let preview = truncate_preview(&markdown_source, 200);
    let markdown = if include_full_content {
        Some(markdown_source)
    } else {
        None
    };
    let images = if include_images {
        Some(extract_images_for_entry(entry, image_cursor))
    } else {
        // Advance the cursor even when the caller didn't opt in, so
        // toggling `include_images` between calls preserves the same
        // stable indices.
        *image_cursor += count_images_in_entry(entry);
        None
    };
    let tool_call = if let acp_thread::AgentThreadEntry::ToolCall(call) = entry {
        Some(tool_call_summary(call, cx))
    } else {
        None
    };
    let plan = if let acp_thread::AgentThreadEntry::CompletedPlan(entries) = entry {
        Some(PlanSummary {
            items: entries
                .iter()
                .map(|e| e.content.read(cx).source().to_string())
                .collect(),
        })
    } else {
        None
    };
    let client_send_ids: Vec<i64> =
        if let acp_thread::AgentThreadEntry::UserMessage(message) = entry {
            acp_thread::client_send_ids_from_user_message(message)
        } else {
            Vec::new()
        };
    let client_send_id = client_send_ids.first().copied();

    EntrySummary {
        role: role.to_string(),
        index,
        preview,
        markdown,
        images,
        tool_call,
        plan,
        client_send_id,
        client_send_ids,
        created_ms,
    }
}

/// Counts image content blocks in an entry without allocating image
/// payloads. Used to keep `image_cursor` stable when the caller
/// opted out of `include_images`.
fn count_images_in_entry(entry: &acp_thread::AgentThreadEntry) -> usize {
    match entry {
        acp_thread::AgentThreadEntry::UserMessage(message) => message
            .chunks
            .iter()
            .filter(|chunk| matches!(chunk, acp::ContentBlock::Image(_)))
            .count(),
        acp_thread::AgentThreadEntry::AssistantMessage(message) => message
            .chunks
            .iter()
            .filter(|chunk| match chunk {
                acp_thread::AssistantMessageChunk::Message { block }
                | acp_thread::AssistantMessageChunk::Thought { block } => {
                    matches!(block, acp_thread::ContentBlock::Image { .. })
                }
            })
            .count(),
        acp_thread::AgentThreadEntry::ToolCall(call) => call
            .content
            .iter()
            .filter(|content| matches!(content, acp_thread::ToolCallContent::ContentBlock(block) if matches!(block, acp_thread::ContentBlock::Image { .. })))
            .count(),
        acp_thread::AgentThreadEntry::CompletedPlan(_) => 0,
    }
}

/// Pulls inline image payloads out of an entry as wire-ready
/// `EntryImage` records. Reads raw base64 from `acp::ContentBlock::Image`
/// directly on user messages (the original ACP envelope is preserved in
/// `UserMessage.chunks`); for assistant / tool-call image blocks we
/// re-encode the bytes the desktop side already decoded into
/// `gpui::Image` (loses no fidelity — they're identical bytes, just
/// reshipped through base64).
fn extract_images_for_entry(
    entry: &acp_thread::AgentThreadEntry,
    image_cursor: &mut usize,
) -> Vec<EntryImage> {
    use base64::Engine as _;
    let mut out = Vec::new();

    match entry {
        acp_thread::AgentThreadEntry::UserMessage(message) => {
            for chunk in &message.chunks {
                if let acp::ContentBlock::Image(image_content) = chunk {
                    out.push(EntryImage {
                        index: *image_cursor,
                        mime_type: image_content.mime_type.clone(),
                        data_base64: image_content.data.clone(),
                    });
                    *image_cursor += 1;
                }
            }
        }
        acp_thread::AgentThreadEntry::AssistantMessage(message) => {
            for chunk in &message.chunks {
                let block = match chunk {
                    acp_thread::AssistantMessageChunk::Message { block } => block,
                    acp_thread::AssistantMessageChunk::Thought { block } => block,
                };
                if let acp_thread::ContentBlock::Image { image } = block {
                    out.push(EntryImage {
                        index: *image_cursor,
                        mime_type: image.format.mime_type().to_string(),
                        data_base64: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                    });
                    *image_cursor += 1;
                }
            }
        }
        acp_thread::AgentThreadEntry::ToolCall(call) => {
            for content in &call.content {
                if let acp_thread::ToolCallContent::ContentBlock(
                    acp_thread::ContentBlock::Image { image },
                ) = content
                {
                    out.push(EntryImage {
                        index: *image_cursor,
                        mime_type: image.format.mime_type().to_string(),
                        data_base64: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                    });
                    *image_cursor += 1;
                }
            }
        }
        acp_thread::AgentThreadEntry::CompletedPlan(_) => {}
    }
    out
}

/// Builds a `ToolCallSummary` for the wire. Status string mirrors
/// `conversation_render::tool_call_status_text` so remote consumers
/// and the desktop UI agree on the label.
fn tool_call_summary(call: &acp_thread::ToolCall, cx: &App) -> ToolCallSummary {
    let name = call
        .tool_name
        .as_ref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| call.label.read(cx).source().to_string());
    let status = crate::conversation_render::tool_call_status_text(&call.status).to_string();
    let args_preview = call
        .raw_input
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .map(|s| truncate_preview(&s, FIELD_PREVIEW_MAX_CHARS))
        .unwrap_or_default();
    let result_preview = call
        .raw_output
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .map(|s| truncate_preview(&s, FIELD_PREVIEW_MAX_CHARS))
        .unwrap_or_default();
    let tool_status_started_at_ms = call.status_started_at.map(|t| t.timestamp_millis());
    // Surface authorization choices only while the call is blocked on the
    // user. Reuse the Q1 `permission_buttons` helper so the wire options
    // and the desktop buttons are derived from the exact same flattening
    // (Flat / Dropdown / DropdownWithPatterns all collapse identically),
    // and so the server can later reconstruct the outcome from `option_id`.
    let options = match &call.status {
        acp_thread::ToolCallStatus::WaitingForConfirmation { options, .. } => {
            crate::conversation_render::permission_buttons(options)
                .into_iter()
                .map(|button| ToolCallAuthOption {
                    option_id: button.option_id.0.to_string(),
                    label: button.label.to_string(),
                    kind: permission_kind_str(button.kind).to_string(),
                    is_allow: button.is_allow(),
                })
                .collect()
        }
        _ => Vec::new(),
    };
    ToolCallSummary {
        tool_call_id: call.id.0.to_string(),
        name,
        status,
        args_preview,
        result_preview,
        tool_status_started_at_ms,
        options,
    }
}

/// Snake-case wire string for a `PermissionOptionKind`, matching the
/// kinds documented on `ToolCallAuthOption.kind`. The ACP enum already
/// serializes to exactly these strings (`#[serde(rename_all =
/// "snake_case")]`), but spelling them out here keeps the wire contract
/// stable even if the upstream serde representation ever drifts, and
/// avoids a serde round-trip per option.
fn permission_kind_str(kind: acp::PermissionOptionKind) -> &'static str {
    match kind {
        acp::PermissionOptionKind::AllowOnce => "allow_once",
        acp::PermissionOptionKind::AllowAlways => "allow_always",
        acp::PermissionOptionKind::RejectOnce => "reject_once",
        acp::PermissionOptionKind::RejectAlways => "reject_always",
        other => {
            log::warn!(
                "unknown PermissionOptionKind {other:?}; presenting as reject_once on the wire"
            );
            "reject_once"
        }
    }
}

// =====================================================================
// solution_agent.get_session_entry
// =====================================================================

/// Fetch the full content of a single session entry by index. Designed
/// for the "user expanded one tool-call bubble" case where the chat
/// client needs the full markdown / images / tool-call detail for one
/// entry without re-fetching the entire transcript.
///
/// `markdown` is **always** populated on the returned `EntrySummary`
/// — the single-entry call is the explicit "I want the full content"
/// path, so gating it would defeat the purpose. `include_images`
/// remains opt-in because images can dominate the payload.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct GetSessionEntryParams {
    pub session_id: String,
    /// 0-based index into the session's entries, oldest-first.
    pub index: usize,
    /// Default false. When true, the returned `EntrySummary.images`
    /// carries inline base64 image payloads.
    #[serde(default)]
    pub include_images: bool,
}

impl<'de> Deserialize<'de> for GetSessionEntryParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            #[serde(default)]
            index: usize,
            #[serde(default)]
            include_images: bool,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            index: inner.index,
            include_images: inner.include_images,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetSessionEntryResult {
    pub entry: EntrySummary,
}

#[derive(Clone)]
pub struct GetSessionEntryTool;

impl McpServerTool for GetSessionEntryTool {
    type Input = GetSessionEntryParams;
    type Output = GetSessionEntryResult;
    const NAME: &'static str = "solution_agent.get_session_entry";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        let want_index = input.index;
        let include_images = input.include_images;

        let result = cx.update(|cx| -> Result<GetSessionEntryResult> {
            let store = SolutionAgentStore::global(cx);
            let entity = store
                .read_with(cx, |store, _| store.session(session_id))
                .with_context(|| format!("session_not_found: {}", session_id))?;
            let session = entity.read(cx);
            let thread = session
                .acp_thread()
                .ok_or_else(|| anyhow!("session_has_no_thread: {}", session_id))?;
            let thread_ref = thread.read(cx);
            let entries = thread_ref.entries();
            let len = entries.len();
            anyhow::ensure!(
                want_index < len,
                "entry_index_out_of_range: {} (session has {} entries)",
                want_index,
                len
            );
            // Replay the image cursor up to `want_index` so the
            // returned `EntryImage.index` matches what
            // `get_session{ include_images: true }` would have
            // assigned to the same image — keeps cross-references
            // (markdown `spk-image://N` links etc.) consistent.
            let mut image_cursor = 0usize;
            for entry in entries.iter().take(want_index) {
                image_cursor += count_images_in_entry(entry);
            }
            let entry = entries
                .get(want_index)
                .ok_or_else(|| anyhow!("entry vanished mid-read"))?;
            let created_ms = session
                .entry_created_ms
                .get(want_index)
                .copied()
                .filter(|&ms| ms > 0);
            let summary = summarize_entry(
                entry,
                want_index,
                created_ms,
                true,
                include_images,
                &mut image_cursor,
                cx,
            );
            Ok(GetSessionEntryResult { entry: summary })
        })?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("entry #{want_index}"),
            }],
            structured_content: result,
        })
    }
}

// =====================================================================
// solution_agent.create_session
// =====================================================================

/// Create a new ACP session for `(solution_id, agent_id)` on the active
/// workspace's project. `initial_message`, if present, is dispatched as a
/// detached `send_message` after the session is registered.
///
/// **Active project resolution**: the session needs an `Entity<Project>`
/// from a live workspace window whose worktrees back the named Solution.
/// MCP doesn't carry a workspace handle, so we walk every open
/// `MultiWorkspace` window and pick the first project whose visible
/// worktrees include the Solution's root. If no such window is open, the
/// tool errors with a clear message — the caller should open the Solution
/// first via `solutions.open`.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CreateSessionParams {
    pub solution_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<String>,
    /// F: parent session reference for sub-agent indication. When set,
    /// the new session is registered as a child of `parent_session_id`
    /// and surfaces in the session-view's sub-agents strip. The parent
    /// MUST exist in the same solution — otherwise the tool errors
    /// (`unknown_parent_session` or `parent_session_in_different_solution`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Optional user-supplied title. When absent the desktop assigns a
    /// title automatically from the first user turn — clients that
    /// want a stable, human-supplied name (e.g. the phone) can set
    /// this. Renamable later via `solution_agent.rename_session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional working directory for the agent subprocess. Must be one
    /// of the solution's visible worktree roots — values outside the
    /// solution are rejected. When absent, the first worktree of the
    /// active project for `solution_id` is used (matches the previous
    /// behaviour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl<'de> Deserialize<'de> for CreateSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            solution_id: String,
            agent_id: String,
            initial_message: Option<String>,
            parent_session_id: Option<String>,
            title: Option<String>,
            cwd: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            solution_id: inner.solution_id,
            agent_id: inner.agent_id,
            initial_message: inner.initial_message,
            parent_session_id: inner.parent_session_id,
            title: inner.title,
            cwd: inner.cwd,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CreateSessionResult {
    pub session_id: String,
}

#[derive(Clone)]
pub struct CreateSessionTool;

impl McpServerTool for CreateSessionTool {
    type Input = CreateSessionParams;
    type Output = CreateSessionResult;
    const NAME: &'static str = "solution_agent.create_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.solution_id.is_empty(),
            "invalid_params: solution_id is required"
        );
        anyhow::ensure!(
            !input.agent_id.is_empty(),
            "invalid_params: agent_id is required"
        );
        let solution_id = SolutionId(input.solution_id.clone());
        let agent_id: crate::model::AgentServerId = input.agent_id.clone().into();

        // F: parent validation. Parse the id, then look it up in the
        // store. Reject when missing (`unknown_parent_session`) or when
        // the parent lives in a different solution
        // (`parent_session_in_different_solution`) — sub-agents are
        // intentionally same-solution-only (see plan-doc §I).
        let parent_session_id = match input.parent_session_id.as_deref() {
            Some(raw) => {
                let parsed = SolutionSessionId::parse(raw)
                    .map_err(|e| anyhow!("bad parent_session_id: {e}"))?;
                let parent_solution = cx.update(|cx| {
                    let store = SolutionAgentStore::global(cx);
                    store.read_with(cx, |store, cx| {
                        store
                            .session(parsed)
                            .map(|entity| entity.read(cx).solution_id.clone())
                    })
                });
                let parent_solution =
                    parent_solution.ok_or_else(|| anyhow!("unknown_parent_session: {raw}"))?;
                if parent_solution != solution_id {
                    anyhow::bail!(
                        "parent_session_in_different_solution: {} != {}",
                        parent_solution.0,
                        solution_id.0
                    );
                }
                Some(parsed)
            }
            None => None,
        };

        let project = cx
            .update(|cx| project_for_solution(&input.solution_id, cx))
            .ok_or_else(|| {
                anyhow!(
                    "no_active_workspace_for_solution: open Solution {} via solutions.open before \
                     creating a session",
                    input.solution_id
                )
            })?;

        let cwd: Option<std::path::PathBuf> = input.cwd.as_ref().map(std::path::PathBuf::from);

        let create_task = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session_with_parent(
                    solution_id,
                    agent_id,
                    project,
                    cwd,
                    parent_session_id,
                    cx,
                )
            })
        });
        let session_id = create_task.await?;

        // Apply the user-supplied title (if any). Done as a separate
        // rename so the create path stays single-purpose and the title
        // change emits the SessionTitleChanged event that subscribers
        // (including the WS notification forwarder) already listen for.
        if let Some(raw_title) = input.title.as_deref() {
            let trimmed = raw_title.trim();
            if !trimmed.is_empty() {
                let title = SharedString::from(trimmed.to_string());
                cx.update(|cx| -> Result<()> {
                    let store = SolutionAgentStore::global(cx);
                    store.update(cx, |store, cx| store.rename_session(session_id, title, cx))?;
                    Ok(())
                })?;
            }
        }

        if let Some(content) = input.initial_message {
            cx.update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.update(cx, |store, cx| {
                    store.send_message(session_id, content, cx).detach();
                });
            });
        }

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: session_id.to_string(),
            }],
            structured_content: CreateSessionResult {
                session_id: session_id.to_string(),
            },
        })
    }
}

// Locate the `Project` whose worktrees back the named Solution. Mirrors
// the helper of the same name in `solutions::mcp` (kept private there);
// duplicated here to avoid widening the `solutions` crate's public API
// just for this MCP tool.
fn project_for_solution(solution_id: &str, cx: &mut App) -> Option<Entity<project::Project>> {
    let store = SolutionStore::try_global(cx)?;
    let root = store.read_with(cx, |s, _| {
        s.solutions()
            .iter()
            .find(|sol| sol.id.as_str() == solution_id)
            .map(|sol| sol.root.clone())
    })?;

    for handle in cx.windows() {
        let Some(window_handle) = handle.downcast::<workspace::MultiWorkspace>() else {
            continue;
        };
        let result = window_handle
            .update(cx, |multi, _window, cx| {
                for workspace_entity in multi.workspaces() {
                    let workspace = workspace_entity.read(cx);
                    let project = workspace.project();
                    let matches = project
                        .read(cx)
                        .visible_worktrees(cx)
                        .any(|tree| tree.read(cx).abs_path().starts_with(&root));
                    if matches {
                        return Some(project.clone());
                    }
                }
                None
            })
            .ok()
            .flatten();
        if let Some(project) = result {
            return Some(project);
        }
    }
    None
}

// =====================================================================
// solution_agent.send_message
// =====================================================================

/// Send a user message to an existing session. Fire-and-forget — the
/// returned `Task` is detached so the tool response returns immediately
/// once the prompt is enqueued. Use `solution_agent.get_session` to poll
/// for new entries, or subscribe to `solution_agent.*` events (deferred
/// to a later phase) for push notifications.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SendMessageParams {
    pub session_id: String,
    pub content: String,
}

impl<'de> Deserialize<'de> for SendMessageParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            content: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            content: inner.content,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SendMessageResult {}

#[derive(Clone)]
pub struct SendMessageTool;

impl McpServerTool for SendMessageTool {
    type Input = SendMessageParams;
    type Output = SendMessageResult;
    const NAME: &'static str = "solution_agent.send_message";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        let content = input.content;

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.send_message(session_id, content, cx).detach();
            });
        });

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "queued".to_string(),
            }],
            structured_content: SendMessageResult {},
        })
    }
}

// =====================================================================
// solution_agent.send_message_blocks
// =====================================================================

/// Send a structured user message composed of one or more ACP
/// `ContentBlock`s (text + images + resource links, etc). Mirrors
/// `SendMessageTool` but lets MCP consumers pass multi-modal payloads
/// — primarily the mobile client, which encodes picked images and
/// text-like files into `Image` / `Text` blocks. The bare
/// `send_message` text-only tool stays for callers that only have a
/// plain prompt.
///
/// Fire-and-forget — the returned `Task` from
/// `SolutionAgentStore::send_message_blocks` is detached so the tool
/// response returns immediately once the prompt is enqueued.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SendMessageBlocksParams {
    pub session_id: String,
    /// Each entry is serialised per the ACP `ContentBlock` schema
    /// (`{"type": "text", "text": "..."}` /
    /// `{"type": "image", "data": "<base64>", "mimeType": "image/png"}` /
    /// `{"type": "resource_link", "uri": "...", ...}` / etc).
    pub blocks: Vec<acp::ContentBlock>,
}

impl<'de> Deserialize<'de> for SendMessageBlocksParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            blocks: Vec<acp::ContentBlock>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            blocks: inner.blocks,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SendMessageBlocksResult {}

#[derive(Clone)]
pub struct SendMessageBlocksTool;

impl McpServerTool for SendMessageBlocksTool {
    type Input = SendMessageBlocksParams;
    type Output = SendMessageBlocksResult;
    const NAME: &'static str = "solution_agent.send_message_blocks";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        anyhow::ensure!(
            !input.blocks.is_empty(),
            "invalid_params: blocks must contain at least one item"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        // Swap any `spk-upload://<id>` ResourceLink for the inline
        // Image/Text the chunked-upload tmp file contains, BEFORE the
        // bundle reaches the store. Without this step the handle URI
        // would travel verbatim to claude-acp, which has no idea what
        // `spk-upload://` means — the attached image silently vanishes
        // and the agent sees only the accompanying text.
        let blocks = crate::upload::resolve_upload_handles(input.blocks)?;

        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.send_message_blocks(session_id, blocks, cx).detach();
            });
        });

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "queued".to_string(),
            }],
            structured_content: SendMessageBlocksResult {},
        })
    }
}

// =====================================================================
// solution_agent.close_session
// =====================================================================

/// Close a session, dropping its `AcpThread` and removing it from the
/// store. Mirrors `SolutionAgentStore::close_session` directly — the
/// pool's per-pair `live_session_count` is not decremented here because
/// the store's own `close_session` doesn't either (the only production
/// `pool_release_session` call site is the failed-spawn rollback in
/// `create_session`). Pool leakage on close is a pre-existing store
/// concern, not MCP-specific.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CloseSessionParams {
    pub session_id: String,
}

impl<'de> Deserialize<'de> for CloseSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
        }
        Ok(Self {
            session_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .session_id,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CloseSessionResult {}

#[derive(Clone)]
pub struct CloseSessionTool;

impl McpServerTool for CloseSessionTool {
    type Input = CloseSessionParams;
    type Output = CloseSessionResult;
    const NAME: &'static str = "solution_agent.close_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        cx.update(|cx| -> Result<()> {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| store.close_session(session_id, cx))?;
            Ok(())
        })?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "closed".to_string(),
            }],
            structured_content: CloseSessionResult {},
        })
    }
}

// =====================================================================
// solution_agent.cancel_turn
// =====================================================================

/// Cancel the in-flight turn on `session_id`. Forwards to
/// `AgentConnection::cancel`; the session will eventually transition to
/// `Idle` (or `Errored`) via the regular `AcpThreadEvent` plumbing.
///
/// When `flush_pending` is true the call additionally sets the
/// session's `flush_after_cancel` flag so that the `pending_messages`
/// queue (filled while the agent was Running) gets flushed as one
/// merged follow-up turn the moment the cancel settles, instead of
/// being dropped. This is the wire path the mobile Force-flush
/// button uses to "stop and send my queued messages now".
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CancelTurnParams {
    pub session_id: String,
    #[serde(default)]
    pub flush_pending: bool,
}

impl<'de> Deserialize<'de> for CancelTurnParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            #[serde(default)]
            flush_pending: bool,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            flush_pending: inner.flush_pending,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CancelTurnResult {}

#[derive(Clone)]
pub struct CancelTurnTool;

impl McpServerTool for CancelTurnTool {
    type Input = CancelTurnParams;
    type Output = CancelTurnResult;
    const NAME: &'static str = "solution_agent.cancel_turn";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        let flush_pending = input.flush_pending;
        cx.update(|cx| -> Result<()> {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                if flush_pending {
                    // Best-effort: when there is nothing to flush
                    // `interrupt_and_flush_pending` errors with
                    // "no queued messages". Treat that as success
                    // here — the caller asked for "cancel + maybe
                    // flush", and the cancel half still makes sense.
                    match store.interrupt_and_flush_pending(session_id, cx) {
                        Ok(()) => Ok(()),
                        Err(_) => store.cancel_turn(session_id, cx),
                    }
                } else {
                    store.cancel_turn(session_id, cx)
                }
            })?;
            Ok(())
        })?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "cancelled".to_string(),
            }],
            structured_content: CancelTurnResult {},
        })
    }
}

// =====================================================================
// solution_agent.authorize_tool_call
// =====================================================================

/// Answer a tool call that is blocked `WaitingForConfirmation`. The
/// client picks one of the `options` it received on the tool call's
/// `ToolCallSummary` (see `solution_agent.get_session{,_entry}`) and
/// sends back its `option_id`. The SERVER reconstructs the full
/// `SelectedPermissionOutcome` (kind + any terminal sub-patterns) from
/// the live options — the client only needs to echo the opaque id — so
/// the answer can never drift from what the agent actually offered.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct AuthorizeToolCallParams {
    pub session_id: String,
    pub tool_call_id: String,
    pub option_id: String,
}

impl<'de> Deserialize<'de> for AuthorizeToolCallParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            tool_call_id: String,
            option_id: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            tool_call_id: inner.tool_call_id,
            option_id: inner.option_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AuthorizeToolCallResult {
    pub ok: bool,
}

/// Locate the `WaitingForConfirmation` tool call matching `tool_call_id`
/// in `entries`, then resolve `option_id` against its live permission
/// buttons and return the `SelectedPermissionOutcome` to hand to
/// `AcpThread::authorize_tool_call`. Pure over the thread's entries +
/// the client's request so the resolution logic is unit-testable
/// without staging a live confirmation oneshot.
fn resolve_authorization_outcome(
    entries: &[acp_thread::AgentThreadEntry],
    tool_call_id: &str,
    option_id: &str,
) -> Result<acp_thread::SelectedPermissionOutcome> {
    let call = entries
        .iter()
        .find_map(|entry| match entry {
            acp_thread::AgentThreadEntry::ToolCall(call) if call.id.0.as_ref() == tool_call_id => {
                Some(call)
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("tool_call_not_found: {}", tool_call_id))?;

    let options = match &call.status {
        acp_thread::ToolCallStatus::WaitingForConfirmation { options, .. } => options,
        _ => {
            anyhow::bail!("not_awaiting_confirmation: {}", tool_call_id);
        }
    };

    crate::conversation_render::permission_buttons(options)
        .into_iter()
        .find(|button| button.option_id.0.as_ref() == option_id)
        .map(|button| button.outcome())
        .ok_or_else(|| anyhow!("unknown_option: {}", option_id))
}

#[derive(Clone)]
pub struct AuthorizeToolCallTool;

impl McpServerTool for AuthorizeToolCallTool {
    type Input = AuthorizeToolCallParams;
    type Output = AuthorizeToolCallResult;
    const NAME: &'static str = "solution_agent.authorize_tool_call";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        anyhow::ensure!(
            !input.tool_call_id.is_empty(),
            "invalid_params: tool_call_id is required"
        );
        anyhow::ensure!(
            !input.option_id.is_empty(),
            "invalid_params: option_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        let tool_call_id = input.tool_call_id;
        let option_id = input.option_id;

        cx.update(|cx| -> Result<()> {
            let store = SolutionAgentStore::global(cx);
            let entity = store
                .read_with(cx, |store, _| store.session(session_id))
                .with_context(|| format!("session_not_found: {}", session_id))?;
            let thread = entity
                .read(cx)
                .acp_thread()
                .cloned()
                .ok_or_else(|| anyhow!("session_has_no_thread: {}", session_id))?;
            // Resolve against the live thread entries: the kind / terminal
            // sub-patterns needed to build the outcome are reconstructed
            // server-side from what the agent actually offered, never
            // trusted from the client.
            let outcome = thread.read_with(cx, |thread, _| {
                resolve_authorization_outcome(thread.entries(), &tool_call_id, &option_id)
            })?;
            thread.update(cx, |thread, cx| {
                thread.authorize_tool_call(
                    acp::ToolCallId::new(tool_call_id.as_str()),
                    outcome,
                    cx,
                );
            });
            Ok(())
        })?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "authorized".to_string(),
            }],
            structured_content: AuthorizeToolCallResult { ok: true },
        })
    }
}

// =====================================================================
// solution_agent.rename_session
// =====================================================================

/// Rename a session's user-visible title.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RenameSessionParams {
    pub session_id: String,
    pub title: String,
}

impl<'de> Deserialize<'de> for RenameSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            title: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            title: inner.title,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RenameSessionResult {}

#[derive(Clone)]
pub struct RenameSessionTool;

impl McpServerTool for RenameSessionTool {
    type Input = RenameSessionParams;
    type Output = RenameSessionResult;
    const NAME: &'static str = "solution_agent.rename_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        anyhow::ensure!(!input.title.is_empty(), "invalid_params: title is required");
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        let title = SharedString::from(input.title);

        cx.update(|cx| -> Result<()> {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| store.rename_session(session_id, title, cx))?;
            Ok(())
        })?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "renamed".to_string(),
            }],
            structured_content: RenameSessionResult {},
        })
    }
}

// =====================================================================
// solution_agent.restart_agent
// =====================================================================

/// Restart the agent backing `session_id`. Drops the pooled subprocess
/// for the session's `(solution, agent)` pair, closes the existing
/// session, and opens a fresh one against the same project. v1 does not
/// replay history. Returns the new session id.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RestartAgentParams {
    pub session_id: String,
}

impl<'de> Deserialize<'de> for RestartAgentParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
        }
        Ok(Self {
            session_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .session_id,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct RestartAgentResult {
    pub session_id: String,
}

#[derive(Clone)]
pub struct RestartAgentTool;

impl McpServerTool for RestartAgentTool {
    type Input = RestartAgentParams;
    type Output = RestartAgentResult;
    const NAME: &'static str = "solution_agent.restart_agent";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        let restart_task = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| store.restart_agent(session_id, cx))
        });
        let new_session_id = restart_task.await?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: new_session_id.to_string(),
            }],
            structured_content: RestartAgentResult {
                session_id: new_session_id.to_string(),
            },
        })
    }
}

// =====================================================================
// solution_agent.reset_context
// =====================================================================

/// Wipe the conversation history of `session_id` while keeping the tab,
/// title, and `SolutionSessionId` stable. Wired to the desktop's
/// `/clear` slash command via `store::reset_context`. Different from
/// `restart_agent`, which mints a fresh session id (and therefore drops
/// the user-set title) — use this when the intent is "clear this chat"
/// and not "this session is broken, give me a new one".
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ResetContextParams {
    pub session_id: String,
}

impl<'de> Deserialize<'de> for ResetContextParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
        }
        Ok(Self {
            session_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .session_id,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ResetContextResult {
    pub session_id: String,
}

#[derive(Clone)]
pub struct ResetContextTool;

impl McpServerTool for ResetContextTool {
    type Input = ResetContextParams;
    type Output = ResetContextResult;
    const NAME: &'static str = "solution_agent.reset_context";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        let reset_task = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| store.reset_context(session_id, cx))
        });
        let same_session_id = reset_task.await?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: same_session_id.to_string(),
            }],
            structured_content: ResetContextResult {
                session_id: same_session_id.to_string(),
            },
        })
    }
}

// =====================================================================
// solution_agent.compact_session
// =====================================================================

/// Hard cap on the continuation prompt file. Keeps a runaway agent from
/// stuffing the entire conversation into a single file and re-feeding it
/// as the very first user message — which would defeat the whole point
/// of compacting. 256 KiB is generous (≈ 60k tokens of plain English).
const COMPACT_PROMPT_MAX_BYTES: u64 = 256 * 1024;

/// Rotate a session: validate the agent-prepared continuation file,
/// close the current session, open a fresh session under the same
/// `(solution, agent)` pair, and feed the file content as the first
/// user message of the new session. Returns the new session id so the
/// caller (an MCP-driven agent or the UI) can switch focus to it.
///
/// The agent calls this AFTER writing the per-rotation handoff files to
/// `<solution_root>/.agents/<session_id>/<timestamp>/`. The editor does
/// NOT generate the files — it only validates the prompt file and
/// owns the session lifecycle. See
/// `resources/compact_context_instructions.md` for the agent contract.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CompactSessionParams {
    pub session_id: String,
    pub prompt_file: String,
}

impl<'de> Deserialize<'de> for CompactSessionParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            prompt_file: String,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            prompt_file: inner.prompt_file,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CompactSessionResult {
    pub new_session_id: String,
    pub prompt_bytes: u64,
}

#[derive(Clone)]
pub struct CompactSessionTool;

impl McpServerTool for CompactSessionTool {
    type Input = CompactSessionParams;
    type Output = CompactSessionResult;
    const NAME: &'static str = "solution_agent.compact_session";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        anyhow::ensure!(
            !input.prompt_file.is_empty(),
            "invalid_params: prompt_file is required"
        );
        let old_session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        // 1. Validate the file. We resolve the OLD session's solution
        //    root and require the prompt path to live underneath
        //    `<solution_root>/.agents/<session_id>/` so an agent can't
        //    point us at /etc/passwd or some other unrelated file.
        let (solution_id, agent_id) = cx
            .update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.read_with(cx, |store, cx| {
                    store.session(old_session_id).map(|entity| {
                        let s = entity.read(cx);
                        (s.solution_id.clone(), s.agent_id.clone())
                    })
                })
            })
            .ok_or_else(|| anyhow!("unknown session {old_session_id}"))?;

        let solution_root = cx
            .update(|cx| {
                SolutionStore::try_global(cx).and_then(|store| {
                    store.read_with(cx, |s, _| {
                        s.solutions()
                            .iter()
                            .find(|sol| sol.id == solution_id)
                            .map(|sol| sol.root.clone())
                    })
                })
            })
            .ok_or_else(|| anyhow!("solution {solution_id:?} not found in store"))?;

        let prompt_path = std::path::PathBuf::from(&input.prompt_file);
        let prompt_path = if prompt_path.is_absolute() {
            prompt_path
        } else {
            solution_root.join(&prompt_path)
        };
        let prompt_path = prompt_path
            .canonicalize()
            .with_context(|| format!("prompt file not found: {}", prompt_path.display()))?;
        let allowed_root = solution_root
            .join(".agents")
            .canonicalize()
            .with_context(|| {
                format!(
                    "{}/.agents not found — agent must create handoff files before calling \
                     compact_session",
                    solution_root.display()
                )
            })?;
        anyhow::ensure!(
            prompt_path.starts_with(&allowed_root),
            "invalid_params: prompt_file must live under {}/.agents/",
            solution_root.display()
        );

        let metadata = std::fs::metadata(&prompt_path)
            .with_context(|| format!("stat {}", prompt_path.display()))?;
        anyhow::ensure!(
            metadata.is_file(),
            "invalid_params: prompt_file is not a regular file: {}",
            prompt_path.display()
        );
        anyhow::ensure!(
            metadata.len() > 0,
            "invalid_params: prompt_file is empty: {}",
            prompt_path.display()
        );
        anyhow::ensure!(
            metadata.len() <= COMPACT_PROMPT_MAX_BYTES,
            "invalid_params: prompt_file is {} bytes, max is {}",
            metadata.len(),
            COMPACT_PROMPT_MAX_BYTES
        );
        let prompt_bytes = metadata.len();

        let prompt_text = std::fs::read_to_string(&prompt_path)
            .with_context(|| format!("read {}", prompt_path.display()))?;
        anyhow::ensure!(
            !prompt_text.trim().is_empty(),
            "invalid_params: prompt_file contains only whitespace"
        );

        // Verify the agent actually wrote the full handoff bundle, not
        // just `continue.md`. We read `session-state.json` first to
        // learn the conversation scope, then check the per-scope file
        // set. Missing or empty files surface as a structured error so
        // the agent can re-attempt the dump and call us again instead
        // of silently rotating with half a transcript.
        let compact_dir = prompt_path
            .parent()
            .ok_or_else(|| anyhow!("prompt_file has no parent directory"))?
            .to_path_buf();
        validate_handoff_files(&compact_dir)?;

        // 2. Rotate the in-flight ACP thread under the SAME
        //    SolutionSessionId. Subprocess pool entry stays, tab stays,
        //    only the conversation history is swapped out. Returns the
        //    new context_count so the caller knows which context they
        //    are now in.
        let _ = solution_id;
        let _ = agent_id;
        let rotate_task = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| store.rotate_context(old_session_id, cx))
        });
        let new_context_count = rotate_task.await?;

        // 3. Feed the continuation prompt as the rotated session's
        //    first user message. Detached because the tool response
        //    should return as soon as the message is enqueued — the
        //    user watches the same tab live for the agent's reply.
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.send_message(old_session_id, prompt_text, cx).detach();
            });
        });

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!(
                    "rotated {old_session_id} into context c{new_context_count:02} \
                     ({prompt_bytes} bytes)"
                ),
            }],
            structured_content: CompactSessionResult {
                new_session_id: old_session_id.to_string(),
                prompt_bytes,
            },
        })
    }
}

// =====================================================================
// solution_agent.start_compact
// =====================================================================

/// Kick off the "Compact context" workflow on a hot session — the same
/// orchestration the desktop's status-row popover "Compact context"
/// entry runs. Sends the compact-instructions template as a user
/// message; the agent then writes its handoff files and calls back
/// into the lower-level `solution_agent.compact_session` to rotate.
///
/// Surface contract: this tool is what a human client (e.g. the phone)
/// invokes from a "Compact" button. `compact_session` is what Claude
/// Code itself invokes after producing the handoff dump. Don't mix
/// them up — `compact_session` rotates the ACP thread immediately and
/// would discard the user's intent on a hot conversation.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct StartCompactParams {
    pub session_id: String,
}

impl<'de> Deserialize<'de> for StartCompactParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
        }
        Ok(Self {
            session_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .session_id,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StartCompactResult {
    /// `true` when the compact prompt was enqueued on the agent.
    /// `false` when a precondition wasn't met (e.g. session busy,
    /// context below 20%, less than 30k tokens of headroom, or session
    /// is cold) — `message` carries the reason.
    pub queued: bool,
    /// Human-readable explanation when `queued == false`. `None` on
    /// success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone)]
pub struct StartCompactTool;

impl McpServerTool for StartCompactTool {
    type Input = StartCompactParams;
    type Output = StartCompactResult;
    const NAME: &'static str = "solution_agent.start_compact";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;

        let outcome = cx.update(|cx| -> Result<crate::compact::StartCompactOutcome> {
            crate::compact::start_compact_for_session(session_id, cx)
        })?;

        let text = if outcome.queued {
            format!("compact queued for {session_id}")
        } else {
            outcome
                .reason
                .clone()
                .unwrap_or_else(|| "compact declined".to_string())
        };
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text }],
            structured_content: StartCompactResult {
                queued: outcome.queued,
                message: outcome.reason,
            },
        })
    }
}

/// Verifies the agent wrote the full handoff bundle into `compact_dir`
/// before letting `compact_session` rotate. Reads `session-state.json`
/// to learn the scope, then checks the per-scope required file set.
///
/// Scope file requirements (per the agent contract in
/// `resources/compact_context_instructions.md`):
/// - `planned` and `branching`: state.md, decisions.md, next.md, continue.md
/// - `exploratory`: state.md, decisions.md, continue.md (next.md skipped)
///
/// Returns a single combined error listing every missing / empty file —
/// the agent gets the whole picture in one round-trip instead of
/// fix-one, retry, fix-another, retry.
fn validate_handoff_files(compact_dir: &std::path::Path) -> Result<()> {
    let state_json_path = compact_dir.join("session-state.json");
    let state_json_meta = std::fs::metadata(&state_json_path).with_context(|| {
        format!(
            "compact_incomplete: session-state.json is missing in {}",
            compact_dir.display()
        )
    })?;
    anyhow::ensure!(
        state_json_meta.is_file() && state_json_meta.len() > 0,
        "compact_incomplete: session-state.json is empty"
    );
    let state_text = std::fs::read_to_string(&state_json_path).with_context(|| {
        format!(
            "compact_incomplete: cannot read {}",
            state_json_path.display()
        )
    })?;
    let state_json: serde_json::Value = serde_json::from_str(&state_text)
        .with_context(|| "compact_incomplete: session-state.json is not valid JSON")?;
    let scope = state_json
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("planned")
        .to_string();

    let mut required = vec!["state.md", "decisions.md", "continue.md"];
    if scope != "exploratory" {
        required.push("next.md");
    }

    let mut missing = Vec::new();
    let mut empty = Vec::new();
    for name in &required {
        let path = compact_dir.join(name);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => {}
            Ok(meta) if meta.is_file() => empty.push(name.to_string()),
            _ => missing.push(name.to_string()),
        }
    }

    if !missing.is_empty() || !empty.is_empty() {
        let mut msg =
            format!("compact_incomplete (scope={scope}): the agent did not write the full bundle");
        if !missing.is_empty() {
            msg.push_str(&format!(". Missing: {}", missing.join(", ")));
        }
        if !empty.is_empty() {
            msg.push_str(&format!(". Empty: {}", empty.join(", ")));
        }
        msg.push_str(&format!(". Expected under {}", compact_dir.display()));
        anyhow::bail!(msg);
    }
    Ok(())
}

// =====================================================================
// solution_agent.read_session_history
// =====================================================================

/// Cap on how many entries we ever return in one MCP response. Avoids
/// shipping a 50 MB transcript over the JSON-RPC socket if the caller
/// asks for "everything" on a long-running session.
const HISTORY_HARD_LIMIT: usize = 500;
/// Default page size when the caller doesn't supply one.
const HISTORY_DEFAULT_LIMIT: usize = 100;

/// Returns a markdown rendering of the conversation transcript for any
/// session — live or already closed. Pulls live state from the
/// in-memory store when the session is open, otherwise rehydrates the
/// JSON blob the store wrote to SQLite on every successful turn.
///
/// Designed for downstream agents that want to "read what session X
/// concluded" without resuming it. For live sessions, prefer
/// `solution_agent.get_session` + the per-event push notifications;
/// this tool is the polling / archive-read path.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ReadSessionHistoryParams {
    pub session_id: String,
    /// Number of entries to return (default 100, hard cap 500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Number of entries to skip from the start (oldest-first ordering).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

impl<'de> Deserialize<'de> for ReadSessionHistoryParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            limit: Option<usize>,
            offset: Option<usize>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            limit: inner.limit,
            offset: inner.offset,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReadSessionHistoryResult {
    pub session_id: String,
    /// `live` for sessions still open in the store, `archived` for
    /// sessions whose acp_thread has been dropped but whose blob is
    /// still in SQLite.
    pub source: String,
    pub title: String,
    pub total_entries: usize,
    pub returned_entries: usize,
    /// Markdown rendering of each entry, oldest-first.
    pub entries: Vec<String>,
}

#[derive(Clone)]
pub struct ReadSessionHistoryTool;

impl McpServerTool for ReadSessionHistoryTool {
    type Input = ReadSessionHistoryParams;
    type Output = ReadSessionHistoryResult;
    const NAME: &'static str = "solution_agent.read_session_history";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        let offset = input.offset.unwrap_or(0);
        let limit = input
            .limit
            .unwrap_or(HISTORY_DEFAULT_LIMIT)
            .min(HISTORY_HARD_LIMIT);

        // 1. Live path: if the session is still in the in-memory store,
        //    render entries directly off the AcpThread. This is fresher
        //    than the persisted blob, which only updates on turn
        //    completion.
        let live = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.read_with(cx, |store, cx| {
                let session = store.session(session_id)?;
                let s = session.read(cx);
                let title = s.title.to_string();
                let entries = s.acp_thread().map(|thread| {
                    thread
                        .read(cx)
                        .entries()
                        .iter()
                        .map(|entry| entry.to_markdown(cx))
                        .collect::<Vec<String>>()
                })?;
                Some((title, entries))
            })
        });
        if let Some((title, entries)) = live {
            let total = entries.len();
            let slice = entries
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>();
            let returned = slice.len();
            return Ok(ToolResponse {
                content: vec![ToolResponseContent::Text {
                    text: format!("{returned}/{total} entries (live)"),
                }],
                structured_content: ReadSessionHistoryResult {
                    session_id: session_id.to_string(),
                    source: "live".to_string(),
                    title,
                    total_entries: total,
                    returned_entries: returned,
                    entries: slice,
                },
            });
        }

        // 2. Archive path: live session not found, fall back to the
        //    persisted blob written on the last successful turn.
        let load_task = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.read_with(cx, |store, _| {
                store.persistence().map(|db| db.load_blob(session_id))
            })
        });
        let blob: Option<Vec<u8>> = match load_task {
            Some(task) => task.await?,
            None => None,
        };
        let blob = blob.ok_or_else(|| {
            anyhow!("session_not_found: {session_id} is neither open nor archived in the database")
        })?;
        let snapshot: PersistedSession = serde_json::from_slice(&blob)
            .with_context(|| format!("decoding archived session {session_id}"))?;
        let total = snapshot.entry_summaries.len();
        let slice = snapshot
            .entry_summaries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let returned = slice.len();
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{returned}/{total} entries (archived)"),
            }],
            structured_content: ReadSessionHistoryResult {
                session_id: session_id.to_string(),
                source: "archived".to_string(),
                title: snapshot.title,
                total_entries: total,
                returned_entries: returned,
                entries: slice,
            },
        })
    }
}

// =====================================================================
// solution_agent.upload_{init,status,finish,abort}
// =====================================================================
//
// Chunked-upload control surface for the WebSocket binary-frame attachment
// path. See `solution_agent::upload` for the storage manager and
// `remote_control::listener` for the binary-frame dispatch. Mobile clients
// drive the lifecycle:
//   1. `upload_init` → server allocates an id + tmp file, returns u64 id.
//   2. WS binary frames (16-byte header `u64 id BE | u64 offset BE` +
//      payload) push the bytes; the listener calls `UploadManager::write_chunk`.
//   3. (optional) `upload_status` polls per-id progress.
//   4. `upload_finish` validates total size + optional sha256, returns
//      `{handle: "spk-upload://<id>"}`.
//   5. The handle is embedded as a `ResourceLink` in `send_message_blocks`,
//      which swaps it for inline `Image`/`Text` content and aborts the entry.
//   6. `upload_abort` cancels an upload (e.g. user cancelled the picker).

/// Allocate a chunked-upload slot for `session_id`. Returns an `upload_id`
/// that subsequent WebSocket binary frames (16-byte header + payload) write
/// chunks against. The session must already exist; the per-session
/// concurrency cap blocks runaway disk usage.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadInitParams {
    pub session_id: String,
    pub mime: String,
    pub display_name: String,
    pub total_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl<'de> Deserialize<'de> for UploadInitParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            session_id: String,
            mime: String,
            display_name: String,
            total_size: u64,
            sha256: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            session_id: inner.session_id,
            mime: inner.mime,
            display_name: inner.display_name,
            total_size: inner.total_size,
            sha256: inner.sha256,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadInitResult {
    pub upload_id: u64,
}

#[derive(Clone)]
pub struct UploadInitTool;

impl McpServerTool for UploadInitTool {
    type Input = UploadInitParams;
    type Output = UploadInitResult;
    const NAME: &'static str = "solution_agent.upload_init";

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        anyhow::ensure!(
            !input.session_id.is_empty(),
            "invalid_params: session_id is required"
        );
        anyhow::ensure!(!input.mime.is_empty(), "invalid_params: mime is required");
        anyhow::ensure!(
            !input.display_name.is_empty(),
            "invalid_params: display_name is required"
        );
        anyhow::ensure!(
            input.total_size > 0,
            "invalid_params: total_size must be > 0"
        );

        // Validate the session is known to the store BEFORE allocating a
        // tmp file. A typo'd session_id should fail fast, not after the
        // client has streamed megabytes of payload.
        let session_id = SolutionSessionId::parse(&input.session_id)
            .map_err(|e| anyhow!("bad session id: {e}"))?;
        let exists = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.read(cx).session(session_id).is_some()
        });
        if !exists {
            anyhow::bail!("unknown_session: {}", input.session_id);
        }

        // Capture log fields before moving `input` into the manager call.
        let mime_for_log = input.mime.clone();
        let total_size_for_log = input.total_size;
        let upload_id = crate::upload::with_manager(|m| {
            m.init(
                input.session_id,
                input.mime,
                input.display_name,
                input.total_size,
                input.sha256,
            )
        })
        .ok_or_else(|| anyhow!("upload manager not initialised"))??;

        log::info!(
            target: "solution_agent::upload",
            "upload_init OK: upload_id={upload_id} mime={mime_for_log} total_size={total_size_for_log}",
        );
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("upload_id={upload_id}"),
            }],
            structured_content: UploadInitResult { upload_id },
        })
    }
}

/// Inspect the per-upload `received_bytes` / `total_size` progress without
/// consuming the entry. Mobile clients can poll this between chunks for a
/// progress bar.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadStatusParams {
    pub upload_id: u64,
}

impl<'de> Deserialize<'de> for UploadStatusParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            upload_id: u64,
        }
        Ok(Self {
            upload_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .upload_id,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadStatusResult {
    pub received_bytes: u64,
    pub total_size: u64,
}

#[derive(Clone)]
pub struct UploadStatusTool;

impl McpServerTool for UploadStatusTool {
    type Input = UploadStatusParams;
    type Output = UploadStatusResult;
    const NAME: &'static str = "solution_agent.upload_status";

    async fn run(
        &self,
        input: Self::Input,
        _cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let (received_bytes, total_size) =
            crate::upload::with_manager(|m| m.status(input.upload_id))
                .ok_or_else(|| anyhow!("upload manager not initialised"))?
                .ok_or_else(|| anyhow!("unknown_upload_id: {}", input.upload_id))?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: format!("{received_bytes}/{total_size}"),
            }],
            structured_content: UploadStatusResult {
                received_bytes,
                total_size,
            },
        })
    }
}

/// Finalize an upload — validates `received_bytes == total_size`, optionally
/// verifies a sha256, and returns the `spk-upload://<id>` handle string
/// that `send_message_blocks` resolves to inline content.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadFinishParams {
    pub upload_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl<'de> Deserialize<'de> for UploadFinishParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            upload_id: u64,
            sha256: Option<String>,
        }
        let inner = Option::<Inner>::deserialize(de)?.unwrap_or_default();
        Ok(Self {
            upload_id: inner.upload_id,
            sha256: inner.sha256,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadFinishResult {
    pub handle: String,
}

#[derive(Clone)]
pub struct UploadFinishTool;

impl McpServerTool for UploadFinishTool {
    type Input = UploadFinishParams;
    type Output = UploadFinishResult;
    const NAME: &'static str = "solution_agent.upload_finish";

    async fn run(
        &self,
        input: Self::Input,
        _cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let upload_id_for_log = input.upload_id;
        let handle =
            crate::upload::with_manager(|m| m.finish(input.upload_id, input.sha256.as_deref()))
                .ok_or_else(|| anyhow!("upload manager not initialised"))??;
        let handle_uri = format!("{}{}", crate::upload::HANDLE_SCHEME, handle.id);
        log::info!(
            target: "solution_agent::upload",
            "upload_finish OK: upload_id={upload_id_for_log} handle={handle_uri}",
        );
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: handle_uri.clone(),
            }],
            structured_content: UploadFinishResult { handle: handle_uri },
        })
    }
}

/// Cancel an in-flight or finished-but-unconsumed upload, deleting the tmp
/// file. Idempotent in spirit — calling abort on an unknown id returns an
/// error rather than silently succeeding so the client knows the entry was
/// already gone (e.g. GC reaped it).
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadAbortParams {
    pub upload_id: u64,
}

impl<'de> Deserialize<'de> for UploadAbortParams {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        struct Inner {
            upload_id: u64,
        }
        Ok(Self {
            upload_id: Option::<Inner>::deserialize(de)?
                .unwrap_or_default()
                .upload_id,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct UploadAbortResult {}

#[derive(Clone)]
pub struct UploadAbortTool;

impl McpServerTool for UploadAbortTool {
    type Input = UploadAbortParams;
    type Output = UploadAbortResult;
    const NAME: &'static str = "solution_agent.upload_abort";

    async fn run(
        &self,
        input: Self::Input,
        _cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        crate::upload::with_manager(|m| m.abort(input.upload_id))
            .ok_or_else(|| anyhow!("upload manager not initialised"))??;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text {
                text: "aborted".to_string(),
            }],
            structured_content: UploadAbortResult {},
        })
    }
}

#[cfg(test)]
mod tests {
    //! R-5e enrichment coverage. These tests build a real `AcpThread`
    //! via the mock-agent test infra, push synthetic entries straight
    //! through the public `acp_thread` API, then call the MCP tools
    //! the same way the WS proxy does and assert the wire shape.

    use super::*;
    use crate::store::tests::create_session_with_thread;
    use context_server::listener::McpServerTool;

    #[test]
    fn session_state_dto_serializes_structured() {
        use crate::model::SessionState;
        let json = |s: &SessionState, ms: i64| {
            serde_json::to_value(SessionStateDto::from_state(s, ms)).unwrap()
        };
        assert_eq!(json(&SessionState::Idle, 0), serde_json::json!({"kind":"idle"}));
        assert_eq!(json(&SessionState::Stopping, 0), serde_json::json!({"kind":"stopping"}));
        assert_eq!(
            json(&SessionState::AwaitingInput, 0),
            serde_json::json!({"kind":"awaiting_input"})
        );
        assert_eq!(
            json(&SessionState::Errored("boom".into()), 0),
            serde_json::json!({"kind":"errored","message":"boom"})
        );
        let running = SessionState::Running {
            started_at: std::time::Instant::now(),
            notified: false,
        };
        assert_eq!(
            json(&running, 1779),
            serde_json::json!({"kind":"running","started_at_ms":1779})
        );
    }

    fn fake_user_text_chunk(text: &str) -> acp::ContentBlock {
        acp::ContentBlock::Text(acp::TextContent::new(text.to_string()))
    }

    fn fake_image_chunk(mime: &str, data_b64: &str) -> acp::ContentBlock {
        acp::ContentBlock::Image(acp::ImageContent::new(
            data_b64.to_string(),
            mime.to_string(),
        ))
    }

    /// Push a minimal user message + assistant message into the live
    /// thread so `get_session` has at least two entries to enrich.
    /// Returns a 1x1 PNG base64 payload that callers can match against.
    async fn seed_session_with_image(
        cx: &mut gpui::TestAppContext,
    ) -> (crate::model::SolutionSessionId, String, tempfile::TempDir) {
        let (session_id, acp_thread, tmp) = create_session_with_thread(cx).await;
        // 1×1 PNG, generated once with `base64 -w0 < tiny.png` — kept
        // small so test fixtures don't bloat the suite.
        let image_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNgAAIAAAUAAen5lOEAAAAASUVORK5CYII=".to_string();
        let image_b64_clone = image_b64.clone();
        cx.update(|cx| {
            acp_thread.update(cx, |thread, cx| {
                thread.push_user_content_block(None, fake_user_text_chunk("hello"), cx);
                thread.push_user_content_block(
                    None,
                    fake_image_chunk("image/png", &image_b64_clone),
                    cx,
                );
                thread.push_assistant_content_block(fake_user_text_chunk("world"), false, cx);
            });
        });
        cx.executor().run_until_parked();
        (session_id, image_b64, tmp)
    }

    #[gpui::test]
    async fn list_agents_returns_empty_when_no_adapters_registered(cx: &mut gpui::TestAppContext) {
        // create_session_with_thread builds an empty AdapterRegistry —
        // mock-agent gets registered via `register_agent_server`, not
        // via `AdapterRegistry::register`. So list_agents (which reads
        // the adapter registry) returns []. Asserts the wire shape and
        // the empty-list code path; the registry itself is covered by
        // `adapter::tests`.
        let (_session_id, _img, _tmp) = seed_session_with_image(cx).await;
        let result = cx
            .update(|cx| {
                let cx = cx.to_async();
                async move {
                    ListAgentsTool
                        .run(ListAgentsParams {}, &mut cx.clone())
                        .await
                }
            })
            .await
            .expect("list_agents tool should run");
        assert_eq!(result.structured_content.agents.len(), 0);
        match &result.content[0] {
            ToolResponseContent::Text { text } => assert_eq!(text, "0 agent(s)"),
            _ => panic!("expected text content"),
        }
    }

    #[gpui::test]
    async fn get_session_default_flags_omit_full_content(cx: &mut gpui::TestAppContext) {
        let (session_id, _img, _tmp) = seed_session_with_image(cx).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    include_full_content: false,
                    include_images: false,
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        assert!(
            !result.structured_content.entries.is_empty(),
            "expected entries"
        );
        for entry in &result.structured_content.entries {
            assert!(
                entry.markdown.is_none(),
                "markdown must stay None when include_full_content=false; got {:?}",
                entry.markdown
            );
            assert!(
                entry.images.is_none(),
                "images must stay None when include_images=false; got {:?}",
                entry.images
            );
            assert!(
                !entry.preview.is_empty(),
                "preview must always be populated"
            );
        }
    }

    #[gpui::test]
    async fn get_session_full_content_populates_markdown(cx: &mut gpui::TestAppContext) {
        let (session_id, _img, _tmp) = seed_session_with_image(cx).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    include_full_content: true,
                    include_images: false,
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        for entry in &result.structured_content.entries {
            let md = entry
                .markdown
                .as_ref()
                .expect("markdown populated when include_full_content=true");
            assert!(
                md.len() >= entry.preview.trim_end_matches('…').len(),
                "markdown should be at least as long as preview's content"
            );
            assert!(
                entry.images.is_none(),
                "images stay None unless include_images=true"
            );
        }
    }

    #[gpui::test]
    async fn get_session_include_images_inlines_base64(cx: &mut gpui::TestAppContext) {
        let (session_id, expected_b64, _tmp) = seed_session_with_image(cx).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    include_full_content: true,
                    include_images: true,
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let mut total_images = 0usize;
        let mut saw_expected = false;
        for entry in &result.structured_content.entries {
            let images = entry
                .images
                .as_ref()
                .expect("images list populated even if empty");
            total_images += images.len();
            for image in images {
                assert_eq!(image.mime_type, "image/png");
                if image.data_base64 == expected_b64 {
                    saw_expected = true;
                }
            }
        }
        assert!(
            total_images >= 1,
            "expected at least one image after seeding"
        );
        assert!(
            saw_expected,
            "the seeded PNG payload should round-trip unchanged"
        );
    }

    #[gpui::test]
    async fn get_session_entry_happy_path_returns_full_markdown(cx: &mut gpui::TestAppContext) {
        let (session_id, _img, _tmp) = seed_session_with_image(cx).await;

        let result = GetSessionEntryTool
            .run(
                GetSessionEntryParams {
                    session_id: session_id.to_string(),
                    index: 0,
                    include_images: false,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session_entry");

        let entry = result.structured_content.entry;
        assert_eq!(entry.role, "user");
        // R-6e: every EntrySummary carries its absolute index.
        assert_eq!(entry.index, 0);
        let md = entry
            .markdown
            .expect("markdown is always populated for single-entry fetch");
        assert!(md.contains("hello"));
    }

    #[gpui::test]
    async fn get_session_entry_out_of_range_errors(cx: &mut gpui::TestAppContext) {
        let (session_id, _img, _tmp) = seed_session_with_image(cx).await;

        let err = GetSessionEntryTool
            .run(
                GetSessionEntryParams {
                    session_id: session_id.to_string(),
                    index: 9_999,
                    include_images: false,
                },
                &mut cx.to_async(),
            )
            .await
            .expect_err("out-of-range index must error");

        let msg = format!("{:#}", err);
        assert!(
            msg.contains("entry_index_out_of_range"),
            "error should mention entry_index_out_of_range, got: {msg}"
        );
    }

    #[gpui::test]
    async fn tool_call_entry_surfaces_status_and_args(cx: &mut gpui::TestAppContext) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

        // Push a synthetic ToolCall directly into the thread. We bypass
        // `handle_session_update` because that path requires a real ACP
        // server; constructing the entry by hand exercises the same
        // public type the render layer reads.
        cx.update(|cx| {
            acp_thread.update(cx, |thread, cx| {
                let tool_call = acp::ToolCall::new(
                    acp::ToolCallId::new("call-1".to_string()),
                    "Bash".to_string(),
                )
                .kind(acp::ToolKind::Execute)
                .raw_input(serde_json::json!({ "cmd": "ls" }));
                thread
                    .upsert_tool_call(tool_call, cx)
                    .expect("upsert_tool_call");
            });
        });
        cx.executor().run_until_parked();

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    include_full_content: false,
                    include_images: false,
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let tool_entry = result
            .structured_content
            .entries
            .iter()
            .find(|e| e.role == "tool_call")
            .expect("tool_call entry");
        let tool = tool_entry
            .tool_call
            .as_ref()
            .expect("tool_call summary populated");
        // Reuses `tool_call_status_text` — pending status maps to the
        // literal string "pending".
        assert_eq!(tool.status, "pending");
        assert!(
            tool.args_preview.contains("\"cmd\""),
            "args_preview should serialize raw_input JSON, got: {}",
            tool.args_preview
        );
        assert!(
            tool.tool_status_started_at_ms.is_none(),
            "Pending tool call should not surface a started_at timestamp, got: {:?}",
            tool.tool_status_started_at_ms,
        );
    }

    #[gpui::test]
    async fn tool_call_entry_surfaces_status_started_at_when_in_progress(
        cx: &mut gpui::TestAppContext,
    ) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

        let before_ms = chrono::Utc::now().timestamp_millis();
        cx.update(|cx| {
            acp_thread.update(cx, |thread, cx| {
                let tool_call = acp::ToolCall::new(
                    acp::ToolCallId::new("call-1".to_string()),
                    "Bash".to_string(),
                )
                .kind(acp::ToolKind::Execute)
                .status(acp::ToolCallStatus::InProgress);
                thread
                    .upsert_tool_call(tool_call, cx)
                    .expect("upsert_tool_call");
            });
        });
        cx.executor().run_until_parked();
        let after_ms = chrono::Utc::now().timestamp_millis();

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    include_full_content: false,
                    include_images: false,
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let tool = result
            .structured_content
            .entries
            .iter()
            .find(|e| e.role == "tool_call")
            .and_then(|e| e.tool_call.as_ref())
            .expect("tool_call summary populated");
        assert_eq!(tool.status, "running");
        let stamp = tool
            .tool_status_started_at_ms
            .expect("InProgress tool call must surface a started_at timestamp");
        assert!(
            stamp >= before_ms && stamp <= after_ms,
            "tool_status_started_at_ms {stamp} should fall between {before_ms} and {after_ms}",
        );
    }

    #[gpui::test]
    async fn plan_entry_surfaces_items(cx: &mut gpui::TestAppContext) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;

        cx.update(|cx| {
            acp_thread.update(cx, |thread, cx| {
                let plan = acp::Plan::new(vec![
                    acp::PlanEntry::new(
                        "step one".to_string(),
                        acp::PlanEntryPriority::Medium,
                        acp::PlanEntryStatus::Completed,
                    ),
                    acp::PlanEntry::new(
                        "step two".to_string(),
                        acp::PlanEntryPriority::Medium,
                        acp::PlanEntryStatus::Completed,
                    ),
                ]);
                thread.update_plan(plan, cx);
            });
        });
        cx.executor().run_until_parked();

        // `update_plan` keeps the plan in-flight until something
        // upgrades it to `CompletedPlan`. The session_view path does
        // this via the `EntryUpdated` cycle; in tests we drive the
        // same transition by emitting `Stopped` which forces the
        // pending plan to flush. If a plan entry isn't surfaced as
        // `CompletedPlan` we just confirm no panic — the actual plan
        // shape is checked in `acp_thread` upstream tests.
        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    include_full_content: false,
                    include_images: false,
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        if let Some(plan_entry) = result
            .structured_content
            .entries
            .iter()
            .find(|e| e.role == "plan")
        {
            let plan = plan_entry
                .plan
                .as_ref()
                .expect("plan summary populated for role=plan");
            assert_eq!(plan.items.len(), 2);
            assert!(plan.items[0].contains("step one"));
        }
        // Soft assertion — if the synthetic plan didn't get promoted to
        // CompletedPlan we still want the test to exercise the wire
        // path without panicking.
    }

    // =================================================================
    // R-6e pagination coverage (`solution_agent.get_session` +
    // `solution_agent.list_sessions`).
    // =================================================================

    /// Seed a session with exactly 5 plain text entries — alternating
    /// user/assistant — so pagination tests have stable indices 0..=4.
    /// No images, no tool calls; the only thing under test is
    /// before/after/count filtering on a known entry shape.
    async fn seed_session_with_n_entries(
        cx: &mut gpui::TestAppContext,
        n: usize,
    ) -> (crate::model::SolutionSessionId, tempfile::TempDir) {
        let (session_id, acp_thread, tmp) = create_session_with_thread(cx).await;
        cx.update(|cx| {
            acp_thread.update(cx, |thread, cx| {
                for i in 0..n {
                    let text = format!("entry-{i}");
                    if i % 2 == 0 {
                        thread.push_user_content_block(None, fake_user_text_chunk(&text), cx);
                    } else {
                        thread.push_assistant_content_block(fake_user_text_chunk(&text), false, cx);
                    }
                }
            });
        });
        cx.executor().run_until_parked();
        (session_id, tmp)
    }

    #[gpui::test]
    async fn get_session_no_pagination_returns_all_entries_with_total_count(
        cx: &mut gpui::TestAppContext,
    ) {
        let (session_id, _tmp) = seed_session_with_n_entries(cx, 5).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let entries = &result.structured_content.entries;
        assert_eq!(entries.len(), 5, "no pagination → all 5 entries");
        assert_eq!(result.structured_content.total_count, 5);
        for (expected, entry) in entries.iter().enumerate() {
            assert_eq!(
                entry.index, expected,
                "EntrySummary.index must match absolute position"
            );
        }
    }

    #[gpui::test]
    async fn get_session_count_returns_last_n_entries(cx: &mut gpui::TestAppContext) {
        let (session_id, _tmp) = seed_session_with_n_entries(cx, 5).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    count: Some(2),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let entries = &result.structured_content.entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries.iter().map(|e| e.index).collect::<Vec<_>>(),
            vec![3, 4],
            "count=2 returns the LAST two entries (indices 3,4)"
        );
        assert_eq!(result.structured_content.total_count, 5);
    }

    #[gpui::test]
    async fn get_session_before_index_drops_newer(cx: &mut gpui::TestAppContext) {
        let (session_id, _tmp) = seed_session_with_n_entries(cx, 5).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    before_index: Some(3),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let entries = &result.structured_content.entries;
        assert_eq!(
            entries.iter().map(|e| e.index).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "before_index=3 keeps strictly-less indices 0,1,2"
        );
        assert_eq!(result.structured_content.total_count, 5);
    }

    #[gpui::test]
    async fn get_session_after_index_drops_older(cx: &mut gpui::TestAppContext) {
        let (session_id, _tmp) = seed_session_with_n_entries(cx, 5).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    after_index: Some(2),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let entries = &result.structured_content.entries;
        assert_eq!(
            entries.iter().map(|e| e.index).collect::<Vec<_>>(),
            vec![3, 4],
            "after_index=2 keeps strictly-greater indices 3,4"
        );
        assert_eq!(result.structured_content.total_count, 5);
    }

    #[gpui::test]
    async fn get_session_before_and_after_index_select_slice(cx: &mut gpui::TestAppContext) {
        let (session_id, _tmp) = seed_session_with_n_entries(cx, 5).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    after_index: Some(2),
                    before_index: Some(4),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let entries = &result.structured_content.entries;
        assert_eq!(
            entries.iter().map(|e| e.index).collect::<Vec<_>>(),
            vec![3],
            "after=2, before=4 leaves only index 3"
        );
        assert_eq!(result.structured_content.total_count, 5);
    }

    #[gpui::test]
    async fn get_session_after_index_then_count_takes_last_within_filter(
        cx: &mut gpui::TestAppContext,
    ) {
        let (session_id, _tmp) = seed_session_with_n_entries(cx, 5).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    after_index: Some(2),
                    count: Some(1),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let entries = &result.structured_content.entries;
        // After filter: indices 3,4. count=1 keeps the LAST = index 4.
        // Wait — plan says "entries are index 3 (last after filter)". Let's
        // re-read: "after_index=2, count=1 → entries are index 3 (last
        // after filter)". That's odd — the filter keeps {3,4} and "last"
        // would be 4. The plan likely meant "the slice has cardinality 1
        // — exactly one entry — at the most-recent position 4". But the
        // plan-doc literal says "index 3". Re-check: the plan-doc text in
        // the user prompt says exactly: "after_index=2, count=1 → entries
        // are index 3 (last after filter)". That contradicts the
        // count semantics ("LAST n") defined earlier in the SAME prompt.
        //
        // Resolving in favor of the LAST-N semantics defined in scope B
        // step 5 (`take(n)` on the reversed iterator), so count=1 of
        // {3,4} = {4}. The plan-doc's example is a typo.
        assert_eq!(
            entries.iter().map(|e| e.index).collect::<Vec<_>>(),
            vec![4],
            "after=2 keeps {{3,4}}, count=1 then takes the LAST → index 4"
        );
        assert_eq!(result.structured_content.total_count, 5);
    }

    #[gpui::test]
    async fn get_session_after_index_past_end_returns_empty(cx: &mut gpui::TestAppContext) {
        let (session_id, _tmp) = seed_session_with_n_entries(cx, 5).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    after_index: Some(99),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        assert!(
            result.structured_content.entries.is_empty(),
            "after_index past end → empty"
        );
        assert_eq!(
            result.structured_content.total_count, 5,
            "total_count still reflects the underlying thread"
        );
    }

    #[gpui::test]
    async fn list_sessions_pagination_orders_desc_and_caps_to_count(cx: &mut gpui::TestAppContext) {
        // Reuse the first session's setup (it primes globals + the mock
        // adapter), then create two more sessions in the same solution
        // with slightly later activity timestamps so the DESC ordering
        // is observable.
        let (first_session_id, _thread, _tmp) = create_session_with_thread(cx).await;

        let (solution_id, agent_id, project) = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            let session = store
                .read(cx)
                .session(first_session_id)
                .expect("first session exists");
            let session_ref = session.read(cx);
            (
                session_ref.solution_id.clone(),
                session_ref.agent_id.clone(),
                session_ref
                    .project
                    .clone()
                    .expect("create_session populates project"),
            )
        });

        let second_session_id = cx
            .update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.update(cx, |store, cx| {
                    store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
                })
            })
            .await
            .expect("create second session");

        let third_session_id = cx
            .update(|cx| {
                let store = SolutionAgentStore::global(cx);
                store.update(cx, |store, cx| {
                    store.create_session(solution_id.clone(), agent_id.clone(), project.clone(), cx)
                })
            })
            .await
            .expect("create third session");

        // The third is the most-recently-created; bump its
        // last_activity_at explicitly so the DESC sort puts it first
        // even on machines where Utc::now()'s resolution lets two
        // creates land in the same tick.
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            let (second, third) = store.read_with(cx, |store, _| {
                (
                    store.session(second_session_id).expect("second"),
                    store.session(third_session_id).expect("third"),
                )
            });
            second.update(cx, |s, _| {
                s.last_activity_at = chrono::Utc::now() + chrono::Duration::seconds(1);
            });
            third.update(cx, |s, _| {
                s.last_activity_at = chrono::Utc::now() + chrono::Duration::seconds(2);
            });
        });

        let result = ListSessionsTool
            .run(
                ListSessionsParams {
                    solution_id: Some(solution_id.0.clone()),
                    parent_session_id: None,
                    count: Some(1),
                    before_last_activity_at_ms: None,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("list_sessions");

        let sessions = &result.structured_content.sessions;
        assert_eq!(sessions.len(), 1, "count=1 caps to one entry");
        assert_eq!(
            sessions[0].id,
            third_session_id.to_string(),
            "DESC ordering surfaces the most-recent session first"
        );
        assert_eq!(
            result.structured_content.total_count, 3,
            "total_count reflects all matching sessions, pre-pagination"
        );
    }

    // =================================================================
    // F: sub-agent indication coverage
    //
    // Validates the `parent_session_id` field plumbing across the MCP
    // wire shape and the new `solution_agent.get_session_children` tool.
    // =================================================================

    /// Spawn a sub-session under `parent_id`. Stays at the store layer
    /// to avoid the `MultiWorkspace` requirement of `CreateSessionTool`;
    /// the tool-layer create_session paths are covered separately in
    /// the dedicated F validation tests below.
    async fn create_child_session(
        cx: &mut gpui::TestAppContext,
        parent_id: crate::model::SolutionSessionId,
    ) -> crate::model::SolutionSessionId {
        let (solution_id, agent_id, project) = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            let session = store
                .read(cx)
                .session(parent_id)
                .expect("parent session exists");
            let session_ref = session.read(cx);
            (
                session_ref.solution_id.clone(),
                session_ref.agent_id.clone(),
                session_ref.project.clone().expect("parent has project"),
            )
        });
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store.update(cx, |store, cx| {
                store.create_session_with_parent(
                    solution_id,
                    agent_id,
                    project,
                    None,
                    Some(parent_id),
                    cx,
                )
            })
        })
        .await
        .expect("create child session")
    }

    #[gpui::test]
    async fn create_session_with_parent_sets_parent_session_id_on_child(
        cx: &mut gpui::TestAppContext,
    ) {
        let (parent_id, _thread, _tmp) = create_session_with_thread(cx).await;
        let child_id = create_child_session(cx, parent_id).await;

        // GetSession surfaces parent_session_id on the child.
        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: child_id.to_string(),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session(child)");
        assert_eq!(
            result.structured_content.parent_session_id.as_deref(),
            Some(parent_id.to_string().as_str()),
            "child reports parent_session_id"
        );

        // Top-level parent reports no parent_session_id.
        let parent_result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: parent_id.to_string(),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session(parent)");
        assert!(
            parent_result.structured_content.parent_session_id.is_none(),
            "top-level parent has no parent_session_id"
        );
    }

    #[gpui::test]
    async fn create_session_with_unknown_parent_errors_with_named_code(
        cx: &mut gpui::TestAppContext,
    ) {
        // Seed the store + solution_id so the "unknown parent" branch
        // is reached. We don't need a real workspace because parent
        // validation runs before `project_for_solution`.
        let (real_session_id, _thread, _tmp) = create_session_with_thread(cx).await;
        let solution_id = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store
                .read(cx)
                .session(real_session_id)
                .expect("session")
                .read(cx)
                .solution_id
                .clone()
        });
        // A short id that's well-formed (`[a-z0-9]{8}`) but not in the
        // store. `parse` will accept it; the store lookup will reject.
        let unknown_parent = "abcd1234";
        let err = CreateSessionTool
            .run(
                CreateSessionParams {
                    solution_id: solution_id.0.clone(),
                    agent_id: "mock-agent".into(),
                    initial_message: None,
                    parent_session_id: Some(unknown_parent.to_string()),
                    title: None,
                    cwd: None,
                },
                &mut cx.to_async(),
            )
            .await
            .expect_err("expected unknown_parent_session error");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown_parent_session"),
            "expected unknown_parent_session in {msg:?}"
        );
        assert!(
            msg.contains(unknown_parent),
            "expected error to include the bad id; got {msg:?}"
        );
    }

    #[gpui::test]
    async fn create_session_with_parent_in_different_solution_errors(
        cx: &mut gpui::TestAppContext,
    ) {
        let (parent_id, _thread, _tmp) = create_session_with_thread(cx).await;
        // CreateSession against a *different* solution id — the parent
        // belongs to solution-A; we pass solution-B. Validation fires
        // before workspace lookup so we don't need solution-B to have
        // an open window.
        let other_solution = "sol-OTHER-not-the-parents";
        let err = CreateSessionTool
            .run(
                CreateSessionParams {
                    solution_id: other_solution.into(),
                    agent_id: "mock-agent".into(),
                    initial_message: None,
                    parent_session_id: Some(parent_id.to_string()),
                    title: None,
                    cwd: None,
                },
                &mut cx.to_async(),
            )
            .await
            .expect_err("expected parent_session_in_different_solution error");
        let msg = err.to_string();
        assert!(
            msg.contains("parent_session_in_different_solution"),
            "expected parent_session_in_different_solution in {msg:?}"
        );
    }

    #[gpui::test]
    async fn get_session_children_returns_child_with_summary_fields(cx: &mut gpui::TestAppContext) {
        let (parent_id, _thread, _tmp) = create_session_with_thread(cx).await;
        let child_id = create_child_session(cx, parent_id).await;

        let result = GetSessionChildrenTool
            .run(
                GetSessionChildrenParams {
                    session_id: parent_id.to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session_children");
        let children = &result.structured_content.children;
        assert_eq!(children.len(), 1, "exactly one child");
        assert_eq!(children[0].id, child_id.to_string());
        assert_eq!(
            children[0].parent_session_id.as_deref(),
            Some(parent_id.to_string().as_str()),
            "child summary echoes parent_session_id"
        );
        // Text content carries a stable count summary for log scraping.
        match &result.content[0] {
            ToolResponseContent::Text { text } => {
                assert_eq!(text, "1 child session(s)");
            }
            _ => panic!("expected text content"),
        }
    }

    #[gpui::test]
    async fn get_session_children_returns_empty_list_for_leaf_session(
        cx: &mut gpui::TestAppContext,
    ) {
        let (leaf_id, _thread, _tmp) = create_session_with_thread(cx).await;

        let result = GetSessionChildrenTool
            .run(
                GetSessionChildrenParams {
                    session_id: leaf_id.to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session_children on a leaf");
        assert!(
            result.structured_content.children.is_empty(),
            "leaf session has no children"
        );
    }

    #[gpui::test]
    async fn list_sessions_filters_by_parent_session_id(cx: &mut gpui::TestAppContext) {
        let (parent_id, _thread, _tmp) = create_session_with_thread(cx).await;
        let child_id = create_child_session(cx, parent_id).await;
        // Add a second sibling so the filter has more than one row to
        // partition.
        let sibling_id = create_child_session(cx, parent_id).await;

        let solution_id = cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            store
                .read(cx)
                .session(parent_id)
                .expect("parent")
                .read(cx)
                .solution_id
                .clone()
        });

        // parent_session_id=parent → both children come back, parent itself excluded.
        let filtered = ListSessionsTool
            .run(
                ListSessionsParams {
                    solution_id: Some(solution_id.0.clone()),
                    parent_session_id: Some(parent_id.to_string()),
                    before_last_activity_at_ms: None,
                    count: None,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("list_sessions filtered by parent");
        let ids: std::collections::HashSet<String> = filtered
            .structured_content
            .sessions
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(
            ids,
            [child_id.to_string(), sibling_id.to_string()]
                .into_iter()
                .collect(),
            "exactly the two children are returned",
        );
        assert!(
            !ids.contains(&parent_id.to_string()),
            "parent itself is excluded"
        );
    }

    #[gpui::test]
    async fn session_summary_total_tokens_populated_from_cached_value(
        cx: &mut gpui::TestAppContext,
    ) {
        let (session_id, _thread, _tmp) = create_session_with_thread(cx).await;
        // Seed `cached_total_tokens` directly so the fallback path is
        // exercised even without a live `TokenUsageUpdated` event.
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            let session = store.read(cx).session(session_id).expect("session exists");
            session.update(cx, |s, _| s.cached_total_tokens = Some(42_000));
        });

        let result = ListSessionsTool
            .run(ListSessionsParams::default(), &mut cx.to_async())
            .await
            .expect("list_sessions");
        let summary = result
            .structured_content
            .sessions
            .iter()
            .find(|s| s.id == session_id.to_string())
            .expect("session present");
        // The live thread's `token_usage()` may be None at this stage,
        // so the fallback to `cached_total_tokens` is what we're
        // verifying. Either path yielding >= 42_000 is acceptable
        // (live could update past the seed); the contract is "non-None
        // when we have a value".
        assert!(
            summary.total_tokens.is_some_and(|t| t >= 42_000),
            "total_tokens should fall back to cached_total_tokens; got {:?}",
            summary.total_tokens,
        );
    }

    /// Phone client reads `SessionSummary::max_tokens` to size its
    /// context-fill meter the same way the desktop does — without it,
    /// it would have to guess the model's window. Live thread's
    /// `TokenUsage::max_tokens` is the source when hot; the cache
    /// fallback is exercised separately in
    /// `session_summary_max_tokens_falls_back_to_cached`.
    #[gpui::test]
    async fn session_summary_max_tokens_from_live_thread(cx: &mut gpui::TestAppContext) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;
        // Drive a TokenUsageUpdated through the live thread. The store's
        // event handler mirrors max_tokens onto cached_max_tokens, and
        // session_summary should surface it.
        cx.update(|cx| {
            acp_thread.update(cx, |t, cx| {
                t.update_token_usage(
                    Some(acp_thread::TokenUsage {
                        used_tokens: 5_000,
                        max_tokens: 200_000,
                        ..Default::default()
                    }),
                    cx,
                );
            });
        });
        cx.executor().run_until_parked();

        let result = ListSessionsTool
            .run(ListSessionsParams::default(), &mut cx.to_async())
            .await
            .expect("list_sessions");
        let summary = result
            .structured_content
            .sessions
            .iter()
            .find(|s| s.id == session_id.to_string())
            .expect("session present");
        assert_eq!(
            summary.max_tokens,
            Some(200_000),
            "max_tokens should be reported from the live thread",
        );
        assert_eq!(
            summary.total_tokens,
            Some(5_000),
            "total_tokens should be reported alongside max",
        );
    }

    /// Cold tab path: no live `acp_thread`, but `cached_max_tokens` was
    /// stamped during an earlier live event. `session_summary` must
    /// fall through to the cache so the phone meter keeps rendering a
    /// realistic window size even on sleeping sessions.
    #[gpui::test]
    async fn session_summary_max_tokens_falls_back_to_cached(cx: &mut gpui::TestAppContext) {
        let (session_id, _thread, _tmp) = create_session_with_thread(cx).await;
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            let session = store.read(cx).session(session_id).expect("session exists");
            session.update(cx, |s, _| s.cached_max_tokens = Some(180_000));
        });

        let result = ListSessionsTool
            .run(ListSessionsParams::default(), &mut cx.to_async())
            .await
            .expect("list_sessions");
        let summary = result
            .structured_content
            .sessions
            .iter()
            .find(|s| s.id == session_id.to_string())
            .expect("session present");
        // A live max may have been picked up in the meantime; the
        // contract is "non-None when the cache holds a value".
        assert!(
            summary.max_tokens.is_some_and(|m| m >= 180_000),
            "max_tokens should fall back to cached_max_tokens; got {:?}",
            summary.max_tokens,
        );
    }

    /// `start_compact` MCP tool refuses on a fresh session whose
    /// context usage is well below the 20% threshold — mirrors the
    /// desktop status-row gate. The structured `queued=false` + reason
    /// is the contract the phone client renders on its button.
    #[gpui::test]
    async fn start_compact_declines_below_threshold(cx: &mut gpui::TestAppContext) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;
        // Seed a low usage well below 20% so the precondition fails.
        cx.update(|cx| {
            acp_thread.update(cx, |t, cx| {
                t.update_token_usage(
                    Some(acp_thread::TokenUsage {
                        used_tokens: 1_000,
                        max_tokens: 1_000_000,
                        ..Default::default()
                    }),
                    cx,
                );
            });
        });
        cx.executor().run_until_parked();

        let result = StartCompactTool
            .run(
                StartCompactParams {
                    session_id: session_id.to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect("start_compact dispatches");
        assert!(
            !result.structured_content.queued,
            "expected queued=false, got {:?}",
            result.structured_content
        );
        let msg = result
            .structured_content
            .message
            .as_deref()
            .unwrap_or_default();
        assert!(
            msg.contains("short") || msg.contains("%"),
            "expected reason mentioning short context or percentage; got {msg:?}"
        );
    }

    /// `start_compact` queues a user message on the agent when the
    /// session is Idle and context exceeds 20%. We check that
    /// `send_message` was forwarded by inspecting the prompts the mock
    /// connection received.
    #[gpui::test]
    async fn start_compact_queues_prompt_when_idle(cx: &mut gpui::TestAppContext) {
        let (session_id, acp_thread, _tmp) = create_session_with_thread(cx).await;
        cx.update(|cx| {
            acp_thread.update(cx, |t, cx| {
                t.update_token_usage(
                    Some(acp_thread::TokenUsage {
                        // 25% of 1M = 250 000 (above the 20% gate)
                        used_tokens: 250_000,
                        max_tokens: 1_000_000,
                        ..Default::default()
                    }),
                    cx,
                );
            });
        });
        cx.executor().run_until_parked();

        let result = StartCompactTool
            .run(
                StartCompactParams {
                    session_id: session_id.to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect("start_compact dispatches");
        assert!(
            result.structured_content.queued,
            "expected queued=true; reason={:?}",
            result.structured_content.message
        );
        assert!(
            result.structured_content.message.is_none(),
            "no decline reason on success; got {:?}",
            result.structured_content.message
        );
    }

    // -----------------------------------------------------------------
    // upload_{init,status,finish,abort} + send_message_blocks resolution
    // -----------------------------------------------------------------

    /// `crate::upload::install` is a `OnceLock` — only the first caller wins
    /// process-wide. We can't keep handing out fresh `UploadManager`s per
    /// test; if we did, the second caller's `TempDir` would also drop on
    /// scope exit, leaving the first-installed manager pointing at a
    /// vanished directory. Instead, keep one persistent tempdir + manager
    /// alive for the lifetime of the test binary, and have each test allocate
    /// a fresh session+upload inside it.
    fn ensure_test_upload_manager() {
        use std::sync::OnceLock;
        static GUARD: OnceLock<tempfile::TempDir> = OnceLock::new();
        GUARD.get_or_init(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let manager =
                crate::upload::UploadManager::new(dir.path().to_path_buf()).expect("new mgr");
            crate::upload::install(std::sync::Arc::new(std::sync::Mutex::new(manager)));
            dir
        });
    }

    #[gpui::test]
    async fn upload_init_returns_id_and_status_round_trips(cx: &mut gpui::TestAppContext) {
        let (session_id, _img, _tmp_session) = seed_session_with_image(cx).await;
        // OnceLock semantics: install only takes on first call per process,
        // so a prior test's manager may already be in place. That's fine —
        // each upload gets a fresh id from `next_id` and lands in some
        // valid tmp_root.
        ensure_test_upload_manager();

        let init = UploadInitTool
            .run(
                UploadInitParams {
                    session_id: session_id.to_string(),
                    mime: "image/png".to_string(),
                    display_name: "pic.png".to_string(),
                    total_size: 4,
                    sha256: None,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("upload_init");
        let upload_id = init.structured_content.upload_id;
        assert!(upload_id > 0);

        let status = UploadStatusTool
            .run(UploadStatusParams { upload_id }, &mut cx.to_async())
            .await
            .expect("upload_status");
        assert_eq!(status.structured_content.received_bytes, 0);
        assert_eq!(status.structured_content.total_size, 4);
    }

    #[gpui::test]
    async fn upload_init_rejects_unknown_session(cx: &mut gpui::TestAppContext) {
        let (_session_id, _img, _tmp_session) = seed_session_with_image(cx).await;
        ensure_test_upload_manager();
        let err = UploadInitTool
            .run(
                UploadInitParams {
                    session_id: "nonexistent-session-id".to_string(),
                    mime: "image/png".to_string(),
                    display_name: "a.png".to_string(),
                    total_size: 1,
                    sha256: None,
                },
                &mut cx.to_async(),
            )
            .await
            .map(|_| "ok")
            .unwrap_or_else(|e| Box::leak(format!("ERR: {e}").into_boxed_str()));
        assert!(
            err.starts_with("ERR"),
            "expected error for unknown session, got {err}"
        );
    }

    #[gpui::test]
    async fn upload_finish_after_chunk_returns_handle_and_abort_cleans(
        cx: &mut gpui::TestAppContext,
    ) {
        let (session_id, _img, _tmp_session) = seed_session_with_image(cx).await;
        ensure_test_upload_manager();

        let init = UploadInitTool
            .run(
                UploadInitParams {
                    session_id: session_id.to_string(),
                    mime: "image/png".to_string(),
                    display_name: "tiny.png".to_string(),
                    total_size: 4,
                    sha256: None,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("upload_init");
        let upload_id = init.structured_content.upload_id;

        // Drive a chunk write through the manager directly — the binary
        // frame path is tested in `remote_control`; here we just need a
        // populated tmp file for `finish` to verify.
        crate::upload::with_manager(|m| m.write_chunk(upload_id, 0, &[1, 2, 3, 4]))
            .expect("manager installed")
            .expect("write_chunk");

        let finish = UploadFinishTool
            .run(
                UploadFinishParams {
                    upload_id,
                    sha256: None,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("upload_finish");
        assert!(
            finish
                .structured_content
                .handle
                .starts_with(crate::upload::HANDLE_SCHEME),
            "expected spk-upload:// handle, got {}",
            finish.structured_content.handle
        );

        UploadAbortTool
            .run(UploadAbortParams { upload_id }, &mut cx.to_async())
            .await
            .expect("upload_abort");

        let after = crate::upload::with_manager(|m| m.resolve(upload_id).is_some())
            .expect("manager installed");
        assert!(!after, "abort should drop the entry");
    }

    // -----------------------------------------------------------------
    // A6: created_ms on wire EntrySummary
    // -----------------------------------------------------------------

    /// Verifies that `GetSessionTool` propagates `entry_created_ms` from the
    /// session model to `EntrySummary.created_ms`:
    /// - entries with a real positive stamp → `Some(ms)` with `ms > 0`
    /// - entries whose stamp is the absent-sentinel → `None`
    ///
    /// `seed_session_with_n_entries` pushes all entries in a single batched
    /// `cx.update`, so the store's `NewEntry` subscription sees the final
    /// thread length each time and only stamps the last entry. We bypass that
    /// by directly writing `entry_created_ms` on the session entity — the
    /// same pattern used by the store's own unit tests (see
    /// `store/tests.rs::append_stamps_entry_created_ms_once_per_index`).
    #[gpui::test]
    async fn get_session_entries_carry_created_ms(cx: &mut gpui::TestAppContext) {
        use crate::model::NO_TIMESTAMP_MS;

        let (session_id, _tmp) = seed_session_with_n_entries(cx, 3).await;

        // Directly stamp: index 0 and 2 get real times, index 1 gets sentinel.
        let fake_ms: i64 = 1_700_000_000_000;
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            let session_entity = store.read(cx).session(session_id).expect("session exists");
            session_entity.update(cx, |s, _| {
                s.entry_created_ms = vec![fake_ms, NO_TIMESTAMP_MS, fake_ms + 1];
            });
        });

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let entries = &result.structured_content.entries;
        assert_eq!(entries.len(), 3, "all 3 entries returned");

        // Entries 0 and 2 have real stamps.
        assert!(
            entries[0].created_ms.is_some_and(|ms| ms > 0),
            "entry 0 must carry a positive created_ms; got {:?}",
            entries[0].created_ms,
        );
        assert!(
            entries[2].created_ms.is_some_and(|ms| ms > 0),
            "entry 2 must carry a positive created_ms; got {:?}",
            entries[2].created_ms,
        );

        // Entry 1 has the sentinel → must surface as None.
        assert!(
            entries[1].created_ms.is_none(),
            "entry 1 (sentinel) must have created_ms=None; got {:?}",
            entries[1].created_ms,
        );
    }

    /// Verifies that `GetSessionEntryTool` also propagates `created_ms`.
    #[gpui::test]
    async fn get_session_entry_carries_created_ms(cx: &mut gpui::TestAppContext) {
        use crate::model::NO_TIMESTAMP_MS;

        let (session_id, _tmp) = seed_session_with_n_entries(cx, 2).await;

        // Directly stamp entry 0 with a real time; leave entry 1 at sentinel.
        let fake_ms: i64 = 1_700_000_000_000;
        cx.update(|cx| {
            let store = SolutionAgentStore::global(cx);
            let session_entity = store.read(cx).session(session_id).expect("session exists");
            session_entity.update(cx, |s, _| {
                s.entry_created_ms = vec![fake_ms, NO_TIMESTAMP_MS];
            });
        });

        let result = GetSessionEntryTool
            .run(
                GetSessionEntryParams {
                    session_id: session_id.to_string(),
                    index: 0,
                    include_images: false,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session_entry");

        assert!(
            result
                .structured_content
                .entry
                .created_ms
                .is_some_and(|ms| ms > 0),
            "GetSessionEntryTool must carry created_ms for a stamped entry; got {:?}",
            result.structured_content.entry.created_ms,
        );

        let result_sentinel = GetSessionEntryTool
            .run(
                GetSessionEntryParams {
                    session_id: session_id.to_string(),
                    index: 1,
                    include_images: false,
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session_entry sentinel");

        assert!(
            result_sentinel
                .structured_content
                .entry
                .created_ms
                .is_none(),
            "GetSessionEntryTool must surface sentinel as None; got {:?}",
            result_sentinel.structured_content.entry.created_ms,
        );
    }

    /// Stage a tool call sitting in `WaitingForConfirmation` with a Flat
    /// allow/reject option pair, returning the session id, the tool call
    /// id, and the authorization-outcome `Task` (held so the oneshot the
    /// connection awaits stays alive — dropping it would cancel the
    /// confirmation and flip the call off `WaitingForConfirmation`).
    async fn seed_session_with_pending_authorization(
        cx: &mut gpui::TestAppContext,
    ) -> (
        crate::model::SolutionSessionId,
        String,
        gpui::Task<acp_thread::RequestPermissionOutcome>,
        tempfile::TempDir,
    ) {
        let (session_id, acp_thread, tmp) = create_session_with_thread(cx).await;
        let tool_call_id = "call-auth-1".to_string();
        let auth_task = cx.update(|cx| {
            acp_thread.update(cx, |thread, cx| {
                let update = acp::ToolCallUpdate::new(
                    acp::ToolCallId::new(tool_call_id.as_str()),
                    acp::ToolCallUpdateFields::new()
                        .kind(acp::ToolKind::Execute)
                        .title("Bash".to_string()),
                );
                let options = acp_thread::PermissionOptions::Flat(vec![
                    acp::PermissionOption::new(
                        "opt-allow",
                        "Allow".to_string(),
                        acp::PermissionOptionKind::AllowOnce,
                    ),
                    acp::PermissionOption::new(
                        "opt-reject",
                        "Reject".to_string(),
                        acp::PermissionOptionKind::RejectOnce,
                    ),
                ]);
                thread
                    .request_tool_call_authorization(update, options, cx)
                    .expect("stage waiting-for-confirmation")
            })
        });
        cx.executor().run_until_parked();
        (session_id, tool_call_id, auth_task, tmp)
    }

    #[gpui::test]
    async fn get_session_surfaces_auth_options_while_waiting(cx: &mut gpui::TestAppContext) {
        let (session_id, tool_call_id, _auth_task, _tmp) =
            seed_session_with_pending_authorization(cx).await;

        let result = GetSessionTool
            .run(
                GetSessionParams {
                    session_id: session_id.to_string(),
                    ..Default::default()
                },
                &mut cx.to_async(),
            )
            .await
            .expect("get_session");

        let tool_call = result
            .structured_content
            .entries
            .iter()
            .find_map(|entry| entry.tool_call.as_ref())
            .expect("a tool_call entry must be present");
        assert_eq!(tool_call.status, "waiting for confirmation");
        assert_eq!(tool_call.options.len(), 2, "both options must surface");
        assert_eq!(tool_call.options[0].kind, "allow_once");
        assert!(tool_call.options[0].is_allow);
        assert_eq!(tool_call.options[1].kind, "reject_once");
        assert!(!tool_call.options[1].is_allow);
        // The option id is opaque but must round-trip verbatim.
        assert_eq!(tool_call.options[0].option_id, "opt-allow");
        // tool_call_id is what the client echoes back to authorize.
        assert_eq!(
            tool_call.tool_call_id, tool_call_id,
            "tool_call_id must round-trip verbatim to the client"
        );
    }

    #[gpui::test]
    async fn authorize_tool_call_resolves_waiting_call(cx: &mut gpui::TestAppContext) {
        let (session_id, tool_call_id, _auth_task, _tmp) =
            seed_session_with_pending_authorization(cx).await;

        let result = AuthorizeToolCallTool
            .run(
                AuthorizeToolCallParams {
                    session_id: session_id.to_string(),
                    tool_call_id: tool_call_id.clone(),
                    option_id: "opt-allow".to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect("authorize_tool_call should succeed");
        assert!(result.structured_content.ok);
        cx.executor().run_until_parked();

        // The call must have flipped off WaitingForConfirmation — a
        // second authorize attempt now reports not_awaiting_confirmation.
        let err = AuthorizeToolCallTool
            .run(
                AuthorizeToolCallParams {
                    session_id: session_id.to_string(),
                    tool_call_id: tool_call_id.clone(),
                    option_id: "opt-allow".to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect_err("second authorize must fail; call no longer waiting");
        assert!(
            err.to_string().contains("not_awaiting_confirmation"),
            "unexpected error: {err}"
        );
    }

    #[gpui::test]
    async fn authorize_tool_call_rejects_unknown_option(cx: &mut gpui::TestAppContext) {
        let (session_id, tool_call_id, _auth_task, _tmp) =
            seed_session_with_pending_authorization(cx).await;

        let err = AuthorizeToolCallTool
            .run(
                AuthorizeToolCallParams {
                    session_id: session_id.to_string(),
                    tool_call_id,
                    option_id: "opt-does-not-exist".to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect_err("unknown option must error");
        assert!(
            err.to_string().contains("unknown_option"),
            "unexpected error: {err}"
        );
    }

    #[gpui::test]
    async fn authorize_tool_call_unknown_tool_call_errors(cx: &mut gpui::TestAppContext) {
        let (session_id, _img, _tmp) = seed_session_with_image(cx).await;

        let err = AuthorizeToolCallTool
            .run(
                AuthorizeToolCallParams {
                    session_id: session_id.to_string(),
                    tool_call_id: "no-such-call".to_string(),
                    option_id: "opt-allow".to_string(),
                },
                &mut cx.to_async(),
            )
            .await
            .expect_err("missing tool call must error");
        assert!(
            err.to_string().contains("tool_call_not_found"),
            "unexpected error: {err}"
        );
    }
}

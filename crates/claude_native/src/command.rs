//! Pure builder for the `claude` subprocess invocation: argv + env +
//! `--mcp-config` JSON. No spawning here — it produces a
//! [`std::process::Command`] the process layer runs.
//!
//! The argv reproduces what the `claude-agent-acp` node wrapper's SDK passes to
//! the binary (captured from a live `ps`): static stream-json flags +
//! `--mcp-config <json>` + the session arg. The system prompt is passed via
//! `--append-system-prompt` (the binary supports it directly — `claude --help`),
//! so no `initialize` control handshake is needed.

use std::path::PathBuf;
use std::process::Command;

use agent_client_protocol::schema as acp;

/// How the spawned `claude` process binds to a session.
pub enum SessionArg {
    /// Start a fresh session with this id (`--session-id`).
    New(String),
    /// Resume an existing session, replaying its transcript (`--resume`).
    Resume(String),
}

impl SessionArg {
    /// The session id this invocation binds to. `claude` honors the
    /// `--session-id`/`--resume` value we pass and echoes it back in its `init`
    /// message, so we can adopt it up front without waiting for `init`.
    pub fn session_id(&self) -> &str {
        match self {
            SessionArg::New(id) | SessionArg::Resume(id) => id,
        }
    }
}

/// Everything needed to build one `claude` invocation.
pub struct ClaudeCommandSpec {
    pub binary: PathBuf,
    pub work_dir: PathBuf,
    pub session: SessionArg,
    /// The value for `--mcp-config` (see [`mcp_config_json`]).
    pub mcp_servers_json: String,
    /// Appended to the default system prompt via `--append-system-prompt`.
    pub append_system_prompt: Option<String>,
    pub extra_env: Vec<(String, String)>,
}

impl ClaudeCommandSpec {
    pub fn to_std_command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&self.work_dir);
        cmd.envs(self.extra_env.iter().cloned());

        cmd.args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
            "--disallowedTools",
            "AskUserQuestion",
            "--tools",
            "default",
        ]);
        cmd.args(["--mcp-config", &self.mcp_servers_json]);
        cmd.args(["--setting-sources", "user,project,local"]);
        cmd.args(["--permission-mode", "bypassPermissions"]);
        cmd.args([
            "--allow-dangerously-skip-permissions",
            "--include-partial-messages",
            "--replay-user-messages",
        ]);
        match &self.session {
            SessionArg::New(id) => cmd.args(["--session-id", id]),
            SessionArg::Resume(id) => cmd.args(["--resume", id]),
        };
        if let Some(prompt) = &self.append_system_prompt {
            cmd.args(["--append-system-prompt", prompt]);
        }
        cmd
    }
}

/// Build the `--mcp-config` JSON from the ACP `mcp_servers` the editor would
/// have sent in the ACP `session/new` request. Only the stdio transport (the
/// `spk-editor` bridge) is mapped — the fork uses no http/sse MCP servers; any
/// such (or future `#[non_exhaustive]`) variants are skipped.
pub fn mcp_config_json(servers: &[acp::McpServer]) -> String {
    let mut map = serde_json::Map::new();
    for server in servers {
        if let acp::McpServer::Stdio(stdio) = server {
            let env: serde_json::Map<String, serde_json::Value> = stdio
                .env
                .iter()
                .map(|e| (e.name.clone(), serde_json::Value::String(e.value.clone())))
                .collect();
            map.insert(
                stdio.name.clone(),
                serde_json::json!({
                    "type": "stdio",
                    "command": stdio.command.to_string_lossy(),
                    "args": stdio.args,
                    "env": env,
                }),
            );
        }
    }
    serde_json::json!({ "mcpServers": serde_json::Value::Object(map) }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_resume_argv_with_flags() {
        let spec = ClaudeCommandSpec {
            binary: "claude".into(),
            work_dir: "/w".into(),
            session: SessionArg::Resume("sid".into()),
            mcp_servers_json: r#"{"mcpServers":{}}"#.into(),
            append_system_prompt: Some("SYS".into()),
            extra_env: vec![("K".into(), "V".into())],
        };
        let cmd = spec.to_std_command();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--input-format" && w[1] == "stream-json")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--output-format" && w[1] == "stream-json")
        );
        assert!(args.contains(&"--include-partial-messages".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "--resume" && w[1] == "sid"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--mcp-config" && w[1] == r#"{"mcpServers":{}}"#)
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--permission-mode" && w[1] == "bypassPermissions")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--append-system-prompt" && w[1] == "SYS")
        );
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/w")));
    }

    #[test]
    fn new_session_uses_session_id_not_resume() {
        let spec = ClaudeCommandSpec {
            binary: "claude".into(),
            work_dir: "/w".into(),
            session: SessionArg::New("uuid".into()),
            mcp_servers_json: "{}".into(),
            append_system_prompt: None,
            extra_env: vec![],
        };
        let args: Vec<String> = spec
            .to_std_command()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--session-id" && w[1] == "uuid")
        );
        assert!(!args.iter().any(|a| a == "--resume"));
        assert!(!args.iter().any(|a| a == "--append-system-prompt"));
    }

    #[test]
    fn maps_stdio_bridge_server() {
        let server = acp::McpServer::Stdio(
            acp::McpServerStdio::new("spk-editor", "/path/exe")
                .args(vec!["--nc".to_string(), "/tmp/mcp.sock".to_string()])
                .env(vec![acp::EnvVariable::new(
                    "SPK_EDITOR_MCP_BRIDGE_CAPS",
                    "write",
                )]),
        );
        let json: serde_json::Value = serde_json::from_str(&mcp_config_json(&[server])).unwrap();
        let s = &json["mcpServers"]["spk-editor"];
        assert_eq!(s["type"], "stdio");
        assert_eq!(s["command"], "/path/exe");
        assert_eq!(s["args"][0], "--nc");
        assert_eq!(s["args"][1], "/tmp/mcp.sock");
        assert_eq!(s["env"]["SPK_EDITOR_MCP_BRIDGE_CAPS"], "write");
    }
}

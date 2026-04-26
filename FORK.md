# Fork-Local Additions

This file is an index of everything **SPK Editor** adds on top of upstream [Zed](https://github.com/zed-industries/zed). It's the canonical place to look for "what's different here" before diving into code or merging upstream.

For fork **philosophy** (rebrand identifiers, what's disabled, build conventions, embedded MCP usage) see `.rules` / `CLAUDE.md` at the repo root.

## Fork-only crates

| Crate | Purpose | Notes |
|---|---|---|
| `crates/editor_mcp` | Embedded JSON-RPC MCP server (`~/.config/spk-editor/mcp.sock`) so an external agent can drive a live editor for E2E tests + autonomous work. Owns `SingleInstanceLock`, server bind, broadcast. | 50 builtin tools across `editor.*` / `windows.*` / `workspace.*` / `project.*` / `diagnostics.*` namespaces. Tools registered from each domain crate's `init`. |
| `crates/solutions` | Multi-project workspace abstraction. A **Solution** groups N catalog projects (each a remote git URL) into one editor window with all members mounted as worktrees. Persisted to `~/.config/spk-editor/solutions.json`; warm clone cache at `~/.cache/spk-editor/catalog/<sha256>/`. | Adds 11 `solutions.*` + 6 `catalog.*` MCP tools. Emits `solution_changed` events. |
| `crates/solutions_ui` | UI for Solutions: dock panel, picker, modals, title-bar segment, welcome integration, status bar. | Touches upstream `title_bar`, `welcome`, `app_menus` for integration points. |
| `crates/solution_agent` | N parallel Claude Code-style AI sessions scoped to a Solution, multiplexed onto a shared `claude` subprocess per `(solution_id, agent_id)` pair. First-class pane items + side-dock navigator + status bar widget. SQLite persistence at `~/.config/spk-editor/solution_agent/solution_agent.db`. | Adds 8 `solution_agent.*` MCP tools. Emits `agent_session_*` event kinds. Auth via subscription (`claude` CLI's own `~/.claude/`); no `ANTHROPIC_API_KEY`. |

## Disabled upstream subsystems

See `.rules` § "What's disabled" for the table. Brief: `auto_update`, `telemetry`, `collab` / `collab_ui`, sign-in, native cloud LLM (`CloudLanguageModelProvider`), `zeta` edit prediction, Sentry uploads, 41 CI workflows. Code stays in tree, init/dispatch/UI sites are commented out (`if false { … }` is fine) — keeps upstream-merge-friendliness.

## Touched upstream files (additive only — NEVER refactor for style)

| File | Change | Owning fork crate |
|---|---|---|
| `crates/zed/src/main.rs` | `editor_mcp::init`, `solutions::init`, `solutions_ui::init`, `solution_agent::init` calls inserted in startup flow. Various subsystem inits commented out. | mixed |
| `crates/zed/Cargo.toml` | Workspace deps on the four fork crates. | mixed |
| `crates/title_bar/src/title_bar.rs` | New segment for active Solution / project / branch. | `solutions_ui` |
| `crates/welcome/src/welcome.rs` | Recent Solutions section + buttons. | `solutions_ui` |
| `crates/paths/src/paths.rs` | `.zed` → `.spke` rename for per-worktree config dir. | rebrand |
| `assets/keymaps/default-*.json` | Default shortcuts for Solutions / sessions. | `solutions_ui` |
| `assets/settings/default.json` | Default `solutions.root`. | `solutions` |
| `crates/zed/Cargo.toml` `[[bin]]` | Binary name overridden to `spk-editor` (cargo crate `zed` unchanged). | rebrand |

Locked rebrand identifiers (display name, bundle ids, URL scheme, config dirs, etc.) — see `.rules` § "Locked rebrand identifiers". Changing any requires explicit approval — they're cross-referenced in spec docs.

## Key architectural decisions

These are decisions where the "obvious" approach was rejected for a non-obvious reason. Knowing the *why* helps you avoid undoing them.

### 1. `editor_mcp` is a sibling crate, not part of `agent`/`workspace`/`zed`

Why: `editor_mcp` would create a dep cycle if it lived inside `workspace` (workspace would depend on it for tool registration; it depends on workspace for the `Workspace` type). Sibling crate + `register_tool` from each domain crate's `init` breaks the cycle.

How to apply: when adding new MCP tools, register them from the crate that owns the underlying state — never from inside `editor_mcp` itself.

### 2. Solutions, not "Workspaces"

Why: upstream Zed already overloads `Workspace` (single-window) and `MultiWorkspace` (sidebar that switches between projects). The Catalog/Solution layer sits **above** all of that and adding another meaning for "workspace" would have been confusing. `Solution` was deliberately picked as a fresh term.

How to apply: never refer to a Solution as a "workspace" in user-facing strings or commit messages.

### 3. Subprocess pool keyed by `(SolutionId, AgentServerId)`, not per-session

Why: `AgentServer::connect()` sets cwd at subprocess spawn time. Multi-session-per-cwd is the normal ACP pattern (sessions multiplex over one subprocess via `acp::SessionId`). Spawning per-session would burn quota + memory; per-pair gives the right granularity for solution-scoped work.

How to apply: when adding a new agent (Codex, Gemini), it gets its own pool entry per Solution. Closing the last session in a `(solution, agent)` pair arms a 60-second debounced shutdown — see `crates/solution_agent/src/pool.rs::SHUTDOWN_DEBOUNCE`.

### 4. Solution sessions live PAST window close

Why: long-running agent tasks (e.g. "refactor across all members") shouldn't die because the user closes the window mid-task. The pool stays alive while the Solution exists in `SolutionStore`; closing the *Solution* is what kills its subprocesses. Notification on completion can re-open the window via `solutions.open`.

How to apply: don't tie session lifecycle to workspace events. The wire-up is in `crates/solution_agent/src/store.rs::on_solution_event`.

### 5. cwd = `solution.root` (always), no per-session member override

Why: the whole point of Solutions is cross-project work. Forcing per-member cwd would make cross-project tasks awkward. Per-member CLAUDE.md / settings still get loaded by the agent reading them on demand inside member subdirs. Trade-off: `git status` etc. need an explicit `cd member` first — agents handle this fine.

How to apply: `make_production_project_for_solution` (in `crates/solution_agent/src/pool.rs`) is currently a stub — Plan B (`create_session` takes `project: Entity<Project>` from the open workspace) is in use. If you wire production-side synthesis, keep cwd = solution.root.

### 6. MCP event kinds use `agent_session_*` prefix (not bare `session_*`)

Why: defensive namespacing. Other future subsystems might emit `session_*` events; the prefix makes the source unambiguous on the wire.

How to apply: any new event from `solution_agent` must follow the same prefix. Tests/consumers reference `agent_session_created`, `agent_session_state_changed`, etc.

### 7. Each session = first-class pane `Item`, not a tab inside a dock panel

Why: parallel-session UX needs split view ("session A on the left, code being changed on the right"). Tabs inside a dock panel can't sit next to a code buffer in the editor area without `detach`-style hacks. Pane items naturally support split, drag, alongside-code layouts — Zed is built around them.

How to apply: `SolutionSessionView` implements `workspace::Item`. The `SolutionSessionsNavigator` panel is a compact navigator only; the actual chat lives in panes.

### 8. AI auth via CLI subscription, NOT API keys

Why: respects the user's Claude subscription policy. The subprocess inherits `~/.claude/` via `$HOME` and authenticates itself; the editor never sees a token. `ANTHROPIC_API_KEY=""` is explicitly empty in the spawn env (set in `crates/agent_servers/src/custom.rs::CLAUDE_AGENT_ID` branch).

How to apply: never inject Anthropic credentials into a subprocess env. If a user wants BYOK, they configure that through Zed's normal language model providers — those are kept but not promoted in UI.

### 9. File drops on a session view insert plain `@path` text, not `MentionSet` entries

Why: upstream `agent_ui::MessageEditor` integrates with a heavy `MentionSet` machinery (mention rendering, project-path resolution, capability negotiation). Pulling that into `solution_agent` would couple us to `agent_ui` internals. v1 keeps the compose box a vanilla `editor::Editor` and the drop handler emits text like `@member-name/src/lib.rs`. The agent reads the path on its own via the `Read` tool — no editor-side resolution needed.

How to apply: if rich mentions or capability-aware path expansion become user requirements, integrate `agent_ui::message_editor::insert_mention_for_project_path` and bring `MentionSet` along — don't half-build a parallel mention layer in `solution_agent`. Plain text paste (`Ctrl+V` for clipboard text) works via `editor::Editor`'s native action; no patch needed.

### 10. Image paste: clipboard `gpui::Image` → base64 → `acp::ContentBlock::Image`

Why: Claude (and other ACP agents that declare the `image` prompt capability) accepts image content blocks alongside text. We want native paste UX without dragging in `MentionSet`. The compose box registers a `capture_action(Paste)` handler that runs **before** the editor's default text-paste, inspects the clipboard, and:
- if the first entry is text → returns without consuming (action falls through to the editor's text paste)
- if the first entry is an image → encodes via `base64::engine::general_purpose::STANDARD`, stashes a `PendingImage` on the view, drops a `[image #N]` placeholder into the buffer, and calls `cx.stop_propagation()`

On submit, `pending_images` are converted to `acp::ContentBlock::Image(ImageContent::new(base64, mime))` and combined with the text block via `SolutionAgentStore::send_message_blocks(...)` (the new structured-content API alongside the legacy text-only `send_message`).

How to apply: this is a deliberate parallel implementation of upstream's `paste_images_as_context`, NOT a reuse. The upstream version requires `MentionSet`, image-upload state, capability checks — all coupled to `agent_ui`. Our path stays self-contained inside `solution_agent`. If the agent doesn't support images (capability missing), the call still goes out — the agent rejects with an error that surfaces to the user as a normal `Errored` state. Adding capability negotiation pre-flight is a follow-up.

## Where specs and plans live

`docs/superpowers/{specs,plans}/` is in `.gitignore` — these are personal working notes, not committed. Each major fork feature has a design spec + step-by-step implementation plan there. They're append-only history; the canonical state of the code lives in code + this file + `.rules`.

If you're picking up a feature mid-stream and the specs are missing locally, recover from git history:

```sh
git log --oneline --all --diff-filter=A -- 'docs/superpowers/specs/*'
```

(Will show empty if no specs were ever committed — that's the steady state for this fork.)

## Memory of subagent dispatches

Some sessions used `superpowers:subagent-driven-development` to land features task-by-task. Those agents make local-pragmatic deviations from plans. Notable plan-vs-code deviations worth knowing:

- `solution_agent::SolutionSession.acp_thread` is `Option<Entity<AcpThread>>`, not `Entity<AcpThread>` — reflects the real lazy-construction lifecycle.
- `solution_agent::SolutionAgentStore::create_session` takes `project: Entity<Project>` (Plan B). Synthetic single-worktree project per session was rejected as too coupled to `Arc<Client>` / `UserStore` / etc.
- `solution_agent` registers `AgentServer` instances via `store.register_agent_server(id, Rc<dyn AgentServer>)`, not via global `AgentServerStore::get_external_agent`. The wire-up call lives in `solution_agent::init`.
- MockAgentServer in `solution_agent::test_support` uses `unsafe impl Send` because the trait requires Send but holds non-Send test state behind a Mutex. Test-only.

## Updating this file

Add to FORK.md when:
- A new fork-local crate is added.
- A new upstream file is touched (additive change, not a style refactor).
- A non-obvious architectural decision is made — record the *why* before it gets lost.

Don't add:
- Per-crate module layout / data flow / type catalogs — those go stale fast and the agent can read the code. Rules are "traps to avoid", not "maps to follow".
- Long-term TODOs — use issues for those.
- Status updates — the git log is canonical.

# Fork-Local Additions

This file is an index of everything **SPK Editor** adds on top of upstream [Zed](https://github.com/zed-industries/zed). It's the canonical place to look for "what's different here" before diving into code or merging upstream.

For fork **philosophy** (rebrand identifiers, what's disabled, build conventions, embedded MCP usage) see `.rules` / `CLAUDE.md` at the repo root.

## Fork-only crates

| Crate | Purpose | Notes |
|---|---|---|
| `crates/editor_mcp` | Embedded JSON-RPC MCP server (`~/.config/spk-editor/mcp.sock`) so an external agent can drive a live editor for E2E tests + autonomous work. Owns `SingleInstanceLock`, server bind, broadcast. | 50 builtin tools across `editor.*` / `windows.*` / `workspace.*` / `project.*` / `diagnostics.*` namespaces. Tools registered from each domain crate's `init`. |
| `crates/solutions` | Multi-project workspace abstraction. A **Solution** groups N catalog projects (each a remote git URL) into one editor window with all members mounted as worktrees. Persisted to SQLite via `SolutionsDb` (one-time migration from legacy `solutions.json`); warm clone cache at `~/.cache/spk-editor/catalog/<sha256>/`. | Adds 11 `solutions.*` + 6 `catalog.*` MCP tools. Emits `solution_changed` events. |
| `crates/solutions_ui` | UI for Solutions: dock panel, picker, modals, title-bar segment, welcome integration, status bar. | Touches upstream `title_bar`, `welcome`, `app_menus` for integration points. |
| `crates/solution_agent` | N parallel Claude Code-style AI sessions scoped to a Solution, multiplexed onto a shared `claude` subprocess per `(solution_id, agent_id)` pair. First-class pane items + side-dock navigator + status bar widget. SQLite persistence at `~/.config/spk-editor/solution_agent/solution_agent.db`. | Adds 8 `solution_agent.*` MCP tools. Emits `agent_session_*` event kinds. Auth via subscription (`claude` CLI's own `~/.claude/`); no `ANTHROPIC_API_KEY`. |

## Disabled upstream subsystems

See `.rules` § "What's disabled" for the table. Brief: `auto_update`, `telemetry`, `collab` / `collab_ui`, sign-in, native cloud LLM (`CloudLanguageModelProvider`), `zeta` edit prediction, Sentry uploads, 41 CI workflows, **`agent_ui::AgentPanel` dock panel + Welcome `render_agent_card`** (the fork's AI is `solution_agent`; upstream's panel is a parallel unconfigured surface). Code stays in tree, init/dispatch/UI sites are commented out (`if false { … }` is fine) — re-enabling stays a one-line change and we haven't audited what other crates implicitly depend on these subsystems' types or globals.

## Touched upstream files

The fork no longer plans periodic `git merge upstream/main`, so this table is **not** a "minimize merge cost" scoreboard. It exists for a narrower purpose: it's the list of files where a future cherry-pick from upstream will *not* apply cleanly and has to be reconciled by hand. Once a file appears here, further edits inside it don't need a new row — the cherry-pick was already going to be manual. New rows only when an upstream file gets its **first** local touch.

Within these files, refactor / rename / cleanup is fine — diff-minimality buys nothing once the file is on the manual-reconcile list. Files **not** listed here are still untouched core; the "don't refactor for style" rule applies there to keep cherry-picks cheap.

| File | Change | Owning fork crate |
|---|---|---|
| `crates/zed/src/main.rs` | `editor_mcp::init`, `solutions::init`, `solutions_ui::init`, `solution_agent::init` calls inserted in startup flow. Various subsystem inits commented out. | mixed |
| `crates/zed/src/zed.rs` | `initialize_agent_panel` call commented out in `futures::join!` (fn kept under `#[allow(dead_code)]` for one-line re-enable). | `solution_agent` |
| `crates/zed/Cargo.toml` | Workspace deps on the four fork crates. | mixed |
| `crates/title_bar/src/title_bar.rs` | New segment for active Solution / project / branch. `render_restricted_mode` call site disabled (function kept under `#[allow(dead_code)]`) — see decision 13. | `solutions_ui` / `solutions` |
| `crates/acp_thread/src/connection.rs` | Adds `AgentConnection::new_session_with_meta` extension point (default impl drops the meta + falls back to `new_session`) so adapters can act on protocol-level `_meta` keys (e.g. `claude-agent-acp` reads `_meta.systemPrompt` to seed the session prompt). | `solution_agent` |
| `crates/agent_servers/src/acp.rs` | (1) `mcp_servers_for_project` prepends a fork-local `acp::McpServer::Stdio` entry pointing at `<current_exe> --nc <editor_mcp.socket_path>` so spawned ACP subagents see the editor's embedded MCP tools (helper: `spk_editor_mcp_bridge_server`) — see decision 14. (2) `AcpConnection::new_session_with_meta` impl splices `extra_meta` into `NewSessionRequest::meta`. | `editor_mcp` / `solution_agent` |
| `crates/agent_servers/Cargo.toml` | New dep on `editor_mcp` for the socket path. | `editor_mcp` / `solution_agent` |
| `crates/gpui/src/elements/list.rs` | `ListState::measure_last(N)` chunked tail prefetch (plus `MEASURE_LAST_DEFAULT_BATCH` / `LOOKAHEAD` / `EAGER_THRESHOLD` knobs) so virtualized lists can pre-warm their most-recent items on the first layout pass without paying the full-list measurement cost. Used by `solution_agent`'s conversation list to keep scroll-up off long resumed conversations from triggering a height-discovery cascade. | `solution_agent` |
| `crates/workspace/src/workspace.rs` | `Workspace::swap_worktrees_to(target_paths)` delta worktree reconciliation used by the in-place Solution switch (decision 16). Drops worktrees not in the set, adds missing ones, preserves overlapping `WorktreeId`s so LSP / panels / caches don't churn. | `solutions_ui` / `solutions` |
| `crates/welcome/src/welcome.rs` | Recent Solutions section + buttons. | `solutions_ui` |
| `crates/workspace/src/welcome.rs` | `render_agent_card` gated off via `false &&` — fork uses `solution_agent`, not upstream agent panel. | `solution_agent` |
| `crates/paths/src/paths.rs` | `.zed` → `.spke` rename for per-worktree config dir. | rebrand |
| `assets/keymaps/default-*.json` | Default shortcuts for Solutions / sessions. | `solutions_ui` |
| `assets/settings/default.json` | Default `solutions.root`; default `icon_theme: "Material Icon Theme"` + auto-install of the matching extension (colored project tree, IDEA-like, vs upstream's monochrome `Zed (Default)`). | `solutions` / rebrand |
| `crates/zed/Cargo.toml` `[[bin]]` | Binary name overridden to `spk-editor` (cargo crate `zed` unchanged). | rebrand |
| `.cargo/config.toml` | `[target.x86_64-unknown-linux-gnu]` block forcing `-fuse-ld=mold`. See decision 15. | build |

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

### 7. Sessions live INSIDE the right-dock chat panel, not as workspace pane Items (reversed)

Why: an earlier draft tried "sessions as pane items + side-panel navigator". In practice the navigator just duplicated the editor's tab strip (same uuid in two places) and the chat ended up competing with code for the main editor area without users actually wanting that split — the "session A next to code on the right" use case is rare while "where is my chat?" was constant. The flagship-AI-editor pattern (Cursor / Cody / Copilot Chat / upstream Zed AgentPanel) puts chat in a dedicated docked panel with its own internal tab strip, and that is what users expect.

How to apply: `SolutionSessionsNavigator` (in `crates/solution_agent/src/navigator.rs`) owns the open-sessions list, tab strip, status row, and "+ New <Adapter> Session" footer. `SolutionSessionView` is a plain `gpui::Render` (no `workspace::Item` impl) and is hosted as a child of the navigator. Do NOT add `Item`/`add_item_to_active_pane` for sessions — re-introducing the duplication.

### 8. AI auth via CLI subscription, NOT API keys

Why: respects the user's Claude subscription policy. The subprocess inherits `~/.claude/` via `$HOME` and authenticates itself; the editor never sees a token. `ANTHROPIC_API_KEY=""` is explicitly empty in the spawn env (set in `crates/agent_servers/src/custom.rs::CLAUDE_AGENT_ID` branch).

How to apply: never inject Anthropic credentials into a subprocess env. If a user wants BYOK, they configure that through Zed's normal language model providers — those are kept but not promoted in UI.

### 9. File drops on a session view insert plain `@path` text, not `MentionSet` entries

Why: upstream `agent_ui::MessageEditor` integrates with a heavy `MentionSet` machinery (mention rendering, project-path resolution, capability negotiation). Pulling that into `solution_agent` would couple us to `agent_ui` internals. v1 keeps the compose box a vanilla `editor::Editor` and the drop handler emits text like `@member-name/src/lib.rs`. The agent reads the path on its own via the `Read` tool — no editor-side resolution needed.

How to apply: if rich mentions or capability-aware path expansion become user requirements, integrate `agent_ui::message_editor::insert_mention_for_project_path` and bring `MentionSet` along — don't half-build a parallel mention layer in `solution_agent`. Plain text paste (`Ctrl+V` for clipboard text) works via `editor::Editor`'s native action; no patch needed.

### 10. Welcome page is the launcher; `restore_on_startup = "none"` by default

Why: the editor is built around Solutions. Restoring "the last workspace" pins the user to whatever they happened to close last (often a one-off `/tmp` or a single member subfolder), hiding the rest of their solutions. The fork's startup story is "open the editor → see all your solutions → pick one (or create one)". Welcome is always shown; the Solutions section in `solutions_ui::welcome` lists every solution (opened-recent first, never-opened in store order) and always shows a `Create new solution` button.

How to apply: the default lives in `assets/settings/default.json` (`"restore_on_startup": "none"`). Users can override it in their own settings if they want upstream behavior. The Welcome section renderer in `crates/solutions_ui/src/welcome.rs::render_section` is the single place that defines what the launcher shows — keep it as the only fork-local Welcome section unless there's a strong reason for more.

### 11. Single-instance handoff with no args focuses the existing window (best-effort on Linux)

Why: when the user runs `spk-editor` a second time without path args while another instance is alive, the new process should NOT silently exit while the existing window stays buried. The handoff endpoint (`workspace::mcp::handle_cli_args`) now picks the first existing window and dispatches `Window::activate_window` (X11 `_NET_ACTIVE_WINDOW` ClientMessage).

How to apply: this is best-effort on Linux. Most window managers implement focus-stealing prevention — the WM will only honor an `_NET_ACTIVE_WINDOW` request from a process with a recent user-interaction timestamp. The new `spk-editor` invocation has no such timestamp, so the WM may downgrade the request to a taskbar-flash or ignore it entirely (mutter / KWin do this aggressively; lighter WMs like i3 / sway honor it). `App::activate(...)` is a documented no-op on the upstream Linux backend (`activate is not implemented on Linux, ignoring the call`). User-facing options: disable focus-stealing prevention in the WM, OR launch with an explicit path which goes through `open_paths` and forces a new window.

### 12. Image paste: clipboard `gpui::Image` → base64 → `acp::ContentBlock::Image`

Why: Claude (and other ACP agents that declare the `image` prompt capability) accepts image content blocks alongside text. We want native paste UX without dragging in `MentionSet`. The compose box registers a `capture_action(Paste)` handler that runs **before** the editor's default text-paste, inspects the clipboard, and:
- if the first entry is text → returns without consuming (action falls through to the editor's text paste)
- if the first entry is an image → encodes via `base64::engine::general_purpose::STANDARD`, stashes a `PendingImage` on the view, drops a `[image #N]` placeholder into the buffer, and calls `cx.stop_propagation()`

On submit, `pending_images` are converted to `acp::ContentBlock::Image(ImageContent::new(base64, mime))` and combined with the text block via `SolutionAgentStore::send_message_blocks(...)` (the new structured-content API alongside the legacy text-only `send_message`).

How to apply: this is a deliberate parallel implementation of upstream's `paste_images_as_context`, NOT a reuse. The upstream version requires `MentionSet`, image-upload state, capability checks — all coupled to `agent_ui`. Our path stays self-contained inside `solution_agent`. If the agent doesn't support images (capability missing), the call still goes out — the agent rejects with an error that surfaces to the user as a normal `Errored` state. Adding capability negotiation pre-flight is a follow-up.

### 14. Editor's embedded MCP socket is bridged into spawned ACP subagents via `<exe> --nc <socket>`

Why: `editor_mcp` exposes 58+ tools (`solution_agent.*`, `solutions.*`, `editor.*`, `windows.*`, `workspace.*`, `project.*`, `diagnostics.*`) over a Unix socket at `~/.config/spk-editor/mcp.sock`. Upstream's `agent_servers::acp::mcp_servers_for_project` only feeds claude-acp / codex-acp / gemini the MCP servers configured in user settings — so the embedded server is invisible to those subagents. ACP's `McpServer` enum supports `Stdio` and `Http` transports, but not Unix sockets. The fork already ships an `--nc <socket>` mode in the editor binary (`crates/nc/src/nc.rs`) that proxies stdin/stdout to a Unix socket — same pattern upstream uses for the `--askpass` SSH flow. So the bridge is: a fork-local entry in `mcp_servers_for_project` that runs `<current_exe> --nc <socket_path>` as the stdio command. Spawned subagents speak JSON-RPC stdio to that subprocess, which forwards to the editor socket.

How to apply: the entry is named `spk-editor`, gated on the socket file existing (so headless test runs that never started the server skip it cleanly) and on `current_exe()` resolving. Implementation in `crates/agent_servers/src/acp.rs::spk_editor_mcp_bridge_server`. **Security note:** any tool exposed via `editor_mcp` is now reachable from inside ACP subagents — including potentially destructive ones like `windows.close` or `editor.handle_cli_args`. Audit the tool surface before adding new tools that should NOT be subagent-accessible (or gate them behind a separate registry).

### 13. Catalog membership IS the trust signal — Restricted Mode badge hidden

Why: upstream Zed's worktree-trust UX prompts before starting a language server in any unfamiliar directory and surfaces a "Restricted Mode" badge in the title bar. The fork's mental model is different: a project is in a Solution because the user explicitly added its remote URL to the catalog AND chose to clone it. That decision IS the trust signal — re-prompting at LSP-start time and parking a yellow badge in the title bar is noise, not safety.

How to apply: `crates/solutions/src/auto_trust.rs` observes new workspaces and trusts every `solution.root` whose path covers any worktree of the project (uses `PathTrust::AbsPath`, so all current and future member subdirs inherit trust via the path-hierarchy in `crates/project/src/trusted_worktrees.rs`). The title-bar render call in `crates/title_bar/src/title_bar.rs` is commented out; the function itself is kept under `#[allow(dead_code)]` for upstream-merge friendliness. Trust still works as upstream for ad-hoc opens (File → Open Folder of a non-Solution path) — they go through the original prompt path.

### 16. Solution switch is in-place — same `Workspace`, swap worktrees, replay tabs

Why: switching the active Solution used to allocate a fresh `Workspace`
via `OpenMode::Add` + `MultiWorkspace::activate`, which retained the
old workspace but visibly tore down all panels in the active one and
re-created them from defaults — losing dock widths, scroll positions
in `ProjectPanel`/`OutlinePanel`, expanded items, panel-specific UI
state — every single switch. The retained-workspace mechanism kept
the *previous* Solution's state alive in memory but didn't help the
in-flight switch UX, which is what the user actually feels several
times an hour. Recreate-on-switch was a holdover from upstream's
`git_ui::worktree_service` flow; for Solutions the cost was paid an
order of magnitude more often.

How to apply: use `solutions_ui::switch_active_solution_in_place`
(orchestrator) when the user wants to swap solution scope without
window churn. The orchestrator (1) snapshots the current Solution's
open editor tabs into `SolutionStore::tab_snapshots`, (2) bumps
`touch_last_opened` (which fires `SolutionStoreEvent::ActiveSolutionChanged(target)`),
(3) reconciles worktrees inside the existing `Project` via
`Workspace::swap_worktrees_to`, and (4) replays the target Solution's
saved tab snapshot. Upstream panels react to `WorktreeAdded`/`Removed`
automatically; fork panels (`SolutionTabStrip`,
`SolutionSessionsNavigator`) listen to `ActiveSolutionChanged` —
*don't* assume your panel will be re-`new`'d on switch. The
`OpenIntent::SameWindow` path in `solutions_ui::open::open_solution`
goes through this orchestrator; `OpenIntent::NewWindow` and
already-open-in-other-window focus paths still use the
`MultiWorkspace::activate` machinery (they're inherently per-window).

Tab restoration is best-effort: snapshot-save failures don't abort the
switch (the user wants to *get to* the new Solution; one lost tab
list is recoverable). Snapshots are runtime-only — losing them across
an editor restart is acceptable, persistence would mean keeping the
map in sync with potentially-stale paths.

### 15. mold mandatory for x86_64-linux builds

Why: system `ld` is the wall-clock bottleneck of `release-fast` incremental rebuilds (multi-GB peak RAM, several seconds per re-link on Zed's link graph). mold is ~5-10× faster and uses a fraction of the RAM. The existing aarch64 entry pins `lld` out of *necessity* (libwebrtc.a fails to link otherwise); the x86_64 entry pins `mold` for *perf* but elevated to required because silent fallback to `ld` is a worse failure mode than a one-line apt install. Mirrors the same "you must install a fast linker before first build" contract.

How to apply: contributors install `mold` (`apt install mold` on Debian/Ubuntu, `brew install mold` on macOS-with-Linux-cross, prebuilt binaries on the [mold releases page](https://github.com/rui314/mold/releases) elsewhere). The pinned block lives in `.cargo/config.toml` — never delete it during an upstream merge (Zed upstream may add their own `[target.x86_64-unknown-linux-gnu]` entry for some unrelated rustflag; merge by combining flags, don't drop ours). To verify mold is active on a build: `cargo build --profile release-fast -v 2>&1 | grep -m1 fuse-ld` should show `-fuse-ld=mold`.

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

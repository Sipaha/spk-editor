# Fork-Local Additions

This file is an index of everything **SPK Editor** adds on top of upstream [Zed](https://github.com/zed-industries/zed). It's the canonical place to look for "what's different here" before diving into code or merging upstream.

For fork **philosophy** (rebrand identifiers, what's disabled, build conventions, embedded MCP usage) see `.rules` / `CLAUDE.md` at the repo root.

## Fork-only crates

| Crate | Purpose | Notes |
|---|---|---|
| `crates/editor_mcp` | Embedded JSON-RPC MCP server (`~/.config/spk-editor/mcp.sock`) so an external agent can drive a live editor for E2E tests + autonomous work. Owns `SingleInstanceLock`, server bind, broadcast. | 50 builtin tools across `editor.*` / `windows.*` / `workspace.*` / `project.*` / `diagnostics.*` namespaces. Tools registered from each domain crate's `init`. |
| `crates/solutions` | Multi-project workspace abstraction. A **Solution** groups N catalog projects (each a remote git URL) into one editor window with all members mounted as worktrees. Persisted to SQLite via `SolutionsDb` (one-time migration from legacy `solutions.json`); warm clone cache at `~/.cache/spk-editor/catalog/<sha256>/`. | Adds 11 `solutions.*` + 6 `catalog.*` MCP tools. Emits `solution_changed` events. |
| `crates/solutions_ui` | UI for Solutions: title-bar tab strip, picker, modals, welcome integration, status bar, plus the per-panel `ActiveProjectSelector` element hosted by `project_panel` and `git_panel`. | Touches upstream `title_bar`, `welcome`, `app_menus`, `project_panel`, `git_ui` for integration points. |
| `crates/solution_agent` | N parallel Claude Code-style AI sessions scoped to a Solution, multiplexed onto a shared `claude` subprocess per `(solution_id, agent_id)` pair. First-class pane items + side-dock navigator + status bar widget. SQLite persistence at `~/.config/spk-editor/solution_agent/solution_agent.db`. | Adds 8 `solution_agent.*` MCP tools. Emits `agent_session_*` event kinds. Auth via subscription (`claude` CLI's own `~/.claude/`); no `ANTHROPIC_API_KEY`. |
| `crates/git_conflict_ui` | Standalone 3-way merge conflict resolver. Independent crate because of the size of the resolver view and isolation of the UI surface, not for merge-friendliness. | Skeleton — full implementation lands in S-CFL (`docs/superpowers/plans/git-panel-plan.md`). Will own `editor.git.{list_conflicts,resolve_conflict,mark_resolved,continue_merge,abort_merge}` MCP tools. |
| `crates/solution_git` | Solution-aware git operations: aggregated log, status dashboard, solution-wide commit/push, cross-member cherry-pick, branch protection. Built on top of `solution`, `git_ui`, and existing `git` crates. Per P-9 (inversion of control), depends *downward* on `git_ui` and registers trait providers via `git_ui::providers::*` at `init()`. | Skeleton — full implementation lands across milestone M4 of the git-panel plan (S-SOL-LOG / S-SOL-DSH / S-SOL-CMT / S-SOL-PSH / S-SOL-CHP / S-SOL-PRT). Will own the `solution.git.*` MCP namespace. |
| `crates/run_config` | Headless Run Configurations model: `RunConfiguration` + a `RunConfigProvider` registry (extensible config *types*), persistence to `.spke/run-configurations.json` (per worktree, watched/hot-reloaded) + a global `~/.config/spk-editor/run-configurations.json`, built-in `shell` / `debug` / `task-ref` providers, and the `run_config.*` MCP tool namespace. Translates a config into a `task::SpawnInTerminal` / `task::DebugScenario` at launch time — sits on top of the `task`/`dap` engines, doesn't replace them. | Adds 6 `run_config.*` MCP tools (`list`, `create`, `delete`, `select`, `run`, `stop`). Emits `RunConfigStoreEvent`. `run` / `stop` / `select` are no-ops until a `RunController` window is live. |
| `crates/run_config_ui` | UI for Run Configurations: the compact run-config widget rendered right-aligned in the title bar (config dropdown + Run / Debug / Stop — IDEA-style), the Edit Configurations modal (schema-driven provider forms, before-launch = Save All, executors, project/global storage), the status-bar run indicator, and the per-`Workspace` `RunController` (launches terminal tasks via `TerminalPanel::spawn_task` — falls back to `Workspace::spawn_in_terminal` when no terminal panel — and debug runs via `start_debug_session`; tracks active runs by terminal handle / `dap` session id so Stop actually kills them; publishes the running set + a command sink for MCP). | Touches upstream `workspace`, `zed/main.rs`, `zed/app_menus.rs`, `settings_content`, keymaps. |
| `crates/remote_control` | Headless model + JSON persistence + (R-2) network listener + (R-4) `remote.*` proxy to the embedded `editor_mcp` Unix socket. State model: `RemoteControlSettings { server_address, server_port, enabled, clients }` persisted to `~/.config/spk-editor/remote-control.json` (live-watched + atomic writes; FS-watcher echo squelched via `self_write_echoes` to prevent round-trip self-clobber). Generates 32-byte client secrets via `OsRng` + base64. R-2 adds the listener (`set_enabled(true)` → load-or-generate self-signed TLS cert via `rcgen` → bind `0.0.0.0:server_port` → TLS 1.3 → WebSocket upgrade → 16-byte HMAC-SHA256 challenge). R-4 swaps the R-2 `MinimalDispatcher` stub for `ProxyDispatcher`: each WS connection lazily opens a `UnixMcpProxy` to the in-process `editor_mcp::socket_path()`, translates `remote.X.Y` → upstream `tools/call { name: "X.Y", arguments }` per the allow-list in `allow_list::translate`, and fans `editor/notification` frames out as `remote/notification` (block-list filter: only `agent_session_*` kinds reach the WS). Per ADR-0003 + R-4 plan-doc. | Emits `RemoteControlStoreEvent::Changed`. Tokio runtime via `gpui_tokio::Tokio::spawn_result`. Watch-channel live client-list propagation. `cert_fingerprint()` / `bound_addr()` accessors expose listener state for the R-3 QR generator. Per-WS proxy lifecycle: connection-scoped `UnixMcpProxy` holds a per-id `oneshot` map for response demux and a bounded mpsc (256, drop-newest on overflow) for notifications; reader task aborts on `Drop` → upstream socket closes → embedded server cleans up subscriptions. `MinimalDispatcher` retained for unit/integration tests that don't want a live MCP socket. |
| `crates/remote_control_ui` | UI for Remote Control: right-aligned status-bar entry (`RemoteControlStatusItem` — colored dot + "Remote Control") and the workspace modal (`RemoteControlModal`: address row + Detect button (HTTP GET to `https://ifconfig.me`), port row, enable/disable toggle, authorized-client list with secret-prefix + "Show QR" stub toast, inline add-client name input). | "Show QR" emits a `TODO R-1.5: QR rendering` toast for now; the address row's Detect uses `cx.http_client()` and writes the trimmed body straight into the address input + store. |

## Disabled upstream subsystems

See `.rules` § "What's disabled" for the table. Brief: `auto_update`, `telemetry`, `collab` / `collab_ui`, sign-in, native cloud LLM (`CloudLanguageModelProvider`), `zeta` edit prediction, Sentry uploads, 41 CI workflows, **`agent_ui::AgentPanel` dock panel + Welcome `render_agent_card`** (the fork's AI is `solution_agent`; upstream's panel is a parallel unconfigured surface). Code stays in tree, init/dispatch/UI sites are commented out (`if false { … }` is fine) — re-enabling stays a one-line change and we haven't audited what other crates implicitly depend on these subsystems' types or globals.

## Notable upstream file modifications

This fork no longer constrains itself to additive-only modifications of upstream files. When refactoring or restructuring upstream code yields a meaningfully better result, it is done — the fork accepts the merge-conflict cost as the price of clean local code. The table below is informational, not normative: it records significant divergences from upstream, but is not a contract that prevents further changes.

**Working principles for upstream modifications:**

1. **Locality over indirection.** Extensions live where the thing they extend lives. Don't create wrapper crates solely to keep upstream files untouched.
2. **Refactor when it pays.** If splitting an upstream file into submodules, renaming types, or restructuring layout meaningfully improves the local code, do it. Document significant divergences in the table below.
3. **Identifiers stay.** Crate names, module paths, and public type names follow upstream unless there's a strong reason — they're cheap to preserve and reduce friction in cross-references.
4. **Prefer file-level rewrites over scattered patches.** If five separate hunks across a file each conflict with upstream, a single full-file rewrite is often easier to maintain than five conflicting patches.

| File | Change | Owning fork crate |
|---|---|---|
| `crates/zed/src/main.rs` | `editor_mcp::init`, `solutions::init`, `solutions_ui::init`, `solution_agent::init`, `run_config::init`, `run_config_ui::init` calls inserted in startup flow. Various subsystem inits commented out. Adds `--headless` CLI flag forwarded to `gpui_platform::current_platform(headless)` (ADR-0002 native headless platform). | mixed |
| `crates/zed/src/zed.rs` | `initialize_agent_panel` call commented out in `futures::join!` (fn kept under `#[allow(dead_code)]` for one-line re-enable). | `solution_agent` |
| `crates/zed/Cargo.toml` | Workspace deps on all fork crates (`editor_mcp`, `solutions`, `solutions_ui`, `solution_agent`, `solution_git`, `git_conflict_ui`, `run_config`, `run_config_ui`). | mixed |
| `crates/zed/src/zed/app_menus.rs` | Run / Debug / Stop / "Edit Configurations…" items (with separator) prepended to the existing "Run" menu (S-RUN). Solutions / sessions items added by earlier work. | `run_config_ui` / `solutions_ui` |
| `crates/title_bar/src/title_bar.rs` | Embeds `solutions_ui::SolutionTabStrip` after the hamburger; project-info segment (solution name + worktree + branch) removed; uses fork-local `fork_title_bar_content_height()` for the content row. Also renders the run-config widget (`Workspace::run_config_strip`) right-aligned in the header, at the start of the right-side controls cluster (IDEA-style). `render_restricted_mode` call site disabled (function kept under `#[allow(dead_code)]`) — see decision 13. | `solutions_ui` / `solutions` / `run_config_ui` |
| `crates/recent_projects/src/recent_projects.rs` | Added `RemoteConnectionOptions::Mock` fallback for non-test builds (cfg-gated `unreachable!()`); pre-existing upstream pattern issue blocking our test runs. | upstream-fix |
| `crates/recent_projects/src/remote_connections.rs` | Added `RemoteConnectionOptions::Mock` fallback for non-test builds (cfg-gated `unreachable!()`); pre-existing upstream pattern issue blocking our test runs. | upstream-fix |
| `crates/recent_projects/src/remote_servers.rs` | Added `RemoteConnectionOptions::Mock` fallback for non-test builds (cfg-gated `unreachable!()`); pre-existing upstream pattern issue blocking our test runs. | upstream-fix |
| `crates/remote_connection/src/remote_connection.rs` | Added `RemoteConnectionOptions::Mock` fallback for non-test builds (cfg-gated `unreachable!()`); pre-existing upstream pattern issue blocking our test runs. | upstream-fix |
| `crates/acp_thread/src/connection.rs` | Adds `AgentConnection::new_session_with_meta` extension point (default impl drops the meta + falls back to `new_session`) so adapters can act on protocol-level `_meta` keys (e.g. `claude-agent-acp` reads `_meta.systemPrompt` to seed the session prompt). | `solution_agent` |
| `crates/agent_servers/src/acp.rs` | (1) `mcp_servers_for_project` prepends a fork-local `acp::McpServer::Stdio` entry pointing at `<current_exe> --nc <editor_mcp.socket_path>` so spawned ACP subagents see the editor's embedded MCP tools (helper: `spk_editor_mcp_bridge_server`) — see decision 14. (2) `AcpConnection::new_session_with_meta` impl splices `extra_meta` into `NewSessionRequest::meta`. | `editor_mcp` / `solution_agent` |
| `crates/agent_servers/Cargo.toml` | New dep on `editor_mcp` for the socket path. | `editor_mcp` / `solution_agent` |
| `crates/gpui/src/elements/list.rs` | `ListState::measure_last(N)` chunked tail prefetch (plus `MEASURE_LAST_DEFAULT_BATCH` / `LOOKAHEAD` / `EAGER_THRESHOLD` knobs) so virtualized lists can pre-warm their most-recent items on the first layout pass without paying the full-list measurement cost. Used by `solution_agent`'s conversation list to keep scroll-up off long resumed conversations from triggering a height-discovery cascade. | `solution_agent` |
| `crates/gpui/src/window.rs` | `Window::render_to_image` ungated (was `#[cfg(any(test, feature = "test-support"))]`) so `workspace.screenshot` works in normal builds. Adds `Window::iter_hitboxes()` — a public accessor over the most-recently rendered frame's hitboxes, used by `workspace::mcp::clickables` to surface clickable regions to the autonomous-testing MCP surface. | `solutions` (screenshot tool) / `workspace` (clickable tree) |
| `crates/gpui/src/platform.rs` | `PlatformWindow::render_to_image` default + the `use image::RgbaImage` import ungated (were `#[cfg(test|test-support)]`). Non-implementing backends still return the "not implemented for this platform" error. | `solutions` (screenshot tool) |
| `crates/gpui_wgpu/src/wgpu_renderer.rs` | Extracted the per-frame primitive-encoding loop from `WgpuRenderer::draw` into `render_scene_into(scene, target_view)` (no behaviour change for `draw`); added `WgpuRenderer::render_to_image` — offscreen render-to-texture matching the swapchain size/format, `copy_texture_to_buffer` + readback, BGRA→RGBA fixup → `RgbaImage`. New `image` dep in `gpui_wgpu/Cargo.toml`. | `solutions` (screenshot tool) |
| `crates/gpui_linux/src/linux/x11/window.rs`, `crates/gpui_linux/src/linux/wayland/window.rs` | `PlatformWindow::render_to_image` override → `renderer.render_to_image(scene)`. | `solutions` (screenshot tool) |
| `crates/gpui_wgpu/src/wgpu_context.rs` | Adds `WgpuContext::instance_offscreen()` + `WgpuContext::new_offscreen()` — surfaceless adapter/device selection with integrated-GPU bias for the native headless platform (ADR-0002). | `gpui_wgpu` (headless platform) |
| `crates/gpui_linux/src/linux/headless.rs`, `crates/gpui_linux/src/linux/headless/client.rs` | `HeadlessClient::open_window` now returns a real `gpui::HeadlessWindow` backed by `gpui_wgpu::WgpuHeadlessRenderer`; `displays()` / `primary_display()` / `active_window()` / `window_stack()` populated against a synthetic 1920×1080 `HeadlessDisplay`. New sibling file `display.rs` for the display impl. (ADR-0002.) | `gpui_linux` (headless platform) |
| `crates/gpui_platform/src/gpui_platform.rs` | `current_headless_renderer()` ungated (was `#[cfg(feature = "test-support")]`); adds Linux/FreeBSD arm returning `WgpuHeadlessRenderer`. macOS arm still gated on `test-support` (existing constraint). (ADR-0002.) | `gpui_platform` (headless platform) |
| `crates/gpui_platform/Cargo.toml` | Adds `gpui_wgpu` dep for the Linux/FreeBSD target (needed by the new headless-renderer arm of `current_headless_renderer`). | `gpui_platform` (headless platform) |
| `crates/util/src/paths.rs` | `home_dir()` honours an `SPK_EDITOR_HOME` env var before the `test-support`→`/home/zed` hard-code, so a `test-support` build can run interactively against the real home. `script/run-mcp` sets it. Unit tests don't set the var. | build / agent testing |
| `crates/workspace/src/workspace.rs` | `Workspace::swap_worktrees_to(target_paths)` delta worktree reconciliation used by the in-place Solution switch (decision 16). Drops worktrees not in the set, adds missing ones, preserves overlapping `WorktreeId`s so LSP / panels / caches don't churn.; adds `run_config_strip` / `run_config_controller` slots (+ `set_run_config_strip` / `set_run_config_controller` / `run_config_strip()` / `run_config_controller()` getters) for the Run Configurations widget (set by `run_config_ui`; the `run_config_strip` view is read + rendered right-aligned in the header by `title_bar`, no longer rendered in `Workspace::render`). | `solutions_ui` / `solutions` |
| `crates/welcome/src/welcome.rs` | Recent Solutions section + buttons. | `solutions_ui` |
| `crates/project_panel/src/project_panel.rs` | Hosts `solutions_ui::ActiveProjectSelector` at the top of the panel; filters `state.visible_entries` to worktrees under the selected member's `local_path` after each `update_visible_entries`; resets `max_width_item_index` and recomputes `last_worktree_root_id` post-filter. | `solutions_ui` / `solutions` |
| `crates/git_ui/src/git_panel.rs` | Hosts `solutions_ui::ActiveProjectSelector` at the top of the panel; `refresh_active_repository_for_selector` overrides `active_repository` with the selected member's matching repo at the start of `update_visible_entries`; `refresh_change_counts_for_selector` builds a per-member changed-file count map and pushes it into the selector for the dropdown badges. | `solutions_ui` / `solutions` |
| `crates/git/Cargo.toml` | `test-support` feature now also activates `db/test-support` — the `db::static_connection!` macro's expansion references `db::open_test_db`, which only exists under that feature; without it, crates that enable `git/test-support` but not `db/test-support` fail to compile. Pre-existing latent workspace bug, fixed in-tree. | build / upstream-fix |
| `crates/git/src/repository.rs` | Adds `branches_containing` / `tags_containing` / `load_commit_against_parent` methods on `GitRepository` (default no-op impls + real impls in `RealGitRepository`) for the S-DET commit-view metadata surface. Adds module-level `parse_contains_output` parser. | `git_ui` (S-DET) |
| `crates/project/src/git_store.rs` | Adds `Repository::branches_containing` / `tags_containing` / `load_commit_diff_against_parent` job-dispatch helpers. | `git_ui` (S-DET) |
| `crates/settings_content/src/settings_content.rs` | Adds `CommitViewSettingsContent` (avatars, lazy threshold, mention parsing) + nested field on `GitPanelSettingsContent`. Also adds `SolutionAgentSettingsContent { ephemeral }` (S-AI-MSG ephemeral-pool sizing). Adds `RunConfigSettingsContent { toolbar }` + nested `run_config` field on `SettingsContent` (S-RUN). | `git_ui` (S-DET) / `solution_agent` (S-AI-MSG) / `run_config` (S-RUN) |
| `crates/settings/src/vscode_import.rs` | Add `solution_agent: None` field initializer to keep VS Code import in lockstep with the new `SettingsContent.solution_agent` field. Adds `run_config: None` for the same reason (S-RUN). | `solution_agent` (S-AI-MSG) / `run_config` (S-RUN) |
| `crates/task/src/task_template.rs` | Adds `before_commit: bool` field on `TaskTemplate` (default `false`). Read by `git_ui::pre_commit` to surface a task as a before-commit check row in the commit panel. | `git_ui` (S-PCH-HK) |
| `crates/project/src/task_inventory.rs` | Adds `Inventory::before_commit_templates(worktree)` accessor (mirrors `templates_with_hooks` shape) so the git panel can enumerate pre-commit-flagged tasks without touching `templates_from_settings`. Also adds `Inventory::task_templates_from_settings(worktree)` — a synchronous, context-free listing of settings-derived task templates used by the `task-ref` run-config provider (language runnables excluded; those need the async `list_tasks`). | `git_ui` (S-PCH-HK), `run_config` (S-RUN) |
| `crates/workspace/src/welcome.rs` | `render_agent_card` gated off via `false &&` — fork uses `solution_agent`, not upstream agent panel. | `solution_agent` |
| `crates/workspace/src/active_file_name.rs` | `ActiveFileName::new` now takes the `Workspace` (holds a `WeakEntity<Project>`); the status-bar label prefixes the worktree-relative path with the worktree's root name so it's unambiguous across a Solution's worktrees. (`status_bar.show_active_file` also flipped to `true` in `default.json`.) | rebrand / solutions |
| `crates/git_ui/src/commit_view.rs` | S-DET commit-view surface (header / parents / refs / contains / affected-files / footer decomposed into `commit_view::*` submodules) **and** a `single_file: Option<RepoPath>` mode (`CommitView::open_file_diff`) that renders just the diff editor — no metadata chrome, tab titled with the file name — used by the git-graph changed-files list. | `git_ui` (S-DET) / `git_graph` |
| `crates/paths/src/paths.rs` | `.zed` → `.spke` rename for per-worktree config dir. Adds `run_configurations_file()` (global `~/.config/spk-editor/run-configurations.json`) and `local_run_configurations_file_relative_path()` (`.spke/run-configurations.json`) for S-RUN. Adds `remote_control_settings_file()` (`~/.config/spk-editor/remote-control.json`) for R-1. Adds `remote_control_cert_file()` / `remote_control_key_file()` siblings for the R-2 self-signed TLS cert + key (persisted across restarts so fingerprint pinning stays stable). | rebrand / `run_config` (S-RUN) / `remote_control` (R-1 / R-2) |
| `crates/gpui_tokio/src/gpui_tokio.rs` | Adds `Tokio::try_handle(cx) -> Option<tokio::runtime::Handle>` — the non-panicking analogue of `Tokio::handle`, used by `remote_control::store::start_listener_async` to short-circuit when the runtime isn't installed (rather than panic deep in the bootstrap path). | `remote_control` (R-2) |
| `assets/keymaps/default-*.json` | Default shortcuts for Solutions / sessions. Adds `alt-shift-f10` → `run_config::Run`, `alt-shift-f9` → `run_config::Debug`, `alt-shift-f2` → `run_config::Stop` (Workspace context; IntelliJ-style — `alt-shift` variants chosen because `shift-f10`/`shift-f9`/`ctrl-f2` are already bound in Editor context). | `solutions_ui` / `run_config_ui` |
| `assets/settings/default.json` | Default `solutions.root`; default `icon_theme: "Material Icon Theme"` + auto-install of the matching extension (colored project tree, IDEA-like, vs upstream's monochrome `Zed (Default)`); default `bottom_dock_layout: "full"` (IDEA-style — the bottom dock spans the full window width, with the left/right docks docked above it, vs upstream's `"contained"`; `Workspace::render` already implements the `Full` arm, only the default flips). | `solutions` / rebrand |
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

### 17. Per-panel project selectors are independent — no global "active project"

The Phase 3 `ActiveProjectSelector` element lives in two places (`project_panel`, `git_panel`) and each instance keeps its own selection in the `panel_member_selections` SQL table keyed by `(solution_id, panel_kind)`. There is no global "the user's active project" concept on `SolutionStore`; cross-panel sync is intentionally absent.

Why: a global active-project field cascades into search-scope, terminal cwd, new-file location, find-in-files default, and several other behaviours — an unbounded set of consequences that have to be designed before the first feature ships. Per-panel scoping keeps Phase 3's footprint tight: each panel filters its own content (project_panel filters worktrees; git_panel drives `active_repository`), and `set_panel_member_selection` emits `PanelMemberSelectionChanged` so multi-window same-solution stays in sync without a global field.

How to apply: if a future feature needs cross-panel "current project" awareness, do **not** add a global field to `SolutionStore`. Pick one of: (a) "follow the focused panel's selection" heuristic, (b) "last-touched panel" heuristic, (c) per-feature opt-in argument that asks the relevant panel for its selection. The two cycle actions `SwitchToNext/PrevProjectInPanel { panel_kind }` already work this way — they're scoped to a single panel, not "the active project."

Initial-selection rule, also intentional: on first load of a (solution, panel) pair, default to the first member in `solution_members.position` order **and persist the default immediately** via `set_panel_member_selection`. Subsequent loads (and other windows on the same solution) read the persisted value. This makes "what does the user see in this panel?" a deterministic single-row lookup, not a derive-from-N-signals computation.

### 18. `SolutionSession::set_acp_thread` is the only legal way to swap the thread

`SolutionSession.acp_thread` swaps on compact rotation, `/clear`, cold→live promotion, and `restart_agent` reuse. Each callsite goes through `SolutionSession::set_acp_thread(thread, cx)` (`crates/solution_agent/src/model.rs`), which atomically reassigns the field, emits `SolutionSessionEvent::ThreadReplaced`, and calls `cx.notify()`. `SolutionSessionView` listens for `ThreadReplaced` via `cx.subscribe(&session, ...)` (field `_session_event_subscription` in `crates/solution_agent/src/session_view.rs`) and re-attaches `_thread_subscription` to the new `AcpThread`.

Why: GPUI auto-notify gets dropped silently when a nested `session_entity.update(cx, |s, _| { s.acp_thread = ... })` runs inside an outer `this.update(cx, |store, cx| ...)` on the store — the deduplication in `App::push_effect` (`crates/gpui/src/app.rs`) collapses pending notifications across the outer flush. Without an explicit notify on the *session* entity, `cx.observe(&session)` callbacks (notably `SolutionSessionView::sync_thread_subscription`) never fire, leaving `_thread_subscription` bound to the dropped thread. Result: the conversation list stops growing while the agent keeps streaming events into the new thread — visible to the user as "messages stopped appearing even though the agent is clearly working." Push-channel via `ThreadReplaced` makes the contract explicit and synchronous: every swap fires exactly one event, every subscriber re-attaches, no auto-notify dependence.

How to apply: never assign to `s.acp_thread` directly outside `set_acp_thread` — it's enforced at compile time. The field is **private**; reads go through `s.acp_thread()` (returns `Option<&Entity<AcpThread>>`), writes only through the setter. Direct struct-literal construction is also blocked (private field), so all `SolutionSession` instances are built via `SolutionSession::new_idle(id, solution_id, agent_id, acp_session_id)` followed by `s.<other_pub_field> = ...` for any defaults that need overriding, then `s.set_acp_thread(thread, cx)` as the *last* mutation if a live thread is being attached so observers wake up to a fully populated session struct. Tests for any new thread-swap path must include the same `cx.subscribe(&session, ThreadReplaced)` + `cx.observe(&session)` probe pair as `model::tests::set_acp_thread_emits_thread_replaced_and_notifies`.

### 19. SQLite `Domain` for cx-less state stores; `OnceLock` cache + `gpui::block_on` for sync API

Why: state stores called from background tasks (`OpRunner` from S-BAK; pre-commit check pipeline; favorites toggles fired from a list-row click; shelf saves) don't have `cx: &App` available. The fork's prevailing persistence convention is SQLite via `db::sqlez::Domain` (`GitGraphsDb`, `SolutionsDb`, `WorkspaceDb`, `solution_agent::db`). `query!` macros generate async functions; `static_connection!` provides the `Domain::open_test_db` helper for tests.

How to apply: per state store, declare a `mod persistence` (or `<name>_db.rs`) inside the owning crate. Define a `Domain` impl + `MIGRATIONS` array. Cache the connection in a module-local `OnceLock<Domain>` populated by `<module>::init(cx)` at app startup right after `cx.set_global(app_db)`. Public sync methods use `gpui::block_on(domain.async_method())` for writes; the connection's executor pool guarantees no deadlock against the calling thread.

Tests use a per-thread `Mutex<HashMap<ThreadId, Domain>>` registry (each parallel test gets its own UUID-named in-memory DB) to sidestep `SQLITE_LOCKED_SHAREDCACHE` under `cargo test`'s parallel runner. The pattern is duplicated across four modules today (`undo_registry`, `branch_picker::favorites`, `shelf`, `pre_commit`); consolidating into a `db::test_registry<T>` helper is a low-priority follow-up — the duplication is `#[cfg(test|test-support)]`-only.

Stores that should NOT use this pattern: caches living in `paths::temp_dir()` (`commit_explanations/`, `ai_cherry_pick_cache/`) — direct file IO is fine for write-once / read-once / age-out shapes. Per-worktree filesystem markers (`.spke-readonly.json` from S-SAR) similarly stay as files because their detection runs at worktree-load time before any DB connection is available.

### 20. Cross-crate dynamic action dispatch via `cx.build_action(name, params)`

Why: `git_ui` is the central git-UI crate; `git_graph` and `solution_git` depend on it (downward). When `git_ui::commit_context_menu` needs to fire an action owned by `git_graph` (`ShowAffectedPathsInLog`) or `solution_git` (`CrossCherryPick`), a direct `Box::new(action)` would invert the dep graph. Each downward crate `pub` declares the action and registers a workspace handler at `init`; `git_ui` discovers it dynamically via `cx.build_action("crate::ActionName", Some(params_json))` which silently no-ops when the action isn't registered.

How to apply: when an upward crate needs to fire a downward-crate action, use the dynamic dispatch path. The action must be JSON-deserializable and take its full payload through the `params` argument. Document the upward call site with the action's owner crate so future contributors can find the registration point. Don't add the upward dep just to call the action statically — the silent no-op behavior is the right semantic for "feature available only when its owning crate is initialized."

Don't use this pattern for tightly-coupled action sequences where a missing handler is a bug. Examples in tree: `commit_context_menu::build_commit_context_menu` for ShowAffectedPathsInLog and CrossCherryPick; the menu entries are gated on whether the relevant crate state is available (e.g. CrossCherryPick entry is hidden when no `member_id` is set on the CommitContext).

### 21. Run Configurations are a UX + model layer on top of `task` / `dap`, not a new execution engine

**Why:** the fork already has a full static-task engine (`task` + `.spke/tasks.json` + language runnables) and a DAP layer (`dap` + `.spke/debug.json`); rebuilding execution would duplicate both. So `run_config` is purely a *model + persistence + provider registry*, and a `RunConfiguration` is translated into a `task::SpawnInTerminal` (Run) or `task::DebugScenario` (Debug) at launch time, which `run_config_ui::RunController` then hands to `Workspace::spawn_in_terminal` / `Workspace::start_debug_session`. Run output lands in the existing terminal panel, debug output in the existing debugger panel — there's no separate "Run console" panel. The picker + Run/Debug/Stop widget lives in the title bar, right-aligned (IDEA-style) — `run_config_ui::install` builds the view and parks it in `Workspace::run_config_strip`; `title_bar::TitleBar::render` reads that slot and renders it in the right-side controls cluster (no separate full-width strip row).

**How to apply:** new configuration *types* are `RunConfigProvider` impls registered via `run_config::register_provider(cx, …)` in some crate's `init` (mirrors `editor_mcp::register_tool`); a provider's `resolve()` returns a `RunRequest` (`Terminal(SpawnInTerminal)` or `Debug(DebugScenario)`) — never spawn processes directly from a provider. **Config identity:** every persisted config carries a stable, name-independent `RunConfigId` (a random uuid) materialized in `run-configurations.json` as the first key, `"id"`. New configs (modal `+` / duplicate / promote-ephemeral, MCP `run_config.create`) get a fresh `RunConfigId::new_random()`; renaming a config keeps its id, and two configs with the same display name are fine (distinct ids — no `-2` slug workaround anymore). Legacy entries without an `"id"` key get a deterministic-from-name id on load (`file_format::legacy_id` = `"<type>:<slugified-name>"`), which is then written into the file on the next save. Ephemeral discovered configs use `RunConfigId::discovered(type, key)` = `"<type>:discovered:<task-label>"`, regenerated each load and never persisted. `RunConfigId::from_raw(s)` wraps an id string verbatim (parsing `"id"` keys, accepting id strings over the MCP surface). There is no `RunConfigId::new(type, key)` anymore. The crate split mirrors `solutions` / `solutions_ui`: `run_config` is headless (deps `task` / `project` / `fs` / `paths` / `editor_mcp`, no `workspace` / `editor`); everything needing `Workspace` / `Window` lives in `run_config_ui`. MCP tools (`run_config.*`) reach the per-`Workspace` `RunController` (in `run_config_ui`) through the `RunConfigStore` command-sink indirection (`set_command_sink` / `dispatch_command`) to avoid a `run_config → run_config_ui` dependency cycle; the running-config set is similarly published *up* into `RunConfigStore::set_running`. Stop for a terminal task spawns it through the terminal panel (`TerminalPanel::spawn_task` → killable `WeakEntity<Terminal>`) and calls `Terminal::kill_active_task()`; if there's no terminal panel (headless test harness) the `RunController` falls back to `Workspace::spawn_in_terminal` and Stop just drops the tracking entry. Stop pressed *during the launch window* (before `spawn_task` hands back the terminal handle) is honoured too: each `run()` gets a monotonic launch token, Stop records it in `terminal_launches_pending_kill` and keeps the completion poller alive (moved to `_detached_tasks` rather than dropped — dropping it would cancel the only thing that'll ever see the handle), and the poller — once the handle resolves — kills the terminal and exits instead of tracking it. The token is keyed per launch (not per config), so a stale poller from a since-stopped-and-rerun launch can't kill the newer launch's terminal. Debug runs are tracked per `dap::client::SessionId`: `Workspace::start_debug_session` hands back nothing, so right before each launch the controller snapshots the set of existing `SessionId`s and pushes `(RunConfigId, snapshot)` onto `pending_debug_launches`; on `DapStoreEvent::DebugClientStarted(id)` it hands `id` to the first pending entry whose snapshot doesn't already contain it — that launch must be the one that created it (see the `claim_started_session` free fn, unit-tested). Matching is by session novelty, never by label, so two configs with the same display name are no longer ambiguous. Stop calls `DapStore::shutdown_session(id)`, and the entry clears on `DebugClientShutdown` for that id. A debug launch that never starts a session (adapter died during launch → no `DebugClientShutdown` will ever come either) is cleared by a per-launch `DEBUG_LAUNCH_TIMEOUT` (20s — generous, since adapters can be slow to come up) timer — see the `debug_launch_timed_out` free fn, unit-tested; a run that did get a session id, or was already stopped, makes the timer a no-op. When this controller's workspace window closes, an `on_release` handler calls `RunConfigStore::clear_running_source` to drop its slice of the running set (entity ids can be reused after release, so this also closes the collision window). Known limitations: if the user manually starts an unrelated debug session in the exact tick a config's debug launch is in flight, that session can be mis-attributed to the launch (a much narrower race than the old name-collision bug, and unavoidable without an id handed back from `start_debug_session`); `run` / `stop` / `select` MCP tools are no-ops when no workspace window with a `RunController` is open.

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
- `solutions_ui::ActiveProjectSelector` (Phase 3) hosts its two popovers via `ui::PopoverMenu<MemberPicker>` / `PopoverMenu<AddProjectPicker>` rather than the manual `anchored()` + `deferred()` + stored-trigger-bounds pattern from `solution_picker_dropdown.rs`. PopoverMenu encapsulates bounds tracking, dismiss subscription, and z-order; nothing the manual pattern provides was needed.
- `ActiveProjectSelector::new` defers its initial `rebuild()` via `cx.spawn(...).detach()` instead of running it synchronously. Reason: panels (`ProjectPanel::new`, `GitPanel::new`) are constructed inside `workspace.update_in(cx, ...)`, which holds a mutable borrow of the `Workspace` entity. The selector's `rebuild()` reads the workspace via `active_solution_in_workspace`, which would panic with "cannot read X while it is already being updated." The defer pushes the first rebuild to the next event-loop turn, after the construction `update_in` has finished. Side-effect: the trigger renders once with the empty-state label ("No project") before the deferred rebuild populates real members; acceptable.

## Updating this file

Add to FORK.md when:
- A new fork-local crate is added.
- A new upstream file gets its first local modification.
- A non-obvious architectural decision is made — record the *why* before it gets lost.

Don't add:
- Per-crate module layout / data flow / type catalogs — those go stale fast and the agent can read the code. Rules are "traps to avoid", not "maps to follow".
- Long-term TODOs — use issues for those.
- Status updates — the git log is canonical.

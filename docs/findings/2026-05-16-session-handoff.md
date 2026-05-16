# Session handoff — 2026-05-16

**Status:** session paused for context reset; resume from current `main`.

This supersedes [`2026-05-15-session-handoff.md`](2026-05-15-session-handoff.md).
The previous session was interrupted before writing its own handoff; this
one captures the cumulative state after Remote Control phases R-2/R-3/R-4
shipped and after auditing the rest of the 2026-05-15 pool.

## What shipped between 2026-05-15 handoff and now

| Phase | Commit chain | Plan / artefact |
|---|---|---|
| Remote Control R-2 (TLS + WS + HMAC listener) | `7020b69` (ADR-0003) → `759365d` (plan) → `5519f82` (deps) → `a38f34e` → `977bd29` → `731a97c` → `8bc441e` (finalize) | [`plans/2026-05-15-remote-control-R2.md`](../plans/2026-05-15-remote-control-R2.md), [`adr/0003-remote-control-protocol.md`](../architecture/decisions/0003-remote-control-protocol.md), [`findings/2026-05-remote-control-watcher-echo.md`](2026-05-remote-control-watcher-echo.md) |
| Remote Control R-3 (server fingerprint in pairing QR) | `85dec79` | (No separate plan-doc — small enough to ride on the R-1.5 / R-4 work.) |
| Remote Control R-4 (`remote.*` proxy to embedded MCP socket) | `25024fe` (plan) → `5d8e013` → `ff8f00e` (tick acceptance + envelope quirk finding) | [`plans/2026-05-16-remote-control-R4.md`](../plans/2026-05-16-remote-control-R4.md), [`findings/2026-05-remote-control-r4-mcp-envelope.md`](2026-05-remote-control-r4-mcp-envelope.md) |
| Workflow doc: resume-from-paused-session bootstrap | `7540f6b` | [`workflow/supervisor-mode.md`](../workflow/supervisor-mode.md) |
| Workflow doc: `pgrep -f` self-match anti-pattern | `0ee8e7d` | [`workflow/supervisor-mode.md`](../workflow/supervisor-mode.md) anti-patterns section |
| INDEX hygiene: R-4 row + fresh handoff link | (this commit) | — |

`cargo test -p remote_control` is 40-green (37 unit + 2 listener_e2e + 1 proxy_e2e). `cargo check -p remote_control --all-targets` clean.

## Phase E (queued message → claude) — **already shipped before this session**

The previous 2026-05-15 handoff listed E as "outstanding". Audit on resume
found it had actually shipped earlier (commits `7b0b4c5` /clear+queue UX,
`e1ebb2e` queue audit logs, then later cold-resume polish in
`06731b9` / `f80f183`). The complete surface:

- `SolutionSession::pending_messages: VecDeque<Vec<acp::ContentBlock>>`
  in [`crates/solution_agent/src/model.rs`](../../crates/solution_agent/src/model.rs).
- Queue bubble + collapse strip + "send now" Bolt button in
  [`crates/solution_agent/src/session_view/render_queue.rs`](../../crates/solution_agent/src/session_view/render_queue.rs).
- Up-arrow recall in
  [`crates/solution_agent/src/session_view/recall.rs`](../../crates/solution_agent/src/session_view/recall.rs).
- Drain-on-`Stopped(Cancelled)` in `store::handle_acp_event`.
- Notifier suppresses `Completed` when the queue is non-empty
  ([`notifier.rs::decide_notification`](../../crates/solution_agent/src/notifier.rs)).
- MCP tool `solution_agent.send_message` returns immediately with `"queued"`.
- Cold-resume optimistic ghost bubble (`render_resuming_section`) so the
  3-4 s ACP handshake doesn't look like a stuck Send.

Don't reopen this phase. If a sub-issue surfaces (a queue UX miss), file
a new dated plan-doc — don't resurrect "E".

## Findings created in this session pair

- [`2026-05-remote-control-watcher-echo.md`](2026-05-remote-control-watcher-echo.md) — FS-watcher self-write echo loop in `remote_control::store`; resolution: `self_write_echoes` counter squelches the next inbound event after each `RemoteControlSettingsBackend::write`.
- [`2026-05-remote-control-r4-mcp-envelope.md`](2026-05-remote-control-r4-mcp-envelope.md) — Bare `{"method":"editor.capabilities"}` returns `-32601`; the embedded `editor_mcp` server actually wants `tools/call { name: "editor.capabilities", arguments }`. R-4's `proxy::call` had to wrap calls in this envelope before the proxy round-trip would succeed.

## Pool — outstanding tasks at session end

| Item | Track | Where | Notes |
|---|---|---|---|
| **R-5** Android client scaffold | HEAVY | `spk-editor-android-client` (**separate repo, doesn't exist yet**) | Jetpack Compose + OkHttp/Ktor WS + zxing QR scanner. Not workable from this cwd. The supervisor must either (a) create the repo elsewhere and dispatch from there, or (b) defer to the user to bootstrap the Android project before agent dispatch becomes meaningful. |
| **R-6** Android client polish | HEAVY | (same separate repo) | Depends on R-5. FCM/push, reconnect, multi-server. |
| **F** Sub-agent indication UI | HEAVY | `spk-cockpit` (**different project, different cwd**) | User-requested 2026-05-15. Show running sub-agents with progress / tokens / interrupt. Pick up in a `spk-cockpit` session, not here. |
| **G** `spk-image://` URL in queued message | LIGHT | `spk-cockpit` (different project) | User-reported 2026-05-15. Note: `spk-image://` IS wired inside `solution_agent` (`render_queue.rs` line ~85 decodes `spk-image://<idx>`); the issue is specifically that `spk-cockpit` doesn't recognise the scheme. So it's a cockpit-side fix, not a spk-editor-side fix. |

**Within this `spk-editor` repo: pool is empty.** All in-tree HEAVY phases
from the 2026-05-15 plan arc are either shipped or in a separate repo.
The natural next thing inside this cwd is whatever the user names next.

## Open architectural decisions

- **R-5 repo location** — confirmed separate repo (not a sibling crate), per [`plans/2026-05-15-remote-control.md`](../plans/2026-05-15-remote-control.md). Needs creation before R-5 work can be dispatched.
- **`MinimalDispatcher` retention** — kept as `#[cfg(test)]`-friendly fallback for unit/integration tests that don't want a live MCP socket. Don't delete it during cleanup passes.

## Active gotchas the next session should know

1. **Agent SDK worktree branches from session-start HEAD.** Inline plan-doc content + tell sub-agent to rebase. See [`findings/2026-05-agent-worktree-staleness.md`](2026-05-agent-worktree-staleness.md).
2. **`script/run-mcp --headless` is the default** for agent-driven runs (post ADR-0002, no Xvfb needed).
3. **MCP `windows.click_id`** by stable ID is preferred over `windows.click_at`.
4. **`workspace.screenshot` works in headless** (offscreen wgpu).
5. **`editor_mcp` over the socket needs `tools/call` envelope** — bare-method calls get -32601. See R-4 envelope finding.
6. **FS-watcher self-write echo** — any settings-file writer that also watches the file must squelch its own writes. R-2 uses `self_write_echoes` counter; mirror the pattern for any new live-watched config.
7. **`pgrep -f` in watch-loops self-matches the running bash.** Use marker grep or `pgrep | grep -v $$`. See `supervisor-mode.md` anti-patterns.
8. **MCP tool catalog count is 60** (unchanged this session — R-4 added the `remote.*` namespace but those tools are proxied through, not registered with the local registry).

## Stale handoff notice

`2026-05-15-session-handoff.md` lists E + R-2/R-3/R-4 as outstanding. They
are not — E shipped earlier (audited above), R-2/R-3/R-4 shipped between
the two handoffs. Treat 2026-05-15 as the snapshot at that moment and
this file as authoritative for current state.

## Resume recipe for the next session

1. Read this file first.
2. Read `docs/INDEX.md`.
3. Read `docs/workflow/supervisor-mode.md`.
4. `git log --oneline -25` to confirm the chain.
5. The in-tree pool is empty — wait for the user to name the next task
   or pick something workable from `spk-cockpit` if cwd has shifted.

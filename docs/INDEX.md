# docs/INDEX.md — documentation map

> The project's bookshelf. Every development session starts here (after
> `CLAUDE.md`).
>
> When adding a new doc — add a row in the appropriate table below.

---

## Entry points

- **Starting a new session?** → read `CLAUDE.md` + this file.
- **About to dispatch a sub-agent or plan a feature?** → [`workflow/supervisor-mode.md`](workflow/supervisor-mode.md).
- **Not sure where to write something?** → [`workflow/doc-discipline.md`](workflow/doc-discipline.md).
- **What's different from upstream Zed?** → [`../FORK.md`](../FORK.md).

---

## workflow/

How sessions are run.

- [`workflow/supervisor-mode.md`](workflow/supervisor-mode.md) — the supervisor's playbook (READ → DECIDE → DISPATCH → VERIFY → FINALIZE).
- [`workflow/doc-discipline.md`](workflow/doc-discipline.md) — when to create / update which doc.
- [`workflow/adr-template.md`](workflow/adr-template.md) — template for new ADRs.

---

## architecture/decisions/ — ADRs

Architectural decisions with long-term consequences (data formats, public-API
contracts, multi-crate invariants). Each ADR is dated `accepted`/`superseded`.

| # | Title | Status | Document |
|---|---|---|---|
| 0001 | Fork philosophy: no scheduled upstream merge | accepted | [`architecture/decisions/0001-fork-philosophy.md`](architecture/decisions/0001-fork-philosophy.md) |
| 0002 | Native headless GPUI platform for autonomous agent driving | accepted | [`architecture/decisions/0002-native-headless-platform.md`](architecture/decisions/0002-native-headless-platform.md) |
| 0003 | Remote Control transport — WebSocket over TLS, fingerprint-pinned, secret-authenticated | accepted | [`architecture/decisions/0003-remote-control-protocol.md`](architecture/decisions/0003-remote-control-protocol.md) |

---

## plans/ — HEAVY-track plan docs (committed)

Per-phase specs (acceptance criteria + verification + commit log). Filename
format: `YYYY-MM-DD-<slug>.md`. Status flips from `ready to dispatch` →
`in progress` → `complete`/`cancelled`.

These are **committed to the repo** so sub-agents dispatched in a worktree
can read them.

| Date | Status | Plan |
|---|---|---|
| 2026-05-15 | complete | [`plans/2026-05-15-picker-and-panel-ui-tweaks.md`](plans/2026-05-15-picker-and-panel-ui-tweaks.md) — Picker dropdown polish + Project panel header. Screenshot: [`plans/2026-05-15-picker-and-panel-ui-tweaks-screenshot.png`](plans/2026-05-15-picker-and-panel-ui-tweaks-screenshot.png). |
| 2026-05-15 | scoping | [`plans/2026-05-15-remote-control.md`](plans/2026-05-15-remote-control.md) — Remote Control panel + Android client (multi-phase arc R-1 through R-6). R-1, R-1.5, R-2, R-3, R-4 shipped; R-5 lives in sibling repo `spk-editor-android-client` (decomposed into R-5a..R-5d). |
| 2026-05-16 | complete | [`plans/2026-05-16-remote-control-R5a-android-bootstrap.md`](plans/2026-05-16-remote-control-R5a-android-bootstrap.md) — R-5a: bootstrap `spk-editor-android-client` repo + two-module Gradle layout (`:core` JVM connection lib, 30 green tests + `:app` Android Compose Material 3 surface). Sibling-repo commits `77eb966` (bootstrap) → `4e478f1` (`:cli` smoke client + six-step `LiveEditorIntegrationTest`) → `d83ab47` (`:core` `api` promotion after Android SDK install surfaced `:app:compileDebugKotlin` failure). `:app:assembleDebug` now produces a real 9.5 MB APK; `:core:test` 30 PASSED. WS+TLS+HMAC + JSON-RPC client wired end-to-end. |
| 2026-05-16 | complete | [`plans/2026-05-16-remote-control-R5b-qr-scanner.md`](plans/2026-05-16-remote-control-R5b-qr-scanner.md) — R-5b: zxing-android-embedded QR scanner replaces the R-5a paste-URL stub. Sibling-repo commit `6e444e5`. APK 10.9 MB. Gotcha: zxing-android-embedded needs an explicit `androidx.appcompat` dep — its AAR registers `CaptureActivity` without bringing the AppCompat style classpath. |
| 2026-05-16 | complete | [`plans/2026-05-16-remote-control-R5c-solutions-sessions-list.md`](plans/2026-05-16-remote-control-R5c-solutions-sessions-list.md) — R-5c: solutions list → solution detail (with sessions list + live `agent_session_state_changed` re-fetch) → session-detail stub. Navigation Compose 2.8.4. Sibling commit `7fa4615`. `:core` tests 30 → 41, APK 11.18 MB. Gotcha: Kotlin smart-cast doesn't fire across module boundaries on `JsonRpcResponse.error` — lift to local val before null-check. |
| 2026-05-16 | complete | [`plans/2026-05-16-remote-control-R5d-chat-streaming.md`](plans/2026-05-16-remote-control-R5d-chat-streaming.md) — R-5d: chat surface (bubbles by role, optimistic user message, Send / Cancel / state-dependent compose row, id-only `agent_session_message_appended` triggers `get_session` refetch). **Closes the R-5 arc** — sibling commit `6ef0cd7`. APK 11.22 MB, `:core` tests 41 → 45. Known limits → filed as R-5e (server-side enrichment of `EntrySummary` + notification payloads). |
| 2026-05-16 | complete | [`plans/2026-05-16-remote-control-R5e-entry-enrichment.md`](plans/2026-05-16-remote-control-R5e-entry-enrichment.md) — R-5e: server-side enrichment shipped on `main` as `d8592b05dc`. `EntrySummary` gains optional `markdown` / `images` / `tool_call` / `plan` fields gated behind `include_full_content` + `include_images` params; `agent_session_message_appended` notification carries `entry_index` + `role` + `preview`; new `remote.solution_agent.get_session_entry` tool. `solution_agent` tests 83 → 90, R-4 proxy_e2e still green, allow-list extended. Wire sizes documented (preview 1.77 KB / full 6.32 KB / full+images 46.5 KB on a 9-entry synthetic). |
| 2026-05-17 | complete | [`plans/2026-05-17-remote-control-R5f-client-rich-rendering.md`](plans/2026-05-17-remote-control-R5f-client-rich-rendering.md) — R-5f: Android client now consumes R-5e's enriched EntrySummary (markdown / images / tool_call / plan). Diff streaming via `get_session_entry(entry_index)` driven by `agent_session_message_appended` payload — bandwidth proportional to new content per turn instead of full history per token batch. Sibling commit `ee804aa`. `:core` tests 45 → 54. APK 11.58 MB. `multiplatform-markdown-renderer-m3:0.27.0` for rich render; Coil dropped because the lib's `ImageTransformer` takes a pre-decoded `Painter` synchronously. |
| 2026-05-17 | complete | R-5g create-session — **server**: new `solution_agent.list_agents` MCP tool + allow-list extension (spk-editor commit `3fb5ee51ac`, solution_agent tests 90 → 91); **client**: FAB stub replaced with Material 3 AlertDialog (RadioButton agent picker + optional initial message + one-shot auto-open-and-retry on `no_active_workspace_for_solution` error) (sibling commit `41531a1`, `:core` tests 54 → 57, APK 11.59 MB). No plan-doc (small focused phase shipped inline). |
| 2026-05-17 | complete | R-6a network resilience — WS reconnect with 1-2-4-8-16-30s exponential backoff, OkHttp `pingInterval(30s)` NAT keep-alive, `queueCall` for `send_message` with 5-min TTL + FIFO reflush on Connected transition, subscription auto-restore, ConnectionState StateFlow + Compose banner (tertiary "Reconnecting…" / error "Re-pair required"). MainViewModel: `lastSeenEntryIndex` per session + refresh on every Disconnected→Connected. New `RemoteTransport` seam (`OkHttpRemoteTransport` + `FakeRemoteTransport` for tests). Sibling commit `c69e7e3`, `:core` tests 57 → 72, APK 11.59 → 11.63 MB. No plan-doc (inline scope). |
| 2026-05-17 | complete | R-6b production polish — Pairing persistence via AndroidX `EncryptedSharedPreferences` (cold-start skips QR if pairing exists) + Settings screen (server info / fingerprint / Forget Server / Re-pair / About) + adaptive launcher icon + INTERNET permission + signed-release Gradle config (gates on `SPK_RELEASE_*` properties; keystore-free `assembleRelease` succeeds for R8 verification) + ProGuard rules for kotlinx.serialization + expanded README. Sibling commit `c517f03`. `:core` tests stable at 72. Debug APK 11.63 → 12.0 MB (+security-crypto); **release APK 2.12 MB after R8** (5.4× shrink). No plan-doc (inline scope). |
| 2026-05-17 | ready to dispatch | [`plans/2026-05-17-remote-control-R6d-android-disk-persistence.md`](plans/2026-05-17-remote-control-R6d-android-disk-persistence.md) — R-6d: Disk persistence for everything user-action-visible. Outbound queue → encrypted disk (24h TTL, was 5 min); compose-field drafts per session; `lastSeenEntryIndex` per session; active nav route. Bounce-to-input on TTL expiry — failed messages reappear in the compose field on the next open (and across restarts) so the user can edit + retry. |
| 2026-05-16 | complete | [`plans/2026-05-16-remote-control-R4.md`](plans/2026-05-16-remote-control-R4.md) — R-4: `remote.*` proxy to the embedded MCP Unix socket (per-WS `UnixMcpProxy`, allow-list, agent_session_* notification fan-out). |
| 2026-05-15 | complete | [`plans/2026-05-15-remote-control-R2.md`](plans/2026-05-15-remote-control-R2.md) — R-2: server listener + TLS 1.3 + HMAC challenge handshake + MinimalDispatcher; FS-watcher reconciles listener state. |
| 2026-05-15 | complete | [`plans/2026-05-15-remote-control-R1-5.md`](plans/2026-05-15-remote-control-R1-5.md) — R-1.5: QR popover rendering replacing the R-1 toast stub |
| 2026-05-15 | complete | [`plans/2026-05-15-remote-control-R1.md`](plans/2026-05-15-remote-control-R1.md) — R-1: settings + status-bar widget + modal panel UI. Screenshot: [`plans/2026-05-15-remote-control-R1-screenshot.png`](plans/2026-05-15-remote-control-R1-screenshot.png). |
| 2026-05-15 | complete | [`plans/2026-05-15-clickable-tree.md`](plans/2026-05-15-clickable-tree.md) — hitbox-based clickable enumeration + click-by-id MCP tool (phase 1 + 1b labels) |
| 2026-05-15 | complete | [`plans/2026-05-15-headless-platform-real.md`](plans/2026-05-15-headless-platform-real.md) — native headless GPUI platform (no Xvfb). Screenshot: [`plans/2026-05-15-headless-platform-real-screenshot.png`](plans/2026-05-15-headless-platform-real-screenshot.png). |

---

## superpowers/ — personal local drafts (gitignored)

The folder `docs/superpowers/{plans,specs}/` is **gitignored** (.gitignore
entry: "Personal agent plans / specs (kept locally, not committed)"). Use
it for in-progress ideas before they're polished into a committed
`docs/plans/` or `docs/specs/` entry.

Pre-existing local drafts (not visible to a fresh clone or worktree):
- `docs/superpowers/plans/2026-05-{06,07,12}-*.md` — earlier rebrand work
- `docs/superpowers/specs/2026-05-*-design.md` — earlier design notes

Promote to `docs/plans/` (or `docs/specs/`) when ready to commit + dispatch.

---

## findings/ — discovery notes

Short, dated, single-fact notes from sessions: "ran a benchmark and got X",
"found a crate Y", "noticed library Z behaves W in case V". Filename
`YYYY-MM-<slug>.md`. 10–50 lines, no fluff.

| Date | Status | Topic |
|---|---|---|
| 2026-05-17 | handoff | [`findings/2026-05-17-session-handoff.md`](findings/2026-05-17-session-handoff.md) — **READ FIRST on session resume.** Supersedes 2026-05-16 handoff. R-5 + R-6 arcs closed end-to-end: Android client is prod-shippable (release APK 2.12 MB, persists pairing, survives flaky network, rich chat with images + tool-calls). Remaining pool: R-6c (FCM + multi-server), outbound-queue disk persistence, crash reporting, F (sub-agent indication UI in spk-editor), G (spk-image:// repro). |
| 2026-05-16 | superseded | [`findings/2026-05-16-session-handoff.md`](findings/2026-05-16-session-handoff.md) — Supervisor-session handoff at the close of 2026-05-16. R-5a–d shipped + R-5e server enrichment. F/G outstanding-pool entries updated after cockpit audit found they belong in spk-editor, not cockpit. |
| 2026-05-15 | superseded | [`findings/2026-05-15-session-handoff.md`](findings/2026-05-15-session-handoff.md) — Original supervisor-session handoff. Listed pool items (E, R-2..R-6, F, G) that have since shipped or moved out-of-tree. See 2026-05-16 handoff for authoritative state. |
| 2026-05 | gotcha | [`findings/2026-05-remote-control-r4-mcp-envelope.md`](findings/2026-05-remote-control-r4-mcp-envelope.md) — Embedded `editor_mcp` over Unix socket needs `tools/call { name, arguments }` envelope; bare `{"method": ..}` returns -32601. Discovered building R-4 proxy. |
| 2026-05 | gotcha | [`findings/2026-05-remote-control-watcher-echo.md`](findings/2026-05-remote-control-watcher-echo.md) — `remote_control::store` FS-watcher echo loop on settings writes; resolution `self_write_echoes` counter. |
| 2026-05 | active | [`findings/2026-05-agent-worktree-staleness.md`](findings/2026-05-agent-worktree-staleness.md) — Agent-tool `isolation: "worktree"` branches from session-start HEAD, NOT current HEAD; freshly-committed plan-docs aren't visible to sub-agents in the same session |
| 2026-05 | resolved | [`findings/2026-05-headless-screenshot-blank.md`](findings/2026-05-headless-screenshot-blank.md) — `workspace.screenshot` returns blank under `--headless` (Xvfb); resolved by ADR-0002 (native headless platform) |

---

## Module docs (`architecture/modules/<crate>.md`)

Per-crate documentation of public API + invariants + pitfalls. Created on
first non-trivial public API in a crate; updated when that API changes.

(None yet — the first fork-owned crate to get one will be one of
`solutions` / `solution_agent` / `solutions_ui` / `editor_mcp` since those
are where the fork's public surface lives.)

---

## What does NOT go here

- **Status updates** ("Phase 3 in progress, blocked on X") — `git log` and the
  plan-doc status field hold this.
- **Per-crate module layouts** ("crate X has modules a/b/c") — `cargo doc` /
  reading the code is faster than maintaining this in docs.
- **mdBook user-facing docs** — those live in `docs/src/` and follow
  `docs/AGENTS.md`. The supervisor workflow does not touch them.
- **Rebrand spec & locked identifiers** — those are in `CLAUDE.md` (always-in-context).
- **What's-disabled list** — also in `CLAUDE.md`.

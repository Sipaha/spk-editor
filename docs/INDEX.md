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
| 2026-05-16 | complete | [`plans/2026-05-16-remote-control-R5a-android-bootstrap.md`](plans/2026-05-16-remote-control-R5a-android-bootstrap.md) — R-5a: bootstrap `spk-editor-android-client` repo + two-module Gradle layout (`:core` JVM connection lib, 30 green tests + `:app` Android Compose stub). Sibling-repo commit `77eb966`. WS+TLS+HMAC handshake + JSON-RPC client wired end-to-end; `:app` ready for SDK-equipped maintainer build. |
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
| 2026-05-16 | handoff | [`findings/2026-05-16-session-handoff.md`](findings/2026-05-16-session-handoff.md) — **READ FIRST on session resume.** Supersedes 2026-05-15 handoff. R-2/R-3/R-4 shipped, queued-message phase E confirmed already-shipped pre-handoff. Remaining pool (R-5/R-6 Android, F/G cockpit) is all out-of-tree. In-tree pool is empty. |
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

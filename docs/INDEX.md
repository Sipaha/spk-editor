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

---

## superpowers/plans/ — HEAVY-track plan docs

Per-phase specs (acceptance criteria + verification + commit log). Filename
format: `YYYY-MM-DD-<slug>.md`. Status flips from `ready to dispatch` →
`in progress` → `complete`/`cancelled`.

The folder is pre-populated with earlier plan docs from before this workflow
landed; only post-2026-05-15 plans follow the supervisor-mode.md ritual.

Recent plans (sorted by date, descending — most recent first):

| Date | Status | Plan |
|---|---|---|
| 2026-05-12 | complete | [`superpowers/plans/2026-05-12-run-configurations.md`](superpowers/plans/2026-05-12-run-configurations.md) |
| 2026-05-07 | complete | [`superpowers/plans/2026-05-07-solutions-ui-overhaul-phase-3-panel-selectors.md`](superpowers/plans/2026-05-07-solutions-ui-overhaul-phase-3-panel-selectors.md) |
| 2026-05-07 | complete | [`superpowers/plans/2026-05-07-solutions-ui-overhaul-phase-2-titlebar.md`](superpowers/plans/2026-05-07-solutions-ui-overhaul-phase-2-titlebar.md) |
| 2026-05-07 | complete | [`superpowers/plans/2026-05-07-solutions-ui-overhaul-phase-1-persistence.md`](superpowers/plans/2026-05-07-solutions-ui-overhaul-phase-1-persistence.md) |
| 2026-05-07 | complete | [`superpowers/plans/2026-05-07-status-row-context-menu.md`](superpowers/plans/2026-05-07-status-row-context-menu.md) |
| 2026-05-07 | complete | [`superpowers/plans/2026-05-07-strip-and-status-bar-sizes.md`](superpowers/plans/2026-05-07-strip-and-status-bar-sizes.md) |
| 2026-05-06 | complete | [`superpowers/plans/2026-05-06-solution-switch-in-place.md`](superpowers/plans/2026-05-06-solution-switch-in-place.md) |
| undated | reference | [`superpowers/plans/git-panel-plan.md`](superpowers/plans/git-panel-plan.md) |

---

## superpowers/specs/ — design specs

Design documents that precede a plan (the "why this design"). One spec may
feed multiple plans. Filename `YYYY-MM-DD-<slug>-design.md`.

Existing specs are linked from their corresponding plan; no separate table
maintained — browse the folder.

---

## findings/ — discovery notes

Short, dated, single-fact notes from sessions: "ran a benchmark and got X",
"found a crate Y", "noticed library Z behaves W in case V". Filename
`YYYY-MM-<slug>.md`. 10–50 lines, no fluff.

(Empty — first finding lands when a session produces one.)

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

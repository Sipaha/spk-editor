# Compact this session and prepare a clean handoff

The user has triggered the **Compact Context** action because this session
is approaching its context budget. Your job now is to capture every load-
bearing piece of state from the current conversation into durable files,
then ask the editor to start a fresh session that will pick up exactly
where this one left off — minus the ballast.

The editor has injected the variables you need below; do not invent
paths, do not write files anywhere else.

## Variables

- `SESSION_ID` = `{{session_id}}`
- `COMPACT_DIR` = `{{compact_dir}}`
  (= `<solution_root>/.agents/<SESSION_ID>/c<NN>/`, where `c<NN>` is
  the 1-based index of the context being closed; the next context
  in this session lives in `c<NN+1>` after rotation.)
- `SOLUTION_ID` = `{{solution_id}}`
- `AGENT_ID` = `{{agent_id}}`
- `STARTED_AT` = `{{started_at_iso}}`
- `TOKENS_USED` = `{{tokens_used}}`
- `TOKENS_MAX` = `{{tokens_max}}`
- The directory `COMPACT_DIR` already exists and is writable.

## Step 1 — Decide the scope

Before writing anything, classify the conversation:

- **A. Clear next task.** You and the user have an agreed-upon plan or an
  in-flight feature with obvious next steps. Capture the plan; the
  continuation prompt should resume that plan.
- **B. Multiple possible next steps, none picked.** The conversation
  branched and you are unsure which direction to take. **Stop and ask
  the user** which direction to compact toward — do NOT call the MCP
  tool until they answer. Once they answer, treat their reply as case
  A and proceed.
- **C. No clear forward task** (exploration, debugging, post-mortem
  with no commitments). Skip the "next task" assumptions; just dump
  what was *learned* so the next session can pick up cold without
  re-deriving it.

## Step 2 — Write the handoff files into `COMPACT_DIR`

Create exactly these files. Use plain, dense prose — no banners, no
emojis, no "I will now …" preamble. Each file stands alone.

### `state.md`
What is the current state of the world?
- What was the user trying to accomplish in this session.
- What got *done* (concretely: files edited, commits, PRs, tools run,
  conclusions reached).
- What is *in flight* (e.g. "branch X has uncommitted changes to Y").
- Any environment / config the next session must know about that it
  cannot rederive (auth tokens already exchanged, mocked services,
  scratch directories created, etc.).

### `decisions.md`
Architectural / design / approach decisions made during the session.
For each: the decision itself, the reasoning *why*, and one line on
"what this rules out". Future-you needs the *why* to handle edge cases
the recap doesn't anticipate.

### `next.md` *(only for cases A and B; omit for case C)*
The plan going forward. A numbered list of concrete next actions, each
with a single-line "done when" criterion. Do not pad — if there are
two real steps, write two.

### `continue.md`
**This is the user-message that will be fed verbatim into the new
session.** Write it as if you are a teammate who has read all of the
above files and is briefing a fresh agent. It must:
- State the goal in one paragraph.
- Reference `state.md`, `decisions.md`, `next.md` by relative path
  (`.agents/<SESSION_ID>/<timestamp>/...`) — the new session has a
  cold context, those files are its only memory.
- End with the *first concrete instruction* (the new agent's first
  step), not with "let me know if you have questions". Be directive.
- For **case C**, the first instruction is "Read the files above and
  ask the user what they want to tackle now."

### `session-state.json`
Machine-readable technical metadata. Write exactly:

```json
{
  "session_id": "{{session_id}}",
  "solution_id": "{{solution_id}}",
  "agent_id": "{{agent_id}}",
  "started_at": "{{started_at_iso}}",
  "compacted_at": "<UTC ISO-8601 of the moment you wrote this file>",
  "tokens_used": {{tokens_used}},
  "tokens_max": {{tokens_max}},
  "scope": "<one of: planned | branching | exploratory>"
}
```

`scope` corresponds to the case you picked in Step 1 (A → planned,
B → branching, C → exploratory).

## Step 3 — Trigger the session rotation

After all files are on disk, call the MCP tool exactly once:

```
solution_agent.compact_session({
  "session_id": "{{session_id}}",
  "prompt_file": "{{compact_dir}}continue.md"
})
```

The editor will validate the file, close this session, open a fresh
one under the same Solution + agent, and feed `continue.md` as the
first user message in the new session. Do not send any other messages
after the tool call — the rotation closes this session.

## Hard rules

- Never write files outside `COMPACT_DIR`.
- Never call any other MCP tool to "clean up" the session yourself —
  rotation is owned by `solution_agent.compact_session`.
- If a previous compact attempt left files in a sibling directory,
  ignore them; they belong to a different rotation.
- If you cannot write a file (permission error, disk full), tell the
  user, stop, and do **not** call the MCP tool. A failed compact must
  be observable, not silent.

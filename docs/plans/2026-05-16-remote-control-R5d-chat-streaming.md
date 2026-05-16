# R-5d: Chat UI with streaming responses + cancel-turn

**Status:** planned (awaiting Android SDK + R-5c)
**Repo:** `spk-editor-android-client/`
**Depends on:** R-5a (`:core`), R-5b (pairing), R-5c (sessions list).
**Goal:** The session detail screen lights up as a real chat. User types → message goes to the agent → reply streams in bubble-by-bubble. Cancel button stops a turn. Closes the R-5 arc.

## Why this phase exists

R-5c leaves session detail as a stub. R-5d wires the actual interaction loop. From the original user ask: *"Главное внутри — доступ к диалогам с агентами (раздавать команды + следить за прогрессом, когда не у компа)"*. This is THE feature.

## Scope

### Screen: `SessionDetailScreen` (replaces R-5c's stub)

Compose layout:

- **Top bar:** session title (editable via long-press? Defer — read-only for now), back arrow.
- **Message list:** lazy column, reversed (newest at bottom), bubbles styled by role:
  - `user` — right-aligned, accent background.
  - `assistant` — left-aligned, surface background.
  - `tool_call` / `tool_result` — middle, smaller, distinct background (collapsed by default, tap to expand).
- **Compose row at bottom:**
  - Multi-line `TextField` for input.
  - "Send" `IconButton` — disabled while empty or while session is `Running`.
  - When session is `Running`: "Send" becomes "Cancel turn" (red, stop icon). Tap → `remote.solution_agent.cancel_turn`.
  - When session is `AwaitingInput`: a banner "Tool requires approval — open on your computer". (Approval flow defer to R-6.)

### Streaming wiring

On screen entry:
1. `client.call("remote.solution_agent.get_session", {id})` to fetch the full message history.
2. `client.call("remote.editor.subscribe", { kinds: ["agent_session_message_appended", "agent_session_state_changed"] })` filtered to *this session*.
3. Render the history; start collecting notifications.

For each `agent_session_message_appended` notification matching this session:
- Append the partial message to the list (deduplicate by message id).
- Auto-scroll to bottom unless the user has scrolled up (then keep their position; show a "Jump to bottom" pill).

State `agent_session_state_changed` updates the compose row (Send vs Cancel vs banner).

Unsubscribe on screen exit.

### Sending a message

`MainViewModel.sendMessage(sessionId, text)`:
1. Optimistically append a `user` bubble.
2. Call `remote.solution_agent.send_message { session_id, message }`.
3. On `"queued"` response (the MCP convention from the server side): the message will be processed when the current turn finishes. The optimistic bubble stays.
4. On error: snackbar + roll back the optimistic bubble.

### Cancel-turn

`MainViewModel.cancelTurn(sessionId)` → `remote.solution_agent.cancel_turn { session_id }`. Optimistic UI: the Cancel button greys out briefly until the next `agent_session_state_changed`.

### Image content blocks

If a message contains `Image` content blocks, render them inline (`Coil` for base64-encoded `data:` URIs). Cap displayed dimensions at 240dp on the long edge. Tap to open a full-screen preview.

## DTO extension in `:core`

`:core` needs `Message`, `ContentBlock` (text / image / tool_call / tool_result) DTOs that round-trip the same JSON shapes spk-editor sends. Use kotlinx.serialization's `polymorphic` discriminator on `type`. Add JSON round-trip tests per DTO variant.

## Out of scope

- Slash commands (`/clear`, `/compact`).
- New-session creation (defer or fold into R-5c if cheap).
- File attachment from phone gallery.
- Editing / deleting prior messages.
- Tool-approval prompts (R-6: needs server-side support too).
- Push notifications when a turn completes — R-6.

## Architectural decisions

1. **Optimistic user bubble** — the user types and Send is pressed; the bubble appears immediately even before the server `send_message` resolves. This matches the existing UX on the spk-editor side (`render_resuming_section`).
2. **Auto-scroll with manual override** — once the user scrolls up, stop auto-scrolling; show a "Jump to bottom" pill so they can opt back in. Standard chat-app pattern.
3. **Server is the source of truth** — when notification arrives, replace the optimistic bubble with the server-acknowledged one (matched by id). If the server never echoes (timeout 30s), surface an error.
4. **Image inline rendering via Coil**, not a separate viewer. Tap → full-screen via `LocalAnimatedContentScope`. Caps applied for memory.

## Verification

```bash
cd /home/spk/.spk/spk-editor/solutions/spk-solutions/spk-editor-android-client
JAVA_HOME=$HOME/.jdks/temurin-21.0.10 ./gradlew :core:test :app:assembleDebug --rerun-tasks 2>&1 | tee /tmp/r5d.txt
grep -E "BUILD SUCCESSFUL|FAILURE:" /tmp/r5d.txt
```

Manual smoke (load-bearing — this is the user-facing feature):
- Pair (R-5b), drill into a session.
- Type "hello" + Send → user bubble appears immediately; agent reply streams in bubble.
- During a long turn, tap Cancel → turn stops, button goes back to Send.
- Send an image (existing session that has an image content block) → image renders inline; tap → full-screen.

## Acceptance

- [ ] `:core:test` + `:app:assembleDebug` BUILD SUCCESSFUL.
- [ ] Manual smoke: full send + receive + cancel cycle works against a live editor.
- [ ] Optimistic bubble lands within 50ms of pressing Send (perception threshold).
- [ ] Stream rendering doesn't drop frames at 20 fps notification rate (typical agent_session_message_appended cadence per the supervisor doc).
- [ ] Cancel button works — the server-side turn actually stops, observed by checking `agent_session_state_changed` arrives.
- [ ] Image content block renders inline; tap opens full-screen viewer.
- [ ] Back nav from session → sessions list still works.

## When done

This closes R-5. Hand off to R-6 (push notifications + reconnect + multi-server) when the user wants it.

Sub-agent reports commit SHA, Compose performance observations (any frame drops on streaming?), Coil version + memory cap chosen, and any server-side schema gap that surfaced (so we can fix on the spk-editor side rather than papering over on the client).

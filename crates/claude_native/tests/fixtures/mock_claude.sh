#!/usr/bin/env bash
# Mock `claude` stream-json harness for claude_native integration tests.
#
# Behavior is driven by env vars so one script covers several test scenarios:
#   MOCK_CLAUDE_CAPTURE   - if set, every received stdin line is appended here
#                           (lets a test assert on what the connection wrote back).
#   MOCK_CLAUDE_CONTROL   - if set, emit a `control_request` (can_use_tool) right
#                           after `init`, then wait for the matching control_response
#                           on stdin before emitting `result`.
#   MOCK_CLAUDE_NO_RESULT - if set, stream text but never emit the final `result`
#                           (the hang scenario).
#   MOCK_CLAUDE_OBEY_INTERRUPT
#                         - if set, the turn streams text but withholds its
#                           `result` until an `interrupt` control_request arrives
#                           on stdin, then emits `result(cancelled)` (the clean
#                           two-stage Stop: interrupt is honored, no kill needed).
#   MOCK_CLAUDE_IGNORE_INTERRUPT
#                         - if set, the turn streams text and never emits a
#                           `result` at all, even after an `interrupt` arrives
#                           (forces the escalation kill+resume path).
#
# On startup it emits the `init` system message (matching the real `claude`
# stream-json binary, which announces the session id before any input). Then on
# the first user message it emits a text delta stream_event, then (unless
# suppressed) a success `result`. It exits when stdin reaches EOF.

emit() { printf '%s\n' "$1"; }

# Real `claude --output-format stream-json` emits `system/init` immediately on
# startup, before reading any input. The connection's `new_session` awaits this
# to learn the canonical session id, so it must not be gated behind a user turn.
emit '{"type":"system","subtype":"init","session_id":"mock-session","uuid":"u-init"}'

while IFS= read -r line; do
  if [ -n "${MOCK_CLAUDE_CAPTURE:-}" ]; then
    printf '%s\n' "$line" >> "$MOCK_CLAUDE_CAPTURE"
  fi

  # Only react to user turns; ignore control responses for stream sequencing.
  case "$line" in
    *'"type":"user"'*)
      if [ -n "${MOCK_CLAUDE_CONTROL:-}" ]; then
        emit '{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"t1","input":{"command":"ls"}}}'
        # Wait for the control_response before finishing the turn.
        while IFS= read -r reply; do
          if [ -n "${MOCK_CLAUDE_CAPTURE:-}" ]; then
            printf '%s\n' "$reply" >> "$MOCK_CLAUDE_CAPTURE"
          fi
          case "$reply" in
            *'"type":"control_response"'*) break ;;
          esac
        done
      fi

      emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}},"uuid":"u1","session_id":"mock-session"}'

      if [ -n "${MOCK_CLAUDE_OBEY_INTERRUPT:-}" ]; then
        # Honor a soft interrupt: hold the turn open until an `interrupt`
        # control_request arrives, then end it with a cancelled `result`.
        while IFS= read -r reply; do
          if [ -n "${MOCK_CLAUDE_CAPTURE:-}" ]; then
            printf '%s\n' "$reply" >> "$MOCK_CLAUDE_CAPTURE"
          fi
          case "$reply" in
            *'"subtype":"interrupt"'*) break ;;
          esac
        done
        emit '{"type":"result","subtype":"success","is_error":false,"result":"","stop_reason":"cancelled","usage":{"input_tokens":1,"output_tokens":0},"uuid":"u2","session_id":"mock-session"}'
      elif [ -n "${MOCK_CLAUDE_IGNORE_INTERRUPT:-}" ]; then
        # Never emit `result`, even after an interrupt: forces the escalation
        # kill+resume path. Keep capturing stdin so the test can assert the
        # interrupt was written before the kill.
        while IFS= read -r reply; do
          if [ -n "${MOCK_CLAUDE_CAPTURE:-}" ]; then
            printf '%s\n' "$reply" >> "$MOCK_CLAUDE_CAPTURE"
          fi
        done
      elif [ -z "${MOCK_CLAUDE_NO_RESULT:-}" ]; then
        emit '{"type":"result","subtype":"success","is_error":false,"result":"Hi","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2},"uuid":"u2","session_id":"mock-session"}'
      fi
      ;;
  esac
done

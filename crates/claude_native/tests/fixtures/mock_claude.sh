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
#
# On the first user message it emits: an `init` system message, a text delta
# stream_event, then (unless suppressed) a success `result`. It exits when stdin
# reaches EOF.

emit() { printf '%s\n' "$1"; }

emitted_init=0

while IFS= read -r line; do
  if [ -n "${MOCK_CLAUDE_CAPTURE:-}" ]; then
    printf '%s\n' "$line" >> "$MOCK_CLAUDE_CAPTURE"
  fi

  # Only react to user turns; ignore control responses for stream sequencing.
  case "$line" in
    *'"type":"user"'*)
      if [ "$emitted_init" -eq 0 ]; then
        emit '{"type":"system","subtype":"init","session_id":"mock-session","uuid":"u-init"}'
        emitted_init=1
      fi

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

      if [ -z "${MOCK_CLAUDE_NO_RESULT:-}" ]; then
        emit '{"type":"result","subtype":"success","is_error":false,"result":"Hi","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2},"uuid":"u2","session_id":"mock-session"}'
      fi
      ;;
  esac
done

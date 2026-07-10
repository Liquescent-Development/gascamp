#!/usr/bin/env bash
# Dual-role claude stand-in for the converse-verb e2e (dispatch-lifecycle
# Phase 1). As campd's [dispatch].command it execs the fake agent (worker
# contract). Invoked with --resume (the CLI's nudge resume path) it records
# its argv + cwd and prints an F2-shaped result envelope.
set -euo pipefail
for arg in "$@"; do
  if [ "$arg" = "--resume" ]; then
    : "${NUDGE_STUB_LOG:?claude-or-agent: NUDGE_STUB_LOG must be set for the resume role}"
    printf 'argv:%s\ncwd:%s\n' "$*" "$(pwd)" > "$NUDGE_STUB_LOG"
    echo '[{"type":"result","is_error":false,"result":"STUB-REPLY","session_id":"stub"}]'
    exit 0
  fi
done
: "${FAKE_AGENT:?claude-or-agent: FAKE_AGENT must point at fake-agent.sh}"
exec "$FAKE_AGENT" "$@"

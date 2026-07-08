#!/usr/bin/env bash
# The fake agent (spec §16): speaks the Gas Camp worker contract via the
# camp CLI exactly as a real worker would — claim → milestones → close —
# with env-controlled outcome, timing, and crashes. campd execs this in
# place of `claude` ([dispatch].command — visible config, not a fallback);
# claude-style argv is accepted and ignored, the contract inputs arrive in
# CAMP_* env vars (Phase 8 plan decision J).
#
# Behavior env (all optional):
#   FAKE_AGENT_MILESTONE  emit this milestone text after claiming
#   FAKE_AGENT_CRASH      "kill" = SIGKILL yourself; any number = exit code,
#                         both BEFORE closing the bead (mid-work crash)
#   FAKE_AGENT_HOLD_DIR   after claiming, wait until $DIR/$CAMP_BEAD exists
#                         (deterministic concurrency tests)
#   FAKE_AGENT_TOUCH      write this file (relative to cwd) to prove where
#                         the worker ran (worktree tests)
#   FAKE_AGENT_OUTCOME    close outcome, default "pass"
set -euo pipefail

: "${CAMP_BIN:?fake-agent: CAMP_BIN must point at the camp binary}"
: "${CAMP_DIR:?fake-agent: CAMP_DIR must be set by campd}"
: "${CAMP_BEAD:?fake-agent: CAMP_BEAD must be set by campd}"
: "${CAMP_SESSION:?fake-agent: CAMP_SESSION must be set by campd}"

"$CAMP_BIN" claim "$CAMP_BEAD" --session "$CAMP_SESSION"

if [[ -n "${FAKE_AGENT_TOUCH:-}" ]]; then
  echo "worked in $(pwd)" > "$FAKE_AGENT_TOUCH"
fi

if [[ -n "${FAKE_AGENT_MILESTONE:-}" ]]; then
  "$CAMP_BIN" event emit "$FAKE_AGENT_MILESTONE" --bead "$CAMP_BEAD" --session "$CAMP_SESSION"
fi

if [[ -n "${FAKE_AGENT_CRASH:-}" ]]; then
  case "$FAKE_AGENT_CRASH" in
    kill) kill -KILL $$ ;;
    *) exit "$FAKE_AGENT_CRASH" ;;
  esac
fi

if [[ -n "${FAKE_AGENT_HOLD_DIR:-}" ]]; then
  # Test-harness gate, not camp machinery: camp never polls; this script is
  # the stand-in for a model thinking. Bounded (plan-review note 3): a test
  # that dies before writing the gate file must not leave this loop
  # spinning after tempdir cleanup.
  tries=0
  until [[ -e "$FAKE_AGENT_HOLD_DIR/$CAMP_BEAD" ]]; do
    sleep 0.05
    tries=$((tries + 1))
    if [ "$tries" -gt 1200 ]; then
      echo "fake-agent: hold gate never opened for $CAMP_BEAD (60s)" >&2
      exit 97
    fi
  done
fi

"$CAMP_BIN" close "$CAMP_BEAD" --outcome "${FAKE_AGENT_OUTCOME:-pass}" --reason "fake agent done"

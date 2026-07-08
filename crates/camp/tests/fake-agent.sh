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
#   FAKE_AGENT_NUDGE_CLOSE     Phase 11 stream-mode contract: line 1 on
#                              stdin is the task message; block until a
#                              LATER line (a patrol nudge) arrives, then
#                              close — the nudge-revival proof
#   FAKE_AGENT_TOUCH_TRANSCRIPT_LOOP  Phase 11: N iterations of appending
#                              to $CAMP_TRANSCRIPT every 250 ms after the
#                              claim — a working agent's heartbeat
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

if [[ -n "${FAKE_AGENT_TOUCH_TRANSCRIPT_LOOP:-}" ]]; then
  # The transcript heartbeat a real claude produces for free (A4-1): the
  # stall timer must keep resetting while this loop runs.
  : "${CAMP_TRANSCRIPT:?fake-agent: CAMP_TRANSCRIPT must be set by campd}"
  mkdir -p "$(dirname "$CAMP_TRANSCRIPT")"
  i=0
  while [ "$i" -lt "$FAKE_AGENT_TOUCH_TRANSCRIPT_LOOP" ]; do
    echo "heartbeat $i" >> "$CAMP_TRANSCRIPT"
    sleep 0.25
    i=$((i + 1))
  done
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

if [[ -n "${FAKE_AGENT_NUDGE_CLOSE:-}" ]]; then
  # Stream-mode contract (Phase 11 Decision C): campd wrote the TASK as
  # the first stdin line at spawn; a LATER line is a patrol nudge. Block
  # silently (no transcript writes = a stalled worker) until nudged, then
  # fall through to the close — the revival the master plan demands.
  read -r _task_line
  read -r _nudge_line
fi

# Close spec (Phase 9): FAKE_AGENT_PLAN names a file whose FIRST line is
# consumed per invocation — "pass", "fail", or "fail-transient", optionally
# followed by "output=<json-file>". Attempts of one looping step are
# strictly sequential (the next attempt exists only after the previous
# close), so the pop is race-free in these tests. An empty/missing plan
# falls through to FAKE_AGENT_OUTCOME.
outcome="${FAKE_AGENT_OUTCOME:-pass}"
transient=""
output_json="${FAKE_AGENT_OUTPUT_JSON:-}"
if [[ -n "${FAKE_AGENT_PLAN:-}" && -s "$FAKE_AGENT_PLAN" ]]; then
  line="$(head -n 1 "$FAKE_AGENT_PLAN")"
  tail -n +2 "$FAKE_AGENT_PLAN" > "$FAKE_AGENT_PLAN.tmp"
  mv "$FAKE_AGENT_PLAN.tmp" "$FAKE_AGENT_PLAN"
  for word in $line; do
    case "$word" in
      pass) outcome="pass" ;;
      fail) outcome="fail" ;;
      fail-transient) outcome="fail"; transient="yes" ;;
      output=*) output_json="${word#output=}" ;;
      *) echo "fake-agent: unknown plan word $word" >&2; exit 96 ;;
    esac
  done
fi

close_args=(close "$CAMP_BEAD" --outcome "$outcome" --reason "fake agent done")
if [[ -n "$transient" ]]; then
  close_args+=(--transient)
fi
if [[ -n "$output_json" ]]; then
  close_args+=(--output-json "$output_json")
fi
"$CAMP_BIN" "${close_args[@]}"

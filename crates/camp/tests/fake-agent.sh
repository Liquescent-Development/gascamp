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
#                         the worker ran (worktree tests); written BEFORE
#                         the claim so ledger-observed claims imply it
#   FAKE_AGENT_RECORD_BRANCH  write `git branch --show-current` (as seen
#                         from the worker's own cwd) to this file — the
#                         Phase 2 isolation evidence; written BEFORE the
#                         claim, same ordering contract as FAKE_AGENT_TOUCH
#   FAKE_AGENT_OUTCOME    close outcome, default "pass"
#   FAKE_AGENT_NUDGE_CLOSE     Phase 11 stream-mode contract: line 1 on
#                              stdin is the task message; block until a
#                              LATER line (a patrol nudge) arrives, then
#                              close — the nudge-revival proof
#   FAKE_AGENT_TOUCH_TRANSCRIPT_LOOP  Phase 11: N iterations of appending
#                              to $CAMP_TRANSCRIPT every 250 ms after the
#                              claim — a working agent's heartbeat
#   FAKE_AGENT_DELIVERY   Phase 3 delivery modes (obligations i/ii/vi):
#                         "ship" = commit on the dispatched branch, close
#                         pass+shipped with the real commit/branch facts;
#                         "deadend" = the #34 scenario — root commit on a
#                         stray branch of a baseless rig, shipped MUST be
#                         rejected (exit 96 if the gate accepts), then
#                         close fail+blocked; "blocked" = commit, then
#                         close fail+blocked (worktree/branch kept)
set -euo pipefail

: "${CAMP_BIN:?fake-agent: CAMP_BIN must point at the camp binary}"
: "${CAMP_DIR:?fake-agent: CAMP_DIR must be set by campd}"
: "${CAMP_BEAD:?fake-agent: CAMP_BEAD must be set by campd}"
: "${CAMP_SESSION:?fake-agent: CAMP_SESSION must be set by campd}"

# The cwd proof precedes the claim ON PURPOSE (issue #44): tests wait for
# bead.claimed in the ledger and then assert this file exists, so the touch
# must happen-before the claim event — bash program order plus the claim's
# durable commit make that ordering observable. Touch-after-claim raced the
# test's ledger poll against this script's scheduling and flaked under
# parallel load.
if [[ -n "${FAKE_AGENT_TOUCH:-}" ]]; then
  echo "worked in $(pwd)" > "$FAKE_AGENT_TOUCH"
fi

if [[ -n "${FAKE_AGENT_RECORD_BRANCH:-}" ]]; then
  # Isolation evidence (Phase 2, dispatch-lifecycle §9 obligation i): the
  # WORKER records the branch of its own cwd — not the test guessing.
  # Written BEFORE the claim (issue #44 ordering contract): a
  # ledger-observed claim implies every proof file already exists.
  git branch --show-current > "$FAKE_AGENT_RECORD_BRANCH"
fi

# cp-0: the camp CLI's human output (e.g. "claimed gc-1") must NOT pollute the
# worker's stdout file — that file is campd's stream-json tail target (spec
# §2.3), and real claude --verbose writes ONLY stream-json to it. Redirect
# the camp CLI's stdout to stderr (campd's stderr, visible in test logs) so
# the stdout file stays stream-json-clean.
"$CAMP_BIN" claim "$CAMP_BEAD" --session "$CAMP_SESSION" 1>&2

if [[ -n "${FAKE_AGENT_MILESTONE:-}" ]]; then
  "$CAMP_BIN" event emit "$FAKE_AGENT_MILESTONE" --bead "$CAMP_BEAD" --session "$CAMP_SESSION" 1>&2
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

# Phase 3 delivery modes (dispatch-lifecycle §9 obligations i/ii/vi).
# GITC pins identity/hermeticity for commits made by the fake worker.
GITC=(-c user.email=fake@agent -c user.name=fake-agent -c commit.gpgsign=false)
if [[ "${FAKE_AGENT_DELIVERY:-}" = "ship" ]]; then
  # Obligation (ii): commit on the branch campd dispatched us onto
  # (camp/<bead> in a worktree) and close shipped with the real facts.
  git "${GITC[@]}" commit --allow-empty -m "fake ship for $CAMP_BEAD"
  ship_commit="$(git rev-parse HEAD)"
  ship_branch="$(git rev-parse --abbrev-ref HEAD)"
  "$CAMP_BIN" close "$CAMP_BEAD" --outcome pass --reason "shipped by fake agent" \
    --work-outcome shipped --work-commit "$ship_commit" --work-branch "$ship_branch" 1>&2
  exit 0
fi
if [[ "${FAKE_AGENT_DELIVERY:-}" = "deadend" ]]; then
  # Obligation (i): the #34 scenario — a root commit on a stray branch of
  # a baseless rig. The shipped close MUST be rejected by the gate; the
  # honest record is fail+blocked. If the gate ever accepts, exit 96 so
  # the test fails loudly (never silence the hole).
  git "${GITC[@]}" checkout -b add-readme
  echo "readme" > README.md
  git "${GITC[@]}" add README.md
  git "${GITC[@]}" commit -m "dead-end readme"
  dead_commit="$(git rev-parse HEAD)"
  if "$CAMP_BIN" close "$CAMP_BEAD" --outcome pass --reason "should be rejected" \
       --work-outcome shipped --work-commit "$dead_commit" --work-branch add-readme 1>&2; then
    echo "fake-agent: THE SHIPPED GATE ACCEPTED A DEAD-END COMMIT" >&2
    exit 96
  fi
  "$CAMP_BIN" close "$CAMP_BEAD" --outcome fail \
    --reason "no base: the branch cannot land" --work-outcome blocked 1>&2
  exit 0
fi
if [[ "${FAKE_AGENT_DELIVERY:-}" = "blocked" ]]; then
  # Obligation (vi): committed-but-unlandable work closes blocked; the
  # worktree and bead branch must survive for forensics.
  git "${GITC[@]}" commit --allow-empty -m "half-done work for $CAMP_BEAD"
  "$CAMP_BIN" close "$CAMP_BEAD" --outcome fail \
    --reason "cannot land: blocked by fake scenario" --work-outcome blocked 1>&2
  exit 0
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
"$CAMP_BIN" "${close_args[@]}" 1>&2

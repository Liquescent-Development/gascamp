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
#   FAKE_AGENT_CONTROL_LOOP    cp-1 (§2.1): read stdin forever; ANSWER every
#                              control_request with the pinned control_response
#                              (request_id echoed back); a plain user turn ends
#                              the loop and closes the bead. The interrupt
#                              round-trip's worker half.
#   FAKE_AGENT_EXIT_AFTER_CONTROL  cp-1: answer ONE control_request and EXIT
#                              IMMEDIATELY — the reap-races-the-drain shape.
#                              The answer is the worker's LAST stdout bytes, so
#                              a reap-before-drain bug destroys it unread.
#   FAKE_AGENT_SPAM_ON_TURN=N  cp-1 (§4.4 backpressure): on a USER TURN, emit N
#                              stream-json lines. The spam must come AFTER the
#                              subscriber is registered, or the backpressure gate
#                              tests nothing.
#   FAKE_AGENT_HUGE_LINE=N     cp-1 (G1): emit ONE stream-json line whose payload
#                              is N bytes — a SINGLE line far larger than
#                              HISTORY_CHUNK_BYTES (64 KiB) and, at N >= 1 MiB,
#                              larger than the whole subscriber cap.
#
#     WHY HUGE_LINE EXISTS: **every other fixture in this repo emits SHORT lines**
#     — `emit_stream` is `printf '%s\n'` and nothing anywhere produces a line
#     bigger than a few hundred bytes. That is precisely WHY a pump that livelocks
#     on any line > 64 KiB was invisible to the entire suite, and why a real
#     Read/Bash/Grep tool-result line — which routinely exceeds 64 KiB — would have
#     hung campd in production while CI stayed green. Without this mode no gate in
#     this phase can see the phase's worst bug.
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

# cp-0 (review fix 5): a real `claude --verbose` worker writes stream-json
# (NDJSON, one object per line) to its stdout for its whole life — and THAT
# FILE IS campd's read channel (spec §2.3). The fake worker must write it
# too. Without this the fake agent emits nothing on stdout, so no test ever
# has a worker produce output and exit — which is precisely the lifecycle the
# read channel exists to serve, and precisely the path a reap-before-drain
# bug destroys. (The camp CLI's own human output is separately redirected to
# stderr below: real claude does not print "claimed gc-1" on stdout.)
emit_stream() { printf '%s\n' "$1"; }

# The terminal line, emitted on EVERY exit path (the trap covers the delivery
# modes' early `exit`s too). A real worker's last stdout bytes carry the
# `result` envelope — the bytes most likely to be lost to a reap-before-drain
# race, since they are written immediately before the process dies.
#   FAKE_AGENT_FINAL_STDOUT  override the terminal line with a raw string.
#                            The read-channel lifecycle test sets a
#                            deliberately NON-JSON value and asserts campd
#                            drained it (a patrol.degraded names it) even
#                            though the worker exited right after writing it.
on_exit() {
  local code=$?
  if [[ -n "${FAKE_AGENT_FINAL_STDOUT:-}" ]]; then
    printf '%s\n' "$FAKE_AGENT_FINAL_STDOUT"
  else
    printf '{"type":"result","subtype":"success","is_error":false,"session_id":"%s"}\n' \
      "$CAMP_SESSION"
  fi
  exit "$code"
}
trap on_exit EXIT

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

# The stream-json a real worker emits as it starts (F2's `system/init`).
emit_stream "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$CAMP_SESSION\"}"

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

# cp-1: answer camp's control_requests, exactly as the real CLI does. The
# request_id is echoed back verbatim — that correlation IS the protocol, and
# the response shape is the one pinned in tests/fixtures/control/.
answer_control() {
  local line="$1"
  local id
  id="$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')"
  [[ -n "$id" ]] || return 0
  printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"still_queued":[]}}}\n' "$id"
}

if [[ -n "${FAKE_AGENT_CONTROL_LOOP:-}" ]]; then
  # cp-1: campd wrote the TASK as the first stdin line at spawn (the HeldStream
  # contract) — consume it, exactly as FAKE_AGENT_NUDGE_CLOSE does. THEN read
  # forever: a control_request is ANSWERED; a plain user turn ends the loop and
  # falls through to the close.
  #
  # This is the shape every interrupt round-trip test drives: campd writes a
  # control line into the held stdin, and the answer comes back out on stdout —
  # which IS campd's read channel (spec §2.3). The whole cp-1 loop, in a worker.
  read -r _task_line
  while IFS= read -r _control_line; do
    case "$_control_line" in
      *'"type":"control_request"'*)
        # cp-1 (B6): FAKE_AGENT_CONTROL_ANSWER_DELAY makes the answer land at a
        # time the TEST chooses. The restart proof needs campd to die BEFORE the
        # answer exists — otherwise campd may ingest it first and rehydration is
        # never exercised. The line is already READ, so campd's death during this
        # sleep does not stop the answer from being written.
        sleep "${FAKE_AGENT_CONTROL_ANSWER_DELAY:-0}"
        answer_control "$_control_line"
        ;;
      *) break ;;  # a user turn: stop listening and close the bead
    esac
  done
  # stdin hit EOF (campd released the pipe — or DIED). FAKE_AGENT_LINGER_ON_EOF
  # makes the worker OUTLIVE campd instead of exiting, which is what B6's restart
  # proof needs: the session must still be LIVE when the new campd starts, or
  # adoption crashes it, it is never re-tailed, and the worker's answer — already
  # sitting in its stdout file — is never read. (A worker that exits with campd is
  # B6's NAMED residual, not its happy path.)
  if [[ -n "${FAKE_AGENT_LINGER_ON_EOF:-}" ]]; then
    sleep "$FAKE_AGENT_LINGER_ON_EOF"
    exit 0
  fi
fi

# cp-1: ONE genuinely huge line, on one line, valid stream-json.
huge_line() {
  local n="$1" pad
  pad="$(head -c "$n" /dev/zero | tr '\0' 'x')"
  printf '{"type":"assistant","message":{"role":"assistant","content":"%s"}}\n' "$pad"
}

if [[ -n "${FAKE_AGENT_HUGE_LINE:-}" ]]; then
  huge_line "$FAKE_AGENT_HUGE_LINE"
  emit_stream '{"type":"assistant","message":{"role":"assistant","content":"after the monster"}}'
  # Stay alive so the session keeps being tailed while the subscriber catches up.
  sleep "${FAKE_AGENT_HUGE_LINE_LINGER:-30}"
  exit 0
fi

if [[ -n "${FAKE_AGENT_SPAM_ON_TURN:-}" ]]; then
  # cp-1 (§4.4): the spam lands AFTER a user turn, so a test can register its
  # subscriber first and THEN make the worker produce a backlog.
  read -r _task_line
  read -r _turn_line
  i=0
  while [ "$i" -lt "$FAKE_AGENT_SPAM_ON_TURN" ]; do
    printf '{"type":"assistant","message":{"role":"assistant","content":"spam %d"}}\n' "$i"
    i=$((i + 1))
  done
  sleep "${FAKE_AGENT_SPAM_LINGER:-30}"
  exit 0
fi

if [[ -n "${FAKE_AGENT_CLOSE_STDIN:-}" ]]; then
  # cp-1 (C12): the worker CLOSES its stdin read end and stays alive. campd still
  # holds the WRITE end, so its next write into that pipe gets EPIPE — a write
  # that is ATTEMPTED and FAILS, which is a different thing from "no pipe" and
  # must be loud in BOTH channels (an error to the caller AND a durable fault).
  #
  # This is the deterministic way to drive ControlWrite::Failed: a FULL pipe
  # cannot be used, because the first write that fails TEARS the pipe down, so
  # any later interrupt would report NoPipe instead.
  exec 0<&-
  # The HAPPENS-BEFORE marker. The test waits for this line in the stdout file
  # before interrupting — without it, campd can deliver the interrupt while the
  # pipe is still open, the write SUCCEEDS, and the test flakes.
  emit_stream '{"type":"system","subtype":"stdin_closed"}'
  sleep "${FAKE_AGENT_CLOSE_STDIN}"
  exit 0
fi

if [[ -n "${FAKE_AGENT_EXIT_AFTER_CONTROL:-}" ]]; then
  # cp-1: consume the task line, answer exactly ONE control_request, and EXIT
  # IMMEDIATELY — the reap-races-the-drain shape. The answer is the worker's LAST
  # stdout bytes, written a breath before the process dies, so it is precisely
  # the line a reap-before-drain bug destroys. If campd's harvest ordering is
  # wrong, this answer is unlinked unread and the interrupt looks unanswered
  # forever.
  read -r _task_line
  IFS= read -r _control_line || true
  answer_control "$_control_line"
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

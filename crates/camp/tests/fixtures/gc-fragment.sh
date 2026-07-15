#!/bin/sh
# A FAITHFUL synthetic of the real gc-role-worker fragment, built from Task 1's
# recording (ci/gc-compat/fixtures/gc-role-worker.observed.json). It is NOT the
# corpus fragment's copyrighted source (compat §10) — it reproduces the OBSERVED
# contract: the same verbs, the same python3 json_pick, the same assignee/route
# comparisons, the same happy + fail-close branches, the same exit shape.
#
# It runs against camp's REAL gc/bd shims (on PATH via .camp/bin). It EXITS
# normally after drain-ack; the fake-claude wrapper is what LINGERS (a real
# `claude -p` does not exit on task completion / stdin EOF, P3), so campd's
# drain-ack → KillReleased is what reaps the worker.
#
# GC_FRAGMENT_MODE=happy (default) | fail  — drive the fail-close branch.
set +e

EXPECTED_ASSIGNEE="${BEADS_ACTOR:-${GC_SESSION_NAME:-${GC_SESSION_ID:-${GC_AGENT:-}}}}"
EXPECTED_ROUTE="${GC_TEMPLATE:-${GC_AGENT:-}}"

if [ -z "$EXPECTED_ASSIGNEE" ]; then
  echo "CONFIG_REJECTED missing expected assignee"
  gc runtime drain-ack
  exit 0
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "CONFIG_REJECTED missing python3"
  gc runtime drain-ack
  exit 0
fi

json_pick() {
  python3 -c '
import json
import sys

path = sys.argv[1]
try:
    data = json.load(sys.stdin)
except Exception:
    print("")
    raise SystemExit(0)

if isinstance(data, list):
    data = data[0] if data else {}
if not isinstance(data, dict):
    print("")
    raise SystemExit(0)

if path.startswith("metadata:"):
    key = path.split(":", 1)[1]
    metadata = data.get("metadata") or {}
    value = metadata.get(key, "") if isinstance(metadata, dict) else ""
else:
    value = data.get(path, "")

if value is None:
    value = ""
print(value if isinstance(value, str) else str(value))
' "$1"
}

WORK_ID=""
tries=0
while true; do
  tries=$((tries + 1))
  if [ "$tries" -gt 20 ]; then
    echo "CLAIM_GAVE_UP" >&2
    exit 1
  fi
  CLAIM_JSON="$(gc hook --claim --json)"
  CLAIM_ACTION="$(printf '%s' "$CLAIM_JSON" | json_pick action)"
  WORK_ID="$(printf '%s' "$CLAIM_JSON" | json_pick bead_id)"

  if [ "$CLAIM_ACTION" = "drain" ]; then
    echo "NO_ROUTED_WORK"
    gc runtime drain-ack
    exit 0
  fi
  if [ "$CLAIM_ACTION" != "work" ] || [ -z "$WORK_ID" ]; then
    sleep 1
    continue
  fi

  SHOW_JSON="$(bd show "$WORK_ID" --json)"
  SHOW_ASSIGNEE="$(printf '%s' "$SHOW_JSON" | json_pick assignee)"
  SHOW_ROUTE="$(printf '%s' "$SHOW_JSON" | json_pick metadata:gc.routed_to)"
  # The load-bearing equalities: bd show's assignee is the SESSION (== env
  # BEADS_ACTOR), its gc.routed_to is the cooked route (== env GC_TEMPLATE).
  if [ -n "$EXPECTED_ASSIGNEE" ] && [ "$SHOW_ASSIGNEE" != "$EXPECTED_ASSIGNEE" ]; then
    echo "CLAIM_REJECTED assignee mismatch"
    sleep 1
    continue
  fi
  if [ -n "$EXPECTED_ROUTE" ] && [ -n "$SHOW_ROUTE" ] && [ "$SHOW_ROUTE" != "$EXPECTED_ROUTE" ]; then
    echo "CLAIM_REJECTED route mismatch"
    sleep 1
    continue
  fi
  break
done

printf 'CLAIMED_BEAD_ID=%s\n' "$WORK_ID"

# work + close (fragment prose: set gc.outcome, then close the same id).
if [ "${GC_FRAGMENT_MODE:-happy}" = "fail" ]; then
  bd update "$WORK_ID" --set-metadata 'gc.outcome=fail' --set-metadata 'gc.failure_class=work_error'
  bd close "$WORK_ID" --reason 'work failed'
else
  bd update "$WORK_ID" --set-metadata 'gc.outcome=pass'
  bd close "$WORK_ID"
fi

# continuation: re-hook; the bead is closed now, so this drains → drain-ack.
CLAIM_JSON="$(gc hook --claim --json)"
CLAIM_ACTION="$(printf '%s' "$CLAIM_JSON" | json_pick action)"
if [ "$CLAIM_ACTION" = "drain" ]; then
  echo "NO_ROUTED_WORK"
fi
gc runtime drain-ack
exit 0

#!/bin/sh
# OPTIONAL PostToolUse breadcrumb — OFF BY DEFAULT (deliberately NOT
# registered in hooks.json). Patrol watches transcripts instead (spec §10),
# so tool-level detail stays out of the event log (§7.6). Provided for
# operators who want explicit breadcrumbs; enable by adding a PostToolUse
# entry to a settings hooks config. Throttled so frequent tool use does not
# flood the ledger. Fire-and-forget — always exit 0.
. "$(dirname "$0")/lib.sh"

INPUT=$(cat)
window="${CAMP_BREADCRUMB_THROTTLE:-5}"
throttle breadcrumb "$window" || exit 0
tool=$(printf '%s' "$INPUT" | sed -n 's/.*"tool_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$tool" ] || tool="tool"
camp_or_note event emit "tool used: $tool"
exit 0

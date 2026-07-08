#!/bin/sh
# SessionEnd hook: mark the attended session ended. --if-registered makes
# it a clean no-op if the session was never live (fire-and-forget robustness
# — an unknown session must not error). Always exit 0.
#
# NOTE: this is wired to SessionEnd only, NOT Stop (which fires per turn)
# and NOT SubagentStop (whose session_id is the PARENT — ending it would
# kill the attended session, a spec §10 violation). See the plan's D5.
. "$(dirname "$0")/lib.sh"

INPUT=$(cat)
printf '%s' "$INPUT" | camp_or_note session end --hook-stdin --if-registered
exit 0

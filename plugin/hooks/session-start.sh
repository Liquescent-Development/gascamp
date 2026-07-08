#!/bin/sh
# SessionStart hook: register this attended session (idempotent) and
# reconcile the registry against reality. Fire-and-forget — always exit 0.
# camp derives the registry name attended/<session_id> from the payload.
. "$(dirname "$0")/lib.sh"

INPUT=$(cat)
printf '%s' "$INPUT" | camp_or_note session register --hook-stdin
camp_or_note adopt
exit 0

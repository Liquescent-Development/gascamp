#!/bin/sh
# Camp fleet statusline snippet (opt-in). Renders `▲live ●ready ✖red` from a
# read-only campd socket query. It NEVER auto-starts campd and degrades to
# empty output + a stderr note when campd is down (spec §11) — the badge
# logic and degradation live in `camp top --statusline` (tested Rust); this
# script just locates the workspace so `camp` resolves the right camp.
#
# This is the MAIN session status line. A plugin cannot auto-set the main
# `statusLine`, so wire it into your own settings.json:
#   "statusLine": { "type": "command",
#                   "command": "\"${CLAUDE_PLUGIN_ROOT}\"/statusline/statusline.sh" }
#
# (`subagentStatusLine` is a DIFFERENT, per-subagent slot with a different
# stdin schema — a `tasks` array rendering one row body per teammate — so it
# is not wired to this fleet-wide badge script.)

INPUT=$(cat)
# Prefer the workspace's current_dir, else the top-level cwd, so `camp`
# walks up to the right .camp when CAMP_DIR is not set.
dir=$(printf '%s' "$INPUT" | sed -n 's/.*"current_dir"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$dir" ] || dir=$(printf '%s' "$INPUT" | sed -n 's/.*"cwd"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$dir" ] && cd "$dir" 2>/dev/null

exec camp top --statusline

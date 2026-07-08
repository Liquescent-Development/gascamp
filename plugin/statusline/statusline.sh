#!/bin/sh
# Camp fleet statusline snippet (opt-in). Renders `▲live ●ready ✖red` from a
# read-only campd socket query. It NEVER auto-starts campd and degrades to
# empty output + a stderr note when campd is down (spec §11) — the badge
# logic and degradation live in `camp top --statusline` (tested Rust); this
# script just locates the workspace so `camp` resolves the right camp.
#
# Wire it in your settings.json:
#   "statusLine": { "type": "command",
#                   "command": "\"${CLAUDE_PLUGIN_ROOT}\"/statusline/statusline.sh" }

INPUT=$(cat)
# Prefer the workspace's current_dir, else the top-level cwd, so `camp`
# walks up to the right .camp when CAMP_DIR is not set.
dir=$(printf '%s' "$INPUT" | sed -n 's/.*"current_dir"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$dir" ] || dir=$(printf '%s' "$INPUT" | sed -n 's/.*"cwd"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$dir" ] && cd "$dir" 2>/dev/null

exec camp top --statusline

#!/bin/sh
# Shared helpers for the camp plugin hooks. Every hook is fire-and-forget:
# it MUST always exit 0 and never block the session. Failures are visible
# (a stderr note), never silent (spec §16, and camp's fail-fast/no-silence
# rule adapted to hooks: degrade visibly, do not hang a session no one is
# watching). JSON parsing lives in `camp ... --hook-stdin` (Rust) — these
# scripts stay trivial and need no `jq`.

# Run `camp` with the given args, forwarding stdin. On failure, print a
# visible note to stderr; ALWAYS return 0 so the hook never blocks.
camp_or_note() {
  if ! camp "$@"; then
    echo "camp hook: \`camp $*\` failed (session not blocked)" >&2
  fi
  return 0
}

# throttle KEY WINDOW_SECONDS
#   return 0 (proceed) when KEY has not fired within the last WINDOW
#   seconds; return 1 (skip) otherwise. WINDOW=0 disables throttling
#   (always proceeds). Markers are timestamps written with `date +%s`
#   (portable) under the camp dir.
throttle() {
  key=$1
  window=$2
  dir="${CAMP_DIR:-${TMPDIR:-/tmp}}/hook-throttle"
  mkdir -p "$dir" 2>/dev/null || return 0
  marker="$dir/$key"
  now=$(date +%s)
  if [ -f "$marker" ]; then
    last=$(cat "$marker" 2>/dev/null || echo 0)
    if [ "$((now - last))" -lt "$window" ]; then
      return 1
    fi
  fi
  echo "$now" >"$marker"
  return 0
}

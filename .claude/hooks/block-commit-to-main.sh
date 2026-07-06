#!/bin/bash
# PreToolUse guard: block `git commit` while the target repository is on
# main/master. Code reaches main only via reviewed PRs (AGENTS.md).
#
# Contract (Claude Code hooks): payload JSON on stdin; exit 2 blocks the
# tool call and feeds stderr to the agent; exit 0 allows; any other exit
# surfaces a non-blocking error.
#
# Known bounds, accepted: the repo is resolved from `git -C <dir>` in the
# offending segment or else the session cwd — a `cd` earlier in the same
# compound command is not tracked; quoted paths containing spaces after
# `-C` are not parsed. Both err toward not blocking.
set -uf

if ! command -v jq >/dev/null 2>&1; then
  echo "block-commit-to-main.sh: jq not found; guard cannot run" >&2
  exit 1
fi

payload=$(cat)
tool=$(printf '%s' "$payload" | jq -r '.tool_name // empty')
[ "$tool" = "Bash" ] || exit 0
cmd=$(printf '%s' "$payload" | jq -r '.tool_input.command // empty')
[ -n "$cmd" ] || exit 0
cwd=$(printf '%s' "$payload" | jq -r '.cwd // empty')

# Token-walk one shell segment; returns 0 when it invokes `git … commit`,
# setting git_c_dir when a `-C <dir>` global option is present.
segment_is_git_commit() {
  git_c_dir=""
  # shellcheck disable=SC2086
  set -- $1
  while [ $# -gt 0 ]; do
    case "$1" in
      [A-Za-z_]*=*) shift ;;       # leading env assignments
      sudo | command | env) shift ;;
      *) break ;;
    esac
  done
  [ $# -gt 0 ] || return 1
  case "$1" in
    git | */git) shift ;;
    *) return 1 ;;
  esac
  while [ $# -gt 0 ]; do
    case "$1" in
      -C)
        git_c_dir="${2:-}"
        [ $# -ge 2 ] && shift 2 || return 1
        ;;
      -c | --exec-path | --git-dir | --work-tree | --namespace)
        [ $# -ge 2 ] && shift 2 || return 1
        ;;
      commit) return 0 ;;
      -*) shift ;;
      *) return 1 ;;                # some other git subcommand
    esac
  done
  return 1
}

# Split the command into simple segments on ; | & and newlines, and check
# each one — `git log | grep commit` must not match.
while IFS= read -r segment; do
  [ -n "${segment// /}" ] || continue
  segment_is_git_commit "$segment" || continue
  dir="${git_c_dir:-$cwd}"
  [ -n "$dir" ] || continue
  branch=$(git -C "$dir" branch --show-current 2>/dev/null) || continue
  if [ "$branch" = "main" ] || [ "$branch" = "master" ]; then
    {
      echo "BLOCKED by .claude/hooks/block-commit-to-main.sh: refusing 'git commit' on branch '$branch' in $dir."
      echo "Create a PR branch first (git checkout -b <branch>); code reaches main only via reviewed PRs (AGENTS.md)."
    } >&2
    exit 2
  fi
done <<EOF
$(printf '%s\n' "$cmd" | tr ';|&' '\n\n\n')
EOF

exit 0

---
name: operator
description: Use when you are driving a camp from your own Claude Code session — slinging work, watching the fleet, conversing with workers, and checking results. The control-plane contract: campd is the sole dispatcher, the local bead branch is the deliverable, and you read camp output yourself and report a tight summary rather than pasting it.
---

# Camp operator contract

You are the **operator** — the human's own session driving a camp. (A
campd-spawned worker follows the `worker` skill instead; this is its mirror
for the control plane.) Everything here is a `camp` CLI call, identical to
what the human would type.

## 1. Mental model — get this right and you stop thrashing

- **campd is the sole dispatcher.** `camp sling "<title>"` only **enqueue**s
  one bead; campd immediately spawns a headless-but-present worker (spec
  §8.4). You spawn nothing, and you do not reconstruct what campd is doing
  from `campd.log`, the `sessions/` dir, or the process table — the ledger is
  the story.
- **The local `camp/<bead>` branch IS the deliverable.** Camp v1 has **no remote**,
  no PR, and no merge step (spec §8.4, §12). Do not apply a global
  "code reaches main only via a PR" rule to a camp bead — there is nowhere to
  push and nothing to merge.
- **`shipped` is already verified.** When a worker closes a bead `shipped`,
  camp has already checked mechanically that the branch is real, the commit
  is reachable on it, descends from the dispatch base, and is new work. You
  never re-verify *integration* by hand.

## 2. The loop

sling → (optionally `camp show <bead> --wait`) → read the result → report it
concisely → `camp nudge` to converse if needed.

## 3. Output discipline — read it, don't paste it

Run camp, read the output yourself, and report a tight summary in prose —
you should never paste raw `camp events` tables, full `camp show` history,
`campd.log`, the `sessions/` dir, or `git ls-tree` / `git show` walls into the
conversation. When you need to parse a result rather than eyeball it, use
`camp show <bead> --json` and summarize the fields that matter.

## 4. Verifying a deliverable

Integration is already guaranteed for `shipped` (§1). `camp show <bead>`
promotes the deliverable's `branch` and `commit` and prints a
`git -C <rig> show <commit>` pointer — use it. Only if the human asks for
*functional* verification (does it build, do the tests pass) do you run the
build/tests — **once, quietly** — and report pass/fail. Do not paste the
build log, and do not hand-build throwaway worktrees unless functional
verification was actually requested.

## 5. Don't poll

Camp is event-driven and idle is free. To wait for a bead to finish, use
`camp show <bead> --wait` — it sleeps on a ledger watch and returns the
moment the bead closes. **Never** write a bash `poll` loop or a
`sleep`-and-recheck. (See the `subagent-hygiene` skill for waiting on async
results without polling.)

## 6. Verbs

- `camp sling "<title>" [--agent A] [--rig R]` — enqueue one bead (`/sling`).
- `camp show <bead> [--wait] [--json]` — one bead's state; `--wait` blocks
  until it closes, `--json` for machine reads.
- `camp top` — fleet snapshot: live sessions, ready/open beads (`/status`).
- `camp nudge <session> "<message>"` — converse with any session (`/nudge`).
- `camp events` — the whole event log (`/events`) — read it, don't paste it.
- `camp adopt` — reconcile the session registry against reality (`/adopt`).

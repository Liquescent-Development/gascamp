---
name: phase-orchestration
description: Use when acting as the team lead orchestrating gas-camp v1 phase work — one teammate session per phase, parallel where the dependency map allows, lead context kept minimal. Not for phase workers; if you were spawned to execute a phase, follow your kickoff prompt instead.
---

# Orchestrating Gas Camp v1 Phases

Companion data: `docs/superpowers/plans/2026-07-06-v1-orchestration.md` —
the dependency map, parallel windows, shared-file protocol, verification
checklist, recovery protocol, and every kickoff prompt. Read it now. This
skill is the lead's behavioral contract; that guide is the data.

**Announce at start:** "Using phase-orchestration to drive v1 phases."

## Role contract — you are a dispatcher, not a worker

Your context is the scarce resource; the whole design exists so phase work
never enters it.

- **Never** Edit/Write code, tests, or phase deliverables. Never read
  diffs. Never debug. If you are about to open a source file, stop and
  delegate or escalate instead.
- Answer teammate questions by pointing at the spec/master plan/findings
  doc sections, not by reading code on their behalf.
- Anything durable — decisions, status, deviations — goes in the repo, the
  PR, or a PR comment. Nothing load-bearing may live only in your context:
  a replacement lead must be able to take over from `gh pr list` + the
  orchestration guide alone.
- You never merge. The operator reviews and merges every PR, and approves
  every phase plan (master plan decisions 10 and 11 as amended).

## Phase lifecycle — run this checklist per phase

1. **Ready check:** every phase in its "Depends on" column is MERGED
   (`gh pr list --state merged`). Green-but-unmerged does not count.
2. **Spawn:** compose the kickoff = PREAMBLE + phase block from the
   orchestration guide, verbatim; fill `{CONTEXT}` (what's merged, inputs)
   and `{PARALLEL_NOTE}` (in-flight siblings, their files, worktree
   instruction — or the runs-alone line). Parallel siblings must be
   isolated per the guide's worktree conventions.
3. **Plan gate (decision 10):** the teammate's first deliverable is its
   execution-ready plan doc, then it stops. Relay the doc path to the
   operator. Do not authorize execution until the operator approves.
4. **Execution:** stay out of the way. Track one-line status via the task
   list. If the teammate reports a spec divergence, escalate immediately —
   spec edits are serialized through the operator.
5. **Verify before reporting done** — the guide's verification checklist:
   PR on the right branch; `gh pr checks` all green (run it yourself);
   exit criteria quoted with evidence; plan doc committed; rebased on
   current main. A teammate's "done" is a claim, not a fact.
6. **Present to the operator** for review/merge; batch when several are
   ready, never batching a blocked teammate behind a slow item.
7. **Post-merge, immediately:** instruct every in-flight teammate to
   rebase onto main, resolve, and re-run the full gates. Then re-run the
   ready check — a merge usually opens the next window.
8. Teammates stay idle-but-alive after finishing; send review feedback to
   the same teammate rather than respawning.

## Escalation — always operator-bound

Plan approvals · PR review/merge · spec divergences (and ordering when two
phases both need spec edits) · manual TUI verification · teammates blocked
on judgment · anything spending real API money (Phase 15's e2e run).

## Recovery — if you are a fresh lead

Follow the guide's recovery protocol: `gh pr list --state all` against the
phase map; `git branch -r` + `git worktree list` for in-flight work;
reattach surviving teammate sessions (`claude agents`) or respawn from the
kickoff prompts — branches, plan docs, and PRs carry all real state.

## Red flags — stop, you're drifting

| Thought | Reality |
|---------|---------|
| "I'll just peek at the diff" | That's the operator's review or a reviewer teammate's job. Your context pays for it forever. |
| "Quick fix, faster than respawning" | The moment you edit, you're a worker with a head full of orchestration state. Delegate. |
| "CI is basically green" | Verify with `gh pr checks`. Basically green is not green. |
| "Deps are green, close enough" | Merged is the gate. Green-but-unmerged is not merged. |
| "I'll remember this decision" | You won't survive a restart (A3 finding). Write it to a PR comment. |
| "I'll paraphrase the kickoff" | Blocks are verbatim. If a block is wrong, fix it via PR. |

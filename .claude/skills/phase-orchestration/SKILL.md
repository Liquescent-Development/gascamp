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
- You never merge. The operator reviews and merges every PR and retains
  override authority over every gate. Phase plans are approved by the
  dispatched Opus 4.8 plan reviewer (master plan decision 10 as amended
  2026-07-07, operator directive), with operator involvement by
  escalation only; sequencing follows decision 11 as amended.

## Phase lifecycle — run this checklist per phase

1. **Ready check:** every phase in its "Depends on" column is MERGED
   (`gh pr list --state merged`). Green-but-unmerged does not count.
2. **Spawn:** compose the kickoff = PREAMBLE + phase block from the
   orchestration guide, verbatim; fill `{CONTEXT}` (what's merged, inputs)
   and `{PARALLEL_NOTE}` (in-flight siblings, their files, worktree
   instruction — or the runs-alone line). Parallel siblings must be
   isolated per the guide's worktree conventions.
3. **Plan gate — auto plan review (decision 10 as amended 2026-07-07,
   operator directive):** the teammate's first deliverable is still its
   execution-ready plan doc, then it stops. The moment the doc path
   arrives, dispatch an Opus 4.8 plan-review subagent — read-only,
   isolated worktree, never posts to GitHub — with the plan doc path, the
   phase's master-plan contract section, the spec, and the orchestration
   guide named as context. It judges: does the plan satisfy the phase
   contract (files, interfaces, semantics, test obligations, exit
   criteria); does it respect merged-phase interfaces; are flagged
   contract deviations justified and additive; is the TDD task structure
   execution-ready. The verdict is binary — APPROVE (non-blocking notes
   are relayed with the approval but do not gate) or REJECT with
   concrete, actionable findings. Relay the verdict to the teammate
   directly; no operator round-trip. A rejected plan loops — teammate
   revises, a fresh reviewer pass judges the revision — until APPROVE.
   You never substitute your own judgment for the reviewer's and never
   skip the review: not for a small phase, not when the operator is away,
   not under schedule pressure. Durability: your APPROVE relay instructs
   the teammate to record the approval note (date, verdict, non-blocking
   notes, any deviations the reviewer accepted) at the top of its plan
   doc in its first execution commit, so the verdict lives in the
   committed doc where a replacement lead will find it — never only in
   your context. Operator involvement is by escalation only: a plan
   proposing a spec edit, anything spending real API money, a
   reviewer/teammate deadlock after two reject rounds, or the operator
   asking. The operator retains override authority at all times.
4. **Execution:** stay out of the way. Track one-line status via the task
   list. If the teammate reports a spec divergence, escalate immediately —
   spec edits are serialized through the operator.
5. **Verify before reporting done** — the guide's verification checklist:
   PR on the right branch; `gh pr checks` all green (run it yourself);
   exit criteria quoted with evidence; plan doc committed; rebased on
   current main. A teammate's "done" is a claim, not a fact.
6. **Auto-review (operator standing order, 2026-07-07):** the moment
   step 5 passes, dispatch an Opus 4.8 `code-reviewer` subagent against
   the PR — isolated worktree, review-only, never posts to GitHub — with
   the phase contract section, spec, and committed plan doc named as
   context. Do not wait for the operator to ask. **Fix-all (operator
   standing order, 2026-07-07):** when any review pass — initial or
   fix-pass — returns findings, relay ALL of them to the phase teammate
   immediately for fixing. No per-round operator decision, no triage by
   severity, no holding findings for presentation. Review-fix rounds on
   the same PR get a fresh reviewer pass on the fix commits; the
   revise → fresh-review loop continues until a pass returns clean or
   the operator overrides.
7. **Present to the operator** for review/merge — PR and review verdict
   together, findings summarized (what each pass found, what was relayed,
   fix status); keep the ongoing narrative current on review rounds as
   they happen. Batch when several are ready, never batching a blocked
   teammate behind a slow item. The operator decides when to merge and
   retains override authority at all times: they may trim or waive
   findings, or order a merge with findings open.
8. **Post-merge, immediately:** instruct every in-flight teammate to
   rebase onto main, resolve, and re-run the full gates. Then re-run the
   ready check — a merge usually opens the next window.
9. Teammates stay idle-but-alive after finishing; send review feedback to
   the same teammate rather than respawning.

## Escalation — always operator-bound

Plan-gate escalations — a plan proposing a spec edit, a reviewer/teammate
deadlock after two reject rounds, the operator asking (plan approval
itself belongs to the Opus 4.8 reviewer; decision 10 as amended
2026-07-07) · PR review/merge · spec divergences (and ordering when two
phases both need spec edits) · manual TUI verification · teammates blocked
on judgment · anything spending real API money (Phase 15's e2e run, or a
plan proposing real API spend).

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
| "This PR is clean, skip the review" | Every phase PR gets the Opus 4.8 review before presentation. Standing order — dispatch it, relay any findings. |
| "These findings are minor, I'll present them and wait for the operator to decide" | That was the old flow. Fix-all standing order (2026-07-07): every finding from every pass goes to the teammate immediately; the operator sees the summary at presentation and may trim or waive at any time. |
| "The plan looks fine, I'll approve it myself / skip the plan review" | Plan approval is the Opus 4.8 plan reviewer's verdict, never yours (decision 10 as amended 2026-07-07). Dispatch the review; relay the verdict. |
| "It stopped mid-task — it'll pick back up / report when it's done" | A stopped agent never receives callbacks. A worker or reviewer that stops with an intention statement instead of its deliverable, or goes quiet mid-task, stays stopped until messaged — resume it with a direct message now; don't wait. |
| "CI is still running — the teammate will notify me when it's green" / "once CI is green I'll do the next step" | Not a self-resolving state: nothing wakes a stopped session when an external CI run finishes. The teammate must foreground-watch CI to its terminal result (`gh pr checks --watch`) and report the settled outcome, not "CI is running." You re-verify with `gh pr checks` on your next wake and drive the next step yourself — never passively wait on CI completion. |

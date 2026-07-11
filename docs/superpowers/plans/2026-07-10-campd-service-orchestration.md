# campd Service Management — Orchestration Guide

Companion to the design spec
(`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`) and
the per-phase plans (`docs/superpowers/plans/2026-07-10-campd-service-phase-*`).
This guide is the **lead's contract** for driving the feature to completion
across a fresh session with parallel dispatched agents, auto-merge on clean
review, and re-review on fixes.

**How to use it:** open a fresh session in the repo root and paste the Kickoff
Prompt below verbatim. It is self-contained — it names the docs to read, the
skills to invoke, the dependency/parallelism map, the per-phase protocol, the
operator-granted auto-merge authority, the repo-specific merge hygiene, and the
escalation boundaries.

**Operator decisions encoded here (change these if they no longer hold):**

- **Auto-merge is operator-granted.** The lead squash-merges a phase PR itself
  once CI is green AND the adversarial review is clean (no Critical/Important).
  Everything still lands via a PR + review + CI; only the human merge click is
  removed. This overrides `phase-orchestration`'s "operator merges every PR"
  rule. To reinstate a human merge gate, delete the "Auto-merge authority"
  section and change protocol step 6 to "present to the operator."
- **Parallel window is Phases 1 + 2**, then 3, then 4 (see the map) — to avoid
  `main.rs` conflicts and the auto-start-before-supervisor hazard.
- **Phasing exists because the spec is large** (writing-plans Scope Check):
  each phase is an independently shippable PR.

---

## Kickoff Prompt

```
You are the orchestration lead for implementing the campd service management feature (cross-platform daemon supervision) end to end in this repo (public at Liquescent-Development/gascamp). Drive it to completion phase by phase, in parallel where the dependency map allows, auto-merging clean PRs and looping reviews until clean. Run to completion without checking in except for the escalations listed below.

## Read first (in order)
1. AGENTS.md — repo invariants and working rules (TDD; never commit to main; gates).
2. Design spec (source of truth): docs/superpowers/specs/2026-07-10-campd-service-management-design.md
3. Phase 1 plan (already authored + self-reviewed): docs/superpowers/plans/2026-07-10-campd-service-phase-1-sigterm.md

The design spec, the Phase 1 plan, and the orchestration guide are committed on the local branch `campd-service-management` (branched from main; not pushed). Step 0: push that branch, open a docs-only PR, watch CI to green, and squash-merge it to main (co-author-safe — see Merge hygiene). Then every phase branches off the updated main and sees the spec + plans.

## Skills to use (invoke them, don't paraphrase)
- phase-orchestration — your lead contract: you are a DISPATCHER, not a worker (never open a source file / never hand-edit code / never read diffs yourself — delegate); keep your context minimal; per phase run the auto-plan-review, then the auto-code-review + fix-all loop; verify before "done"; keep durable state in the repo/PRs. NOTE the operator has AMENDED the "operator merges every PR" rule — see Auto-merge authority.
- superpowers:subagent-driven-development — how each phase's plan is executed: fresh implementer subagent per task → task review (spec compliance + code quality) → fix-loop → broad review; durable progress ledger; file handoffs (scripts/task-brief, scripts/review-package).
- subagent-hygiene — never end a turn "waiting to be notified"; run implementers/reviewers to a terminal result (synchronous or poll-on-wake); foreground-watch CI (gh pr checks <pr> --watch) to a settled pass/fail; file handoffs; address agents by explicit ID.
- superpowers:writing-plans — author the Phase 2 / 3 / 4 plans (Phase 1 is done).
- superpowers:using-git-worktrees — isolate parallel phases in separate worktrees.
- superpowers:requesting-code-review — the adversarial whole-branch review template for the per-phase final review.

## The feature = 4 phases (all detailed in the design spec)
1. SIGTERM/SIGINT graceful shutdown (campd core). Plan written, ready to execute.
2. camp service {install,uninstall,status,restart,list} + cross-platform launchd/systemd unit generation + env-aware camp init.
3. Remove the CLI on-demand auto-start → pure socket client (migration; blast radius top/adopt/sling + daemon/autostart.rs + tests, per spec §8).
4. Reference container setup (contrib/docker/) + docs + the docs/design/2026-07-05-gas-camp-design.md §5/§9/§12 amendments.

## Dependency + parallelism map
- Phases 1 and 2 are independent → develop in parallel, each in its own worktree/branch off main.
- Phase 3 merges after Phase 2 (both edit main.rs; and a supervisor must exist before auto-start is removed, or users get stranded).
- Phase 4 is last (needs SIGTERM from 1, init --no-service from 2, and the auto-start removal from 3 for the §5 amendment to be accurate).
- After any merge: rebase every in-flight sibling branch onto main, re-run the full gates, re-run the ready check.
- Give each phase its own branch (e.g. phase-1-campd-sigterm); do NOT reuse campd-service-management (that's the merged docs branch). Where a phase plan's text says "branch campd-service-management", treat it as that phase's own branch.

## Per-phase protocol
1. Plan: if not written (2/3/4), author it with superpowers:writing-plans against the merged reality of its dependencies.
2. Auto plan-review: dispatch an Opus-class reviewer (read-only, isolated worktree, never posts to GitHub) to judge the plan against the design spec, the phase's scope, and merged interfaces. Binary: APPROVE (relay non-blocking notes) or REJECT (revise → fresh review → loop). Applies to Phase 1 too. Have the approved verdict recorded at the top of the plan doc in the first execution commit.
3. Execute via superpowers:subagent-driven-development. Dispatch independent phases in parallel worktrees; never two implementers on the same files at once.
4. Verify: PR opened against main; gh pr checks <pr> --watch all green (watch to terminal — CI never wakes a stopped session); the plan's verification run passes.
5. Adversarial whole-branch review: Opus-class, superpowers:requesting-code-review, isolated worktree, review-only, never posts to GitHub.
6. Auto-merge on clean / re-review on fix:
   - If CI is green AND the review returns zero Critical/Important findings → squash-merge to main yourself (co-author-safe), delete the branch.
   - If the review finds Critical/Important issues → relay ALL of them to the phase implementer; they fix + re-run the covering tests; a fresh review pass judges the fix commits; loop until a pass is clean, then merge.
   - Minor findings: record them in the progress ledger; fold into the phase or a follow-up; do not block merge on Minors.
7. Rebase in-flight siblings; advance to the next ready phase.

## Auto-merge authority (operator-granted — overrides phase-orchestration's "operator merges")
You MAY squash-merge a phase PR to main once (a) CI is green and (b) the adversarial review is clean (no Critical/Important). Everything still lands via a PR. Do NOT merge with red CI, open Critical/Important findings, or an unresolved plan/spec question.

## Merge hygiene (repo-specific — important)
gh squash merges auto-append a Co-authored-by: Review trailer unless you pass an explicit body. Always merge with:
gh pr merge <pr> --squash --delete-branch --subject "<subject> (#<pr>)" --body-file <file>
using a clean body, then verify the landed commit on origin/main has NO Co-authored-by / self-attribution / AI attribution before moving on. No co-author or self/AI attribution in ANY commit or PR body.

## Escalate to the operator (stop and ask) ONLY for
- A plan proposing to edit the authoritative spec beyond the amendments already in the design doc.
- A genuine design/spec ambiguity, or a reviewer↔implementer deadlock after two reject rounds.
- Anything spending real API money, or a destructive/irreversible op beyond a normal squash-merge.
- A CI infrastructure failure you cannot resolve (runner/billing/etc.).
Otherwise, run all four phases to completion without checking in.

## Done criteria
When all 4 phases are merged to main and green, report a summary (per phase: PR #, what merged, review verdict, any deferred Minors) and stop. Do not cut a release or bump versions unless asked.
```

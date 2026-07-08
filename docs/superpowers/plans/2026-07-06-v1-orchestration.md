# Gas Camp v1 — Phase Orchestration Guide

| Field | Value |
|---|---|
| Date | 2026-07-06 |
| Scope | How the remaining v1 phases (3–15) are dispatched, parallelized, verified, and recovered |
| Authority | Operator > spec (`docs/design/2026-07-05-gas-camp-design.md`) > master plan (`2026-07-05-gas-camp-v1-implementation.md`, incl. decision 10 as amended 2026-07-07 by operator directive and decision 11 as amended 2026-07-06) > this guide |
| Companion | `.claude/skills/phase-orchestration/SKILL.md` — the lead session's behavioral contract |

This guide is data and prompts; the skill is behavior. A fresh lead session
reconstructs the entire orchestration state from this file plus
`gh pr list --state all` — nothing load-bearing may live only in a session's
context (the harness does not persist team state across restarts; see the
A3 finding in `docs/design/2026-07-06-assumption-findings.md`).

## Roles

- **Operator (the human):** reviews and merges every PR, answers
  escalations, and retains override authority over every gate. Nothing
  merges without the operator. *Amended 2026-07-07 (operator directive):
  the operator no longer approves each phase's execution plan — the
  lead's dispatched Opus 4.8 plan reviewer does (master plan decision 10
  as amended by this directive). Operator involvement in plan approval is
  by escalation only: a plan proposing a spec edit, anything spending
  real API money, a reviewer/teammate deadlock after two reject rounds,
  or the operator asking.* *Amended 2026-07-07 (operator directive):
  review findings are relayed to the teammate in full by default — the
  lead sends every finding from every code-review pass (initial or
  fix-pass) straight to the phase teammate, with no per-round operator
  decision; the operator sees findings summarized when the PR is
  presented and retains override and merge authority (may trim or waive
  findings, or order a merge with findings open).*
- **Lead (one session):** dispatcher only. Spawns one teammate per phase
  with a kickoff prompt composed from this guide, runs the plan gate
  (*amended 2026-07-07, operator directive:* dispatches an Opus 4.8
  plan-review subagent per its contract's step 3 and relays the verdict —
  decision 10 as amended by this directive), verifies completion
  evidence, relays code-review findings to the teammate in full per its
  contract's step 6 (*amended 2026-07-07, operator directive*), triggers
  rebases after merges, and batches what needs the operator. The lead
  never edits code or docs deliverables, never reads diffs, never
  debugs. Its behavioral contract is the `phase-orchestration` skill.
- **Phase teammate (one session per phase):** does the actual work exactly
  as a solo session would — reads AGENTS.md, the spec, and its master-plan
  contract section; expands the phase into an execution-ready plan
  (superpowers:writing-plans); waits for approval; executes with strict
  TDD; lands one PR; is done only when CI is green.

## Dependency map (from the master plan Phase Map — binding)

| Phase | Branch | Depends on (all must be MERGED) |
|---|---|---|
| 3 | `phase-3-beads-readiness` | 1 |
| 4 | `phase-4-search-memory` | 3 |
| 5 | `phase-5-formula-subset` | 3 |
| 6 | `phase-6-gc-compat-ci` | 5 |
| 7 | `phase-7-campd-skeleton` | 1, 3 |
| 8 | `phase-8-dispatch-workers` | 2, 5, 7 |
| 9 | `phase-9-graph-execution` | 8 |
| 10 | `phase-10-orders` | 7 |
| 11 | `phase-11-patrol-adoption` | 8 |
| 12 | `phase-12-plugin-packs` | 8, 11 |
| 13 | `phase-13-perf-volume` | 4, 9 |
| 14 | `phase-14-export-bridge` | 5, 10 |
| 15 | `phase-15-e2e` | 12 |

Ready check before any spawn: every dependency's PR is merged to main
(`gh pr list --state merged`). A dependency being "green but unmerged" does
not count — merge is the review checkpoint.

## Recommended parallel windows (conservative — critical path stays serial)

The critical path is 3 → 5/7 → 8 → 11 → 12 → 15. Keep it serial; hang the
rest off it:

| Window | After merge of | Run concurrently | Conflict notes |
|---|---|---|---|
| W1 | 3 | 4 ∥ 5 ∥ 7 | Nearly disjoint (`search.rs` vs `formula/` vs `daemon/`); all touch `main.rs`/`Cargo.toml` — trivial rebases |
| W2 | 5 | 6 alongside the 7→8 track | 6 touches only `ci/` + workflow file |
| W3 | 7 | 10 ∥ 8 | Different daemon concerns (cron heap vs spawn); both touch `event_loop.rs` — coordinate |
| W4 | 8 | 9 ∥ 11 | **Highest-risk pair**: both extend the daemon dispatch/event-loop area. Acceptable, but expect a real rebase |
| W5 | tail | 13 ∥ 14 ∥ 12 (→ 15 after 12) | Three independent tracks: 13 needs 4+9, 14 needs 5+10, 12 needs 8+11 |

The lead may run fewer things in parallel than a window allows (operator
review bandwidth is the true bottleneck); it must never run more.

## Shared files and the rebase protocol

Files nearly every phase touches (guaranteed small conflicts):

- `crates/camp/src/main.rs` — every phase wires new clap subcommands
- `crates/camp-core/src/event.rs` and `src/vocab.rs` — new `EventType`
  variants and vocab consts (mind the mirror rules)
- `crates/camp-core/src/ledger/fold.rs` — new fold arms
- `crates/camp/Cargo.toml`, `crates/camp-core/Cargo.toml`, `Cargo.lock`
- Phase-specific hot spots: `.github/workflows/ci.yml` (6), `Makefile`
  (13, 15), `crates/camp/src/daemon/*` (7, 8, 9, 10, 11)

Protocol, non-negotiable for parallel phases:

1. Each kickoff prompt for a parallel phase names its in-flight siblings
   and the files they own; a teammate must not touch a sibling's files
   beyond the shared list above.
2. **After any PR merges to main, the lead immediately instructs every
   in-flight teammate to rebase onto main, resolve, and re-run the full
   gates before continuing.** No teammate opens or updates a PR from a
   branch that isn't rebased on current main.
3. Spec edits are serialized: if two in-flight phases both need spec
   changes, the lead escalates to the operator to order them — concurrent
   spec edits are forbidden.

## Worktree conventions (parallel phases only)

A phase running alone may use the main checkout at
`/Users/kiener/code/gascamp`. Parallel teammates must be isolated:

- Preferred: spawn the teammate with `isolation: "worktree"` so the
  harness gives it an isolated copy; its first step is
  `git checkout -b <branch>` there, and it pushes with `-u origin`.
- Alternative (teammate-managed): `git -C /Users/kiener/code/gascamp
  worktree add ../gascamp-<branch> -b <branch> origin/main` and do all
  work under `/Users/kiener/code/gascamp-<branch>`, prefixing commands
  with that path. Remove the worktree after merge
  (`git worktree remove ../gascamp-<branch>`).
- Never do phase work directly in the main checkout while any sibling
  phase is in flight.

## Lead verification checklist (before reporting a phase "ready for review")

All of these, with evidence quoted from the teammate — the lead verifies
the checkable ones itself with `gh`, and never by reading the diff:

1. PR exists on the correct `phase-N-<slug>` branch and targets main.
2. `gh pr checks <n>` — every check green (fmt, clippy, test ×2, plus the
   gc-compat job once Phase 6 lands).
3. The teammate quoted its master-plan **exit criteria** line by line with
   how each was verified (test names, command outputs).
4. The phase's execution-ready plan doc is committed under
   `docs/superpowers/plans/`, carrying its plan-review approval note
   (*amended 2026-07-07, operator directive:* verdict, date, and any
   reviewer-accepted deviations recorded in the doc per the skill's
   step 3).
5. Any spec divergence found was handled spec-first (spec edit in the same
   PR), or explicitly reported as "none".
6. Branch is rebased on current main.

## Escalation to the operator

Always operator-bound: PR review/merge, any spec divergence, any manual
TUI verification, ordering of concurrent spec edits, a teammate blocked
on judgment, and the plan-gate escalation cases — a plan proposing a spec
edit, anything spending real API money, a reviewer/teammate deadlock
after two reject rounds, or the operator asking. Batch non-urgent items
("PRs #7 and #8 are green with review verdicts attached") — but never
batch a blocked teammate behind a slow item.

*Amended 2026-07-07 (operator directive): execution-plan approval (master
plan decision 10 as amended by this directive) moved from the operator to
the lead's dispatched Opus 4.8 plan reviewer; only the plan-gate
escalation cases above reach the operator. The operator retains override
authority at all times.*

*Amended 2026-07-07 (operator directive): review findings are relayed to
the teammate in full by default; the operator retains override and merge
authority. Deciding which code-review findings go back to the teammate is
no longer an operator escalation — the lead relays every finding from
every pass immediately (skill step 6). The escalation list above is
otherwise unchanged.*

## Recovery protocol (fresh lead after a lead-session loss)

1. Read this guide and the `phase-orchestration` skill.
2. `gh pr list --state all` → mark each phase merged / open / absent.
3. `git -C /Users/kiener/code/gascamp branch -r` and `git worktree list`
   → find in-flight branches and stray worktrees.
4. Former teammates may survive as attachable background sessions
   (`claude agents`) — reattach if useful; otherwise respawn from the
   kickoff prompt: the branch, plan doc, and PR carry all real state, so a
   respawned teammate resumes from "read your branch and continue".
5. Resume the lifecycle. Nothing else needs restoring — by design.

---

# Kickoff prompts

Compose each kickoff as: **PREAMBLE** (slots filled) + **the phase block**
verbatim. Do not paraphrase blocks; edit this file via PR if one is wrong.

## PREAMBLE (common to all phases)

*Amended 2026-07-07 (operator directive): the approval the PREAMBLE tells
the teammate to await is now the plan-review verdict relayed by the lead
(master plan decision 10 as amended by this directive), no longer operator
approval. The teammate-facing behavior is unchanged — stop, report the
plan doc path, await approval — so only the word "operator" was dropped
from the block below. This note stays outside the fenced block: kickoffs
are composed from the block verbatim.*

*Amended 2026-07-08 (operator directive): added the subagent-hygiene
pointer to Method and rules — sessions spawning helper agents kept
hitting stranded completion callbacks and misrouted reports (six
incidents this project); the skill (.claude/skills/subagent-hygiene) is
the durable fix. This note stays outside the fenced block: kickoffs are
composed from the block verbatim.*

```
Work in the gascamp repository (private repo richardkiene/gascamp, default
branch main — NEVER commit to main; all work lands via PR branches). Read
AGENTS.md first; it carries the project invariants and working rules.

{CONTEXT}   ← lead fills: which phases/PRs are merged, plus one sentence
              per input this phase consumes.

{PARALLEL_NOTE}   ← lead fills when siblings are in flight: their branches,
              the files they own, the worktree instruction (see the
              orchestration guide), and the rebase-on-merge rule. When the
              phase runs alone, replace with: "You are the only phase in
              flight; work in the main checkout."

Authoritative documents, in order:
1. docs/design/2026-07-05-gas-camp-design.md — the approved spec. Its §4
decision record is settled — do not re-litigate it. If implementation
reality contradicts the spec, stop and update the spec via PR in the same
change — spec and code never silently diverge.
2. docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md — the
approved master plan. Your phase's section (named in your phase block) is
your contract: files, exact interfaces, semantics, test obligations, exit
criteria. Per the plan's decision 10, your FIRST step is to expand your
phase into its own execution-ready plan doc in docs/superpowers/plans/
using the superpowers:writing-plans skill, then STOP and report the plan
doc's path back to the team lead for approval. Do not execute until
approval comes back.
3. Any extra documents named in your phase block.

Method and rules:
- TDD strictly: write the failing test, run it, watch it fail, implement,
watch it pass. Run every new or changed test before claiming anything.
- Respect existing interfaces from merged phases — extend, don't rework.
New events use deny_unknown_fields payload structs, keep the
one-transaction event+state property, satisfy the vocab-pin partition
tests, and keep the refold property test green.
- Branch {BRANCH}; one reviewable PR; gates before push: cargo fmt --all
--check && cargo clippy --workspace --all-targets --all-features -- -D
warnings && cargo test --workspace. Work is not complete until pushed and
CI is green (gh pr checks --watch).
- When done, report to the team lead: PR number, CI status, and your
master-plan exit criteria quoted line by line with the evidence for each.
- If you hit a genuine spec/contract ambiguity, stop and ask the lead
rather than guessing.
- If you spawn helper agents, follow the subagent-hygiene skill
(.claude/skills/): poll for results — completion callbacks don't wake
stopped sessions; use file handoffs; no helper task may need a
permission escalation.

House rules: never add co-authors or mention yourself in commits; never
silence errors; fail fast, no fallbacks; no panics in library code; never
call something complete unless it is 100% complete.
```

## Phase 3 — Beads, Rigs, Readiness, Queries

Lead notes: deps = 1. First phase after this guide lands; runs alone.

```
Your task is Phase 3. Contract: master plan section "Phase 3 — Beads,
Rigs, Readiness, Queries (phase-3-beads-readiness)". {BRANCH} =
phase-3-beads-readiness.

Scope highlights (read the full section; these are binding):
- camp-core: config.rs (CampConfig/RigConfig, camp.toml parsing with
deny_unknown_fields), id.rs (per-rig prefixed bead IDs like gc-142 —
allocation is folded state via a counters table so refold stays exact),
readiness.rs (is_ready, ready_beads, newly_ready — the affected-subgraph
recompute campd uses from Phase 7 on).
- camp CLI verbs: rig add/ls, claim, close, ls (--ready/--mine/--rig,
--json), show (current state + full event history — the one sanctioned
history read, spec §7.4).
- New event rig.added (camp-specific — verify against the vocab pin
rules); camp rig add writes camp.toml AND appends the event, since config
changes are events (spec §13.4).
- Readiness rule is plan decision 6, binding: ready = status='open' ∧
every needs target exists, is closed, AND has outcome='pass'. A failed or
missing dependency never unblocks dependents.
- Tests per the contract: readiness truth table (unmet dep, closed-fail
dep, missing dep, diamond graphs); newly_ready returns exactly the newly
ready subgraph; id allocation survives refold (extend the property test);
CLI round-trips with --json golden output; rig add writes both TOML and
event.

Exit criteria: a bead can live its whole Tier-0 ledger life via the CLI
(create → claim → close); doctor --refold stays clean throughout (assert
it in tests); CI green.
```

## Phase 4 — Search and Memory

Lead notes: deps = 3. Window W1 — may run parallel to 5 and 7.

```
Your task is Phase 4. Contract: master plan section "Phase 4 — Search and
Memory (phase-4-search-memory)". {BRANCH} = phase-4-search-memory.

Scope highlights:
- camp-core/src/search.rs: search(conn, query, type_filter, limit) ->
Vec<SearchHit> ranked by bm25(search); SearchHit { bead_id, kind, snippet,
rank }.
- CLI: camp search <query> (unfiltered); camp remember "<fact>" [--rig r]
= bead.created with type='memory' (title = the fact — memory is beads, not
a new table); camp recall <query> = search filtered to memory beads.
- FTS query syntax errors must surface as a clean domain error (exit 1),
never a panic — escape/validate before handing to FTS5.
- The search FTS table and its body/close rows are folded state from Phase
1 — any new search rows you introduce must be written through the fold so
refold stays exact.
- Tests: remember→recall round-trip; ranking sanity (exact phrase beats
scattered terms); close-note content searchable; rig scoping; malformed
FTS query → clean error.

Exit criteria: the worker skill's recall-before/remember-after contract
has its verbs; CI green.
```

## Phase 5 — Formula Subset Compiler + Cook

Lead notes: deps = 3. Window W1 — may run parallel to 4 and 7. On the
critical path.

```
Your task is Phase 5. Contract: master plan section "Phase 5 — Formula
Subset Compiler + Cook (phase-5-formula-subset)". {BRANCH} =
phase-5-formula-subset. Also read spec §8.2 closely — the compatibility
invariant (every valid camp formula is a valid Gas City formula-v2 file)
is repo invariant 6.

Scope highlights:
- camp-core/src/formula/{mod,ast,parse,validate,cook}.rs with the exact
structs/signatures pinned in the contract (Formula, Step, Check, Retry,
OnComplete, parse_and_validate — FormulaError lists ALL violations, not
just the first — and cook(...) -> CookedRun in ONE append_batch txn).
- Acceptance table with gc semantics verbatim: header keys (formula = file
stem, enforced), [requires].formula_compiler semver; steps id/title/
description/needs (known ids, acyclic)/assignee/timeout; [steps.check]
max_attempts≥1 + inner check mode="exec"/path/timeout; [steps.retry]
max_attempts≥1, on_exhausted ∈ {hard_fail, soft_fail} (default hard_fail);
[steps.on_complete] for_each+bond together, for_each starts "output.",
parallel/sequential mutually exclusive.
- Combination rules mirrored from gc: check ∦ {retry, assignee}; retry ∦
{check, on_complete}. Explicit-declaration rule: any graph-only construct
without [requires] formula_compiler is an error.
- Rejection table (plan decision 5 — camp is stricter than gc):
deny_unknown_fields everywhere; every city-only construct rejected with an
error naming the construct and pointing to the city (drain, gate, loop,
expand, children, waits_for, condition, pour, phase, tally, extends, vars
tables, any authored metadata incl. gc.*, depends_on, type, priority,
tags, description_file, notes).
- Cook: runs/<run-id>/ (run_id = <utc-compact>-<6-hex>) with pinned
formula copy + manifest.json; root bead + one bead per step (run_id/
step_id set, needs edges rig-prefixed) + camp-specific run.cooked event —
all one transaction; run is file-independent afterwards.
- Fixture corpus: valid/ includes spec §8.2 guarded-change verbatim, a
minimal single-step, a retry example, a diamond needs graph with
assignees, and an on_complete fixture; invalid/ has one file per rejection
row plus dup-step-id, unknown-needs-id, cycle, bad semver,
check-without-requires.
- Tests: table-driven acceptance/rejection (errors must name the
construct); cook atomicity via fault injection; cooked runs satisfy Phase
3 readiness (dag roots ready, dependents not); doctor --formula exit codes
0/1.

Exit criteria: corpus green under camp's validator; cook produces
dispatchable graphs; CI green. (Phase 6 will validate the same corpus
against the real gc compiler — write fixtures knowing they face gc.)
```

## Phase 6 — gc Compatibility Gates in CI

Lead notes: deps = 5. Window W2 — runs alongside the 7→8 track.

```
Your task is Phase 6. Contract: master plan section "Phase 6 — gc
Compatibility Gates in CI (phase-6-gc-compat-ci)". {BRANCH} =
phase-6-gc-compat-ci. The mechanism is fixed by plan decisions 3 and 4 —
do not redesign it.

Scope highlights:
- ci/gc-compat/{GASCITY_REF, camp_corpus_validate.go, check_vocab.sh,
selftest-invalid.toml} + a new gc-compat job in .github/workflows/ci.yml.
- camp_corpus_validate.go: the complete source is in the master plan
section — use it verbatim. It is camp-owned but compiled INSIDE a checkout
of gastownhall/gascity at the SHA in GASCITY_REF (copied to
cmd/camp-corpus-validate/main.go so it may import the internal compiler).
- GASCITY_REF pins the same SHA as the existing vocab pin provenance
(crates/camp-core/tests/fixtures/gc-vocab.json — 1241030188…); the two
must reference one ref.
- CI job outline (from the contract): checkout gascamp; checkout gascity
at the pinned ref; setup-go with go-version-file from the checkout; copy
the shim in; run over crates/camp-core/tests/fixtures/formulas/valid;
SELF-TEST: run over a dir holding only selftest-invalid.toml and require
exit 1 (a shim that always passes must be impossible to miss); then
check_vocab.sh extracts quoted string constants from internal/events/
events.go and internal/beadmeta/values.go and asserts every gc-list name
in gc-vocab.json exists in source and no CAMP_SPECIFIC_EVENTS name does.
- Bumping GASCITY_REF is a deliberate PR; drift fails loudly.

Exit criteria: gc-compat job green on the Phase 5 corpus and marked
required for merge thereafter; during PR review, demonstrate a
deliberately-bad corpus file failing in CI, then remove it (coordinate the
demo commit/revert inside your PR).
```

## Phase 7 — campd Skeleton

Lead notes: deps = 1, 3. Window W1 — may run parallel to 4 and 5. On the
critical path.

```
Your task is Phase 7. Contract: master plan section "Phase 7 — campd
Skeleton (phase-7-campd-skeleton)". {BRANCH} = phase-7-campd-skeleton.
Repo invariant 1 (idle is free — no ticks, no polling, zero wakeups when
idle) is the soul of this phase; violations are bugs.

Scope highlights:
- camp/src/daemon/{mod,socket,event_loop,cursor}.rs; cmd/{stop,top}.rs;
integration tests daemon_lifecycle.rs. campd invocation per plan decision
2: [[bin]] camp + campd symlink on install; main() dispatches to daemon
mode when invoked as campd or camp daemon.
- Socket protocol pinned (newline-delimited JSON over <camp>/campd.sock):
{"op":"poke","seq":N} → {"ok":true}; {"op":"status"} → {"ok":true,
"live_sessions":[…],"ready":N,"open":N,"campd_pid":N}; {"op":"stop"} →
{"ok":true} then graceful exit with campd.stopped event.
- Liveness = the socket accepts (spec §5): stale socket file that refuses
connections is unlinked and rebound; bind conflict on a live socket means
the second daemon refuses to start.
- Event loop on the polling crate: socket accept + per-connection reads +
timer-heap deadline as the poll timeout; no timer armed = infinite wait.
- Cursor: cursors row 'campd'; on start emit campd.started, catch up past
the cursor through an EventProcessor trait (Phase 7's processor updates
readiness bookkeeping only — dispatch plugs in at Phase 8); cursor
advances transactionally with processing effects (exactly-once).
- Auto-start: a CLI verb needing the daemon connects; on failure spawns
camp daemon detached (double-fork/setsid via std::process), appends
camp-specific campd.autostarted event, retries once, then errors — no
retry loop.
- camp top: one status query rendered as plain text (a query, not a loop).
- Tests: start → socket accepts → status sane; kill -9 → stale socket
detected → restart → cursor caught up exactly once; graceful stop; second-
daemon bind refusal; auto-start path with its event trail (spec §13.3).

Exit criteria: kill -9 is a supported shutdown method, demonstrably; the
idle daemon blocks in poll with no timeout-driven code path anywhere; CI
green.
```

## Phase 8 — Dispatch and Workers

Lead notes: deps = 2, 5, 7. Window W3 — may run parallel to 10. On the
critical path. The Phase 2 findings doc is a first-class input here.

```
Your task is Phase 8. Contract: master plan section "Phase 8 — Dispatch
and Workers (phase-8-dispatch-workers)". {BRANCH} =
phase-8-dispatch-workers. Extra authoritative input:
docs/design/2026-07-06-assumption-findings.md — its fixture facts F1–F7
pin the claude -p mechanics and are BINDING for spawn design:
- F1: pre-assign the session id with --session-id (uuid campd generates) —
the registry row / session.woke event is written BEFORE exec.
- F2: --output-format json emits a JSON ARRAY; parse the element with
type=="result" (fields incl. is_error, result, session_id,
permission_denials, num_turns, total_cost_usd).
- F3: transcript_path(cwd, sid) = ~/.claude/projects/<cwd with every
non-alphanumeric char replaced by '-'>/<sid>.jsonl — compute it from the
WORKER's cwd (worktree spawns land under per-worktree project dirs).
- F4: exit 0 → session.stopped; nonzero/signal (SIGTERM=143, SIGKILL=137)
→ session.crashed. Tool denials exit 0 — failure routing comes from the
worker contract and envelope parsing, never the exit code.
- F5: spawn workers with stdin at /dev/null (an open non-pipe stdin costs
a 3 s sniff delay) — except stream-json workers where campd holds stdin.
- F6: --resume keeps the session id and appends to the same transcript.
- F7: bare claude -p inherits the user's ambient config — pin every worker
explicitly per its agent definition (--permission-mode, --allowedTools/
--disallowedTools, --model, --append-system-prompt; --settings/--bare for
full isolation).

Scope highlights:
- camp-core/src/pack.rs: AgentDef (name, model, tools, permission_mode,
isolation None|Worktree, prompt), resolve_agent(cfg, name) with last-wins
pack layering.
- camp.toml [dispatch]: max_workers = 10; command = "claude" (tests point
this at fake-agent.sh — visible config, not a fallback); default_agent
routing incl. [[rigs]] default_agent override; neither set = hard error
naming the fix.
- Spawn: session name <camp>/<agent>/<n>; session.woke (registry at birth)
with claude session id + computed transcript path; exec with the agent's
prompt + worker-contract instructions + bead id; cwd = rig path or a fresh
worktree under <camp>/worktrees/ when isolation = "worktree".
- SIGCHLD via signal-hook self-pipe into the poll loop; reap; map exit per
F4; fold releases the crashed session's claimed bead.
- Worktrees: removed on clean close; kept with an event on failure.
- camp sling "<title>" [--agent a] [--rig r] (one bead.created; campd
dispatches — Tier 0 = one spawn, ~3 writes total; attended-teammate
surface is Phase 12); camp event emit <text> [--bead].
- New events: worker.milestone, campd.autostarted, worktree.kept
(camp-specific), bead.worktree.reaped (gc-mirrored) — satisfy the vocab
partition tests.
- fake-agent.sh: speaks the whole worker contract via the camp CLI
(claim → milestones → close) with env-controlled outcome/timing/crash.
- Tests (fake agent, no Claude): sling → dispatch → claim → milestone →
close pass with the full event-with-cause trail (spec §13.3 asserted
literally); crash mid-work → SIGCHLD → session.crashed → bead back to
open; concurrency cap (11 ready, 10 spawned, 11th dispatched on first
close); worktree created/removed on pass, kept on fail; registry row
precedes process start.

Exit criteria: Tier-0 path complete and evented end to end with the fake
agent; real-claude spawn arguments match F1–F7 exactly; CI green.
```

## Phase 9 — Graph Execution

Lead notes: deps = 8. Window W4 — may run parallel to 11 (watch the
daemon/dispatch overlap; coordinate via the lead).

```
Your task is Phase 9. Contract: master plan section "Phase 9 — Graph
Execution (phase-9-graph-execution)". {BRANCH} = phase-9-graph-execution.
Zero-Framework-Cognition (spec §8.3) is the law here: campd executes
structure declared in TOML; every judgment comes from agents or
user-supplied check scripts.

Scope highlights:
- Extend camp/src/daemon/dispatch.rs; add camp-core/src/formula/runtime.rs
(attempt/iteration bookkeeping as pure functions over ledger state).
- Close of a step → newly_ready subgraph → immediate dispatch up to the
cap.
- check steps: campd runs check.path (cwd = rig, check.timeout enforced,
step timeout as general bound); exit 0 → close pass; nonzero with budget →
next iteration bead + spawn; budget exhausted → step closes fail. Events:
camp-specific check.passed/check.failed with attempt numbers.
- retry steps: the worker's close carries the classification — pass, hard
fail, or transient (camp close --outcome fail --transient → data
failure_class:"transient", gc's key vocabulary). Transient + budget →
respawn; exhausted → on_exhausted disposition. Per plan decision 6,
soft_fail still does NOT satisfy needs — it exists for runs whose
remaining steps don't depend on the soft-failed step.
- on_complete: on close-pass, read the step's recorded structured output
(camp close --output-json <file|-> stores it in the close event's
data.output); resolve the for_each path; cook the bond per item with
{item}/{item.field}/{index} substitution; parallel or sequential
(sequential = each child's root needs the previous).
- Root finalization: last step close → root closes with aggregated outcome
→ run.finalized (camp-specific) with its cause chain.
- Tests (fake agent): diamond fan-out to completion; check loop passing on
2nd iteration; check budget exhaustion fails the run; transient retry
exhaustion hard vs soft table; on_complete over a 3-item output fans out 3
bonds (parallel and sequential); dispatch-latency functional assertion.

Exit criteria: every spec §8.2 construct executes with gc semantics;
doctor --refold clean after every integration run (asserted in the tests);
CI green.
```

## Phase 10 — Orders

Lead notes: deps = 7. Window W3 — may run parallel to 8.

```
Your task is Phase 10. Contract: master plan section "Phase 10 — Orders
(phase-10-orders)". {BRANCH} = phase-10-orders. Repo invariant 1 applies:
the cron machinery is a timer heap, never a tick.

Scope highlights:
- camp-core/src/orders/{mod,parse,cron}.rs; heap deadline integrated into
the daemon poll timeout; cmd/order.rs.
- Interfaces pinned in the contract: Trigger::Cron{expr} |
Event{event_type, label}; Order {name, trigger, formula, rig,
catch_up_window (default 2h; "0" disables)}; CronHeap {next_deadline() →
poll timeout, fire_due(now), recompute(now, last_seen) → Vec<CatchUp>}.
- camp.toml [[order]] exactly as spec §9 (on = "cron:…" /
"event:type[label=x]"); parse errors name the order and the field.
- Event orders evaluate on the same post-commit processing path as
readiness — zero standing cost; the label filter matches bead.* events
whose bead carries the label.
- Wall-clock jumps: each wake compares expected vs actual time; jumps
recompute deadlines; missed fires within catch_up_window fire once on
wake.
- camp order ls / camp order run <name>; camp.toml watched via notify;
reload emits camp-specific config.changed event (spec §13.4).
- Ship contrib/launchd/com.gascamp.campd.plist.example (example only,
never auto-installed) with its install one-liner documented alongside the
honest away-mode limits (spec §9).
- Vocab: order.fired / order.completed / order.failed move into
GC_MIRRORED_EVENTS (verify against the pin).
- Tests: cron parse/next-fire table with FixedClock (5-field, DST
boundaries, month ends); heap ordering under interleaved schedules;
sleep/wake catch-up inside and outside the window; "0" disables; event
order fires on matching close and not otherwise; integration: a cron order
cooks and completes a formula via the fake agent; config hot-reload with
event.

Exit criteria: away-mode is demonstrably the same code path (an order
fires with no user session; the ledger tells the story); no polling
anywhere (idle heap = infinite poll wait); CI green.
```

## Phase 11 — Patrol and Adoption

Lead notes: deps = 8. Window W4 — may run parallel to 9 (highest-conflict
pair; coordinate via the lead). The Phase 2 findings doc matters here too.

```
Your task is Phase 11. Contract: master plan section "Phase 11 — Health
Patrol and Adoption (phase-11-patrol-adoption)". {BRANCH} =
phase-11-patrol-adoption. Extra authoritative input:
docs/design/2026-07-06-assumption-findings.md — the A4 resolution
(stronger) and spec §10 as amended: the nudge action's live path is
delivering a status-request turn over stdin for campd-spawned workers
running in stream-json input mode; session resume is the path otherwise
(F6: resume keeps the session id and transcript). The A4-4 caution also
applies: resuming a session whose process is still alive works but makes
two writers share one transcript — prefer the stream path for campd-owned
workers.

Scope highlights:
- camp-core/src/patrol/{mod,timers}.rs (pure timer/ladder state machines);
camp/src/daemon/patrol.rs (notify watches + nudge/restart actions);
cmd/adopt.rs.
- One armed timer per active campd-spawned worker (heap-integrated, same
poll-timeout mechanism as orders); reset by transcript-file activity (a
notify watch on the registry row's transcript path — compute it per
fixture fact F3) and by any ledger event from that session. Threshold:
[patrol] stall_after = "10m" in camp.toml, agent-frontmatter override.
- Fire → agent.stalled event (camp-specific) → the agent definition's
mechanical ladder: nudge, then restart (kill, respawn, re-hook the bead)
with exponential backoff and a bounded budget; ladder exhaustion emits and
stops — escalation to judgment is an order matching event:agent.stalled
(pack content, not Rust).
- Attended teammates: annotate only (agent.stalled + statusline badge),
never kill a session in the user's TUI (spec §10).
- camp adopt (auto at campd start, manual verb): for each registered live
session probe the process (safe kill(pid,0) wrapper or ps) and transcript
mtime; dead → session.crashed (fold releases beads, budgets intact);
living → re-arm; sweep <camp>/worktrees/ against the registry — orphans
removed with bead.worktree.reaped events. Observation over state, always.
- Tests: timer arm/reset/fire state machine with FixedClock (transcript
touch resets; ledger event resets; threshold fires); ladder table (nudge →
restart → budget exhausted) incl. the backoff series; integration: fake
agent goes silent → stall → nudge revives it; nudge fails → restart
re-hooks the bead; kill -9 campd mid-run → restart → adopt reconciles
exactly (crashed marked, live re-armed, orphan worktree swept).

Exit criteria: every patrol action is an event with its cause; zero patrol
code paths poll (watches + timers only); CI green.
```

## Phase 12 — Plugin and Packs

Lead notes: deps = 8, 11. Window W5 — may run parallel to 13 and 14. The
A1 resolution applies.

```
Your task is Phase 12. Contract: master plan section "Phase 12 — Plugin
and Packs (phase-12-plugin-packs)". {BRANCH} = phase-12-plugin-packs.
Extra input: docs/design/2026-07-06-assumption-findings.md A1 — resolved
HOLDS: attended Tier-0 sling spawns the worker as a teammate exactly as
spec §8.4 designs (the headless+attach fallback is NOT needed). One
behavior note from the finding: messages to an attended teammate deliver
at its next step boundary and are answered at the agent's discretion.

Scope highlights:
- plugin/: manifest; commands/{sling,status,adopt,events}.md as thin
wrappers over the camp CLI (identical scripting surface, spec §13.6);
hooks/ SessionStart (register/adopt), Stop and SubagentStop (session-end
events), optional PostToolUse breadcrumb hook OFF by default (patrol
watches transcripts instead, §10); skills/worker/SKILL.md; statusline/.
- Worker skill text = the lifecycle contract: recall → claim → work →
event emit milestones → remember non-obvious findings → close with outcome
→ exit.
- Hooks are fire-and-forget appends with throttling (spec §16 requires
this verified by test).
- Statusline snippet: one status socket query rendering ▲live ●ready ✖red;
degrades to empty output WITH a stderr note when campd is down — visible
degradation, not silence.
- packs/starter/ (clearly content, not machinery): agents/dev.md,
agents/reviewer.md, formulas/guarded-change.toml (= the corpus file),
orders.toml example.
- The plugin ships ZERO agent definitions — enforce with a repo-policy
test.
- Tests: each hook against recorded fixture stdin payloads (exit codes,
appended events, throttle behavior); command-markdown ↔ CLI flag parity
check (script); starter pack passes camp doctor --formula and the Phase 6
gc gate (corpus symlink).

Exit criteria: driving a camp from inside a Claude Code session works end
to end; zero shipped agent definitions (tested); CI green.
```

## Phase 13 — Perf and Volume Suite (local-only)

Lead notes: deps = 4, 9. Window W5 — may run parallel to 12 and 14.

```
Your task is Phase 13. Contract: master plan section "Phase 13 — Perf and
Volume Suite, local-only (phase-13-perf-volume)". {BRANCH} =
phase-13-perf-volume. Perf policy is settled (2026-07-05): these suites
are LOCAL-ONLY via make perf; CI carries no perf job.

Scope highlights:
- crates/camp-core/tests/perf_volume.rs and crates/camp/tests/
perf_daemon.rs, all #[ignore], run --release by make perf; camp backup
(VACUUM INTO); Makefile with perf: (and an e2e: stub for Phase 15).
- Exact assertions (spec §14 numbers, from the contract's table):
  - Volume fixture: 30 heavy days, ≥1M events, ~100k beads, seeded RNG,
    generated through the REAL append path; doctor --refold clean at
    volume.
  - Ledger write p50 AND p99 < 1 ms over 10k appends into the 1M-event db.
  - Ranked FTS over the year-scale corpus: a 10-query set, each < 50 ms.
  - ls --ready indexed read at volume < 10 ms.
  - Idle campd: CPU time delta == 0 (±10 ms) over a 30 s idle window; RSS
    < 20 MB (via ps -o cputime=,rss=).
  - Sling → worker spawn ≤ 2 s and step close → dependent dispatched ≤ 1 s
    with fake-agent timing.
  - camp backup of the 1M-event db completes and passes integrity_check.
- The suite either runs and asserts or is not invoked — no silent skips.

Exit criteria: make perf green on the dev machine with the measured
numbers recorded in the PR description (the operator verifies them there);
CI untouched.
```

## Phase 14 — Export Bridge

Lead notes: deps = 5, 10. Window W5 — may run parallel to 12 and 13.

```
Your task is Phase 14. Contract: master plan section "Phase 14 — Export
Bridge (phase-14-export-bridge)". {BRANCH} = phase-14-export-bridge.
Graduation is an export, not a backend (spec §15.3); camp never writes
into a live city's store.

Scope highlights:
- camp-core/src/export.rs; cmd/export.rs; golden-output tests.
- beads.jsonl in bd wire format — research the field mapping against
gascity's docs/reference/exec-beads-provider.md and internal/beads JSON
tags at the pinned GASCITY_REF as part of your plan expansion; write the
field-level mapping table into BOTH your phase plan and
docs/reference/export.md. Statuses map 1:1; camp memory beads →
issue_type "task" + label camp-memory unless your research finds a native
memory type in bd import; metadata carries gc.outcome /
gc.final_disposition per the vocabulary mirror.
- formulas/ = the pinned copies from runs/ (already valid v2 subset files
— Phase 6 proved the corpus).
- pack/ = agent definitions verbatim + generated pack.toml wrapper + camp
orders translated to gc order TOML (on="cron:X" → trigger="cron",
schedule="X"; on="event:T" → trigger="event", on="T"); untranslatable
orders (e.g. camp's [label=…] filter) FAIL the export listing them, with
--skip-untranslatable as the explicit opt-out.
- Tests: golden export of a fixture camp (beads incl. closed-with-outcome
history, one cooked run, both order kinds); JSONL parses line by line and
field-maps exactly; untranslatable-order failure and explicit skip;
optional local check if a bd binary is present (not in CI).

Exit criteria: a Gas City operator could import the output directory with
standard tooling; CI green.
```

## Phase 15 — Opt-in E2E with real Claude

Lead notes: deps = 12. Final phase; runs alone. Real API spend — flag the
run to the operator before executing the suite.

```
Your task is Phase 15. Contract: master plan section "Phase 15 — Opt-in
E2E with real Claude (phase-15-e2e)". {BRANCH} = phase-15-e2e. Extra
input: docs/design/2026-07-06-assumption-findings.md — the e2e suite is
the canary for the F1–F7 fixture facts (claude was 2.1.201 when pinned);
if real-claude behavior diverges from a pinned fact, STOP and update the
findings doc and any affected spec text in this same PR before proceeding.

Scope highlights:
- crates/camp/tests/e2e.rs, #[ignore], requires CAMP_E2E=1 and an
authenticated claude CLI; make e2e target; fixture mini-repo
crates/camp/tests/fixtures/toy-project/ — a tiny CLI with a real test
suite a worker can extend.
- Scenarios (spec §16's e2e bullet, §14 numbers):
  1. Tier-0: camp sling "add a --json flag to toy ls, TDD it" against the
     toy rig → worker claims, works, closes pass; assert sling→first
     transcript token ≤ 2 s; total ledger writes for the Tier-0 envelope
     ≈ 3 (created/claimed/closed + milestones); camp show tells the whole
     story.
  2. One guarded-change formula run with a real verification script;
     assert step-close → dependent-dispatch ≤ 1 s.
  3. Post-run: idle campd 0.0% CPU window re-asserted with real
     transcripts on disk.
- This suite is opt-in and local-only; CI never runs it.

Exit criteria: make e2e green locally with the measured numbers in the PR
description; the "hours for a flag" problem is measurably dead.
```

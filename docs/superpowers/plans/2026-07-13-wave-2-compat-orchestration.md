# Wave 2 — Gas City compat + control plane: Phase Orchestration Guide

| Field | Value |
|---|---|
| Date | 2026-07-13 |
| Scope | How the compat phases (spec §12) and control-plane phases (spec §7) are dispatched, parallelized, verified, and recovered — the wave-2 companion to `2026-07-06-v1-orchestration.md` |
| Authority | Operator > specs (`docs/design/2026-07-05-gas-camp-design.md` as amended by compat §11; `docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` rev 4; `…-camp-control-plane-design.md` rev 3; `…-camp-pack-imports-design.md` rev 3) > this guide |
| Companion | `.claude/skills/phase-orchestration/SKILL.md` — the lead's behavioral contract, as amended by the wave-2 process amendments below (amendment 4 overrides its never-merge rule). Where this guide and the v1 guide differ, this guide wins for wave-2 streams. |

A fresh wave-2 lead reconstructs the entire state from this file plus
`gh pr list --state all` — nothing load-bearing lives only in a session's
context.

## Wave-2 process amendments (operator directives, 2026-07-13)

These bind every wave-2 stream and amend the v1 guide's flow:

1. **Two-session split.** Planning and implementation are SEPARATE
   sessions per stream. The planner session (superpowers:writing-plans;
   systematic-debugging first for bugfixes) commits its plan doc to the
   stream branch and STOPS. After the plan gate's APPROVE, the lead
   spawns a FRESH implementer session with superpowers:executing-plans
   against the approved doc on the same branch. The implementer records
   the approval note (date, verdict, non-blocking notes, accepted
   deviations) at the top of the plan doc in its first execution commit.
2. **Model policy.** The lead session runs on Fable 5. EVERY dispatched
   agent — planners, implementers, plan reviewers, code reviewers,
   helpers — runs on Opus 4.8 (Agent tool `model: "opus"`). No
   exceptions.
3. **Mid-execution plan defects** go back to the SAME plan reviewer that
   approved the plan (its context is intact) for a binary
   AMENDMENT: APPROVE/REJECT; the implementer holds until the lead
   relays the ruling, and the accepted deviation is appended to the plan
   doc's approval note. (Precedent: fix-82, 2026-07-13.)
4. **Merge on clean review.** When a PR's code-review pass returns
   CLEAN — after the lead's own verification checklist passed — the
   lead merges it (squash, explicit `--subject`/`--body`, no co-author
   trailers) and immediately runs the post-merge rebase protocol. A
   pass WITH findings still goes through the fix-all loop to a fresh
   clean pass first. The operator retains override authority at all
   times.
5. Everything else from the v1 guide stands: plan gate before any
   execution; auto code-review with the fix-all relay loop until clean;
   the lead never edits code, never reads diffs, and merges only under
   amendment 4.

## Prior state (what wave 1 left)

- The three specs (rev 4 / rev 3 / rev 3) merged via PR #87. KNOWN-DEFECTS
  is ADDRESSED with a resolution map.
- Wave-1 bugfix streams (dispatched from the wave-1 lead; may still be in
  flight when wave 2 starts — check `gh pr list --state all`):
  `fix-86-worker-verbose` (issue #86 — --verbose argv + the $0
  real-claude gate), `fix-81-patrol-config-reload` (#81),
  `fix-82-dispatch-branch-collision` (#82),
  `fix-83-failed-dispatch-recovery` (#83). Their kickoffs live verbatim
  as comments on their issues (plus an amendment comment adding the
  two-session split).
- `compat-1-import-binding` was planned in wave 1 (planner + plan gate
  only — implementation is wave 2's first dispatch). Its kickoff is the
  comment on issue #80; its approved plan doc lives on the branch.

## Dependency map (binding — all dependencies must be MERGED)

| Stream | Branch | Contract | Depends on (merged) |
|---|---|---|---|
| compat-1 | `compat-1-import-binding` | compat §12.1 (+ §3/§5/§7/§13/§14), component spec rev 3; fixes #80, #85; creates `GCPACKS_REF` | — (plan already approved) |
| compat-2 | `compat-2-formulas` | compat §12.2 = §9 (rungs 2a–2e incl. `drain`), gate counts pinned per rung (§10) | compat-1 |
| compat-3 | `compat-3-worker-contract` | compat §12.3 = §6 (shims, bead-side claim invariant §6.1, drain-ack release §6.2, python3, real-fragment test §14) | compat-2 · fix-86 |
| compat-4 | `compat-4-mail-prime` | compat §12.4 = §8.2 (`send human` + operator inbox; NO inject hook) + `prime` | compat-3 |
| cp-0 | `cp-0-read-channel` | control-plane §7 phase 0 minus #86 (done in wave 1): byte-offset tailing drained on every wake, Rescan handling, append-only + `max_stream_bytes`, offsets persisted after ledger commit (§2.3) | fix-86 |
| cp-1 | `cp-1-control-protocol` | control-plane §7 phase 1: the one wire-format module, pinned fixtures, socket verbs `interrupt` + `send_turn`, subscribe connection mode §4.4 | cp-0 |
| cp-2 | `cp-2-camp-watch` | control-plane §7 phase 2: the fleet view | cp-1 |
| cp-3 | `cp-3-permission-flow` | control-plane §7 phase 3: `can_use_tool`, BLOCKED, stall-disarm §5.3.3, adoption rule §5.3.4, per-agent stdio flag §5.3.1 | cp-2 |
| cp-4 | `cp-4-camp-attach` | control-plane §7 phase 4: per-agent view (`--include-partial-messages`) | cp-2 *(hard verb dep is cp-1's `subscribe`; the cp-2 edge is conservative sequencing)* |
| cp-5 | `cp-5-overseer` | control-plane §7 phase 5: the operator skill as a control-plane client | cp-3 · cp-4 |

gastown (compat §12.6) is v2 and NOT in this wave — it re-opens
invariant 2 and must start with its own spec round.

## Parallel windows (conservative — compat critical path stays serial)

The compat critical path is 1 → 2 → 3 → 4; the control-plane path is
0 → 1 → 2 → {3, 4} → 5. They join only at fix-86 (cp-0's dep) and at
compat-3 (which touches spawn/dispatch alongside nothing else).

| Window | After merge of | Run concurrently | Conflict notes |
|---|---|---|---|
| W1 | fix-86 (+ compat-1 plan approved) | compat-1 ∥ cp-0 | Nearly disjoint: import/pack machinery vs daemon read channel. Both touch `event.rs`/`vocab.rs`/`fold.rs` additively. |
| W2 | compat-1 | compat-2 ∥ cp-0 (if still open) or cp-1 | compat-2 is formula compiler + graph runtime (`formula/`, `dispatch.rs` drain arm); cp-1 is socket/event_loop. Both touch `dispatch.rs`/`event_loop.rs` — coordinate via the lead; expect a real rebase (v1's W4 precedent). |
| W3 | compat-2, cp-1 | compat-3 ∥ cp-2 | compat-3 owns `spawn.rs` (env, shims) + the shim binary surface; cp-2 is a client-side view. Low overlap. |
| W4 | compat-3, cp-2 | compat-4 ∥ cp-3 ∥ cp-4 | compat-4 is mail verbs on the bead type (mostly camp-core + CLI); cp-3/cp-4 both extend the protocol client/daemon — the highest-overlap pair in the wave (subscribe/eventing), **including the same `spawn.rs` argv-construction region** (cp-3 adds `--permission-prompt-tool stdio`, cp-4 adds `--include-partial-messages`); acceptable with worktree isolation, expect a real rebase between them. |
| W5 | cp-3, cp-4 | cp-5 | Runs alone at the tail. |

The lead may run fewer streams than a window allows (operator review
bandwidth is the true bottleneck); it must never run more.

## Shared files and the rebase protocol (wave 2)

Guaranteed-contention files (keep every touch additive):
`crates/camp/src/main.rs` · `crates/camp-core/src/event.rs` ·
`crates/camp-core/src/vocab.rs` · `crates/camp-core/src/ledger/fold.rs` ·
`Cargo.toml` / `Cargo.lock`. Wave-specific hot spots: `config.rs`
(compat-1 owns the big change; later phases additive), `dispatch.rs`
(compat-2 drain runtime ∥ cp-0/cp-1 daemon work), `spawn.rs` (compat-3;
and again in W4 — cp-3's stdio flag and cp-4's `--include-partial-messages`
land in the same argv region), `.github/workflows/ci.yml`
(compat-1/-2 gate).

Protocol, unchanged from v1 and non-negotiable: kickoffs name in-flight
siblings and their owned files; after ANY merge to main the lead
immediately instructs every in-flight teammate (planners included) to
rebase onto main and re-run the full gates; spec edits are serialized
through the operator — concurrent spec edits are forbidden.

Wave-1 interaction: while any wave-1 fix PR is open, its files are owned
by that stream (`spawn.rs` argv arm + worktree region, `patrol.rs` +
CONFIG_WATCH arm, `dispatch.rs` failed-set region). cp-0 and compat
streams rebase over those merges like any sibling's.

## Worktrees, verification, recovery

Identical to the v1 guide: every parallel teammate in an isolated
worktree (harness `isolation: "worktree"`; first step
`git checkout -b <branch>`; push `-u origin`); the lead's verification
checklist before presenting a PR (branch, `gh pr checks` green — run it
yourself, exit criteria quoted with evidence, plan doc committed with
its approval note, rebased on main); recovery = this guide +
`gh pr list --state all` + `git branch -r` + the kickoff comments on the
issues (bugfixes) / the kickoff blocks below (phases).

## Kickoff composition

Kickoff = the v1 guide's PREAMBLE (from
`2026-07-06-v1-orchestration.md`, with three wave-2 substitutions: the
repo is `github.com/Liquescent-Development/gascamp`; item 2's
"master plan" reads "the specs named in your phase block", and the
plan-approval sentence carries the two-session amendment — the planner
STOPS at the plan doc and a fresh implementer session executes) + the
phase block below, verbatim. The `{PARALLEL_NOTE}` is filled from the
window table above at dispatch time. Planner and implementer sessions
get the same kickoff; the planner is told it is PLANNING-ONLY, the
implementer is told the plan is APPROVED and given the doc path plus
the approval note to record.

## Phase blocks

### compat-1 — implementer only (plan approved in wave 1)

```
Your task is compat phase 1: import machinery + the binding namespace +
pack loader. {BRANCH} = compat-1-import-binding. Your kickoff (context,
scope, acceptance) is the comment beginning "Dispatched as work stream
`compat-1`" on issue #80, plus its amendment. The approved plan doc is
on the branch (docs/superpowers/plans/ — the lead names the exact path
and the approval note when dispatching you). Execute it with
superpowers:executing-plans. Fixes #80 and #85; creates
ci/gc-compat/GCPACKS_REF.
```

### compat-2 — formulas

```
Your task is compat phase 2: the formula key sets, phase-gated 2a–2e.
{BRANCH} = compat-2-formulas. Contract: compat spec §9 in full (every
rung's semantics are verified-in-gc facts — do not re-derive), §10 (the
gate asserts exact counts per rung against GCPACKS_REF; extend
ci/gc-compat accordingly), §12.2. The KNOWN-DEFECTS "verified correct"
list is settled. Scope highlights, binding: dead keys ignored per §4's
three-trap rule; description_file through formula layers; extends
append/replace-in-place; condition pruning; the {{var}} substitution
asymmetry; type="expansion"; drain (2e) with same-session REFUSED,
on_item_failure/single_lane per gc's compiler defaulting, exclusive
reservations as member-bead metadata (gc.exclusive_drain_reservation,
verbatim key). The 21 no-contract formulas are refused, not assumed.
Ceiling is 97–98 and the gate names which (§9).
Exit criteria: every §9 rung's count pinned by a test at GCPACKS_REF;
refusals name their key and land as ledger events; camp ⊆ gc gate still
green (invariant 6); CI green.
```

### compat-3 — the worker contract

```
Your task is compat phase 3: the gc worker contract. {BRANCH} =
compat-3-worker-contract. Contract: compat spec §6 in full — the shims
(§6.3: absolute camp path, gitignored .camp/bin, dispatch-only), the
bead-side claim invariant (§6.1: one ledger row; hook/bd-shim/env are
three byte-projections; the worker env vars), hook --claim --json with
qualified routes, action:"drain" for a closed-bead session and drain-ack
as the release signal (§6.2), shim refusals as shim.refused ledger
events, python3 declared + added to contrib/docker. §12.3.
THE ONE UNSKIPPABLE TEST (§14): render the REAL gc-role-worker fragment
from the corpus at GCPACKS_REF and run it under sh against the real
shims and a fixture camp (fake claude, real ledger); assert claim →
close → drain-ack → exit under a deadline — a hang is the failing
signal. Plus the byte-projection equality test.
Exit criteria: a gc worker closes a gc bead end to end via the real
fragment; every §6 verb served or refused loudly; CI green.
```

### compat-4 — mail + prime

```
Your task is compat phase 4: operator-directed mail + prime. {BRANCH} =
compat-4-mail-prime. Contract: compat spec §8.2 EXACTLY — camp mail
send human (any other recipient refused naming gastown/v2), operator
inbox/read/archive/count, check with gc's exit-code contract (0 = has
mail, 1 = empty), sanitization against a </system-reminder> breakout,
NO injection hook and NO per-turn worker check (invariant 1 ships
intact — §11.2); prime renders the agent's prompt template to stdout
(§6 verb table). Mail beads ride the existing type = "mail"
(fold.rs:13), dispatch-excluded.
Exit criteria: the 10 corpus send-human calls work through the shim;
statusline/status unread surfacing; no polling anywhere; CI green.
```

### cp-0 — the read channel

```
Your task is control-plane phase 0 (minus #86, fixed in wave 1):
campd hears its workers. {BRANCH} = cp-0-read-channel. Contract:
control-plane spec §2.3 in full — per-session byte-offset reads of the
worker stdout file, drained to EOF on EVERY campd wake (any poll
token); notify watch as latency optimization only; Rescan/empty-path/
unknown events drain everything; partial-line buffering; offsets
persisted only after the line's ledger effect commits, adoption
reconciliation before tailing resumes; stream files append-only until
reap, max_stream_bytes breach = loud session failure; §4.3's obligation
to extend the make perf idle gate (M tailed quiescent workers + N
subscribers, 0.0% CPU, <20 MB). §8's state-machine tests: read-on-wake,
Rescan drain, append-only cursors across a campd restart.
Exit criteria: a can_use_tool line written with its notify event
suppressed is consumed on the next unrelated wake; perf gate extended
and green locally; invariant 1 intact; CI green.
```

### cp-1 — protocol + control module

```
Your task is control-plane phase 1: the wire-format module and the
first socket verbs. {BRANCH} = cp-1-control-protocol. Contract:
control-plane spec §2 (one module owns the wire format; shapes pinned
by recorded fixtures; failures loud), §4.1 verbs sessions.list,
session.subscribe, session.send_turn, session.interrupt, §4.4 subscribe
as a connection MODE (per-connection output buffering,
subscriber_buffer_bytes = 1 MiB cap, drop-loudly with subscriber.dropped,
hello within REQUEST_TIMEOUT then timeout-exempt), §8's fixture and
backpressure tests. interrupt's control_response arrives on cp-0's read
channel — that is the end-to-end slice to prove first.
Exit criteria: interrupt and send_turn work end to end against a fake
worker over the real socket; a wedged-campd subscribe fails fast at the
hello; fixtures pin every message shape camp sends or parses; CI green.
```

### cp-2 — camp watch

```
Your task is control-plane phase 2: the fleet view. {BRANCH} =
cp-2-camp-watch. Contract: control-plane spec §5.1 (one line per live
session: agent, bead, state incl. BLOCKED-placeholder, FOR, LAST),
fleet.subscribe (§4.1), clients are stateless renderers addressed by
name (§4.2). BLOCKED rendering lands with cp-3; build the state column
so it drops in.
Exit criteria: camp watch renders live sessions from the socket alone
(no file access), updates push-driven with zero polling; CI green.
```

### cp-3 — the permission flow

```
Your task is control-plane phase 3: can_use_tool end to end. {BRANCH} =
cp-3-permission-flow. Contract: control-plane spec §5.3 in full — the
per-agent stdio flag (§5.3.1: only when the resolved mode can ask;
bypassPermissions agents spawn unchanged; incoherent combo refused at
spawn), BLOCKED state in the ledger, slot exemption + max_blocked
(§5.3.2), stall-timer disarm/re-arm and the ladder-drains-first rule
(§5.3.3), ledger-before-pipe decision ordering and the adoption kill
with its named reason + patrol-restart re-hook (§5.3.4),
session.permission_decision with first-answer-wins (§9),
request_user_dialog answered with a deterministic error (§9). §8's
tests: blocked-forever-not-killed, ladder-drains-first, adoption both
ways.
Exit criteria: a fake worker's can_use_tool blocks, surfaces, is
answered, and the decision is a ledger event with its cause; no
BLOCKED worker is ever nudged, restarted, or killed by the ladder;
CI green.
```

### cp-4 — camp attach

```
Your task is control-plane phase 4: the per-agent view. {BRANCH} =
cp-4-camp-attach. Contract: control-plane spec §5.2 (live typed event
stream; filter, replay from the durable transcript, send-turn,
interrupt), §2.2 (--include-partial-messages gates deltas — attach
needs it, autonomous dispatch must NOT gain it), cursors are byte
offsets (§9). DEFERRED, mirroring cp-2's BLOCKED column: §5.2's
"answer a permission request" needs cp-3's session.permission_decision
verb, which does not exist while you run parallel to cp-3 — build the
view so the answer action drops in after cp-3 merges; do NOT build the
permission path yourself.
Exit criteria: attach + detach against a live fake worker without the
worker noticing; replay of a finished session; CI green.
```

### cp-5 — the overseer

```
Your task is control-plane phase 5: the operator skill as a first-class
control-plane client. {BRANCH} = cp-5-overseer. Contract: control-plane
spec §5.4 — the existing camp:operator skill drives sessions.list /
subscribe / send_turn / interrupt / permission_decision through the
socket, with NO private paths (no file tails, no pids) per §4. The
plugin ships zero agent definitions (master §11 companion clause —
policy test exists).
Exit criteria: the overseer skill performs every §5.4 action against a
fake fleet through the socket alone; CI green.
```

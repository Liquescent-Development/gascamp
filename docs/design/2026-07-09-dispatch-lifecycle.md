# Gas Camp — Dispatch Lifecycle: Isolation, Delivery, and Attended/Autonomous Coordination

| Field | Value |
|---|---|
| Status | Design record — ALL decisions SETTLED (2026-07-09), approved for merge. Docs-only; NO behavior change in this PR. |
| Date | 2026-07-09 |
| Author role | design agent (proposal only) |
| Refs | #29 (attended sling races autonomous dispatch), #31 (no worktree/branch isolation), #34 (pass on un-integrable work) |
| Depends on | #35 (`.camp/` gitignore, parallel PR) |
| Authoritative spec | `docs/design/2026-07-05-gas-camp-design.md` — its §4 decision record is SETTLED |

## Final settled model (2026-07-09) — read this first

All open questions the operator owned are now **SETTLED**. This is the design a
fresh reader should take away; the deprecated sections below are kept only for
history. Nothing here is implemented in this PR — the spec §8.4/§12 edits and the
`WorkOutcome` vocabulary addition are **future implementation, serialized through
the operator.**

- **One dispatch path (Q6, APPROVED).** `camp sling` and `/camp:sling` are the
  **same single path**: enqueue a bead → campd dispatches → the worker claims.
  The slash command no longer spawns a teammate. **Spec §8.4's "attended teammate
  is the one surface exception" is deleted** (operator signed off; future edit).
  The #29 race is structurally gone — there is only one spawner.
- **Talk to work via a converse verb (Q6).** A new uniform `camp` verb sends a
  turn to any running session (worker or overseer), delivered live over the
  existing held-stdin pipe or `claude --resume` after the turn (A4) — mirror of
  Gas City's `gc nudge`/session-message. Interactivity is a runtime/harness
  capability, never a bespoke dispatch mode. No reservation, no second spawner.
- **The overseer is the human's own session (Q7, DECIDED).** The human's Claude
  Code session + camp plugin **is** the interactive overseer (spec §4 made
  literal). An **optional on-demand pack overseer agent** covers away-mode.
  **No core standing `named_session`** — that preserves "idle is free / zero
  agent processes" (spec §8.4). Fuller gc mirror (a core standing session)
  deferred.
- **Roles are pack content.** Overseer / coder / committer are **pack-defined**
  agents (mirroring Gas City's swarm pack), over the six primitives. The camp
  plugin stays machinery-only (spec §11).
- **Isolation is pack/agent-declared, default worktree (Q1, APPROVED).** The
  `isolation=` agent field stays the knob (gc-mirroring: swarm=none,
  gastown=worktree); autonomous dispatch **defaults to worktree** (spec §12 edit,
  signed off). #31 fixed: autonomous workers never touch the rig's live branch.
- **Delivery is pack behavior + a mirrored outcome axis (Q3, REVISED; #34).**
  *How* work is committed/branched/landed is **pack/prompt content** (a
  delivery-aware coder + optional committer agent, gc's swarm model).
  *Whether* it landed is recorded on Gas City's **`WorkOutcome` axis**
  (`shipped` / `no-op` / `blocked` / `abandoned`), **mirrored verbatim from gc**
  as a SEPARATE axis from the control `Outcome` (`pass`/`fail`/`skipped`/…).
  Un-integrable work is `blocked` on the WorkOutcome axis — not shoehorned into
  `outcome`. campd still fails fast when a rig can't host a worktree.
- **v1 "landed" = a local bead branch with a base (Q4, SETTLED).** Landed means
  committed on the `camp/<bead>` branch that descends from / merges into the rig's
  integration branch — the branch is the reviewable/mergeable artifact. **Remote
  push / PR-host / MR creation is explicitly OUT of scope for v1** (camp has zero
  git-remote code today; a future scoping decision if ever wanted).
- **Nothing remains open.** All operator questions — **Q1, Q3, Q4, Q5, Q6, Q7 —
  are SETTLED.** (Q5, SETTLED: the two worker-contract copies are unified into one
  source *before* delivery semantics are added — the first step of Phase 3.) This
  note is the merge-ready design record.

### Decision log

- **2026-07-09 — Operator APPROVED Q1.** Worktree isolation is the DEFAULT for
  autonomous dispatch; the operator signs off on the spec §12 edit this implies
  (the actual §12 change + implementation land later, serialized through the
  operator, per §9). Attended teammates remain the documented exception (A2).
  Folded into §4.2 and §7.
- **2026-07-09 — Q3 EVOLUTION.** *Original* (same day): gate un-integrable work
  as `fail`-with-reason; no new `outcome` value (mirror-safe, minimal). *Then* the
  Gas City source study found gc records "blocked" on a separate `WorkOutcome`
  axis. *Revised & SETTLED* (below): **adopt gc's `WorkOutcome` axis verbatim**
  instead. See the Q3-REVISED entry.
- **2026-07-09 — Q2 investigated** against the pinned Gas City reference
  (§7 Q2 investigation). Verdict: **no meaningful deviation from Gas City**;
  recommended claim-at-creation reservation. **SUPERSEDED by Q6** (no
  reservation) — retained for history.
- **2026-07-09 — REFRAME (operator directive), SUPERSEDES the reservation
  approach.** Camp is k3s-to-gc's-k8s: a conformant lighter implementation of the
  SAME model, not a snowflake. Broad Gas City source study (see the **Reframe**
  section) confirmed the operator's premise: Gas City is driven "talk-to-overseer
  + converse-with-any-worker" entirely by **pack content** (a `mayor` agent, a
  `committer` agent, `named_session` modes, mail) over **one dispatch path**,
  with interactivity supplied by the **runtime/harness**. The reservation
  approach (old §4.1 / §7 Q2) is **deprecated below, kept for history.**
- **2026-07-09 — Operator APPROVED Q6 (SETTLED). Mirror Gas City: one dispatch
  path.** `/camp:sling` == `camp sling` (enqueue only); add a uniform **converse
  verb** (send a turn to any running session, live over held-stdin or
  `claude --resume`); overseer/coder/committer are **pack-defined**. **Delete
  spec §8.4's "attended teammate is the one surface exception"** — operator signed
  off on the §8.4 edit (future, serialized). #29 fix = one dispatch path +
  pack/harness converse; **no reservation, no second spawner.** Folded into the
  Reframe + §7 Q6.
- **2026-07-09 — Operator DECIDED Q7 (SETTLED). Overseer = the human's Claude
  Code session + plugin** (spec §4 made literal), with an **optional on-demand
  pack overseer agent** for away-mode. **No core standing `named_session`
  overseer** — preserves idle-is-free (idle = zero processes). Tradeoff recorded:
  fuller gc mirror (a core standing session) deferred. Folded into Reframe + §7 Q7.
- **2026-07-09 — Operator REVISED Q3 (SETTLED). Adopt Gas City's `WorkOutcome`
  axis** (`shipped`/`no-op`/`blocked`/`abandoned`), mirrored VERBATIM from gc, as
  a SEPARATE axis from the control `Outcome` (`pass`/`fail`/`skipped`/`missing_root`).
  Un-integrable/blocked work is recorded as `blocked` on the WorkOutcome axis, not
  shoehorned into `outcome`. Future-impl mechanics: pin the WorkOutcome set in
  `crates/camp-core/tests/fixtures/gc-vocab.json` and validate via
  `ci/gc-compat/check_vocab.sh` — an additive **mirrored** axis, not a
  redefinition, so the mirror invariant is preserved. Folded into §4.3 + §7 Q3.
- **2026-07-09 — Operator SETTLED Q4. v1 "landed" = committed on the bead branch
  with a base** (local): the `camp/<bead>` branch, reachable and diffable, is the
  reviewable/mergeable artifact. **Remote push / PR-host / MR creation is
  explicitly OUT of scope for v1** (camp has no git-remote code; a future scoping
  decision if ever wanted). Folded into §4.3 + §7 Q4.
- **2026-07-09 — Operator SETTLED Q5. Unify the two worker-contract copies**
  (`plugin/skills/worker/SKILL.md` + the mechanical `WORKER_CONTRACT` in
  `crates/camp/src/daemon/spawn.rs`) into ONE source **before** adding delivery
  semantics — the first step of Phase 3. Folded into §7 Q5 + Phase 3.
- **2026-07-09 — ALL QUESTIONS SETTLED.** Q1/Q3/Q4/Q5/Q6/Q7 are closed; Q2 is
  superseded (history). Nothing remains open — the note is the merge-ready design
  record. Approved for merge as PR #39.

## Reframe (2026-07-09, operator): pack-first, mirror Gas City — SUPERSEDES §4.1 reservation & the §8.4 exception

> **This section supersedes the reservation mechanism (old §4.1) and the
> "attended teammate is the one surface exception" framing.** Those are kept
> below, marked DEPRECATED, for history. Everything here is still
> proposal-only — no code, no spec edit, no pack change.

**The reframing.** Camp is k3s to Gas City's k8s: a lighter, *conformant*
implementation of the **same** model for daily-driving from Claude Code — not a
snowflake with camp-specific concepts. Invariant 4 ("six primitives, zero roles
in code; campd moves work, never reasons about it") means the "how you drive it"
experience — an overseer you talk to, plus the ability to converse with any
worker — must live in **packs over the six primitives**, not baked into
core/spec. The operator's premise: *Gas City is already driven this way with the
right pack.* I verified it against the actual Gas City source at the pinned ref
(`ci/gc-compat/GASCITY_REF` = `12410301…`); the premise **holds**.

### Gas City ground truth (source-cited, gascity@`12410301…`)

**1. How a human drives work.** One path: create/`sling` beads → the
controller/reconciler dispatches sessions → workers **claim ready, unassigned
beads** and work them. The mayor's own prompt spells it out: "You plan work,
break it into tasks (beads), and let the rig coders self-organize to claim them"
and coders "Run `gc bd ready --unassigned` … then claim and work on a task"
(`examples/swarm/packs/swarm/agents/mayor/prompt.template.md`,
`agents/coder/agent.toml`). The human converses with any session — overseer or
worker — through **uniform verbs**: `gc mail`, `gc nudge`, a session-message API
(`sendUserMessageToSession`, `internal/api/handler_session_interaction.go`),
`gc handoff`, `gc prime` (`cmd/gc/cmd_{mail,nudge,handoff,prime,session}.go`).
There is **no "attended vs autonomous" dispatch fork.** In Gas City "attended"
means only *a human is attached to a session's process, so the controller does
not restart it* — a lifecycle property, not a way to dispatch work
(`cmd/gc/cmd_handoff.go`: "the controller cannot restart the **user-attended
process**"). Source search: `attended` appears 11× (all docs / tmux / handoff /
reconciler lifecycle), `teammate` 1× (a design doc) — neither is a core dispatch
concept.

**2. Is there an overseer / "mayor"?** Yes — and it is an **agent (a prompt),
not a core service.** Gas City ships a *default* mayor prompt
(`cmd/gc/prompts/mayor.md`: "You are the mayor … plan work, manage rigs and
agents, dispatch tasks, monitor progress"), and packs define their own
(`examples/swarm/packs/swarm/agents/mayor/`). The mayor's `agent.toml` is three
lines (`scope`, `nudge`, `idle_timeout`); everything it *does* is prompt-driven
and uses ordinary verbs (`gc bd …`, `gc mail …`, `gc status`). "Never Code: If
you see a bug … file a bead. Don't fix it yourself." A pack declares standing
overseer sessions via `[[named_session]]` with a `mode`
(`always`/`on_demand`) and `scope` (`examples/swarm/packs/swarm/pack.toml`:
mayor=on_demand, deacon=always). The overseer is **pack content over the
primitives**, full stop.

**3. Pack/formula-defined vs core.** The whole "talk-to-overseer +
converse-with-worker" experience is **pack content**: a `mayor` agent (planning
+ mail), `coder` agents (claim + work), even a dedicated **`committer` agent**
that is "the only agent in the swarm that touches git"
(`examples/swarm/packs/swarm/agents/committer/prompt.template.md`), wired by
`named_session` modes + mail. **Core** provides only the mechanical substrate:
the store/ledger, one controller dispatch path, sessions, the runtime
abstraction, and the verbs (`sling`/`mail`/`nudge`/session-message). So yes — a
pack turns Gas City into exactly the experience the operator wants, with zero
core role logic. Camp already mirrors the shape (plugin = machinery/verbs, roles
= packs, spec §11).

**4. Attach / converse mechanics — core vs runtime vs harness.** Conversing with
a running worker is a **core verb** (`session message` / `nudge` →
`SessionHandle.Respond`, `internal/worker/handle_interaction.go`) whose
*interactivity is a runtime/provider capability*: `PendingStatus` returns
`supported bool` — "whether the underlying runtime supports interactive blocking
requests at all." Interactive attach is the **tmux provider** (attachable panes,
`docs/reference/tmux-agent-slice.md`); the `exec`/subprocess providers are
headless children; `acp` is an interactive protocol runtime
(`internal/runtime/{tmux,exec,acp}/`). Each provider is conformance-tested for
"Interactions" (`internal/worker/builtin/README.md`, grid: claude/codex/gemini/…
all ✅). **So "attach to / talk to a running worker" = a core verb + a
runtime/harness capability, never a bespoke dispatch mode.**

### Verdict on the premise

**Confirmed, strongly.** Gas City delivers "talk to an overseer, converse with
any worker" with **pack content over one dispatch path**, interactivity from the
runtime/harness. Camp's §8.4 "attended Tier-0 sling spawns the worker as a
teammate … the one surface exception," and the reservation that a second spawner
needs to avoid racing campd, are a **camp snowflake with no Gas City analog** —
exactly what invariant 4 says not to build. Remove it.

### Pack-first redesign — what is pack, core, harness

| Concern | Where it lives (mirror-gc) |
|---|---|
| The overseer you talk to ("mayor") | **Pack** — a starter-pack agent + prompt (plans, `camp sling`s beads, reads `camp ls`/`camp top`, never codes). Camp's twist: the **human's own Claude Code session + camp plugin already IS the interactive overseer** (spec §4 "drive from inside Claude Code"); a *persistent* pack overseer is optional, for away-mode, mirroring gc's `mode="on_demand"` mayor. |
| Deciding who does the work | **Core, one path** — `camp sling`/`camp create` → campd dispatches → worker claims. No second spawner, no reservation. |
| Git delivery (how/what/when to commit, branch, land) | **Pack** — coder/committer agent prompts (gc's `committer` role). Camp already ships a starter `dev` agent; a delivery-aware prompt (and/or a committer agent) is pack content, not core. |
| Isolation (worktree/branch) | **Pack/agent-declared** — already the `isolation=` field (gc packs choose per pack: swarm=none, gastown=worktree). The *default* is a core policy (Q1). |
| Converse with a running worker | **Core verb + harness** — a uniform `camp` "send a turn to a session" verb (mirror of `gc nudge`/session-message), delivered live over the held stdin pipe (already built: `nudge_via_stdin`, dispatch.rs) or via `claude --resume` after the turn (A4). Camp has **no user-facing converse verb today** — this is the one small core surface the drive-experience needs. |
| Standing/named overseer session | **SETTLED (Q7): human-session-only; NO core standing session.** The human's Claude Code session + plugin is the overseer; an optional **on-demand pack overseer agent** covers away-mode. Camp does **not** add gc's `[[named_session]] mode=always` core capability — that preserves "idle = zero agent processes" (spec §8.4). Fuller gc mirror (a core standing session) deferred. |

### §8.4 disposition — SETTLED (Q6 APPROVED, 2026-07-09)

**REMOVE the "attended teammate is the one surface exception" mechanism**;
collapse to one dispatch path + pack-defined drive + a uniform converse verb.
The operator **approved the spec §8.4 edit** (delete the surface exception;
state that conversing with any worker is a verb over the runtime/harness, and
the overseer is pack content / the human's own session). The §8.4 amendment is
**future implementation, serialized through the operator — not edited in this
PR.**

### Reframed #29 / #31 / #34

- **#29 (the race) — dissolved, not reserved.** The bug is that `/camp:sling`
  adds a **second spawner** (an attended teammate) racing campd's dispatch. The
  mirror-gc fix: `/camp:sling` and `camp sling` are the **same single path** —
  enqueue a bead; campd dispatches; the worker claims. "Talking to the work" is
  the uniform converse verb (held-stdin live / resume), **not** a reservation
  that lets a competing spawner win. No reservation, no `dispatch="attended"`
  field, no §8.4 exception. (The Q2 investigation stands as the record of *why*
  reservation was mirror-safe-but-unnecessary: the cleaner answer is to not have
  a second spawner at all.)
- **#31 (isolation) — already pack/agent-shaped; keep Q1.** gc treats isolation
  as a per-pack/agent choice (swarm=none, gastown=worktree); camp already mirrors
  this via `isolation=`. The operator-approved worktree **default** (Q1) is a
  core default policy, fully consistent with mirroring gc — not a snowflake.
  Unchanged.
- **#34 (delivery) — SETTLED: pack behavior + the mirrored `WorkOutcome` axis
  (Q3 REVISED).** *How* work is committed/branched/landed is **pack/prompt
  content** (gc's `coder`+`committer` roles), not a core mechanism — the delivery
  contract belongs in a starter-pack agent prompt (and optionally a committer
  agent), mirroring gc. *Whether* it landed is recorded on Gas City's
  **`WorkOutcome` axis (`shipped`/`no-op`/`blocked`/`abandoned`)**, **mirrored
  verbatim from gc** as a SEPARATE axis from the control `Outcome`
  (`pass`/`fail`/`skipped`/`missing_root`). Un-integrable work is `blocked` on the
  WorkOutcome axis — not shoehorned into `outcome`. Future-impl: pin the
  WorkOutcome set in `gc-vocab.json` + validate via `check_vocab.sh` (an additive
  mirrored axis, mirror invariant preserved). campd still fails fast when a rig
  can't host a worktree (mechanical, core). See §7 Q3.

## 0. Scope and constraint

This note designs ONE subsystem — the lifecycle of a dispatched worker from
"a bead is ready" to "the work is landable and the outcome is honest" — as a
single coherent model. It covers three issues that are facets of that one
problem:

- **#29** — WHO does the work (attended teammate vs autonomous headless), and
  how that is chosen without a race.
- **#31** — WHERE the work happens (the working-tree / branch contract) so
  autonomous workers do not mutate a rig's live branch.
- **#34** — WHAT "done" means (a delivery contract) so `pass` cannot be
  reported over stranded, un-integrable work.

This PR adds ONLY this note. It changes no `crates/**`, no
`plugin/skills/worker/SKILL.md`, no `packs/**`, no command. Anything that would
require editing the authoritative spec is called out as an OPEN QUESTION for
operator/spec sign-off (§7); it is not decided here.

## 1. The problem, verified against the code

All three issues are reproducible from one first-run scenario (a `git init`'d
rig, no `main`, no remote, no `.gitignore`, default `dev` agent). The
observable failure was: `/camp:sling "give this repo a README"` → a **headless**
worker won a race for the bead, edited the rig's **live `main`**, committed the
README as a **root commit on a lone `add-readme` branch** with no integration
path, and closed **`pass`**. Three defects, one flow. Verified findings:

### 1.1 #29 — attended and autonomous dispatch race for one bead

`/camp:sling` (`plugin/commands/sling.md`) runs `camp sling $ARGUMENTS` and then,
in the *same* LLM turn, is instructed to spawn the pack agent as a teammate.
But `camp sling` (`crates/camp/src/cmd/sling.rs:98-107`) appends `bead.created`
(with the routed agent as `assignee`) and immediately **pokes campd** via
`socket::require(..., Poke)` — the pure-client poke (campd down is a loud
error, never a spawn). On that poke, `campd`'s dispatcher
(`crates/camp/src/daemon/dispatch.rs:332` `converge`) queries the full
dispatchable set and spawns a headless `claude -p` worker
(`launch`, dispatch.rs:488), which claims the bead.

The two paths are uncoordinated. campd is a fast native process reacting to a
socket poke; the attended teammate must be spawned by the LLM, read the worker
skill, and only then run `camp claim`. **campd essentially always wins.** The
README's promise ("spawns it as a teammate you can talk to") loses to an opaque
headless worker.

Crucial mechanical detail: `converge` re-runs `dispatchable_beads()` **in full
on every wake** (dispatch.rs:338), not just for the poked bead. So "just don't
poke on an attended sling" is **not** a fix — the next unrelated poke (another
sling, any `bead.closed`) would sweep up the still-dispatchable attended bead.
Suppression must be a **durable ledger fact the dispatchable query respects**,
not the absence of a poke.

The dispatchable query
(`crates/camp-core/src/readiness.rs:114` `dispatchable_beads`) already excludes
a bead when EITHER its `status != 'open'` OR a `sessions` row is bound to it:

```sql
WHERE b.status = 'open' AND b.type = 'task'
  AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.bead = b.id)
  AND NOT (unmet deps) AND NOT (run root)
```

So a bead that is **claimed (in_progress)** or **has a session bound** is
already invisible to autonomous dispatch. The fix rides on this existing
exclusion — see §4.1.

**Spec-vs-code note (finding, not a re-litigation):** spec §8.4 states attended
Tier-0 sling is "the one surface exception" and "spawns the worker as a teammate
inside your session." The *intent* is settled. But **the code has no mechanism
to make that exception hold** against campd's autonomous dispatcher — the
teammate surface is layered on in the slash command with nothing stopping campd
from grabbing the bead first. This is a case where implementation reality does
not yet realize the spec's stated behavior; AGENTS.md requires spec and code not
to silently diverge, so closing this gap is realizing the spec, not changing it.

### 1.2 #31 — autonomous workers run on the rig's live branch

`Isolation` defaults to `None` (`crates/camp-core/src/pack.rs:19-23`,
`#[default] None`), and the starter `dev` agent
(`packs/starter/agents/dev.md`) sets no `isolation`. So `prepare`
(dispatch.rs:440, `make_worktree = agent.isolation == Isolation::Worktree`)
resolves the worker cwd to the **rig path itself** (dispatch.rs:458), on
whatever branch is checked out there — observed: `main`. Autonomous workers
therefore edit and commit the rig's primary branch in place, and two concurrent
workers on one rig would collide on a single working tree.

This **matches the current spec** — §12 says "Dispatch sets the worker's cwd to
the rig — *or* to a camp-managed worktree ... when the agent definition sets
`isolation = "worktree"`." Worktree isolation is opt-in by design today. So #31
is not a spec-vs-code divergence; it is a request to **change the spec's
default** — a spec §12 edit, **APPROVED by the operator 2026-07-09** (§4.2, §7
Q1). The §12 amendment and code change land later, serialized through the
operator; this note only records the decision.

### 1.3 #34 — `pass` has no delivery semantics

Neither worker contract mentions committing, branching, or integration:

- The mechanical floor `WORKER_CONTRACT` (`crates/camp/src/daemon/spawn.rs:18-25`)
  says only "Do the work in the current directory" then "Close it ... `pass`."
- The richer skill (`plugin/skills/worker/SKILL.md` §3 "work", §6 "close")
  says "Make the change" and "`pass` on success" — zero delivery guidance.

The worker filled that vacuum from an unrelated global rule ("never commit to
`main`, always PR"), producing a root commit on a stray branch of a repo with no
`main` and no remote, then closed `pass`. `close`
(`crates/camp/src/cmd/close.rs`) accepts any outcome in
`CAMP_OUTCOMES = ["pass","fail","skipped"]` (`crates/camp-core/src/vocab.rs:45`);
nothing gates `pass` on the work being landable.

**This is not only an empty-repo problem.** Even the *isolated* path strands
work: `remove_worktree` (spawn.rs:295-297) explicitly **leaves the `camp/<bead>`
branch standing** ("it may hold unpushed work; sweeping is Phase 11 policy").
There is **no `git push`, no PR, no merge anywhere in the codebase** (verified by
grep). So today "delivered" tops out at "committed to a local branch that
nothing integrates." The delivery contract must define what counts as landed,
for both the isolated and the fresh-repo cases.

## 2. Invariants and settled decisions this must honor

From AGENTS.md and spec §2/§4 — cited so the proposal stays inside the lines:

- **Idle is free / no query loops** (inv. 1; spec §7.3). The suppression and
  landability facts must be evaluated on the existing append→fold→dispatch path,
  never a new poll.
- **Cost proportional to job** (inv. 2). Tier-0 must stay "one worker spawn, ~3
  ledger writes." No delivery machinery may tax the small job.
- **Nothing hidden** (inv. 3; spec §13). Reservation, isolation choice, and
  every non-`pass` delivery verdict must each be a ledger event with its cause.
- **Six primitives, zero roles in code; campd moves work, never reasons about
  it** (inv. 4; spec §8.3 Zero-Framework-Cognition line). campd may *honor* a
  declared reservation flag or a mechanical "is this a git repo with a base
  commit" check, but it must not judge *content* or *landability by inspection*.
  The landability *judgment* stays with the worker (its close outcome) and with
  mechanical git facts, exactly like check-scripts (`check.mode="exec"`).
- **Fail fast** (inv. 5; spec §15.1). A rig that cannot support the delivery
  workflow must fail at dispatch with a ledger event, not silently strand work.
- **Vocabulary mirror** (inv. 7; spec §8.2, §15.2). camp's `outcome` values stay
  a subset/mirror of Gas City's control `Outcome` set
  (`["pass","fail","skipped","missing_root"]`,
  `crates/camp-core/tests/fixtures/gc-vocab.json`). **Q3-REVISED (SETTLED): camp
  adopts Gas City's `WorkOutcome` axis (`shipped`/`no-op`/`blocked`/`abandoned`)
  VERBATIM as a SEPARATE additive axis** — un-integrable work is `blocked` on
  WorkOutcome, never shoehorned into `outcome`. Because it mirrors gc's own set,
  it is additive (not a redefinition) and city export stays native; the future
  impl pins it in `gc-vocab.json` and validates via `check_vocab.sh`.
- **Settled §4 / §8.4 that this proposal keeps:** campd is the sole dispatcher
  of autonomous/graph work; the attended teammate is the single surface
  exception; A2 (resolved) — a teammate's cwd is pinned to the parent session's
  directory, there is no per-agent cwd for teammates. This last fact is
  load-bearing (§4.4).

## 3. The model in one paragraph

> **NOTE (2026-07-09):** gate (1) below is SUPERSEDED by the Reframe — the live
> model for coordination is **one dispatch path + a converse verb**, not an
> attended/autonomous reservation. Gates (2) isolation and (3) delivery still
> hold. Read the Reframe section for the current gate (1).

A bead's lifecycle has three declared, mechanical gates, each a ledger fact:
(1) **Coordination** — at sling time the operator makes an *explicit* choice,
attended or autonomous, and an attended choice writes a durable reservation that
removes the bead from campd's dispatchable set atomically with its creation, so
there is no race. (2) **Isolation** — an autonomous worker is given a
camp-managed worktree on a per-bead branch (`camp/<bead>`); the worker never
touches the rig's live tree; campd refuses to dispatch a worktree into a rig
that cannot support one (not a git repo / no base commit), failing fast rather
than stranding work. (3) **Delivery** — *how* work is committed/branched/landed
is pack/prompt content (a coder + optional committer agent), and *whether* it
landed is recorded on Gas City's mirrored `WorkOutcome` axis
(`shipped`/`no-op`/`blocked`/`abandoned`), separate from the control `outcome`
(Q3-REVISED). The three gates compose into one honest lifecycle: one dispatch
path chose who does the work, isolation gave it a clean tree, and the WorkOutcome
tells the truth about whether it landed.

## 4. Proposed model

### 4.1 Coordination — attended vs autonomous is an explicit, atomic choice (#29)

> **DEPRECATED (2026-07-09) — SUPERSEDED by the Reframe section.** The
> reservation mechanism below is kept for history and for the Q2 mirror-safety
> record. The adopted direction is: **one dispatch path + a uniform converse
> verb; no reservation, no attended/autonomous fork.** Read the Reframe section
> for the live design; this subsection no longer drives #29.

**Principle:** attended-vs-autonomous is a *declared choice*, resolved in the
`camp` CLI before any poke — never a race, never campd "reasoning" about
presence.

**Mechanism (recommended, no new vocabulary):** an autonomous sling behaves as
today (create + poke → campd dispatches headless). An **attended** sling
atomically writes, in one WAL batch, `bead.created` **plus** a reservation that
makes `dispatchable_beads()` skip the bead. The reservation reuses the existing
exclusion: the bead is created **already claimed by the attended session**
(`attended/<session_id>`, which the SessionStart hook has already registered).
Because the bead is then `in_progress`, campd's dispatchable query excludes it
**from the instant it exists** — campd can never see it dispatchable, on this
poke or any later one. The slash command then spawns the teammate, which takes
ownership of the already-reserved bead and runs the worker skill.

Why this shape:

- **Atomic by construction.** The single writer that creates the bead also
  reserves it, in one transaction, before the poke. There is no window.
- **Rides existing mechanics.** `status != 'open'` already excludes a bead from
  dispatch (readiness.rs:117); no query change, no new campd branch.
- **Explicit, not inferred.** The choice is a CLI flag / plugin-context signal
  (e.g. `camp sling --attended`, or the plugin passing an attended marker), with
  a config default. campd never inspects "is a user present" — it only honors a
  declared ledger fact (inv. 4).

**Mirror-safety (Q2, verified — see §7 Q2 investigation).** Claim-at-creation is
the mirror-safe choice: it reuses camp's *existing* additive `bead.claimed`
event (`CAMP_SPECIFIC_EVENTS`, CI-verified absent from Gas City source) and
introduces **zero** new vocabulary. A reserved bead is simply `in_progress` with
an `assignee`/`claimed_by` — a state Gas City understands natively, so it exports
cleanly. Gas City has no attended-vs-autonomous concept at all, so this
coordination is a permissible camp-specific addition regardless of variant; the
claim-at-creation variant is preferred precisely because it mints no new name to
collide with a future Gas City concept.

**The one mechanical detail that needs a decision:** the teammate must be able
to take over a bead the attended *orchestrator* session reserved. Options:
(a) a claim reassignment (`camp claim --force` / a `reassign` verb) from
`attended/<id>` to the teammate's session; (b) the teammate inherits the
reservation identity; (c) reserve under a neutral `attended` marker and let the
teammate `claim` cleanly. Recommended: (a) — smallest, fully mechanical, keeps
the worker skill's "claim first" shape. See §7 Q2.

**Alternative (more explicit, needs a new field):** stamp `bead.created` with
`dispatch = "attended"` and add `AND dispatch != 'attended'` to
`dispatchable_beads`. The teammate then `claim`s cleanly (no reassignment), and
"release to autonomous" is a re-stamp. This is arguably the clearer expression
of "attended vs headless is an explicit choice," but it adds a bead field and a
camp-additive concept → §7 Q2/Q3.

**Hand-off / release policy** (DEPRECATED — was §7 Q2, now SUPERSEDED; moot with
no reservation): if the operator dismissed the teammate without it claiming, the
reserved bead would sit `in_progress`, visible
in `camp ls`. Do we (i) leave it for the operator, (ii) offer
`camp sling --headless <bead>` / a release verb to hand it to autonomous
dispatch, or (iii) let adoption reclaim it after the attended session ends?
Adoption already returns a dead session's claimed beads to ready (spec §8.5) —
so (iii) may fall out for free once the reservation is a real claim.

### 4.2 Isolation — the working-tree contract (#31)

**Principle:** an autonomous worker must never mutate the rig's live tree. Its
working tree is a camp-managed worktree on a per-bead branch; its output is that
branch.

The machinery **already exists** and is correct — `create_worktree` /
`ensure_worktree` / `remove_worktree` (spawn.rs:236-313) build
`<camp>/worktrees/<bead>` on branch `camp/<bead>`, reap on clean pass, keep on
failure for forensics, and adoption sweeps orphans (spec §8.5). The only gaps
are **which agents get it** and **fail-fast when a rig can't support it**:

1. **Default vs opt-in — APPROVED 2026-07-09 (SPEC §12 EDIT signed off).** Today
   isolation is opt-in per agent (pack.rs default `None`, matching spec §12).
   #31 asked to make it the default; **the operator has approved this and signed
   off on the spec §12 edit it implies.** Approved direction: **autonomous
   dispatch defaults to worktree isolation**, with `isolation = "none"` as an
   explicit, loud opt-out for agents that intentionally want the live tree —
   because "autonomous edits landing on `main`" is precisely the hazard #31
   reports. The actual §12 spec amendment and the code change land later,
   serialized through the operator (Phase 2, §9). This note records the decision;
   it makes no code or spec edit here. Mirror check: worktree-per-worker
   isolation is consistent with Gas City, which itself events
   `bead.worktree.reaped` / `bead.worktree.reap_skipped` — not a deviation.

2. **Fail fast when the rig can't support a worktree (ties to #34).**
   `git worktree add -b camp/<bead>` requires a git repo with a base commit; on
   a freshly `git init`'d rig with zero commits it fails. Today that failure is
   handled per-bead (`launch` appends `dispatch.failed`, dispatch.rs:494-503) —
   which is exactly the right shape: **campd should not dispatch code work into
   a rig that cannot support the workflow; it should fail fast with a ledger
   event, not strand work.** This is the same gate #34 wants (§4.3), reached via
   isolation. Building on #35 (`.camp/` gitignored) means the worktree/branch
   and camp runtime state do not pollute the rig's tracked files.

3. **The working-tree contract, documented** (not code): dispatched autonomous
   work happens on `camp/<bead>`, isolated from the operator's checkout; the
   branch is the deliverable; the worktree is reaped on pass, kept on failure.

### 4.3 Delivery — what "landed" means, and the `pass` gate (#34)

**Principle:** `pass` asserts *delivered*, not merely *edited*. "Delivered"
must be defined and mechanically checkable at the boundary of what camp can know
without judging content.

Proposed delivery contract (to be added to `plugin/skills/worker/SKILL.md` §3/§6
and mirrored in the mechanical floor `WORKER_CONTRACT` — **in a later
implementation PR, not here**):

- **Commit to the bead branch.** In an isolated autonomous worker, work is
  committed to `camp/<bead>` (the branch it was dispatched onto). The worker does
  **not** invent branching policy from unrelated global rules.
- **"Landed" for v1 — SETTLED 2026-07-09 (Q4).** Work is *landed* when it is
  **committed on the bead branch and that branch has a base** (it descends from,
  or is mergeable into, the rig's integration branch). The `camp/<bead>` branch,
  reachable and diffable, **is** the reviewable/mergeable artifact — matching the
  operator's "changes reach the integration branch only via review" workflow
  **without** camp needing a remote or a PR host. **Remote push / PR-host / MR
  creation is explicitly OUT of scope for v1** (§8); camp has zero git-remote code
  today, and adding it would be a future spec + scope decision.
- **Fresh / empty repo — DECIDED 2026-07-09: fail fast at dispatch.** The
  "always branch + PR" pattern is meaningless with no base and no `main`. campd
  **refuses to dispatch** code work into such a rig: `git worktree add` fails,
  `dispatch.failed` is evented, no worker ever runs and nothing is stranded
  (§4.2.2). This is the fail-fast, invariant-5 answer and needs no new worker
  cleverness; the operator prepares the rig (a base commit) before dispatching
  code work, the same way #35 prepares `.gitignore`. (The rejected alternative —
  letting the worker's first commit establish the integration branch — would put
  content judgment in the worker for the empty-repo edge; fail-fast is cleaner
  and consistent with the WorkOutcome delivery gate below.)
- **Record delivery on the `WorkOutcome` axis — REVISED & SETTLED 2026-07-09.**
  *(This supersedes the earlier same-day `fail`-with-reason bullet.)* Whether work
  landed is recorded on Gas City's **`WorkOutcome` axis
  (`shipped`/`no-op`/`blocked`/`abandoned`)**, mirrored VERBATIM from gc as a
  SEPARATE axis from the control `outcome` (`pass`/`fail`/`skipped`/`missing_root`).
  Un-integrable work is **`blocked`** on WorkOutcome (the worktree is kept for
  forensics, so nothing is lost); shipped work is **`shipped`**. Because it
  mirrors gc's own set, it is additive (not a redefinition) and city export stays
  native — the future impl pins the set in
  `crates/camp-core/tests/fixtures/gc-vocab.json` and validates via
  `ci/gc-compat/check_vocab.sh`. *How* the worker commits/branches to reach
  `shipped` is pack/prompt content (a delivery-aware coder + optional committer
  agent), mirroring gc's swarm. See §7 Q3 for the full evolution.

**campd's role stays mechanical.** campd does not read diffs or judge quality. It
(i) honors the declared isolation, (ii) fails fast when the rig can't host a
worktree, and (iii) records the worker's own close outcome. The landability
*judgment* is the worker's (an agent) plus mechanical git facts — the same
division as check-scripts (spec §8.3). No role names, no `if stranded then…` in
Rust.

### 4.4 How the three compose — and the teammate-cwd tension

> **PARTIALLY SUPERSEDED (2026-07-09) by the Reframe section.** With the attended
> teammate removed (one dispatch path + converse verb), the attended-teammate
> cwd tension below is moot for the *dispatch* decision. It survives only as
> context for isolation (autonomous workers isolate; the human's own overseer
> session runs in the operator's tree). Read the Reframe section first.

The three gates are one lifecycle, but they interact at one sharp point:
**A2 (resolved, settled): a teammate's cwd is pinned to the parent session's
directory — there is no per-agent cwd for teammates.** Therefore:

- **Autonomous (campd headless):** campd sets cwd, so worktree isolation (§4.2)
  and branch delivery (§4.3) apply fully. This is the path #31/#34 were observed
  on and the path the isolation+delivery contract fixes.
- **Attended (teammate):** the teammate **cannot** be given its own worktree cwd
  (A2). It runs in the operator's checkout. So worktree isolation is
  *structurally unavailable* to attended work. This is acceptable **because a
  human is present**: the operator owns integration, sees every commit, and is
  the review gate. The working-tree contract for attended work is therefore
  "human-supervised, operator's tree, operator integrates" — explicitly distinct
  from the autonomous contract, and it must be **documented** so the difference
  is not a surprise.

This is why coordination (#29) is the *first* gate: choosing attended vs
autonomous chooses which isolation/delivery contract applies. If the operator
wants isolation, they dispatch autonomously; if they want to drive it live, they
accept the supervised-live-tree contract. Making the choice explicit (§4.1) is
what makes the two contracts coherent instead of a surprise.

**Consequence — CONFIRMED at Q1 sign-off (2026-07-09):** with isolation now the
*default* (Q1 APPROVED), attended slings are a standing exception to that default
(they can't honor it, A2). The operator accepted "attended = no worktree
isolation by design (A2)"; this is documented, not a code contradiction. Phase 2
must document this contract prominently so it is not a surprise.

## 5. Fit with spec §4 decisions and the invariants

> **NOTE (2026-07-09):** rows mentioning "reservation" / "teammate exception"
> describe the DEPRECATED approach. Under the Reframe, the fit is *stronger*:
> one dispatch path + pack-defined drive + a converse verb removes the §8.4
> snowflake and honors invariant 4 more directly. See the Reframe section.

| Constraint | How the proposal fits |
|---|---|
| campd sole dispatcher; ~~teammate is the one surface exception (§8.4)~~ → **one dispatch path (Reframe)** | Reframe: campd is the *only* dispatcher; the §8.4 teammate exception is removed (Q6). Conversing with a worker is a verb over the harness, not a second spawner. |
| Zero roles / campd never reasons (§2 inv.4, §8.3) | campd honors a declared reservation and mechanical git facts; all judgment stays with the worker's close outcome and git, like check-scripts. |
| Idle is free / no query loops (§7.3) | All new facts evaluated on the existing append→fold→dispatch path; no poll added. |
| Cost proportional to job (§2 inv.2) | Attended reservation = one extra event in the same batch; delivery gate = worker behavior + one git check at dispatch. Tier-0 stays ~3 writes + one spawn. |
| Nothing hidden (§13) | Reservation, isolation choice, fail-fast dispatch refusal, and every non-`pass` verdict are ledger events with causes. |
| Fail fast (§2 inv.5, §15.1) | A rig that can't host a worktree fails at dispatch (`dispatch.failed`), never strands work. |
| Vocabulary mirror (§8.2, §15.2) | Q3-REVISED (SETTLED): adopt gc's `WorkOutcome` axis (`shipped`/`no-op`/`blocked`/`abandoned`) VERBATIM as a separate additive mirrored axis — un-integrable work is `blocked`, never a new `outcome` value; pinned in `gc-vocab.json` + `check_vocab.sh`. City export stays native. |
| A2 teammate cwd (§17, resolved) | Respected: attended work is supervised-live-tree; only autonomous work is isolated. |

## 6. What this touches when implemented (for reviewers — NOT in this PR)

> **UPDATED for the Reframe (2026-07-09).** The reservation-era items are struck;
> the live list reflects one dispatch path + a converse verb + pack-defined drive.

- `plugin/commands/sling.md` — **remove the teammate-spawn instruction**;
  `/camp:sling` becomes a thin wrapper over `camp sling` (enqueue only), same
  single path as the CLI.
- **NEW core verb** (mirror of `gc nudge`/session-message): a user-facing
  `camp` verb to send a turn to any running session (worker or overseer),
  delivered live over the held stdin pipe (`nudge_via_stdin` already exists,
  dispatch.rs) or via `claude --resume` after the turn (A4). Camp has no such
  user verb today.
- **Pack (starter), not core** — a delivery-aware `dev`/coder prompt and,
  optionally, a `committer` agent and a persistent `overseer`/mayor agent
  (mirroring gc's swarm pack). Ships as pack content; the camp plugin stays
  role-free (spec §11).
- ~~`crates/camp/src/cmd/sling.rs` — reservation write~~ (dropped: no
  reservation).
- ~~`crates/camp-core/src/readiness.rs` — reservation exclusion~~ (dropped: one
  dispatch path needs no new exclusion).
- `crates/camp-core/src/pack.rs` — flip the `Isolation` default to worktree
  (Q1 APPROVED); add the explicit `isolation = "none"` opt-out.
- `plugin/skills/worker/SKILL.md` + `crates/camp/src/daemon/spawn.rs`
  `WORKER_CONTRACT` — the delivery contract text (kept in lockstep; two copies of
  the worker contract exist today and both lack delivery semantics).
- `crates/camp/src/cmd/close.rs` + `crates/camp-core/src/vocab.rs` +
  `crates/camp-core/tests/fixtures/gc-vocab.json` + `ci/gc-compat/check_vocab.sh`
  — **add the `WorkOutcome` axis** (`shipped`/`no-op`/`blocked`/`abandoned`),
  mirrored verbatim from gc, as a SEPARATE additive axis from `outcome`
  (Q3-REVISED). The control `outcome` axis is unchanged; the WorkOutcome set is
  pinned + mirror-validated.
- The spec `docs/design/2026-07-05-gas-camp-design.md` §8.4 — remove the
  "attended teammate is the one surface exception"; state one dispatch path + the
  converse verb + pack-defined drive (Q6 APPROVED). And **§12** — the
  isolation-default
  amendment (Q1 APPROVED). Edited later, serialized through the operator; NOT in
  this PR.

## 7. Questions — ALL SETTLED (operator, 2026-07-09)

Status: **Q1, Q3, Q4, Q5, Q6, Q7 are SETTLED**; **Q2 is SUPERSEDED** (retained
for history). **Nothing remains open** — this section is the resolution record
for the merged design. The spec §8.4/§12 edits and the `WorkOutcome` vocabulary
addition are future implementation, serialized through the operator.

- **Q1 — Default isolation (SPEC §12 EDIT). RESOLVED 2026-07-09 — APPROVED.**
  Autonomous dispatch defaults to worktree isolation, with `isolation = "none"`
  as an explicit opt-out. The operator signed off on the spec §12 edit; the §12
  amendment and code land later, serialized through the operator (Phase 2).
  Attended teammates are the documented standing exception (A2 forbids
  per-teammate cwd, §4.4). Folded into §4.2.

- **Q2 — Reservation mechanism and hand-off. SUPERSEDED 2026-07-09 by the
  Reframe (no reservation).** The Reframe removes the second spawner entirely, so
  there is nothing to reserve against. The **Q2 investigation** below is retained
  as the durable record of *why* a reservation would have been mirror-safe but
  is unnecessary — Gas City has no attended/autonomous fork; the mirror answer is
  one dispatch path + a converse verb, not a reservation. Replaced by Q6.

- **Q3 — Delivery outcome + fresh-repo policy. REVISED & SETTLED 2026-07-09.**
  **Final:** adopt Gas City's **`WorkOutcome` axis**
  (`shipped`/`no-op`/`blocked`/`abandoned`), mirrored VERBATIM, as a SEPARATE axis
  from the control `Outcome` (`pass`/`fail`/`skipped`/`missing_root`).
  Un-integrable work is recorded as **`blocked` on the WorkOutcome axis**, not on
  `outcome`. Fresh/empty repo → **fail fast at dispatch** (§4.3). *How* work is
  committed/landed is **pack/prompt content** (coder + optional committer agent).
  Future-impl mechanics: pin the WorkOutcome set in
  `crates/camp-core/tests/fixtures/gc-vocab.json` and validate via
  `ci/gc-compat/check_vocab.sh` — an additive **mirrored** axis (not a
  redefinition), so the mirror invariant is preserved and city export stays
  native. **Evolution (kept for the record):** the original same-day decision was
  `fail`-with-reason + no new `outcome` value; the gc-source finding below
  surfaced the `WorkOutcome` axis, and the operator then revised to adopt it.
  - **Original rationale (SUPERSEDED by the revision above; kept for history).**
    The Q2 source investigation revealed that Gas City's
    `internal/beadmeta/values.go` *does* define a `blocked` value — but on a
    **separate axis** camp does not model: a `WorkOutcome` set
    (`shipped` / `no-op` / `blocked` / `abandoned`), distinct from the control
    `Outcome` set (`pass` / `fail` / `skipped` / `missing_root`) that camp's
    `outcome` mirrors. `crates/camp-core/tests/fixtures/gc-vocab.json` pins only
    the control-`Outcome` list, which is why it shows "no `blocked`." So the
    precise, correct rationale is: **camp's `outcome` axis (mirroring gc's
    control `Outcome`) has no `blocked`, and adding one there would be a
    redefinition/mismatch that breaks native export** — exactly the operator's
    conclusion. If camp ever wants a first-class "un-integrable" signal, the
    **mirror-safe home is a future, additive `WorkOutcome`-style axis
    (`shipped`/`no-op`/`blocked`/`abandoned`) mirrored verbatim from gc**, never
    an extra `outcome` value. Flagged for the operator as a future option; not
    proposed for v1. (AGENTS.md: reference reality and the doc must not silently
    diverge — hence this correction is recorded rather than buried.)
  - **Operator REVISION → SETTLED (2026-07-09).** The operator weighed this and
    **chose the fuller mirror: adopt gc's `WorkOutcome` axis
    (`shipped`/`no-op`/`blocked`/`abandoned`) verbatim** — native to gc and to
    city export, and where gc *does* record "blocked." This supersedes the earlier
    same-day `fail`-with-reason choice. Un-integrable work is `blocked` on
    WorkOutcome, a SEPARATE additive mirrored axis from `outcome`; future impl pins
    it in `gc-vocab.json` + `check_vocab.sh`. *How* work is delivered
    (commit/branch/land) is pack/prompt content (gc's `committer` role), not core.

### Q2 investigation — does camp deviate meaningfully from Gas City?

Investigated against the pinned reference, not from memory: `ci/gc-compat/`
(`GASCITY_REF` = `12410301884b51131a35e101a335dbaae16cdcb0`, `check_vocab.sh`),
`crates/camp-core/tests/fixtures/gc-vocab.json`, `crates/camp-core/src/vocab.rs`,
the spec's vocabulary-mirror/formula-subset/§8.4 sections, and the Gas City
source at the pinned ref (`internal/events/events.go`,
`internal/beadmeta/values.go`).

**1. Does Gas City have claiming / reservation / an attended-vs-autonomous
distinction?**
- **Claiming: YES.** Gas City workers claim work beads. `bead.claim_rejected`
  (events.go) fires "when a worker attempts to claim a work bead already
  live-claimed by a different worker; the claim is rejected as an idempotent
  no-op rather than fanning out concurrent claims." Gas City tracks an
  `assignee` (which session holds a bead), referenced by
  `session.drain_acked_with_assigned_work`. **But Gas City emits no
  successful-claim event** — there is no `bead.claimed` in gc source.
- **Reservation (block the autonomous dispatcher so an attended teammate can
  take a bead): NO analog.** This exists only because camp has an attended
  surface.
- **Attended-vs-autonomous distinction: NO.** Gas City has no user-driven /
  attended session concept and no event classifying a session that way — the
  fleet is entirely controller-dispatched. This matches the spec: camp's
  attended teammate is "the one surface exception" (§8.4), a camp premise
  ("drive from inside Claude Code"), not a Gas City feature.

**2. Gas City HAS claiming — does camp's claim-at-creation match verbatim or
redefine it?** It is **additive, not a redefinition**, and this is a
*pre-existing, CI-verified* camp choice the reservation merely reuses:
- `bead.claimed` is in `CAMP_SPECIFIC_EVENTS` (`vocab.rs:24`). `check_vocab.sh`
  assertion (b) asserts no `CAMP_SPECIFIC_EVENTS` name appears as a string
  constant in gc source — so `bead.claimed` is guaranteed absent from Gas City,
  i.e. additive. It does not collide with, or redefine, gc's `bead.claim_rejected`
  (a different string with a different meaning).
- Mechanically, camp's claim is atomic and guarded: the `bead_claimed` fold
  (`fold.rs:170-188`) transitions only an `open` bead to `in_progress`; a claim
  on any non-open bead is an `InvalidTransition` rejected inside the one WAL
  transaction — camp's semantic equivalent of gc's idempotent claim rejection,
  surfaced as an error rather than a distinct event. (Minor, out-of-scope
  observation: camp could later mirror `bead.claim_rejected` verbatim as an
  additive event for the double-claim path — exactly the #29 race — but that is
  not required here.)

**3. Gas City does NOT have the reservation/attended concept — is camp's
reservation a permissible additive mechanism, and is it named safely?** Yes.
- **Claim-at-creation (recommended)** mints **no new name at all** — it reuses
  the already-additive `bead.claimed` and the existing `in_progress` /
  `assignee` state, all of which Gas City understands, so there is nothing to
  collide and export is native.
- The **`dispatch = "attended"` field alternative** would mint a new
  camp-specific bead-metadata key. Verified: gc's `internal/beadmeta/values.go`
  has **no** `dispatch` / `attended` / `autonomous` / `reserved` / `reservation`
  key today, so the name is currently free — but minting a new top-level
  beadmeta key is precisely the kind of addition that can collide with a future
  Gas City key and must be camp-namespaced and scrubbed on export. Strictly less
  mirror-safe for no functional gain.

**4. Bottom line + recommendation.** **Camp does not deviate meaningfully from
Gas City.** The attended-vs-autonomous *coordination* is a camp-specific problem
with no Gas City analog (gc has no attended surface), so any solution is a
permissible additive camp mechanism. Where the two overlap — *claiming* — camp
already models a successful claim additively (`bead.claimed`), CI-verified absent
from Gas City, and the reservation simply reuses it. **Recommendation: option (a),
claim-at-creation reservation.** It introduces zero new vocabulary, rides camp's
existing additive claim + the `in_progress`/`assignee` state Gas City understands
natively, exports cleanly, and cannot collide with a future Gas City concept —
strictly more mirror-safe than the `dispatch="attended"` field, which adds a new
namespaced key and an export scrub for no functional benefit. The only follow-on
detail is the teammate take-over of a bead the attended orchestrator reserved
(claim reassignment), plus the release policy — both listed under Q2 above for
the operator's pick.

- **Q4 — What "landed" means / remote scope. RESOLVED 2026-07-09 — SETTLED.**
  v1 "landed" = **committed on the bead branch with a base** (local); the
  `camp/<bead>` branch is the reviewable/mergeable artifact. **Remote push /
  PR-host / MR creation is explicitly OUT of scope for v1** — camp has zero
  git-remote code today; a future scoping decision if ever wanted. Folded into
  §4.3.

- **Q5 — Worker-contract duplication. RESOLVED 2026-07-09 — SETTLED: unify
  first.** The two worker-contract copies (`plugin/skills/worker/SKILL.md` and the
  mechanical `WORKER_CONTRACT` in `crates/camp/src/daemon/spawn.rs`) are unified
  into ONE source **before** delivery semantics are added — the first step of
  Phase 3. Folded into Phase 3.

- **Q6 — Remove the spec §8.4 "attended teammate is the one surface exception"
  (SPEC §8.4 EDIT). RESOLVED 2026-07-09 — APPROVED.** Collapse to **one dispatch
  path** (campd only) + a **uniform converse verb** (send a turn to any session,
  live over held-stdin or via `claude --resume`) + **pack-defined drive**
  (overseer/coder/committer agents), mirroring Gas City. Deletes the §8.4 surface
  exception and the reservation idea entirely; makes the §4 mental model literal
  (the human's own session is the interactive overseer). The operator **approved
  the §8.4 edit**; the amendment is drafted and lands **serialized through the
  operator** (future). Supersedes Q2; reshapes Phase 1. Folded into the Reframe.

- **Q7 — Persistent overseer: core `named_session`, or human-session-only?
  RESOLVED 2026-07-09 — human-session-only.** The overseer **is** the human's
  Claude Code session + plugin (spec §4 made literal); an **optional on-demand
  pack overseer agent** covers away-mode. Camp does **not** add a core standing
  `[[named_session]] mode=always` capability — that preserves "idle = zero agent
  processes" (spec §8.4). Tradeoff recorded: the fuller gc mirror (a core standing
  session) is deferred until away-mode planning demands it. Folded into the
  Reframe.

## 8. Explicitly out of scope for v1 (unless sign-off says otherwise)

- Remote push, PR/MR creation, or any git-host integration (Q4).
- `.camp/` gitignore handling — owned by #35 (this note assumes it lands).
- Warm agent pools / any change to the "spawn per bead, exit on close" model
  (spec §8.4).
- Changing campd into anything that inspects diffs or judges content.

## 9. Phased implementation plan

Ordered by dependency. Each phase is independently landable, TDD per AGENTS.md,
gates green (`fmt`, `clippy -D warnings`, `cargo test --workspace`) before push.
**All design questions are now signed off** — the remaining gates are code
dependencies (#35) and the serialized spec edits (§8.4, §12), not open decisions.

### Phase 1 — Coordination: one dispatch path + converse verb (#29) — REFRAMED

*Q6 APPROVED (2026-07-09); gated only on the serialized spec §8.4 amendment.
Supersedes the deprecated reservation plan.* Depends on nothing else in the code
(removes a spawner; adds a verb over the existing held-stdin/resume capability).
Highest-value, lowest-risk — lands first.

- **Remove the second spawner:** `/camp:sling` (`plugin/commands/sling.md`) stops
  spawning an attended teammate; it becomes a thin wrapper over `camp sling`
  (enqueue only). campd is the sole dispatcher — the race cannot occur because
  there is only one path.
- **Add the converse verb:** a user-facing `camp` verb to send a turn to any
  running session (worker or overseer), delivered live over the held stdin pipe
  (`nudge_via_stdin` exists) or `claude --resume` after the current turn (A4).
  This is "talking to the work," mirror of `gc nudge`/session-message.
- **Pack (starter), not core:** a delivery-aware coder prompt and optionally a
  `committer` / persistent `overseer` agent (mirroring gc's swarm pack) — pack
  content; the plugin stays role-free.
- **Spec:** draft the §8.4 amendment (delete the surface exception; state the
  converse verb + pack-defined drive), landed serialized through the operator.
- **Test obligations:** (i) a single `camp sling` / `/camp:sling` produces exactly
  ONE `session.woke` (campd), never two — the #29 race is structurally gone;
  (ii) the converse verb delivers a turn to a live worker (held-stdin) and to an
  exited worker (`claude --resume`), asserted end-to-end with a fake agent;
  (iii) `/camp:sling` and `camp sling` are behaviorally identical (same single
  path); (iv) no reservation state exists in the ledger (regression guard that
  the deprecated approach did not leak in).

### Phase 2 — Isolation contract (#31)

*Q1 APPROVED (2026-07-09); still gated on #35 (gitignore) and the serialized
spec §12 amendment.* Independent of Phase 1.

- Flip the `Isolation` default to worktree and add the explicit `none` opt-out
  (Q1 approved). Make running on a live branch **loud** (an event / prominent
  doc) for the opt-out case. Land the spec §12 amendment in lockstep (serialized
  through the operator).
- Confirm and test the fail-fast-on-unworktree-able-rig path (`dispatch.failed`),
  which the machinery already produces.
- Document the working-tree contract (autonomous = worktree/`camp/<bead>`;
  attended = supervised live tree, §4.4).
- **Test obligations:** (i) autonomous worker's cwd is a worktree, never the
  rig's live branch (integration test asserts `branch --show-current` in the
  worker cwd ≠ the rig's checked-out branch); (ii) empty/`git init`-only rig →
  `dispatch.failed`, no worker spawned, no stranded commit; (iii) two concurrent
  autonomous workers on one rig get distinct worktrees (no shared-tree collision).

### Phase 3 — Delivery via pack + the `WorkOutcome` axis (#34) — SETTLED

*Q3 REVISED, Q4 & Q5 SETTLED (2026-07-09) — no open gates.* Depends on Phase 2
(the bead branch is the delivery vehicle) and #35. Steps, in order:

- **(a) Unify the worker-contract copies FIRST (Q5).** Collapse
  `plugin/skills/worker/SKILL.md` and the mechanical `WORKER_CONTRACT` in
  `crates/camp/src/daemon/spawn.rs` into ONE source of truth, so delivery
  semantics are added once and cannot drift. This precedes everything else in the
  phase.
- **(b) Add the delivery contract as pack/prompt content** (mirror gc's swarm):
  a delivery-aware `dev`/coder prompt covering how to commit to the `camp/<bead>`
  branch, and optionally a dedicated `committer` agent that owns git. Ships in the
  starter pack; the plugin stays role-free (spec §11).
- **(c) Adopt the `WorkOutcome` axis (Q3):** pin gc's set
  (`shipped`/`no-op`/`blocked`/`abandoned`) in
  `crates/camp-core/tests/fixtures/gc-vocab.json`, extend the fold/close path to
  record a `WorkOutcome` **separately** from the control `outcome`, and have
  `ci/gc-compat/check_vocab.sh` validate it as an additive mirrored axis.
  Un-integrable work is `blocked`; landed work is `shipped`. campd still fails
  fast when a rig can't host a worktree (mechanical).
- **(d) v1 "landed" = local bead branch with a base (Q4), no remote.** The
  `camp/<bead>` branch (descends from / mergeable into the integration branch) is
  the deliverable; **no remote push / PR-host / MR creation** in v1.
- **Spec:** amend §8.4's worker-lifecycle-contract portion for delivery (the
  §8.4 surface-exception removal is Phase 1); serialized through the operator.
- **Test obligations:** (i) a fake worker that commits to a dead-end branch on a
  no-base rig records `blocked` (WorkOutcome), never `shipped`; (ii) a worker that
  commits to `camp/<bead>` on a rig with a base records `shipped` and the branch
  is reachable/diffable post-close; (iii) `check_vocab.sh` passes with the added
  WorkOutcome set (mirror invariant intact) and `camp export --city` emits
  city-native history including WorkOutcome; (iv) the control `outcome` axis is
  unchanged (regression guard — WorkOutcome is additive, not a redefinition);
  (v) the unified worker contract is the single source (no second copy remains);
  (vi) worktree kept when work is not `shipped`, so nothing is lost.

### Sequencing summary

```
Q6 APPROVED ─► Phase 1 (one dispatch path + converse verb)   (independent; lands FIRST)
                    · drop the /camp:sling teammate spawn; add the converse verb
                    · spec §8.4 surface-exception removal (serialized)

#35 (gitignore) ─┐
Q1 APPROVED ─────┼─► Phase 2 (isolation default = worktree) ─► Phase 3 (delivery)
spec §12 edit ───┘                                                    ▲
Q3/Q4/Q5 SETTLED ────────────────────────────────────────────────────┘
   Phase 3 = (a) unify worker contract (Q5) → (b) delivery pack →
             (c) WorkOutcome axis (Q3) → (d) local "landed", no remote (Q4)
```

Status of the gates (2026-07-09) — **ALL SETTLED, nothing open:** **Q1 APPROVED**
(isolation default), **Q3 REVISED & SETTLED** (adopt the `WorkOutcome` axis),
**Q4 SETTLED** (local bead branch = landed; no remote), **Q5 SETTLED** (unify the
worker contract first), **Q6 APPROVED** (one dispatch path + converse verb; §8.4
removal), **Q7 SETTLED** (overseer = the human's session; no core standing
session). **Q2 SUPERSEDED** (history). The note is merge-ready.

Phase 1 (coordination) is the highest-value, lowest-risk first land: one dispatch
path + a converse verb, no new isolation/delivery semantics, and it structurally
removes the #29 race. Phases 2 and 3 are the isolation→delivery spine and must
land in that order because the bead branch produced by isolation is the delivery
vehicle. Every spec edit (§8.4, §12) and the WorkOutcome vocabulary addition are
future implementation, serialized through the operator.

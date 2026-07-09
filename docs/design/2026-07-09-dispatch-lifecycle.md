# Gas Camp — Dispatch Lifecycle: Isolation, Delivery, and Attended/Autonomous Coordination

| Field | Value |
|---|---|
| Status | Proposal for review — NO behavior change in this PR |
| Date | 2026-07-09 |
| Author role | design agent (proposal only) |
| Refs | #29 (attended sling races autonomous dispatch), #31 (no worktree/branch isolation), #34 (pass on un-integrable work) |
| Depends on | #35 (`.camp/` gitignore, parallel PR) |
| Authoritative spec | `docs/design/2026-07-05-gas-camp-design.md` — its §4 decision record is SETTLED |

### Decision log

- **2026-07-09 — Operator APPROVED Q1.** Worktree isolation is the DEFAULT for
  autonomous dispatch; the operator signs off on the spec §12 edit this implies
  (the actual §12 change + implementation land later, serialized through the
  operator, per §9). Attended teammates remain the documented exception (A2).
  Folded into §4.2 and §7.
- **2026-07-09 — Operator DECIDED Q3.** Gate un-integrable work as
  `fail`-with-reason (mirror-safe). Do NOT add a `blocked`/`needs-integration`
  value to the `outcome` axis — preserving native city export is a hard
  requirement. Fresh-repo/no-remote = fail-fast at dispatch. Folded into §4.3
  and §7. (An honest correction to the *rationale* — gc does have a `blocked`
  value on a *separate* axis — is recorded in §7 Q3; it does not change the
  decision.)
- **2026-07-09 — Q2 investigated** against the pinned Gas City reference
  (§7 Q2 investigation). Verdict: **no meaningful deviation from Gas City**;
  recommend **claim-at-creation reservation**. Pending operator pick.

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
`request_with_autostart(..., Poke)`. On that poke, `campd`'s dispatcher
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
- **Vocabulary mirror** (inv. 7; spec §8.2, §15.2). camp's `outcome` values must
  stay a subset/mirror of Gas City's control `Outcome` set
  (`["pass","fail","skipped","missing_root"]`,
  `crates/camp-core/tests/fixtures/gc-vocab.json`) so exported history reads
  natively in a city. **camp's `outcome` axis has no `blocked`, and Q3 keeps it
  that way** (un-integrable work → `fail`-with-reason). Note (Q2 source finding):
  Gas City *does* define a `blocked` value, but on a separate `WorkOutcome` axis
  camp does not model — see §7 Q3; it does not license adding `blocked` to the
  `outcome` axis.
- **Settled §4 / §8.4 that this proposal keeps:** campd is the sole dispatcher
  of autonomous/graph work; the attended teammate is the single surface
  exception; A2 (resolved) — a teammate's cwd is pinned to the parent session's
  directory, there is no per-agent cwd for teammates. This last fact is
  load-bearing (§4.4).

## 3. The model in one paragraph

A bead's lifecycle has three declared, mechanical gates, each a ledger fact:
(1) **Coordination** — at sling time the operator makes an *explicit* choice,
attended or autonomous, and an attended choice writes a durable reservation that
removes the bead from campd's dispatchable set atomically with its creation, so
there is no race. (2) **Isolation** — an autonomous worker is given a
camp-managed worktree on a per-bead branch (`camp/<bead>`); the worker never
touches the rig's live tree; campd refuses to dispatch a worktree into a rig
that cannot support one (not a git repo / no base commit), failing fast rather
than stranding work. (3) **Delivery** — the worker skill gains an explicit
delivery contract (commit to the bead branch; define "landed"), and `pass` is
gated on landable work — un-integrable work closes `fail`-with-reason (Q3
DECIDED: mirror-safe, no new outcome value). The three gates compose into one
honest lifecycle: the operator chose the surface, the work happened in
isolation, and the outcome tells the truth about whether it can land.

## 4. Proposed model

### 4.1 Coordination — attended vs autonomous is an explicit, atomic choice (#29)

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

**Hand-off / release policy** (open question §7 Q2): if the operator dismisses
the teammate without it claiming, the reserved bead sits `in_progress`, visible
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
- **Define "landed" for v1** (recommended, minimal scope): work is *landed* when
  it is **committed on the bead branch and that branch has a base** (it descends
  from, or is mergeable into, the rig's integration branch). The bead branch,
  reachable and diffable, **is** the reviewable/mergeable artifact — matching the
  operator's "changes reach the integration branch only via review" workflow
  **without** camp needing a remote or a PR host. Remote push / PR creation is
  **explicitly out of scope for v1** (§8) and would be a spec + scope decision
  (§7 Q4).
- **Fresh / empty repo — DECIDED 2026-07-09: fail fast at dispatch.** The
  "always branch + PR" pattern is meaningless with no base and no `main`. campd
  **refuses to dispatch** code work into such a rig: `git worktree add` fails,
  `dispatch.failed` is evented, no worker ever runs and nothing is stranded
  (§4.2.2). This is the fail-fast, invariant-5 answer and needs no new worker
  cleverness; the operator prepares the rig (a base commit) before dispatching
  code work, the same way #35 prepares `.gitignore`. (The rejected alternative —
  letting the worker's first commit establish the integration branch — would put
  content judgment in the worker for the empty-repo edge; fail-fast is cleaner
  and consistent with the `fail`-with-reason gate below.)
- **Gate `pass` on landable work — DECIDED 2026-07-09: `fail`-with-reason, no
  new outcome value.** A worker that produced changes it cannot land closes
  **`fail`** with a precise reason ("work committed to `camp/<bead>` but the rig
  has no base/integration branch — cannot land"). This is mirror-safe (`fail` is
  already mirrored) and honest; the worktree is kept for forensics (existing
  behavior), so the work is not lost. **No `blocked`/`needs-integration` value is
  added to the `outcome` axis** — camp's `outcome` mirrors Gas City's control
  `Outcome` (`pass|fail|skipped|missing_root`), which has no `blocked`, and
  preserving native city export is a hard requirement (vocabulary-mirror
  invariant). See §7 Q3 for an honest correction to the *rationale* (gc does
  carry a `blocked` value, but on a *different* axis camp does not model) — the
  correction does not change this decision.

**campd's role stays mechanical.** campd does not read diffs or judge quality. It
(i) honors the declared isolation, (ii) fails fast when the rig can't host a
worktree, and (iii) records the worker's own close outcome. The landability
*judgment* is the worker's (an agent) plus mechanical git facts — the same
division as check-scripts (spec §8.3). No role names, no `if stranded then…` in
Rust.

### 4.4 How the three compose — and the teammate-cwd tension

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

| Constraint | How the proposal fits |
|---|---|
| campd sole dispatcher; teammate is the one surface exception (§8.4) | Kept. Autonomous path unchanged; attended reservation only *removes* a bead from campd's set via an existing exclusion. |
| Zero roles / campd never reasons (§2 inv.4, §8.3) | campd honors a declared reservation and mechanical git facts; all judgment stays with the worker's close outcome and git, like check-scripts. |
| Idle is free / no query loops (§7.3) | All new facts evaluated on the existing append→fold→dispatch path; no poll added. |
| Cost proportional to job (§2 inv.2) | Attended reservation = one extra event in the same batch; delivery gate = worker behavior + one git check at dispatch. Tier-0 stays ~3 writes + one spawn. |
| Nothing hidden (§13) | Reservation, isolation choice, fail-fast dispatch refusal, and every non-`pass` verdict are ledger events with causes. |
| Fail fast (§2 inv.5, §15.1) | A rig that can't host a worktree fails at dispatch (`dispatch.failed`), never strands work. |
| Vocabulary mirror (§8.2, §15.2) | DECIDED (Q3): v1 gates un-integrable work as `fail`-with-reason — only mirrored `outcome` values, no new value on that axis. Attended reservation (Q2) reuses the existing additive `bead.claimed`; zero new vocabulary. |
| A2 teammate cwd (§17, resolved) | Respected: attended work is supervised-live-tree; only autonomous work is isolated. |

## 6. What this touches when implemented (for reviewers — NOT in this PR)

- `crates/camp/src/cmd/sling.rs` — attended vs autonomous resolution + atomic
  reservation write.
- `plugin/commands/sling.md` — pass the attended signal; keep the teammate spawn.
- `crates/camp-core/src/readiness.rs` — only if the "new field" reservation
  variant (§4.1 alternative) is chosen; the recommended claim-at-creation
  variant needs no query change.
- `crates/camp-core/src/pack.rs` — flip the `Isolation` default to worktree
  (Q1 APPROVED); add the explicit `isolation = "none"` opt-out.
- `plugin/skills/worker/SKILL.md` + `crates/camp/src/daemon/spawn.rs`
  `WORKER_CONTRACT` — the delivery contract text (kept in lockstep; two copies of
  the worker contract exist today and both lack delivery semantics).
- `crates/camp/src/cmd/close.rs` + `crates/camp-core/src/vocab.rs` — **no change
  to the outcome vocabulary** (Q3 DECIDED: `fail`-with-reason, no new value). The
  landability gate is worker-contract text plus a mechanical git check; the close
  outcome stays within the mirrored set.
- The spec `docs/design/2026-07-05-gas-camp-design.md` §12 — the isolation-default
  amendment (Q1 APPROVED). Edited later, serialized through the operator; NOT in
  this PR.

## 7. Open questions requiring operator / spec sign-off

Each is crisp and answerable. Q1 and Q3 are RESOLVED (operator, 2026-07-09); Q2
is researched with a recommendation pending the operator's pick; Q4 and Q5
remain open.

- **Q1 — Default isolation (SPEC §12 EDIT). RESOLVED 2026-07-09 — APPROVED.**
  Autonomous dispatch defaults to worktree isolation, with `isolation = "none"`
  as an explicit opt-out. The operator signed off on the spec §12 edit; the §12
  amendment and code land later, serialized through the operator (Phase 2).
  Attended teammates are the documented standing exception (A2 forbids
  per-teammate cwd, §4.4). Folded into §4.2.

- **Q2 — Reservation mechanism and hand-off. RESEARCHED — recommendation
  pending operator pick.** See the **Q2 investigation** below for the Gas City
  findings, verdict, and recommendation. Decision still owned by the operator:
  (a) reserve via claim-at-creation + a teammate claim-reassignment
  (**recommended** — mirror-safe, no new vocabulary), or (b) a new
  `dispatch = "attended"` bead field; and the release policy when an attended
  teammate never claims (adoption reclaim after the attended session ends is
  likely free once the reservation is a real claim; an explicit
  `camp sling --headless <bead>` / release verb; or leave for the operator).

- **Q3 — Delivery gate outcome + fresh-repo policy. RESOLVED 2026-07-09.**
  Un-integrable work closes **`fail`** with a precise reason (mirror-safe). **No
  `blocked`/`needs-integration` value is added to the `outcome` axis** —
  preserving native city export is a hard requirement. Fresh/empty repo →
  **fail fast at dispatch** (§4.3). Folded into §4.3.
  - **Honest correction to the rationale (finding, does NOT change the
    decision).** The Q2 source investigation revealed that Gas City's
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

- **Q4 — What "landed" means / remote scope.** Is v1 "landed" = "committed on
  the bead branch with a base" (recommended; no remote/PR host needed), or does
  the operator want camp to grow remote-push / PR-creation awareness (new scope;
  camp has zero git-remote/PR code today; would be a spec addition)? **Answerable:
  confirm local-branch definition, or open a separate scoping decision.**

- **Q5 — Worker-contract duplication.** The delivery contract must live in two
  places kept in lockstep (`plugin/skills/worker/SKILL.md` and the mechanical
  `WORKER_CONTRACT` in `spawn.rs`). Should these be unified to one source before
  adding delivery semantics, to avoid drift? **Answerable: unify first, or accept
  two synchronized copies.**

## 8. Explicitly out of scope for v1 (unless sign-off says otherwise)

- Remote push, PR/MR creation, or any git-host integration (Q4).
- `.camp/` gitignore handling — owned by #35 (this note assumes it lands).
- Warm agent pools / any change to the "spawn per bead, exit on close" model
  (spec §8.4).
- Changing campd into anything that inspects diffs or judges content.

## 9. Phased implementation plan

Ordered by dependency. Each phase is independently landable, TDD per AGENTS.md,
gates green (`fmt`, `clippy -D warnings`, `cargo test --workspace`) before push.
**No phase begins until its blocking open question is signed off.**

### Phase 1 — Coordination: kill the race (#29)

*Blocked on Q2 (recommendation: option (a), claim-at-creation; awaiting operator
pick + release policy).* Depends on nothing in the code (uses existing
dispatchable exclusion).

- Resolve attended vs autonomous explicitly in `camp sling`; attended writes the
  atomic reservation (§4.1) in the same batch as `bead.created`.
- Update `/camp:sling` to pass the attended signal and keep the teammate spawn.
- Add the teammate take-over step (reassign or clean claim per Q2).
- **Test obligations:** (i) attended sling → NO `session.woke` from campd for
  that bead across converge, even after a subsequent unrelated poke (the
  full-requery race); (ii) autonomous sling → campd dispatches exactly one
  headless worker; (iii) reservation atomicity — no window between `bead.created`
  and exclusion (assert via the ledger, fake-agent integration test); (iv)
  hand-off: teammate that never claims + attended session ends → bead returns to
  the chosen state (adoption test).

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

### Phase 3 — Delivery contract + `pass` gate (#34)

*Q3 RESOLVED (2026-07-09); still gated on Q4 (remote scope) and Q5 (contract
unification).* Depends on Phase 2 (the bead branch is the delivery vehicle) and
#35.

- (Q5) Optionally unify the two worker-contract copies first.
- Add the delivery contract to the worker skill (+ mechanical floor): commit to
  the bead branch; define "landed" (Q3: committed on `camp/<bead>` with a base);
  fresh/empty repo → fail fast at dispatch (Q3), no worker cleverness.
- Implement the `pass` gate per Q3 (DECIDED): un-integrable work closes
  **`fail`-with-reason**; **no new `outcome` value**. The gate is worker-contract
  text + a mechanical git check.
- **Test obligations:** (i) a fake worker that commits to a dead-end branch on a
  no-base rig CANNOT close `pass` (worker closes `fail`-with-reason); (ii) a
  worker that commits to `camp/<bead>` on a rig with a base **can** close `pass`
  and the branch is reachable/diffable post-close; (iii) `camp export --city`
  still emits mirror-valid history — the close vocabulary is unchanged
  (regression guard on the Q3 decision); (iv) worktree kept-on-non-pass so
  stranded work is recoverable.

### Sequencing summary

```
#35 (gitignore, parallel) ─┐
                           ├─► Phase 2 (isolation) ─► Phase 3 (delivery)
Q1 APPROVED ───────────────┘        ▲
Q2 (rec: (a); pending pick) ─► Phase 1 (coordination)   (independent; land first)
Q3 RESOLVED; Q4,Q5 open ────────────┘► Phase 3
```

Status of the gates (2026-07-09): **Q1 APPROVED**, **Q3 RESOLVED**; **Q2**
researched (recommend option (a) claim-at-creation) — awaiting operator pick +
release policy; **Q4** (remote scope) and **Q5** (contract unification) open.

Phase 1 (coordination) is the highest-value, lowest-risk first land: it needs no
new isolation or delivery semantics and directly removes the observed race.
Phases 2 and 3 are the isolation→delivery spine and must land in that order
because the bead branch produced by isolation is what delivery gates on.

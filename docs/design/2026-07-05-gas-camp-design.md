# Gas Camp — v1 Design

| Field | Value |
|---|---|
| Status | Draft for review |
| Date | 2026-07-05 |
| Relationship | Sibling to Gas City (the `gascity` SDK/platform); not a fork, not a replacement |
| Implementation language | Rust |
| Agent runtime | Claude Code (exclusively, by design) |
| Working name | Gas Camp — binary `camp`, daemon `campd` (rename is an open item, §17) |

## 1. Why camp exists

Gas City is built for fleets, and its cost model is proportional to its
machinery, not to the job. Two separate taxes make it the wrong tool for
small tasks:

- **A fixed per-step tax.** Every formula step pays bead materialization →
  ready-query → dispatch → session spawn/nudge → close → control-bead
  evaluation. A method-heavy pack (e.g. superpowers-style
  brainstorm/plan/TDD/review chains) walks a trivial change through many such
  steps. Observed result: adding a CLI flag — five minutes of actual work —
  takes hours end to end.
- **A standing tax.** The orchestrator is tick-driven. Every tick re-runs
  health probes, gate evaluation, and per-agent `scale_check` queries against
  a Dolt-backed beads store, which answers each query with a full SQL engine.
  Observed result: all cores of a Docker VM saturated by per-second query
  storms while no useful work is happening.

The thesis of this design: **robustness comes from durable work plus
convergence, not from heavyweight machinery.** A WAL-mode SQLite transaction
is exactly as crash-proof as a Dolt commit for a single-box, ten-agent
daily driver. Gas Camp keeps Gas City's convergence loop — work persists
outside sessions; any executor advances it idempotently; crash recovery is
"read the state and continue" — and deletes everything whose cost does not
scale with the job.

Camp is for lunch; the city is for fleets. When a job outgrows camp, it
migrates to a city (§15.3) instead of camp growing city-shaped.

## 2. North star

Four principles, each falsifiable. Violations are bugs, not trade-offs.

1. **Idle is free.** No ticks anywhere. Every component sleeps on OS
   events — file watches, armed timers, `SIGCHLD`, socket accepts. Idle
   targets: `campd` < 20 MB RSS, 0.0% CPU, zero wakeups except armed timers.
2. **Cost proportional to job.** The smallest job (Tier 0 sling, §8.1) pays
   one worker spawn and roughly three ledger writes over doing the work by
   hand — seconds of overhead. Graphs, verification loops, retries, and
   fan-out exist but are opt-in per job.
3. **Nothing hidden, nothing server-shaped.** All durable truth lives in
   one directory: one documented-schema SQLite ledger (a library and a
   file, not a server) plus human-readable config and run files. The event
   history is complete and replayable, and `camp events --json` exports
   canonical JSONL for any range, so text-tool audit is one command away.
   No database server, no daemon-private state: `kill -9` any component,
   restart it, and it picks up exactly where the ledger says things stand.
4. **Same six primitives as Gas City, zero roles in code.** Agent, Bead,
   Formula, Rig, Pack, Event — the mental model transfers in both
   directions. `campd` moves work; it never reasons about it. If a line of
   Rust contains a role name or a judgment call, it is a bug.

## 3. Scope

**In scope (v1):**

- Packs: user-designed roles, formulas, and orders as configuration
- Parallel workflows: dependency-gated graphs with fan-out, verification
  loops, and bounded retries
- Every agent reachable for introspection *and* conversation — live when
  attended, by resume when not, by transcript forever
- One control plane: the user drives everything from inside a Claude Code
  session (slash commands), with the `camp` CLI as the identical scripting
  surface
- Durable work ledger: work survives session crashes, daemon crashes, and
  reboots; fresh sessions converge on it
- Orders: cron- and event-triggered formulas, including while the user is
  away
- Health patrol: death and stall detection with mechanical nudge/restart
  policies
- Multi-rig: one camp driving work across multiple repositories
- Comfortable concurrency envelope: ~10 simultaneous agents, with
  heavy-day volume — 10–15 h sessions creating thousands of beads — as the
  ledger design point (§7.6)

**Out of scope (v1), each deliberate — see §14 for rationale:** provider
abstraction, Dolt/bd storage, web dashboard, multi-machine operation, warm
agent pools, the full Gas City formula-v2 contract, MCP/skill
materialization.

## 4. Decision record

Decisions made during design review, recorded so future sessions do not
re-litigate them:

- Deliverable is this design document first; implementation is a separate
  decision after review.
- Purpose: a daily-driver tool. Ergonomics and reliability outrank
  conceptual purity.
- New sibling tool in its own repository — not a lite mode inside `gc`, and
  not only perf-tuning of `gc` (the per-step ceremony floor would remain).
- The user drives from **inside Claude Code**; a dedicated cockpit TUI was
  considered and rejected as the primary surface (`camp top` remains as a
  read-only view).
- **Rust**, not Go, for the implementation.
- Compatibility with Gas City is a **requirement**: formula files, outcome
  vocabulary, and event names stay a strict subset/mirror of Gas City's so
  that camp → city migration is mechanical (§15).
- bd/Dolt is deliberately not the ledger: the standing query cost is one of
  the two problems camp exists to remove. An export bridge covers
  graduation (§15.3).
- The ledger is **one SQLite file**. An earlier journal-file +
  derived-SQLite split was collapsed during review: a WAL-mode append-only
  events table already provides durable, replayable, seq-numbered history,
  so the separate text journal was redundant double-writing. Canonical
  JSONL is an export (`camp events --json`), not a storage format.

## 5. System shape

The whole system is three artifacts plus content:

| Artifact | What it is | Why it exists |
|---|---|---|
| `camp` CLI | one static Rust binary: `init`, `sling`, `ls`, `show`, `search`, `remember`, `recall`, `events`, `rig`, `order`, `top`, `adopt`, `doctor`, `stop` | the verbs — used identically by the user, slash commands, hooks, and agents |
| `campd` | the same binary in daemon mode, auto-started on demand | the only standing process: watches the ledger, dispatches ready work, schedules orders, arms stall timers — purely mechanical |
| camp plugin | a Claude Code plugin: slash commands, lifecycle hooks, the worker skill | makes the user's Claude Code session the control plane and every agent session observable |

**Packs are content, not machinery** (§11): directories of Claude Code
agent definitions, formula TOML, and orders. The camp plugin ships zero
agent definitions.

```
                        YOU ── one Claude Code session: drive camp,
                         │      talk to any worker
                 ┌───────▼────────┐
                 │  YOUR SESSION  │  /sling /status /adopt /events
                 │  (+ teammates) │  Tier-0 workers spawn here, in view
                 └───┬────────────┘
                     │ camp CLI / hooks          headless workers
                     │                        ┌────────────────────┐
        ┌────────────▼───────────┐  spawns    │ claude -p …        │
        │ campd — event-driven:  ├───────────►│ (session id        │
        │ watch → dispatch →     │  SIGCHLD   │  recorded; attach  │
        │ gate → retry → patrol  │◄───────────┤  via resume)       │
        └────────────▲───────────┘            └─────────┬──────────┘
                     │ appends + socket pokes           │ hooks
        ┌────────────┴───────────────────────────────────▼──────────┐
        │  camp dir: camp.db (ledger: event log + state) · camp.toml │
        │  runs/ · camp.toml — all human-readable, all greppable    │
        └───────────────────────────────────────────────────────────┘
```

### campd lifecycle

- **Liveness is the socket, per the no-status-files principle:** `campd`
  serves a unix socket at `<camp>/campd.sock`. Alive means the socket
  accepts; a stale socket file that refuses connections is removed and the
  daemon restarted. No pidfiles, no lockfiles-as-status.
- **Auto-start:** any `camp` verb that needs the daemon connects; on
  failure it spawns `campd` (detached), logs the spawn as an event, and
  retries once. `camp stop` shuts it down. An optional launchd agent (shipped
  as an example plist, not installed by default) starts `campd` at login for
  users who want orders firing without first running a `camp` command.
- **Crash-only design:** `campd` holds no exclusive state. On start it
  opens the ledger, processes any events past its cursor, runs adoption
  (§8.5), and continues. `kill -9` is a supported shutdown method.

## 6. Primitive mapping

| Gas City primitive | Camp realization | Claude Code feature used |
|---|---|---|
| **Agent** (WHO) | a Claude Code agent definition file in a pack — frontmatter (model, tools, permissions) + prompt | agent definitions; teammates (spawned while you are present); headless-but-present sessions (campd-dispatched) |
| **Bead** (WHAT) | a ledger record — append-only event history plus current state; tasks, mail, and memory are all beads differing by `type`; dependencies are `needs` edges; ready = open ∧ no open blockers | — (camp-owned ledger) |
| **Formula** (HOW) | a TOML file, strict subset of Gas City formula v2 (§8.2); cooking materializes a run into the ledger | worker sessions execute step beads |
| **Rig** (WHERE) | a registered repo path; beads carry a rig; dispatch sets cwd or worktree | per-session cwd; worktree isolation |
| **Pack** (CONFIGURES) | a directory, optionally installed as a Claude Code plugin (§11) | plugins (commands, agents, skills, hooks) |
| **Event** (OBSERVE) | an event row in the ledger; the event log is simultaneously store history and bus | hooks emit; statusline/`/status` consume |

Gas City machinery → camp machinery: the **orchestrator** becomes `campd`
(event-driven, mechanical-only); the **bead store** and **event bus**
collapse into one SQLite ledger — an append-only event log plus state
tables; **sling** is `camp sling`;
**health patrol** is §10; **orders** are §9.

## 7. The ledger

### 7.1. Layout

```
camp/                      # ~/camps/<name>/ (multi-rig) or <repo>/.camp/ (single rig)
  camp.toml                # rigs, packs, orders, caps, stall thresholds
  campd.sock               # daemon socket (liveness = accepts connections)
  camp.db                  # THE ledger (SQLite, WAL mode): append-only events
                           #   table (seq-numbered — the bus and the audit),
                           #   current beads + deps, session registry, memory,
                           #   FTS5 search. `camp events --json` exports the
                           #   canonical JSONL for any range (7.2)
  runs/<run-id>/           # one dir per formula run: pinned formula copy,
                           #   cook manifest, step status snapshot
  sessions/                # per-worker stdout capture (the claude result
                           #   envelope JSON) + stderr log, one pair per session
  formulas/                # camp-local formula definitions, resolved by
                           #   name (§9; packs layer beneath, §11)
  worktrees/               # camp-managed worktrees (per agent isolation flag)
```

### 7.2. The event log

One append-only `events` table is simultaneously the bead store's history
and the event bus — a bead mutation *is* an event. The canonical JSON form
(the shape of an `events` row, and what `camp events --json` emits for any
range):

```json
{"seq":412,"ts":"2026-07-05T21:14:03Z","type":"bead.closed","rig":"gascity",
 "actor":"session:8f3c2e01","bead":"gc-142","data":{"outcome":"pass"}}
```

- **Event names mirror Gas City verbatim where the concept exists**
  (`bead.created`, `bead.closed`, `session.woke`, `session.crashed`,
  `order.fired`, `order.completed`); camp-specific events are additive and
  documented in one table in the reference docs (e.g. `agent.stalled`,
  `run.finalized`, `campd.started`).
- **A write is one transaction.** Every mutation goes through one library
  path shared by the CLI and `campd`: a single WAL transaction inserts the
  event row (monotonic `seq`) *and* applies its state effect — bead
  status, dependency edges, session registry, memory, FTS index. Current
  state therefore can never lag the history. After commit, the writer
  pokes the `campd` socket with the new `seq`; if `campd` is down, writes
  still succeed and it catches up from its processed-cursor on start. A
  reader can replay from any sequence number.
- **Readers never page through history for current state.** `camp ls
  --ready`, `/status`, statusline, `top`, and `camp search` read the state
  tables (7.4). The event log is for audit, replay, and the bus — not for
  sifting: an agent that wants something *queries*; it never scans
  history.

### 7.3. Readiness is computed on write, never polled

When a poke lands (or on startup catch-up), `campd` processes events past
its cursor: it recomputes readiness *for the affected subgraph only*,
dispatches anything newly ready, and matches event-triggered orders (§9).
Nobody — not `campd`, not agents, not the UI — ever asks the store
"anything ready?" on a loop.

The precise claim, since `grep` is a query too: **camp has queries, but no
query loops.** A query is a read someone chooses to run when they want
information; a query loop is a read the architecture *requires repeatedly
to make progress*. Gas City's controller must keep asking the store to
advance work; camp is told — append → fold → dispatch. Read frequency is
therefore coupled to curiosity, not to liveness, and each read is a
local-file/SQLite access measured in microseconds, not a round-trip to a
SQL server. This is the design decision that makes the Dolt query storm
structurally impossible.

### 7.4. Derived views, search, and memory

`camp.db` is a WAL-mode SQLite file — a library and a file, not a server.
The CLI and `campd` write through the one shared transactional path
(7.2); readers are concurrent and never block the writer. It holds the
append-only event log, current bead states and the dependency index, the
session registry, closed-bead history, and an FTS5 full-text index over
titles, descriptions, close notes, and memory. The event-sourcing
property is kept *internal*: the state tables are a fold of the event
log, and `camp doctor --refold` rebuilds them from history and
cross-checks for drift. (k3s made the same move when it swapped etcd for
SQLite — one proportionate file; camp swaps Dolt for SQLite.)

Agents never sift the ledger — they query it:

- `camp ls --ready` / `--mine` / `--rig <r>` — indexed state queries
- `camp show <bead>` — the full record, history included
- `camp search "auth refactor"` — ranked full-text search over everything,
  all time
- `camp remember "<fact>"` / `camp recall <query>` — bd-style persistent
  memory: memory-type beads, camp- or rig-scoped, FTS-indexed. The worker
  skill instructs workers to `recall` before starting a bead and to
  `remember` non-obvious findings at close — knowledge survives sessions
  the same way work does.

**Reachability is a ledger fact.** Every spawned agent gets a registry row
**at birth** (mirrored by its `session.woke` event): camp session
name (`<camp>/<agent>/<n>`), pack agent name, rig, Claude Code session ID,
transcript path, spawn time, and status. "Every agent reachable for
introspection and conversation" then degrades gracefully and never to
"gone":

1. live attended worker → a teammate in your TUI: talk in place;
2. live campd-dispatched worker → transcript tailable now, or attach with
   `claude --resume <session-id>`;
3. exited worker → resume the session by ID and converse;
4. long gone → the transcript file persists.

### 7.5. Why not Dolt/bd — and the alternatives considered

The standing tax (§1) lives in the store choice: a SQL engine that must be
*asked* invites the architecture to ask it in a loop, and Dolt answers
every ask with a versioned SQL engine's weight. What Dolt provides that
camp's ledger does not (multi-writer merge, versioned SQL, multi-machine
sync) is exactly what a single-box daily driver does not use.

Two alternatives were considered seriously and are recorded here:

- **Fork beads to emit an event on every write** (subscribe, never poll).
  This would kill the query storm while keeping bd's search, memory, and
  dependency features — but it keeps Dolt's per-operation weight and
  standing footprint, and it couples camp to a fork of an actively moving
  upstream. Rejected for camp. However, *proposing write-event emission
  upstream to beads is worth doing regardless of camp* — it would let Gas
  City's controller subscribe instead of poll, attacking the standing tax
  at its source for city users too (§17).
- **A bd-compatible replacement store** — a real project in its own right,
  not a camp subcomponent. Camp's ledger is deliberately
  concept-compatible with bd (ids, `needs` edges, status, labels, memory),
  so a bd-CLI-compatible shim over `camp.db` can become that project
  later without changing camp (§17). Explicitly out of scope for v1.

Graduation to bd/Dolt is an export (§15.3), not a backend option.

### 7.6. Scale envelope

The design point is a heavy day, not a demo: 10–15 hours of continuous
use, thousands of beads created, ~10 concurrent workers. With
milestone-level eventing (tool-level detail lives in transcripts, which
patrol watches rather than records — §10), that is on the order of 30–50k
event rows per heavy day. A year of heavy use is roughly 10–15 M event
rows and ~1 M beads — single-digit-GB SQLite territory; indexed reads and
FTS stay in the low-millisecond range. Backup is file-level: `camp backup`
runs `VACUUM INTO`, and any snapshot tool works on one file. No archival
tiering is designed until real usage shows it is needed.

## 8. Execution model

### 8.1. Tier 0 — bare sling (the 90% path)

```
you:   /sling add a --json flag to `gc ls`, TDD it
camp:  gc-142 open → worker gc-dev-1 (teammate)
```

One ledger write (`bead.created`), one worker spawn, done. The worker
runs the pack's worker skill: claim the bead (`bead.claimed`), do the work,
emit milestone events, close with an outcome, exit. No formula, no run
directory, no graph machinery. Overhead over asking Claude directly:
roughly two seconds and three ledger writes. This is the answer to "hours
for a flag."

Routing: `/sling` with no agent argument routes to the pack's default
worker for the current rig; `/sling --agent reviewer` targets a specific
definition. Judgment about *content* stays with agents; a worker that
discovers follow-up work slings new beads itself.

### 8.2. Tier 1 — formulas: a strict subset of Gas City formula v2

**The compatibility invariant: every valid camp formula is a valid Gas
City formula-v2 file.** Camp adopts constructs with Gas City's exact syntax
and semantics or not at all; `camp doctor --formula <f>` enforces the
subset (and the implementation plan includes validating camp's corpus
against `gc`'s own compiler in CI). Camp v1 accepts:

| Construct | Semantics (as specified by Gas City formula-spec-v2) |
|---|---|
| `formula`, `description`, `[requires] formula_compiler = ">=2.0.0"` | file header; camp requires the same contract declaration for graph-only constructs |
| `[[steps]]` with `id`, `title`, `description`, `needs` | dependency-gated steps |
| `assignee` | routing hint to a pack agent (with Gas City's combination rules) |
| `[steps.check]` | run/check verification loop: `max_attempts`, inner `check` with `mode = "exec"`, `path`, `timeout` — the checker is a script, which keeps verification mechanical; step-level `timeout` (general bound on the check script) as specified |
| `[steps.retry]` | transient retry loop: `max_attempts`, `on_exhausted = "hard_fail" \| "soft_fail"`, with Gas City's pass/hard/transient attempt classification |
| `[steps.on_complete]` | runtime fan-out over structured step output: `for_each` (an `output.` path), `bond` (formula per item), `vars` (`{item}`, `{item.field}`, `{index}`), `parallel`/`sequential` |

Example — the everyday guarded change:

```toml
formula = "guarded-change"
description = "Implement with script verification and bounded retries"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "implement"
title = "Implement the change"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "scripts/verify.sh"
timeout = "5m"

[[steps]]
id = "review"
title = "Review the final diff"
needs = ["implement"]
```

**City-only in v1** (rejected by camp with a pointer to the city):
`drain` and convoy semantics, scopes/cleanup and failure-policy metadata
(spec §3.5), `pour`, authored `gc.*` metadata (rejected exactly as Gas City
rejects it), and the v1-era constructs Gas City still accepts (`gate`,
`loop`, `expand`, `children`). Outcome metadata vocabulary (`outcome = pass|fail`,
`final_disposition = hard_fail|soft_fail`) mirrors Gas City so exported
history reads natively in a city.

Cooking: `/sling --formula guarded-change <args>` (or `camp sling
--formula`) pins a copy of the formula into `runs/<run-id>/`, materializes
root + step beads + edges into the ledger, and from that moment the run is
independent of the file — Gas City's materialization property, kept.

### 8.3. campd as control dispatcher

From cook onward `campd` advances the run, event-driven end to end: a close
event unblocks dependents → newly ready steps dispatch immediately (up to
the concurrency cap) → a `check` step's script verdict routes mechanically
(pass closes; fail with budget spawns the next iteration) → `retry`
classification follows Gas City's pass/hard/transient rules → `on_complete`
expands its fan-out from the step's recorded output → the last step's close
finalizes the root (`run.finalized`).

**The Zero-Framework-Cognition line, drawn exactly:** `campd` executes
structure — edges, budgets, caps, cron expressions, timer thresholds — all
of it declared in TOML by the user. Agents produce every judgment, and
verification verdicts come from user-supplied scripts (`check.mode =
"exec"`), the same mechanical checker Gas City v2 supports. No role names,
no heuristics, no `if stuck then…` cleverness in Rust.

### 8.4. Dispatch: one dispatcher; visibility is invariant, surface is not

The invariant comes first: **no execution path affects visibility.** Every
worker, however spawned, is registered at birth (§7.4), records its
milestones in the ledger, streams a transcript you can tail live, and can
be conversed with. Camp runs nothing hidden; some things start
*minimized*.

- **`campd` dispatches all graph work** — formula steps, orders, patrol
  respawns — whether or not you are watching, because forward progress
  must not depend on a user session existing. These workers are
  **headless-but-present**: `claude -p` with a recorded session ID, the
  pack agent's prompt/tools/permission configuration, and cwd or worktree
  per the bead's rig. Minimized windows, not hidden ones — `/status` (and
  `camp top`) lists them live, their transcripts tail in real time, and
  attaching to any of them — a full conversation with its entire context —
  is one keystroke (`claude --resume <id>`).
- **The one surface exception:** attended Tier-0 sling spawns the worker
  as a **teammate inside your session**, because when you are sitting
  right there, in-place conversation beats even a one-keystroke attach.
  (Assumption A1, §17, covers teammate-interaction mechanics; the fallback
  is headless + instant attach — a UX tweak, not a structural change.)

Put differently: the *dispatcher* for graph work is always `campd`, and
the *surface* a worker's conversation starts on varies with whether you
were present when it spawned — never with whether you may see it.

No warm pools in v1: workers spawn per bead and exit on close, so an idle
camp has zero agent processes. At ~2 s spawn cost and ≤10 concurrency, warm
reuse is premature optimization; noted as a future option if spawn latency
ever dominates.

**Worker lifecycle contract** (the worker skill, shipped by the camp
plugin): claim → work → emit milestones (`camp event emit`) → close with
outcome → exit. Workers run under the permission mode and tool allowlist
their agent definition declares. `campd`-spawned workers run
non-interactively: anything the agent definition has not pre-allowed fails
fast (and lands in the ledger) rather than hanging on a prompt no one
will answer.

### 8.5. Adoption

`camp adopt` (run automatically at `campd` start, available manually)
reconciles the session registry against reality: for each registered live
session, probe the process/transcript; mark the dead as `session.crashed`
(their claimed beads return to ready with retry budgets intact); re-arm
stall timers for the living. State is never trusted over observation — the
process table and transcript files are the ground truth, per the
no-status-files principle.

## 9. Orders

```toml
[[order]]
name    = "morning-triage"
on      = "cron:0 7 * * 1-5"
formula = "triage-inbox"
rig     = "gascity"

[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "fix-ci"
```

An order's `formula` names `<camp>/formulas/<name>.toml`; when packs land
(§11) they layer beneath these local definitions, last-wins.

- **Cron orders:** `campd` keeps a min-heap of next fire times and sleeps
  until the earliest deadline — a timer, not a tick. On wall-clock jumps
  (sleep/wake, NTP), deadlines are recomputed against real time.
  Missed-while-asleep fires apply a catch-up policy: fire once on wake if
  within `catch_up_window` (default `"2h"`, configurable per order; `"0"`
  disables catch-up).
- **Event orders:** a pattern match evaluated once per event, on the same
  post-commit processing path as readiness (§7.3), so event orders add no
  standing cost.
- **Away-mode is the same code path.** An order fires, `campd` cooks and
  dispatches headless workers, everything lands in the ledger. You come
  back, `/status` shows what happened, and every worker it spawned is
  resumable. Limits stated honestly: with the default on-demand daemon,
  orders fire only while `campd` is running (from first `camp` use until
  `camp stop`/reboot); install the optional launchd agent for
  fire-at-login coverage; a powered-off laptop fires nothing until wake
  (catch-up policy applies).

## 10. Health patrol

Three mechanisms, all push, all mechanical:

1. **Death:** `campd` is the parent of headless workers — `SIGCHLD` lands
   instantly. The worker's claimed bead returns to ready (retry budget
   decremented), `session.crashed` is emitted, and dependents are
   re-evaluated.
2. **Stall:** one armed timer per *active* worker, reset by activity on
   the worker's transcript file (a filesystem watch on the path recorded
   in its registry row — a working agent appends to it constantly, for
   free) and by any ledger event from that session. Tool-level detail
   thus stays out of the event log (§7.6) while patrol still sees every
   heartbeat. Timer fires → `agent.stalled` event → the agent
   definition's policy ladder executes: `nudge` (a status-request turn —
   delivered live over stdin when campd spawned the worker in stream-json
   input mode, per the A4 resolution in §17, otherwise via session
   resume), then `restart` (kill, respawn, re-hook the bead)
   with exponential backoff and a bounded budget. Safe because the bead is
   the work; the session is disposable.
3. **Escalation to judgment is pack content, not Rust:** an order matching
   `event:agent.stalled` can sling an investigator formula. `campd`
   notices; it never diagnoses.

Attended teammates are in the user's face already; patrol only annotates
(`agent.stalled` event + statusline badge), never kills a session inside
the user's TUI.

## 11. Packs and the plugin

A **pack is a directory** — installable as a Claude Code plugin, or
referenced by path in `camp.toml`:

```
mypack/
  agents/gc-dev.md            # Claude Code agent definitions, verbatim:
  agents/reviewer.md          #   frontmatter (model, tools, permissions) + prompt
  formulas/guarded-change.toml
  orders.toml
  skills/  commands/          # optional extras, plain Claude Code format
```

- **Zero invented formats.** A role is a Claude Code agent file, so packs
  are useful in bare Claude Code too, and everything written for camp stays
  portable. Formulas are Gas City formula files (subset, §8.2).
- **Layering:** `camp.toml` imports packs; resolution is last-wins with
  local definitions highest — Gas City's layering, simplified.
- **The camp plugin is machinery only:** `/sling`, `/status`, `/adopt`,
  `/events` slash commands (thin wrappers over the `camp` CLI); lifecycle
  hooks — SessionStart (register/adopt), Stop and SubagentStop (session
  end), plus an optional PostToolUse breadcrumb hook (off by default:
  patrol watches transcripts instead, §10); the worker skill; an optional
  statusline snippet showing
  a fleet badge (`▲3 ●2 ✖1` — live, ready, red) fed by the `campd` socket.
  It ships **no agent definitions**. Roles are pack content. Same law as
  the city: if the machinery mentions a role, it is a bug.
- A starter pack (clearly content, not machinery) ships alongside as an
  example to copy, not a dependency.

## 12. Multi-rig and worktrees

- A camp dir stands alone (`~/camps/dev/` + `camp rig add ~/code/gascity`)
  or lives repo-local (`.camp/`, rig = self).
- Beads carry their rig; bead IDs get per-rig prefixes (`gc-142`,
  `t3-17`) — one ledger, scoped queries, Gas City's namespacing idea
  without a shared database.
- Dispatch sets the worker's cwd to the rig — or to a camp-managed worktree
  under `<camp>/worktrees/` when the agent definition sets
  `isolation = "worktree"`. Worktrees are removed on clean close and kept
  (with an event) on failure for forensics; the Gas Town worktree-cleanup
  lessons (leaked worktrees from crashed agents) are addressed by adoption
  (§8.5) sweeping orphaned worktrees against the registry.
- Cross-rig workers default to campd-spawned headless sessions (one attach
  away) regardless of how assumption A2 (§17) resolves for teammates.

## 13. Nothing-hidden guarantees

Each of these is a testable guarantee, not a vibe:

1. Every unit of work is an append-only event row plus current state in
   one documented-schema ledger; the event log alone reconstructs the
   whole system, and `camp events --json` exports it as JSONL so text
   tools audit it in one command. Queries exist for convenience; no query
   loop is ever load-bearing (§7.3).
2. Every agent has a registry entry from birth: role, rig, Claude session
   ID, transcript path. Live → talk in place or attach; exited → resume;
   long gone → transcript persists.
3. Every `campd` action is an event **with its cause**: `gc-142 closed →
   gc-143 ready → dispatched as gc-dev-2 (session 8f3c…)`.
4. Every automation is declared in visible TOML; config changes are
   themselves events.
5. `kill -9` anything; the ledger tells the whole story; `camp doctor`
   verifies — and `--refold` rebuilds — current state from the event
   history and reports drift.
6. Every verb works identically from slash commands and the `camp` CLI —
   the session is the control plane, not a privileged client.

## 14. Cost budget

Targets the implementation is held to (measured in the e2e suite, §16):

| Metric | Camp target | Gas City as observed on the motivating setup |
|---|---|---|
| Idle CPU | 0.0% — blocked on kqueue/timers | all cores busy; ~1 Hz query storm against Dolt |
| Idle footprint | one process, < 20 MB RSS | controller + Dolt server + session overhead |
| Sling → worker's first token | ≤ 2 s | minutes (ceremony + tick latency) |
| Step close → dependent dispatched | ≤ 1 s | next tick + SQL round-trips |
| "Add a CLI flag" end-to-end | agent's actual work + seconds | hours |
| Ledger write (event + state effect, one WAL transaction) | < 1 ms | Dolt SQL transaction |
| Ranked full-text search over a year of history | < 50 ms (SQLite FTS5) | Dolt SQL query |

The right-hand column reflects the observed behavior that motivated this
design (gascity + superpowers pack on a Docker VM); it is a motivation
record, not a benchmark of Gas City in general.

## 15. Implementation decisions

### 15.1. Rust

- Single static binary (`camp` = CLI + daemon mode), trivially installable,
  no runtime.
- No GC and a small footprint make the < 20 MB idle-RSS target comfortable
  (single-digit MB is realistic).
- Concurrency model: **OS threads + event loop primitives, no async
  runtime in v1.** The daemon's surface — a handful of transcript
  watches, one unix socket, ≤ ~10 child processes, a timer heap — does
  not justify tokio; the
  decision is revisited only if the surface grows. Candidate crates:
  `notify` (FSEvents/inotify), `polling` or `mio` (socket + timers),
  `signal-hook` (SIGCHLD), `serde`/`toml`/`serde_json`, `clap` (CLI),
  `rusqlite` (bundled SQLite, WAL + FTS5), `ratatui` (only if `camp top`
  earns more than plain text).
- Errors: fail fast, no silent fallbacks; every error path either surfaces
  to the caller or lands in the ledger as an event. No panics in library
  code.

### 15.2. Compatibility with Gas City is a requirement

Three concrete contracts, each CI-enforced in the implementation plan:

1. **Formula subset invariant (§8.2):** every valid camp formula is a valid
   Gas City formula-v2 file; camp's corpus is validated against `gc`'s
   compiler in CI.
2. **Vocabulary mirror:** event type names and outcome metadata
   (`outcome`, `final_disposition`) match Gas City's where the concept
   exists; camp-specific names are additive, never redefinitions.
3. **Agent definitions are Claude Code files**, which Gas City's Claude
   provider can drive — a camp pack's roles are reusable as city
   configuration with a thin `pack.toml` wrapper.

### 15.3. Migration: camp → city (and back)

- `camp export --city <dir>` emits: (a) bd-importable JSONL for open and
  historical beads — IDs, titles, status, `needs` edges, labels, outcome
  metadata; (b) the pinned formulas from `runs/` (already valid v2 subset
  files); (c) the pack's agent definitions with a generated `pack.toml`
  wrapper, the camp's orders translated into city order files
  (`orders/<name>.toml` — gc packs declare orders as files by convention;
  gc's `pack.toml` cannot declare orders inline), and the authored
  formulas those orders reference. Field-level mapping:
  `docs/reference/export.md`. The output is a directory a Gas City
  operator imports with standard `bd import` / pack installation; camp
  does not write into a live city's store directly.
- City → camp is documented as manual subset extraction (take the formulas
  that fit the subset, the agent prompts, and open beads); automating it is
  explicitly out of scope until someone actually wants it.
- The migration contract's field-level mapping (bd issue types, label
  conventions) is design work for the implementation plan, pinned by the
  vocabulary mirror above.

## 16. Testing strategy

TDD throughout; unit tests live next to code.

- **Unit (pure, fast):** event application → state tables, with refold
  equivalence (state tables ≡ fold(event log), the `doctor --refold`
  property); readiness computation
  over dependency graphs; formula parse/validate (subset acceptance and
  city-only rejection tables); cron heap with a mocked clock, including
  wall-clock jumps and catch-up windows; stall-timer arming/reset with a
  mocked clock; retry/check classification tables mirroring the Gas City
  spec.
- **Integration (no Claude, CI-safe):** a **fake agent** — a shell script
  that speaks the worker contract (claims via `camp` CLI, emits breadcrumb
  events, closes with configurable outcomes/timing/crashes) — drives full
  runs through `campd`: fan-out, check loops, retry exhaustion, stall →
  nudge → restart ladders (driven by transcript-watch resets), adoption
  after `kill -9`. A synthetic-volume fixture (30 heavy days, ≥1M event
  rows) asserts the §14 write, query, and search targets hold; memory
  `remember`/`recall` round-trips are covered here too.
- **Compatibility:** camp's formula corpus validated against the real `gc`
  compiler; event/outcome vocabulary checked against a pinned Gas City
  reference list.
- **E2E (opt-in, real `claude -p`):** the Tier-0 flag-add scenario and one
  formula run, asserting the §14 latency targets and idle-CPU measurement.
- **Plugin hooks:** exercised against fixture stdin payloads; throttling
  and fire-and-forget append behavior verified.

## 17. Open items and assumptions to verify

Design-insulated assumptions — each has a decided fallback, so resolution
tunes UX rather than structure:

- **A1 — teammate interaction mechanics.** Assumed: the user can select an
  attended teammate in the Claude Code TUI and converse mid-run. Fallback
  if weaker than assumed: Tier-0 spawns headless + instant attach.
  *Resolved 2026-07-06 — holds (claude 2.1.201):* the agent panel lists
  teammates; arrow-select + Enter messages one directly; a mid-run message
  is delivered at the teammate's next step boundary and answered without a
  restart (delivery is not preemption — the agent chooses when to act).
  Fallback not needed. Evidence:
  `docs/design/2026-07-06-assumption-findings.md`.
- **A2 — teammate working directory.** Assumed unresolved whether a
  teammate can run with cwd in a different repo than the session. Camp
  already routes cross-rig work headless by default (§12), so this only
  affects whether *same-rig* attended work in a multi-rig camp can be a
  teammate.
  *Resolved 2026-07-06 — weaker:* a teammate's cwd is pinned to the parent
  session's directory; no per-agent cwd exists. Cross-repo access is
  file-level only (`--add-dir`; headless writes additionally need
  `acceptEdits`). §12's headless routing for cross-rig work is therefore
  required, not provisional; same-rig attended teammates are unaffected.
  Evidence: findings doc above.
- **A3 — no dependence on harness team persistence.** Camp deliberately
  assumes Claude Code team/task state does **not** survive restarts; the
  ledger is the only durability. If the harness persists more, camp gets
  free UX, not changed semantics.
  *Resolved 2026-07-06 — holds:* harness task state is per-session
  (resume-scoped); team config is removed at session end, and a live team
  could not be carried across a restart in testing. Backgrounded sessions
  (`claude attach <id>`) are free reachability UX, not shared state.
  Evidence: findings doc above.
- **A4 — headless mid-run conversation.** Assumed: conversation with a
  running headless worker is tail-the-transcript now, converse via resume
  after its current turn. If input streaming into a live headless session
  is available, the patrol nudge action (§10) gains a live path instead of
  waiting for the turn boundary.
  *Resolved 2026-07-06 — stronger:* tail-now and resume-after both hold
  (resume keeps the session id and appends to the same transcript), and
  live input **is** available — a `claude -p --input-format stream-json`
  worker accepts additional user turns over stdin mid-lifetime, so §10's
  nudge gains the live path for campd-spawned workers. Concurrent resume
  against a live session also works. Dispatch mechanics for Phase 8 are
  pinned as fixture facts F1–F7 in the findings doc above.
- **Upstream proposal, independent of camp:** propose write-event emission
  to beads upstream — a bd that pushes mutation events would let Gas
  City's controller subscribe instead of poll, attacking the standing tax
  at its source for city users too (§7.5). Camp does not depend on it.
- **Possible future standalone project:** a bd-CLI-compatible shim over
  camp's ledger (§7.5) — "beads-compatible, with camp's efficiency
  trade-offs." Deliberately enabled by concept-compatibility, deliberately
  not part of camp v1.
- **Name.** "Gas Camp" (`camp`/`campd`) is the working name; alternatives
  raised: Outpost, Bivouac. Decide before the repo is published.
- **Order coverage while logged out** is consciously v1-limited (§9);
  revisit only if real usage misses fires that matter.

## 18. Relationship to Gas City

Camp is not a competitor and not a replacement. It is to Gas City what
**k3s is to k8s**: the same conceptual API — six primitives, the
zero-roles law, convergence through persistent work — delivered as one
small binary with the heavyweight store swapped for something
proportionate (k3s traded etcd for SQLite; camp trades Dolt for
SQLite), sized for one box and ten agents, with a mechanical
migration path for the day a job turns out to be city-sized. The two
share vocabulary so that learning either teaches the other.

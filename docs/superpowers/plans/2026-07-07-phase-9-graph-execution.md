# Phase 9 — Graph Execution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Review status:** first-pass Opus review 2026-07-07 (relayed by the lead): REJECT on two plan-internal blockers, design otherwise sound; decisions 2, 4, 5, 7, and 9 ACCEPTED as argued. This revision resolves Blocker A (disposition-vocabulary contradiction: `bead.closed` never carries a `"pass"` disposition — the run-level disposition lives solely in `run.finalized`; anchor exhaustion closes keep `hard_fail|soft_fail`, exactly the `⊆ gc.on_exhausted` pin), resolves Blocker B (the #17 fire budget is per-WAKE — reset once at `event_loop::settle` entry — with a through-converge regeneration test), and folds in the review's five non-blocking notes. Resubmitted for a fresh pass.

**Goal:** Spec §8.3 — campd as a purely mechanical control dispatcher: check loops, retry classification, `on_complete` fan-out, and run finalization, all executing structure declared in TOML with zero judgment in Rust.

**Architecture:** Graph bookkeeping (anchor claims, attempt beads, anchor closes, root finalization) runs inside the existing `process_past_cursor` processor via `Ledger::append_on`, so every graph action commits atomically with the cursor advance — exactly-once across `kill -9` with zero new recovery machinery. The only two side-effectful actions that cannot be cursor-atomic (spawning a check script, cooking a bond formula) use the Phase 10 pattern: an in-memory pending queue drained by the settle loop, plus a pure-ledger-state startup reconciliation. Check scripts run as non-blocking campd children reaped on SIGCHLD, with deadlines folded into the poll timeout — no new wake sources, no polling.

**Tech Stack:** Rust (edition 2024), rusqlite, serde/serde_json, mio, signal-hook, jiff — all existing dependencies. No new crates.

## Global Constraints

- Spec `docs/design/2026-07-05-gas-camp-design.md` is authoritative; §4 decision record settled. No spec contradiction was found while planning; no spec edit ships in this phase.
- AGENTS.md invariants 1–7 bind: no ticks/polling; cost proportional to job; nothing hidden; six primitives, zero roles in code; fail fast, no panics in library code (`clippy::unwrap_used/expect_used/panic` denied, `unsafe_code` forbidden); formula subset invariant; vocabulary mirror.
- Zero-Framework-Cognition (spec §8.3): campd executes edges, budgets, caps declared in TOML. Every judgment comes from agents or user-supplied check scripts. No role names, no heuristics.
- New events use `deny_unknown_fields` payload structs; every state effect happens in the same transaction as its event row (the one-transaction property); vocab-pin partition tests and the refold property must stay green.
- Event-loop ground rules (lead ruling): `event_loop.rs` edits stay additive and minimal — token layout 0/1/2/3+ and poke-arm semantics untouched. The seam touches this plan makes are enumerated in Decision 11 and announced to the lead with this plan.
- Gates before push: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- Branch `phase-9-graph-execution`; one reviewable PR; never commit to main; no co-author lines.
- Siblings in flight: phase-11-patrol-adoption (shares daemon/event-loop area) and phase-14-export-bridge. On a sibling merge notice: rebase onto current main, re-run all gates before continuing.

---

## Design Overview (read first)

### The anchor/attempt model

Cook (Phase 5) materializes one bead per formula step — this plan calls it the **anchor**. Dependents' `needs` edges point at anchors, and anchors are what finalization aggregates. Steps split into two classes:

- **Plain steps** (no `check`, no `retry`): exactly Phase 8's path. The anchor is dispatched to a worker; the worker claims it, works, closes it. Zero new cost.
- **Looping steps** (`check` or `retry` — gc validation S9 already rejects the combination, and rejects `check`+`assignee` and `retry`+`on_complete`): campd owns the anchor. When the anchor becomes ready, campd **claims it** (`bead.claimed`, session `"campd"` — honest: campd is working that step's control loop) and creates **attempt bead 1**. Attempt beads are ordinary task beads carrying the same `run_id`/`step_id` as their anchor (distinguished by id ≠ the manifest's anchor id); they are ready at birth, so the existing dispatcher spawns workers for them unchanged. The worker's close of an attempt is the input to the mechanical loop; campd closes the anchor when the loop resolves. Dependents therefore unblock exactly when the *step* (not an attempt) passes.

Why not reopen a closed step bead for the next attempt: `bead.closed` is final in the fold (closing a closed bead is an invalid transition), a reopen event would be a new state-mutating vocabulary with outcome-clearing semantics, and the attempt-bead history reads better — each attempt has its own claim/close/session trail (invariant 3: every action with its cause).

Attempt numbering is pure ledger state: attempts of `(run_id, step_id)` are the step's beads excluding the anchor, ordered by `(created_ts, id)`. Check iterations used = count of attempts closed `pass` (each passing attempt triggers exactly one check run). Retry attempts used = count of attempts closed `fail` with `failure_class:"transient"`.

### Exactly-once without new machinery

`CampdProcessor::process` (daemon/orders.rs) runs per committed event inside the cursor transaction; `Ledger::append_on` is the sanctioned same-transaction append (full fold). All of the following are therefore cursor-atomic and exactly-once across `kill -9`:

- claim ready looping anchors + create attempt 1 (on `bead.created` / newly-ready-after-close),
- retry classification actions (next attempt bead, or anchor close with disposition),
- anchor close pass for retry steps (with the passing attempt's `output` copied),
- root finalization (skipped-closes + root close + `run.finalized`, one transaction).

Two actions have non-ledger side effects and use the Phase 10 queue+reconcile pattern instead:

- **check script execution**: processor queues `PendingCheck`; the settle loop spawns the script (non-blocking child); the SIGCHLD reap appends the verdict batch. Crash windows are re-derived at startup: an anchor `in_progress` claimed by `"campd"` whose latest attempt closed `pass` with no verdict yet (any verdict either closed the anchor or created the next attempt — both visible state) means "check due" and re-queues. Check scripts are re-runnable by contract (gc's mechanical checker semantics).
- **bond cooking** (`on_complete`): processor queues `PendingFanout`; the settle loop cooks. Child roots carry the label `bond:<anchor>:<index>`, so fan-out completeness is pure beads-table state; startup reconciliation re-queues incomplete fan-outs.

**Termination argument (issue #17 adjacency):** every graph append is guarded by a state precondition that the append itself destroys (claim requires `open`; attempt creation requires budget remaining; anchor close requires anchor not closed; finalization requires root open). So the settle fixpoint does bounded work per wake, and the new machinery cannot widen #17's class. The #17 class itself (self-triggering event orders) gets a per-WAKE fire budget — Decision 8. Both #17 scenarios are covered: the in-settle feedback loop (`event:bead.created`) and the through-converge loop (`event:dispatch.failed` + routing hole), which regenerates one fire per OUTER settle iteration and therefore demands the budget survive across `orders::settle` calls within one wake.

### Settle-loop shape after this phase (event_loop.rs)

```text
runtime.reset_fire_budget();               // ONCE per wake (Decision 8 / Blocker B)
loop {                                     // event_loop::settle
    orders::settle(ledger, readiness, runtime, clock, graph)?;  // + graph hooks in CampdProcessor
    graph.execute(ledger)?;                // spawn pending checks; cook pending fanouts
    dispatcher.converge(ledger)?;          // unchanged
    if cursor == head { return Ok(()); }
}
```

`graph.execute`'s cooks append events, so the loop re-settles them in the same wake; check spawns append nothing (verdicts arrive via SIGCHLD → `graph.reap_checks` → settle).

## Plan-Time Decision Log

Decisions 2, 4, 5, 7, and 9 were ACCEPTED by the first review pass. Decision 3 is revised per Blocker A; Decision 8 is revised per Blocker B (mechanism accepted, scope corrected to per-wake). Remaining open ruling: Decision 8's budget value and failure-event shape.

1. **Anchor/attempt model** (above). Attempt beads inherit the step's `assignee` (retry steps; check steps have none by gc rule S9) and title `"<step title> (attempt N)"`. Attempt-bead descriptions for check respawns carry the check failure evidence (exit code + log tail) — mechanical copying, the worker needs it, nothing hidden.
2. **`skipped` outcome + one schema edit.** At finalization, open anchors whose needs can never be satisfied (a dependency closed non-pass, recursively) are closed by campd with `outcome:"skipped"` — gc's own vocabulary for exactly this (pinned in gc-vocab.json `outcome` list; camp's outcomes stay a strict subset). Requires widening `beads.outcome CHECK (outcome IN ('pass','fail'))` to include `'skipped'` in `STATE_DDL` and adding `"skipped"` to `CAMP_OUTCOMES`. Pre-v1: no migration shipped (operator rule: no backward compatibility unless asked); refold rebuilds shadow state from the same DDL so the refold property is unaffected.
3. **Finalization = quiescence + mechanical aggregation** (revised per review Blocker A). A run finalizes when every anchor is closed or unsatisfiable-open, and none is in flight (`in_progress`, or open-and-satisfiable). Then, in ONE cursor transaction: skipped-closes for unsatisfiable anchors → root `bead.closed {outcome}` (**outcome only — no disposition field on the root close**) → `run.finalized` carrying the run-level `final_disposition` (with `cause_seq` = the close event that triggered quiescence — the spec §13.3 cause chain). Disposition placement, pinned:
   - `bead.closed.final_disposition` appears ONLY on looping-step anchor fail-closes, with values `hard_fail | soft_fail` — validated against `CAMP_FINAL_DISPOSITIONS`, which stays exactly `⊆ gc.on_exhausted` (vocab.rs:42's pin and comment anticipated precisely this use). A `"pass"` disposition on a close is impossible by construction and rejected by the fold.
   - `run.finalized.final_disposition` carries the run-level verdict `pass | hard_fail | soft_fail` — validated against the new `CAMP_RUN_DISPOSITIONS`, pinned `⊆ gc.final_disposition` (all three values are in the fixture).
   Aggregation table (fully mechanical; last two columns land on `run.finalized`):
   | run contains | root close outcome | run.finalized outcome | run.finalized final_disposition |
   |---|---|---|---|
   | any anchor fail with `hard_fail` disposition (or plain fail) | fail | fail | hard_fail |
   | else any skipped anchor | fail | fail | soft_fail |
   | else any soft-failed anchor | pass | pass | soft_fail |
   | else (all pass) | pass | pass | pass |
   This preserves master-plan decision 6: soft_fail never satisfies `needs`; a soft-failed step with dependents skips them and fails the run; without dependents the run completes `pass`/`soft_fail`. A single hard-failed step finalizes the run as soon as nothing is in flight ("check budget exhaustion fails the run"), rather than dangling forever on unreachable steps.
4. **Sequential `on_complete` = lazy cooking chained on child-root pass.** Parallel cooks all items in the settle that processes the close. Sequential cooks item 0 then cooks item i+1 when child i's root closes `pass` (the processor watches root closes of beads labeled `bond:…`); a non-pass child halts the chain (the faithful translation of "each child's root needs the previous" — an eager cook with that edge would gate nothing, because steps dispatch independently of their root's readiness). The literal needs edge (child root needs previous child root) is still added for auditability. **Needs a ruling** — the master plan sentence taken literally is inert; this is the mechanical reading that actually serializes.
5. **Bond children are independent runs.** The parent run finalizes without waiting for children (they have their own roots, finalization, and `run.finalized`). **Needs a ruling.**
6. **Vars substitution is cook-time string substitution.** `{item}` (the item itself: strings raw, other JSON compact-serialized), `{item.<path>}` (path into the item; a missing path fails the fan-out), `{index}` (0-based) substitute into the `on_complete.vars` values; the resulting map substitutes `{key}` occurrences in the child's step titles and descriptions at cook time and is recorded in the child's `manifest.json` under `"vars"`. The pinned child formula file stays verbatim (materialization property).
7. **`camp sling --formula <name>` ships in this phase** (scope addition — flagged). Spec §8.2 names it; no other phase owns it (Phase 12's slash command wraps the CLI). Loads `<camp>/formulas/<name>.toml`, `parse_and_validate`, cooks into `<camp>/runs/`, pokes with autostart, prints the run id and root bead. Without it the only cook surface is orders.
8. **Issue #17 closure: per-(order, WAKE) fire budget** (revised per review Blocker B — mechanism accepted, scope corrected). `FIRE_BUDGET: usize = 256` fires per order per WAKE; the counter map lives in `OrdersRuntime` and is reset by `reset_fire_budget()` called ONCE at `event_loop::settle` entry — never inside `orders::settle`, because `event_loop::settle` loops `orders::settle` with `dispatcher.converge` between iterations, and #17's scenario 2 (`event:dispatch.failed` + a routing hole) regenerates exactly one fire per OUTER iteration: a per-call reset never accumulates and the wake never terminates. Beyond the budget the processor appends `order.failed {order, fired_seq: <matching event's seq>, error: "event-trigger fire budget (256) exhausted in one wake — likely a self-triggering order"}` once per wake instead of `order.fired`, and suppresses that order until the next wake. Suppressed matches advance behind the cursor and never re-fire, so the loop quiesces and an idle camp stays idle (no self-wakes: suppressed fires cook nothing). Legitimate bursts are bounded by the pre-existing backlog; only regenerative loops hit the budget. Compile-time rejection was considered and rejected: `bead.closed` — the flagship trigger — is itself campd-produced. Both #17 scenarios get tests (Task 9). **Open ruling:** the budget value (256) and the `fired_seq`-carries-the-cause-seq shape for the budget failure event.
9. **Crashed attempt workers are Phase 11's lane.** A `session.crashed` on an attempt bead releases it (existing fold) but Phase 9 does not respawn it — `dispatchable_beads` already excludes ever-sessioned beads, and the master plan gives respawn/restart ladders (with budgets and backoff) to Phase 11 patrol. Phase 9 consumes only the worker's *declared* classification (close events). **Needs a ruling** (boundary with phase-11, per readiness.rs's "Phase 9/11" comment).
10. **Check-script spawn failure = immediate anchor hard fail** (no budget loop): a script that cannot start (missing, not executable) is structural, not flaky; burning the budget on it would be noise. The anchor closes fail/hard_fail with the OS error as the reason.
11. **event_loop.rs seam touches (announced to the lead with this plan):** (a) thread a `&mut GraphRuntime` through `run`/`settle`/`serve_connection`/`drain_lines`/`reap_and_refill` signatures; (b) `graph.execute` call between `orders::settle` and `dispatcher.converge` in `settle`; (c) the poll timeout becomes `min_deadline(runtime.poll_timeout(wall_now), graph.poll_timeout(Instant::now()))` via a small shared combinator `fn min_deadline(a: Option<Duration>, b: Option<Duration>) -> Option<Duration>` — deliberately shaped as THE poll-timeout composition point, because phase-11 adds a third deadline source (stall timers) at the same seam (event_loop.rs:116); expect a textual collision there and resolve at rebase by adding their source to the same combinator. Mixing wall-anchored (cron) and monotonic (check) sources is correct — each contributor converts its own deadline to a `Duration`-from-now; (d) one `graph.kill_expired(Instant::now())` call at wake head (after the fires computation) so a deadline-only wake enforces check timeouts — monotonic, NOT the wall-clock `now` used for cron fires; (e) `graph.reap_checks(ledger)` in `reap_and_refill` before the settle; (f) `runtime.reset_fire_budget()` at `settle` entry (Decision 8). No token changes, no restructuring, poke-arm untouched.
12. **Check deadline = `min(check.timeout, step.timeout)`**, whichever are set; neither set = no deadline (declared structure only — campd invents no defaults). A timed-out check is killed (SIGKILL) and classified as a failed check iteration (`check.failed` with `timed_out: true`).
13. **A transient close on a non-retry step is a hard fail** (no declared budget = nothing to spend); on a check step likewise (the check budget counts check runs, not worker failures). Mechanical: budgets exist only where declared.

## File Map

| File | Change |
|---|---|
| `crates/camp-core/src/vocab.rs` | `check.passed`/`check.failed`/`run.finalized` → CAMP_SPECIFIC_EVENTS; `CAMP_OUTCOMES` += `"skipped"`; `CAMP_FAILURE_CLASSES` const |
| `crates/camp-core/src/event.rs` | 3 new `EventType` variants |
| `crates/camp-core/src/ledger/schema.rs` | outcome CHECK widened (Decision 2) |
| `crates/camp-core/src/ledger/fold.rs` | `BeadClosed` payload extension; 3 new log-only arms |
| `crates/camp-core/src/formula/runtime.rs` | NEW — pure runtime: `RunContext`, attempts, budgets, unsatisfiability, aggregation, for_each, vars |
| `crates/camp-core/src/formula/cook.rs` + `mod.rs` | `CookOptions` (vars, extra root needs/labels) |
| `crates/camp/src/campdir.rs` | `runs_path()` helper |
| `crates/camp/src/cmd/close.rs` | `--transient`, `--output-json <file|->` |
| `crates/camp/src/cmd/sling.rs` + `main.rs` | `--formula` mode; new close flags wired |
| `crates/camp/src/daemon/dispatch.rs` | NEW `GraphRuntime` (processor hooks, pending queues, check children, reconcile) |
| `crates/camp/src/daemon/orders.rs` | `CampdProcessor` + `settle` gain the graph hook; fire budget |
| `crates/camp/src/daemon/event_loop.rs` | Decision 11 touches only |
| `crates/camp/src/daemon/mod.rs` | construct `GraphRuntime`; startup reconcile |
| `crates/camp/tests/fake-agent.sh` | `FAKE_AGENT_PLAN`, `FAKE_AGENT_OUTPUT_JSON`, `--transient` support |
| `crates/camp/tests/daemon_graph.rs` | NEW — the master-plan integration suite |

Master-plan deviation note: the plan's Files line names `dispatch.rs (extend)`; `GraphRuntime` lives there (it is dispatch machinery — children, spawning, convergence queues). `Dispatcher` itself is untouched.

---

### Task 1: Vocabulary, event types, fold payloads, schema

**Files:**
- Modify: `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/event.rs`, `crates/camp-core/src/ledger/schema.rs`, `crates/camp-core/src/ledger/fold.rs`

**Interfaces produced:** `EventType::{CheckPassed, CheckFailed, RunFinalized}` (`"check.passed"`, `"check.failed"`, `"run.finalized"`); `CAMP_OUTCOMES = ["pass","fail","skipped"]`; `CAMP_FAILURE_CLASSES = ["transient"]`; `CAMP_RUN_DISPOSITIONS = ["pass","hard_fail","soft_fail"]` (run-level, `run.finalized` only); `bead.closed` data accepts optional `failure_class`, `final_disposition` (`hard_fail|soft_fail` ONLY — Decision 3), `output`.

- [ ] **Step 1: Failing tests** — in `fold.rs` tests add:

```rust
#[test]
fn close_payload_accepts_phase9_fields_and_validates_them() {
    let (_d, mut l) = ledger();
    create(&mut l, "gc-1", &[]);
    // transient requires outcome fail
    let err = l.append(EventInput { kind: EventType::BeadClosed, rig: Some("gc".into()),
        actor: "t".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"outcome":"pass","failure_class":"transient"}) });
    assert!(err.is_err(), "transient on a pass close must be rejected");
    // unknown failure_class rejected
    let err = l.append(EventInput { kind: EventType::BeadClosed, rig: Some("gc".into()),
        actor: "t".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"outcome":"fail","failure_class":"flaky"}) });
    assert!(err.is_err());
    // a close never carries a "pass" disposition (Decision 3 / review Blocker A)
    let err = l.append(EventInput { kind: EventType::BeadClosed, rig: Some("gc".into()),
        actor: "t".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"outcome":"pass","final_disposition":"pass"}) });
    assert!(err.is_err(), "the run-level pass disposition lives only in run.finalized");
    // legal: fail + transient + output + disposition
    l.append(EventInput { kind: EventType::BeadClosed, rig: Some("gc".into()),
        actor: "t".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"outcome":"fail","failure_class":"transient",
            "final_disposition":"soft_fail","output":{"items":[1,2]}}) }).unwrap();
}

#[test]
fn skipped_is_a_legal_outcome() {
    let (_d, mut l) = ledger();
    create(&mut l, "gc-1", &[]);
    l.append(EventInput { kind: EventType::BeadClosed, rig: Some("gc".into()),
        actor: "campd".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"outcome":"skipped","reason":"needs cannot be satisfied"}) }).unwrap();
    assert_eq!(l.get_bead("gc-1").unwrap().unwrap().outcome.as_deref(), Some("skipped"));
}

#[test]
fn phase9_log_events_validate_their_payloads() {
    let (_d, mut l) = ledger();
    create(&mut l, "gc-1", &[]);
    // check.passed happy path
    l.append(EventInput { kind: EventType::CheckPassed, rig: Some("gc".into()),
        actor: "campd".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r1","step_id":"s1","attempt":2}) }).unwrap();
    // check.failed requires exit evidence or timeout
    assert!(l.append(EventInput { kind: EventType::CheckFailed, rig: Some("gc".into()),
        actor: "campd".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r1","step_id":"s1","attempt":1}) }).is_err());
    l.append(EventInput { kind: EventType::CheckFailed, rig: Some("gc".into()),
        actor: "campd".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r1","step_id":"s1","attempt":1,
            "exit_code":1,"log":"runs/r1/checks/s1-attempt-1.log"}) }).unwrap();
    // run.finalized
    l.append(EventInput { kind: EventType::RunFinalized, rig: Some("gc".into()),
        actor: "campd".into(), bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r1","root":"gc-1","outcome":"fail",
            "final_disposition":"hard_fail","cause_seq":3,
            "soft_failed":[],"skipped":["s2"]}) }).unwrap();
}
```

- [ ] **Step 2:** `cargo test -p camp-core fold` — expect FAIL (unknown fields / unknown event types).
- [ ] **Step 3: Implement.**
  - `event.rs`: add `CheckPassed`, `CheckFailed`, `RunFinalized` to the enum, `ALL`, and `as_str` (`"check.passed"`, `"check.failed"`, `"run.finalized"`).
  - `vocab.rs`: append the three names to `CAMP_SPECIFIC_EVENTS`; `CAMP_OUTCOMES = &["pass","fail","skipped"]`; add `pub const CAMP_FAILURE_CLASSES: &[&str] = &["transient"];`.
  - `schema.rs`: `outcome TEXT CHECK (outcome IN ('pass','fail','skipped'))`.
  - `fold.rs`: extend the payload struct and validation:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadClosed {
    outcome: String,
    #[serde(default)] reason: Option<String>,
    #[serde(default)] failure_class: Option<String>,
    #[serde(default)] final_disposition: Option<String>,
    #[serde(default)] output: Option<serde_json::Value>,
}
```

  In `bead_closed`, after the outcome check: `failure_class` must be in `CAMP_FAILURE_CLASSES` and requires `outcome == "fail"`; `final_disposition` must be in `CAMP_FINAL_DISPOSITIONS` (`hard_fail|soft_fail` — the `⊆ gc.on_exhausted` pin; `"pass"` is rejected here by construction, resolving review Blocker A) and likewise requires `outcome == "fail"`. `output` is any JSON (audit content; readers pull it from the event row). New arms in `apply`, each log-only with `deny_unknown_fields` structs:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckPassed { run_id: String, step_id: String, attempt: u32 }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckFailed {
    run_id: String, step_id: String, attempt: u32,
    #[serde(default)] exit_code: Option<i64>,
    #[serde(default)] signal: Option<i64>,
    #[serde(default)] timed_out: Option<bool>,
    #[serde(default)] error: Option<String>,
    #[serde(default)] log: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunFinalized {
    run_id: String, root: String, outcome: String, final_disposition: String,
    cause_seq: i64, soft_failed: Vec<String>, skipped: Vec<String>,
}
```

  Validation: `check.passed`/`check.failed` require non-empty `run_id`/`step_id`, `attempt >= 1`, and event.bead set + known (the attempt bead); `check.failed` requires at least one of `exit_code`/`signal`/`timed_out:true`/`error`; `run.finalized` requires event.bead = `root`, `outcome ∈ CAMP_OUTCOMES`, `final_disposition ∈ CAMP_RUN_DISPOSITIONS` (new in vocab.rs: `pub const CAMP_RUN_DISPOSITIONS: &[&str] = &["pass","hard_fail","soft_fail"];`). Add pin assertions to `crates/camp-core/tests/vocab_pin.rs` with the same rigor as the existing consts (review note 3): every `CAMP_RUN_DISPOSITIONS` value must appear in the fixture's `final_disposition` list (all three are), and every `CAMP_OUTCOMES` value in the fixture's `outcome` list (`skipped` is). The fixture carries no `failure_class` list, so `CAMP_FAILURE_CLASSES` has no pin to assert against — it follows the master plan's wording (`failure_class:"transient"`, gc's key vocabulary) verbatim.
- [ ] **Step 4:** `cargo test -p camp-core` — PASS, including `vocab_pin` (the pin fixture's `events` list contains no `check.*`/`run.*` names, so the camp-specific partition holds) and `refold_prop`.
- [ ] **Step 5:** Commit `feat(core): phase-9 vocabulary — check/run.finalized events, close payload extensions, skipped outcome`.

### Task 2: `camp-core/src/formula/runtime.rs` — the pure runtime

**Files:**
- Create: `crates/camp-core/src/formula/runtime.rs`; modify `crates/camp-core/src/formula/mod.rs` (`pub mod runtime;`)

**Interfaces produced (consumed by Tasks 5–8):**

```rust
pub struct RunContext {
    pub run_id: String,
    pub rig: String,
    pub root: String,
    pub formula: Formula,
    pub anchors: BTreeMap<String, String>, // step_id -> anchor bead id
}
pub fn load_run(runs_dir: &Path, run_id: &str) -> Result<RunContext, CoreError>;
pub fn is_looping(step: &Step) -> bool;                     // check.is_some() || retry.is_some()
pub struct StepRef<'a> { pub step: &'a Step, pub anchor: &'a str }
impl RunContext {
    pub fn step_for_bead(&self, row: &BeadRow) -> Option<StepRef<'_>>; // by step_id; anchor vs attempt via id
    pub fn is_anchor(&self, bead_id: &str) -> bool;
}
pub fn attempts(conn: &Connection, ctx: &RunContext, step_id: &str) -> Result<Vec<BeadRow>, CoreError>;
pub fn check_runs_used(attempts: &[BeadRow]) -> u32;        // closed-pass count
pub fn transient_fails_used(conn: &Connection, attempts: &[BeadRow]) -> Result<u32, CoreError>;
pub fn unsatisfiable(conn: &Connection, bead: &str) -> Result<bool, CoreError>;
pub enum RunVerdict { NotQuiescent, Finalize { outcome: &'static str, disposition: &'static str,
                                               soft_failed: Vec<String>, skipped: Vec<String> } }
pub fn finalization(conn: &Connection, ctx: &RunContext) -> Result<RunVerdict, CoreError>;
pub fn resolve_for_each<'v>(output: &'v serde_json::Value, path: &str) -> Result<&'v Vec<serde_json::Value>, String>;
pub fn substitute_vars(vars: &BTreeMap<String, String>, item: &serde_json::Value, index: usize)
    -> Result<BTreeMap<String, String>, String>;
pub fn bond_label(anchor: &str, index: usize) -> String;    // "bond:<anchor>:<index>"
pub fn parse_bond_label(label: &str) -> Option<(&str, usize)>;
```

Implementation notes (keep them pure — `&Connection` reads only, no writes, no clock):
- `load_run` reads `<runs_dir>/<run_id>/manifest.json` (fields `run_id`, `formula`, `rig`, `root`, `steps`) and re-parses the pinned `<formula>.toml` via `formula::parse_and_validate`. Any missing file or drift (manifest step ids ≠ formula step ids) is `CoreError::Corrupt` naming the run dir.
- `attempts`: `SELECT` beads `WHERE run_id=?1 AND step_id=?2 AND id<>?3 ORDER BY created_ts, id` (anchor id from `ctx.anchors`). `transient_fails_used` inspects each closed-fail attempt's close event via `SELECT data FROM events WHERE bead=?1 AND type='bead.closed'` (the `events_bead` index) for `failure_class == "transient"`.
- `unsatisfiable(b)`: for each `needs` target of `b`: missing → true; closed with outcome ≠ pass → true; not closed → recurse. Cycles are impossible (validate rejects same-run cycles; cross-run edges point at closed roots).
- `finalization`: over anchors only. Any anchor `in_progress`, or open-and-not-unsatisfiable → `NotQuiescent`. Otherwise gather: `hard` = closed fail without `final_disposition:"soft_fail"`; `soft_failed` = closed fail with it (step ids); `skipped` = open unsatisfiable anchors (step ids — the daemon closes those beads) plus anchors already closed `skipped` (restart idempotency). Apply the Decision 3 table. Reading a closed anchor's disposition uses its close event (`events_bead` index).
- `resolve_for_each`: strip the validated `output.` prefix; walk remaining dot segments through the close event's `data["output"]`; the terminal value must be an array — every violation returns a human-actionable `Err(String)` naming the path and what was found.
- `substitute_vars`: for each var value, replace `{index}` with the 0-based index, `{item}` with the item (strings raw; other values `serde_json::to_string` compact), and `{item.<p>}` for every occurrence, walking `<p>` into the item; a missing path or non-scalar terminal is an `Err(String)` naming the var and path. (Scalars: string raw; number/bool via `to_string`.)

- [ ] **Step 1: Failing tests** (same-file `#[cfg(test)]`, using `Ledger` + `FixedClock` fixtures in the style of `readiness.rs` tests). Cover, minimally:

```rust
#[test] fn attempts_order_and_budget_counters() { /* create anchor + 3 attempt beads;
    close a1 pass, a2 fail+transient, a3 fail (hard). assert attempts() order,
    check_runs_used == 1, transient_fails_used == 1 */ }
#[test] fn unsatisfiable_walks_transitively() { /* A(closed fail) <- B(open) <- C(open):
    unsat(B) && unsat(C); D open with open-but-satisfiable dep is NOT unsat */ }
#[test] fn finalization_table() { /* four mini-runs exercising each Decision-3 row,
    incl. NotQuiescent while an anchor is in_progress */ }
#[test] fn for_each_resolution_and_errors() { /* output.items happy; output.missing errors
    naming the path; non-array terminal errors */ }
#[test] fn var_substitution_matrix() { /* {item} string vs object, {item.name},
    {index}, missing field error names the var */ }
#[test] fn bond_label_round_trips() { assert_eq!(parse_bond_label(&bond_label("gc-7", 2)), Some(("gc-7", 2))); }
#[test] fn load_run_round_trips_a_cooked_run_and_errors_on_missing_dir() { /* cook a fixture
    formula into a tempdir ledger+runs dir; load_run returns matching anchors;
    load_run on a bogus id is Err */ }
```

- [ ] **Step 2:** `cargo test -p camp-core runtime` — FAIL (module missing).
- [ ] **Step 3:** Implement `runtime.rs` per the notes above.
- [ ] **Step 4:** `cargo test -p camp-core` — PASS.
- [ ] **Step 5:** Commit `feat(core): formula runtime — pure attempt/finalization/fan-out bookkeeping over ledger state`.

### Task 3: Cook options (vars, root needs/labels)

**Files:**
- Modify: `crates/camp-core/src/formula/cook.rs`, `crates/camp-core/src/formula/mod.rs`, `crates/camp-core/tests/cook.rs`, callers (`crates/camp-core/src/orders/mod.rs` `execute_fire`)

**Interfaces produced:**

```rust
#[derive(Default)]
pub struct CookOptions {
    pub vars: BTreeMap<String, String>,        // substituted into step titles/descriptions; recorded in manifest
    pub extra_root_needs: Vec<String>,         // sequential bond chaining edge
    pub extra_root_labels: Vec<String>,        // ["bond:<anchor>:<i>"]
}
pub fn cook_with(ledger: &mut Ledger, formula: &Formula, run_dir: &Path, rig: &RigConfig,
                 actor: &str, opts: &CookOptions) -> Result<CookedRun, CoreError>;
// existing cook(..) delegates to cook_with(.., &CookOptions::default())
```

Semantics: every `{key}` in each step's `title`/`description` is replaced by `opts.vars[key]` (unknown `{...}` tokens left verbatim — the pinned file is authored text, not a template language); root bead `needs` = step beads + `extra_root_needs`; root `labels` = `extra_root_labels`; `manifest.json` gains `"vars"` when non-empty. The pinned formula copy stays byte-verbatim.

- [ ] **Step 1: Failing test** in `crates/camp-core/tests/cook.rs`:

```rust
#[test]
fn cook_with_substitutes_vars_and_links_the_root() {
    // fixture formula: one step, title "Handle {name} at {position}"
    // opts: vars {name: "alpha", position: "0"}, extra_root_needs ["gc-1"] (a pre-created bead),
    //       extra_root_labels ["bond:gc-1:0"]
    // assert: step bead title == "Handle alpha at 0"; root labels contain the bond label;
    // deps rows for root include gc-1; pinned file byte-identical to source;
    // manifest["vars"]["name"] == "alpha"
}
```

- [ ] **Step 2:** run — FAIL. **Step 3:** implement (`root_data["labels"]`, needs extension, a `substitute` helper applied to title/description at bead-input build time). **Step 4:** `cargo test -p camp-core` PASS (existing cook tests keep passing via the delegating `cook`). **Step 5:** Commit `feat(core): cook options — bond vars substitution and root linkage`.

### Task 4: CLI — `close --transient/--output-json`, `sling --formula`, `runs_path`

**Files:**
- Modify: `crates/camp/src/cmd/close.rs`, `crates/camp/src/cmd/sling.rs`, `crates/camp/src/main.rs`, `crates/camp/src/campdir.rs`
- Test: `crates/camp/tests/cli_claim_close.rs`, `crates/camp/tests/cli_sling.rs`

**Interfaces produced:** `camp close <bead> --outcome pass|fail [--reason r] [--transient] [--output-json <file|->]`; `camp sling --formula <name> [--rig r]` (prints `"<run_id> root <root-bead>"`); `CampDir::runs_path() -> PathBuf`. DRY (review note 5): add `pub const RUNS_SUBDIR: &str = "runs";` to `camp-core/src/formula/mod.rs`; both `CampDir::runs_path()` (`root.join(RUNS_SUBDIR)`) and `orders::execute_fire` (replacing its `camp_root.join("runs")` literal) use it — one definition of where runs live, across the crate boundary campdir cannot cross.

- [ ] **Step 1: Failing tests.** In `cli_claim_close.rs`: `--transient` with `--outcome pass` exits nonzero naming the rule; `--transient --outcome fail` produces a close event with `data.failure_class == "transient"`; `--output-json <file>` embeds the file's JSON at `data.output` (and `-` reads stdin); malformed JSON exits nonzero naming the file. In `cli_sling.rs`: `sling --formula one-step` against an initialized camp cooks (a `run.cooked` event exists, `runs/<id>/` pinned), errors name the formula when the file is missing or invalid.
- [ ] **Step 2:** run both — FAIL (unknown flags).
- [ ] **Step 3:** Implement. close.rs signature: `run(camp, bead, outcome, reason, transient: bool, output_json: Option<String>)`; client-side rule: `--transient` requires `--outcome fail` (bail with the same wording the fold uses). sling.rs: a `--formula` arg makes `title` optional/forbidden (`bail!` if both given); formula mode = load `camp.root.join("formulas").join(format!("{name}.toml"))` → `parse_and_validate` → `cook` into `camp.runs_path()` with `actor: "cli"` → `autostart::request_with_autostart(camp, &Request::Poke { seq: cooked_seq }, "sling")` → print. main.rs: clap wiring.
- [ ] **Step 4:** `cargo test -p camp` for the two files — PASS. **Step 5:** Commit `feat(cli): close classification/output flags; sling --formula cooks a run`.

### Task 5: `GraphRuntime` processor hooks (claims, attempts, anchor closes, finalization)

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` (append `GraphRuntime`), `crates/camp/src/daemon/orders.rs` (`CampdProcessor` + `settle`), `crates/camp/src/daemon/event_loop.rs` (threading only, per Decision 11a/11b)

**Interfaces produced:**

```rust
// dispatch.rs
pub struct GraphRuntime {
    camp_root: PathBuf,
    runs: HashMap<String, Option<Arc<RunContext>>>,   // None = load failed and evented once
    pending_checks: Vec<PendingCheck>,
    pending_fanouts: Vec<PendingFanout>,
    check_children: HashMap<u32, CheckChild>,          // Task 6
}
impl GraphRuntime {
    pub fn new(camp_root: PathBuf) -> GraphRuntime;
    /// Cursor-atomic hook, called from CampdProcessor::process for every event.
    pub fn process(&mut self, conn: &Connection, now: &str, event: &Event) -> Result<(), CoreError>;
    /// Side-effect executor, called from event_loop::settle between orders::settle and converge.
    pub fn execute(&mut self, ledger: &mut Ledger) -> Result<()>;                 // Tasks 6–7 fill it
    pub fn reconcile(&mut self, ledger: &Ledger) -> Result<()>;                   // Task 8
    pub fn poll_timeout(&self, now: Instant) -> Option<Duration>;                 // Task 6
    pub fn kill_expired(&mut self, now: Instant);                                 // Task 6
    pub fn reap_checks(&mut self, ledger: &mut Ledger) -> Result<(), ReapFailure>; // Task 6
}
```

`process` logic (all appends via `Ledger::append_on(conn, now, …)`, actor `"campd"`; every action guarded by a state precondition it destroys):

1. `RunCooked` → warm the ctx cache via `runtime::load_run(camp_root.join("runs"), run_id)`. A load failure appends `run.finalized`? No — the run just cooked from that dir; a failure here is `CoreError` (fail fast).
2. `BeadCreated` with `run_id`+`step_id` where the bead IS the anchor of a looping step and `is_ready(conn, id)` → append `bead.claimed {session:"campd"}` on the anchor + `bead.created` for attempt 1 (`title "<step.title> (attempt 1)"`, same rig/run_id/step_id, `assignee` = step's, `actor "campd"`).
3. `BeadClosed`:
   a. outcome `pass` → `newly_ready(conn, bead)`; each newly-ready looping anchor gets the claim + attempt-1 pair from (2).
   b. bead is an **attempt** of a looping step whose anchor is `in_progress` and `claimed_by == "campd"`:
      - closed `pass`, step has `check` → push `PendingCheck { run_id, step_id, anchor, attempt_bead, attempt_no: check_runs_used(&attempts) }` (the just-closed attempt is included in the count — it is the Nth check run).
      - closed `pass`, step has `retry` → append anchor close `{outcome:"pass", output: <attempt close data.output if present>, reason:"attempt <n> passed"}`.
      - closed `fail` + `failure_class:"transient"` + step has `retry` + `transient_fails_used < retry.max_attempts` → append `bead.created` attempt n+1 (description = step description + `"\n\nattempt <n> failed transient: <reason>"`).
      - closed `fail` + transient + retry exhausted → append anchor close `{outcome:"fail", final_disposition: retry.on_exhausted.as_str(), reason:"retry budget (<max>) exhausted"}`.
      - closed `fail` otherwise (hard; or transient without retry — Decision 13) → anchor close `{outcome:"fail", final_disposition:"hard_fail", reason:"attempt <n> failed: <reason>"}`.
   c. bead is an **anchor** closed `pass` and its step has `on_complete` → push `PendingFanout { run_id, step_id, anchor }`.
   d. bead is a **bond-child root** (any label parses via `parse_bond_label`) closed with any outcome → push `PendingFanout` for the PARENT anchor (sequential chains advance; `execute` computes whether anything is due — Task 7).
   e. bead belongs to a run (`run_id` set) → run `runtime::finalization(conn, ctx)`; on `Finalize{..}` append, in this same transaction: `bead.closed {outcome:"skipped", reason:"needs cannot be satisfied"}` for each open skipped anchor, then root `bead.closed {outcome, reason}` (**outcome only — Decision 3**), then `run.finalized {run_id, root, outcome, final_disposition, cause_seq: event.seq, soft_failed, skipped}` (the disposition lives here, validated against `CAMP_RUN_DISPOSITIONS`).
   Context loads for (3) come from the cache, loading on miss; a run dir that fails to load appends nothing and records `None` (evented once via `dispatch.failed`-style? No bead fits) — **ruling applied:** a missing/corrupt run dir closes the ROOT `{outcome:"fail", reason:"run dir unreadable: <err>"}` + `run.finalized {…, final_disposition:"hard_fail"}`, if the root is still open; that is the honest mechanical dead-end (nothing else can ever advance the run).

`orders.rs` changes: `CampdProcessor` gains `pub graph: &'a mut GraphRuntime`; its `process` calls `self.graph.process(conn, &self.clock.now_utc(), event)?` after the existing four stages; `settle(...)` gains the `graph: &mut GraphRuntime` parameter and passes it through. `event_loop.rs`: thread `graph` through `run`/`settle`/`serve_connection`/`drain_lines`/`reap_and_refill`; in `settle`, call `graph.execute(ledger)?` between `orders::settle` and `dispatcher.converge` (11a/11b only in this task).

- [ ] **Step 1: Failing tests** in `dispatch.rs` `mod tests` (ledger-only, no processes — drive `orders::settle` with a `GraphRuntime` the way `settle_all` does in orders.rs tests; formulas cooked via `camp_core::formula::{parse_and_validate, cook}` into a tempdir `runs/`):

```rust
#[test] fn a_ready_looping_anchor_is_claimed_with_attempt_one() { /* cook retry-fetch fixture;
    settle; anchor in_progress claimed_by "campd"; exactly one attempt bead, dispatchable */ }
#[test] fn retry_classification_pass_hard_transient() { /* three scenarios closing attempt 1:
    pass -> anchor closed pass (output copied);
    fail hard -> anchor fail/hard_fail;
    fail transient (budget 3) -> attempt 2 exists, anchor still in_progress */ }
#[test] fn retry_exhaustion_honors_on_exhausted() { /* max_attempts 2: two transient fails ->
    soft_fail formula: anchor fail + final_disposition soft_fail;
    hard_fail formula: hard_fail; assert reason names the budget */ }
#[test] fn finalization_closes_root_with_cause_and_skips_unreachable() { /* two-step run,
    step2 needs step1; close step1 fail -> step2 closed skipped, root closed fail
    (outcome only — no disposition on the close), run.finalized final_disposition
    "hard_fail" and cause_seq == the step1 close seq */ }
#[test] fn a_passing_check_step_attempt_queues_a_check_not_an_anchor_close() { /* guarded-change-like
    fixture; close attempt pass; anchor still in_progress; pending_checks len 1 */ }
#[test] fn graph_appends_are_idempotent_across_resettles() { /* settle twice more; event count stable */ }
```

- [ ] **Step 2:** run — FAIL. **Step 3:** implement. **Step 4:** `cargo test -p camp` PASS (existing daemon tests updated for the new `settle` parameter — mechanical). **Step 5:** Commit `feat(daemon): graph runtime — looping-step anchors, retry classification, run finalization (cursor-atomic)`.

### Task 6: Check execution (spawn, deadline, reap)

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs`, `crates/camp/src/daemon/event_loop.rs` (Decision 11c/11d/11e)

**Interfaces produced:** `GraphRuntime::{execute (check half), poll_timeout, kill_expired, reap_checks}` per Task 5's signatures.

Implementation:
- `execute` (check half): drain `pending_checks`; skip any whose `(run_id, step_id, attempt_no)` already has a live child (defensive dedupe). Resolve ctx → `check.path` relative to the RIG path (`config` is not in `GraphRuntime` — resolve the rig path from the anchor bead's rig via the manifest's `rig` + a `rig_paths: HashMap<String, PathBuf>` snapshot passed at construction from `CampConfig`; hot-reload nicety deferred: paths re-snapshot at campd start, same as `Dispatcher`'s config). Spawn `Command::new(path)` with `cwd = rig path`, env `CAMP_DIR`, `CAMP_BEAD` (anchor), `CAMP_RUN_ID`, `CAMP_STEP_ID`, `CAMP_ATTEMPT`, stdout+stderr appended to `runs/<run_id>/checks/<step>-attempt-<n>.log` (dir created). Deadline = `Instant::now() + min(check.timeout, step.timeout)` per Decision 12. Spawn failure → Decision 10: `append_batch`: `check.failed {…, error}` + anchor close fail/hard_fail. On error mid-drain, requeue survivors and surface (the Phase 10 `settle` requeue pattern).
- `poll_timeout(now)`: earliest child deadline − now, `Duration::ZERO` floor, `None` when no deadlines; event_loop combines: `let timeout = min_deadline(runtime.poll_timeout(ts_now), graph.poll_timeout(Instant::now()));` — `fn min_deadline(a: Option<Duration>, b: Option<Duration>) -> Option<Duration>` in event_loop.rs, documented as THE poll-timeout composition point so phase-11's stall timers join the same combinator at rebase (Decision 11c, review note 1).
- `kill_expired(now)`: children past deadline get `child.kill()` + `timed_out = true`; called once per wake right after the fires computation (11d).
- `reap_checks(ledger)`: `try_wait` sweep mirroring `Dispatcher::reap`'s durable-then-forget discipline (same `ReapFailure` type — retryable ledger errors keep the child mapped; a `try_wait` OS error is non-retryable). Verdict batches (one `append_batch` each — atomic):
  - success && !timed_out → `check.passed {run_id, step_id, attempt}` (bead = attempt) + anchor close `{outcome:"pass", output: <passing attempt's data.output>, reason:"check passed on attempt <n>"}`.
  - failure/timeout → `check.failed {run_id, step_id, attempt, exit_code|signal, timed_out?, log}` + if `attempt_no < check.max_attempts`: `bead.created` attempt n+1 (description carries `"check failed (attempt <n>): exit <code>; log: <path>"` + last ~2 KB of the log — mechanical evidence copy); else anchor close `{outcome:"fail", final_disposition:"hard_fail", reason:"check budget (<max>) exhausted"}`.
  - `reap_and_refill` calls `graph.reap_checks(ledger)?` before `dispatcher.reap(ledger)?` (11e); both feed the same settle.

- [ ] **Step 1: Failing tests** (dispatch.rs `mod tests`; scripts are tempdir shell scripts; use `spawn_probe_guard` around child spawns as the existing tests do):

```rust
#[test] fn a_passing_check_closes_the_anchor_with_output() { /* pending check on a fixture run;
    script `exit 0`; execute + wait child + reap_checks; check.passed present;
    anchor closed pass; output copied from the attempt close */ }
#[test] fn a_failing_check_with_budget_creates_the_next_attempt_with_evidence() { /* script exit 1;
    check.failed carries exit_code 1 and the log path; attempt 2 exists; its description
    contains "check failed (attempt 1)" */ }
#[test] fn check_budget_exhaustion_fails_the_anchor() { /* max_attempts 1; exit 1 ->
    anchor fail/hard_fail, reason names the budget */ }
#[test] fn an_expired_check_is_killed_and_counts_as_a_failed_iteration() { /* script sleeps 60;
    check.timeout 50ms; poll_timeout Some(<=50ms); kill_expired then reap ->
    check.failed timed_out:true */ }
#[test] fn a_missing_check_script_hard_fails_the_anchor_without_burning_budget() { /* Decision 10;
    exactly one check.failed with error, anchor fail, no second attempt */ }
```

- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** `cargo test -p camp` PASS. **Step 5:** Commit `feat(daemon): check scripts as reaped children — verdict batches, deadlines in the poll timeout`.

### Task 7: `on_complete` fan-out execution

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` (`GraphRuntime::execute`, fan-out half)

Implementation (`execute`, after checks): drain `pending_fanouts` (dedupe by anchor). For each: load ctx + the anchor's close event (`events_for_bead`) → `data.output`; `resolve_for_each`; enumerate existing children (`SELECT` root beads whose labels parse to `(anchor, i)` — via `list_beads`-style query on `run_id IS NOT NULL AND step_id IS NULL` then label-parse in Rust); then:
- parallel → cook every missing index now;
- sequential → cook index 0 if no children; else if child `i` (highest existing) closed `pass` and `i+1 < items.len()` → cook `i+1`; a non-pass child halts (no append — the child's `run.finalized` and the needs edge tell the story).
Each cook: load `formulas/<bond>.toml`, `parse_and_validate`, `substitute_vars(step.on_complete.vars, item, index)`, `cook_with` using `CookOptions { vars, extra_root_labels: vec![bond_label(anchor, index)], extra_root_needs: previous child root for sequential (empty for parallel/index 0) }`, actor `"campd"`, rig = the parent run's rig, run dir = `<camp>/runs`. A malformed output/for_each/vars or missing bond formula is a **fan-out failure**: append root-of-parent-run… the parent run may already be finalized; instead the failure lands on the ANCHOR's run as `run.finalized`? Both wrong — pin: fan-out failure appends `order.failed`? No order exists. **Pinned rule:** a fan-out failure appends `dispatch.failed {reason}` on the ANCHOR bead (the fold accepts any known bead; the reason names for_each/vars/bond and the error) and drops the fan-out — evented, never silent, never fatal to campd. Cook events from successful children re-enter the settle loop and dispatch in the same wake.

- [ ] **Step 1: Failing tests** (dispatch.rs tests, ledger-only — fake formulas `formulas/child.toml` in the camp tempdir):

```rust
#[test] fn parallel_fanout_cooks_every_item_with_substituted_vars() { /* 3-item output;
    3 run.cooked; child roots labeled bond:<anchor>:0..2; child step titles carry item fields */ }
#[test] fn sequential_fanout_cooks_lazily_and_chains_on_pass() { /* item 0 cooked alone;
    close child-0 steps+root pass via settle -> child 1 cooked with root needs child-0 root;
    close child-1 root FAIL -> no child 2 ever */ }
#[test] fn a_bad_for_each_path_events_a_dispatch_failed_on_the_anchor() { /* output lacks the path;
    dispatch.failed reason names "output.items" */ }
#[test] fn fanout_is_idempotent_across_resettles() { /* re-execute; still 3 children */ }
```

- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** `cargo test -p camp` PASS. **Step 5:** Commit `feat(daemon): on_complete fan-out — parallel and lazily-chained sequential bond cooking`.

### Task 8: Startup reconciliation + daemon wiring

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` (`GraphRuntime::reconcile`), `crates/camp/src/daemon/mod.rs`

`reconcile(ledger)` (observation over state, mirrors `unresponded_fires`):
1. **Checks due:** anchors `in_progress` with `claimed_by = "campd"` (one indexed beads query) whose step has `check` and whose latest attempt closed `pass` → queue `PendingCheck` (an interrupted check re-runs — checks are re-runnable by contract).
2. **Attempts due:** same query, step has `retry`, latest attempt closed fail-transient with budget left but no successor attempt → this is impossible-by-construction (cursor-atomic) but reconcile re-queues anyway if found (defense costs one query; a hand-edited ledger heals).
3. **Fan-outs due:** every run root (open or closed) whose formula has an `on_complete` step with a closed-pass anchor and incomplete/halted-pending children → queue `PendingFanout`. Bounded by total run count (startup-only).
`daemon/mod.rs`: construct `let mut graph = dispatch::GraphRuntime::new(camp.root.clone(), &config);` next to the Dispatcher; call `graph.reconcile(&ledger)?` right after the `unresponded_fires` loop; pass `&mut graph` through both startup settles and `event_loop::run`.

Orphan run dirs (review note 4, documented — no sweep built): a `kill -9` between cook's run-dir write and its `append_batch` commit leaves `runs/<id>/` with no ledger record; reconciliation and fire-dedupe then re-cook under a NEW id, so recovery is idempotent but the orphan dir lingers. This is the crash window cook.rs's header already records as the safe direction (files-first), with the sweep explicitly deferred to a future `camp doctor` check. This task adds a sentence to `GraphRuntime::reconcile`'s doc comment naming the window and pointing at cook.rs's note, and builds nothing — consistent with that recorded decision.

- [ ] **Step 1: Failing test** (dispatch.rs tests): build a ledger where an anchor is campd-claimed with its only attempt closed pass and no verdict (simulated kill -9 after the cursor advanced); `reconcile` queues exactly one `PendingCheck`; running it again queues nothing new after the verdict lands. Second test: a closed-pass on_complete anchor with 1 of 3 children cooked → reconcile queues the fan-out.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** `cargo test -p camp` PASS. **Step 5:** Commit `feat(daemon): startup graph reconciliation — kill -9 self-heals checks and fan-outs`.

### Task 9: Issue #17 per-WAKE fire budget

**Files:**
- Modify: `crates/camp/src/daemon/orders.rs`, `crates/camp/src/daemon/event_loop.rs` (the Decision 11f reset line — added to the seam list per review Blocker B)

**Interfaces produced:** `OrdersRuntime::reset_fire_budget()` (public; clears the per-order counter map and per-order suppression-evented flags); `const FIRE_BUDGET: usize = 256` in orders.rs.

`OrdersRuntime` gains `fires_this_wake: HashMap<String, usize>`. It is cleared ONLY by `reset_fire_budget()`, which `event_loop::settle` calls once at entry — NOT per `orders::settle` call, because `event_loop::settle` loops `orders::settle` with `dispatcher.converge` between iterations and #17's through-converge scenario regenerates one fire per outer iteration (Decision 8). The event-trigger loop in `CampdProcessor::process` increments per order; at `FIRE_BUDGET` it appends `order.failed {order, fired_seq: event.seq, error: "event-trigger fire budget (256) exhausted in one wake — likely a self-triggering order"}` exactly once per wake and suppresses that order's fires until the next reset. Startup call sites need no extra lines — `daemon::run` reaches settle only through `event_loop::settle`, which resets at entry. Module docs reference issue #17 and name both scenarios.

- [ ] **Step 1: Failing test (in-settle feedback, orders.rs tests):** an order on `event:bead.created` whose formula's cook creates beads (any formula does — the cook's own `bead.created` events re-match) must NOT hang the settle: drive it through `event_loop::settle`-equivalent behavior by calling `orders::settle` after a manual `reset_fire_budget()`; assert it returns, exactly one budget `order.failed` for the order, and the event count is bounded (< `FIRE_BUDGET` × (steps + 2) + slack).
- [ ] **Step 2:** confirm the test HANGS or unboundedly appends without the fix (run with a timeout; this is the red state — document the observed behavior in the commit).
- [ ] **Step 3: Failing test (through-converge regeneration, event_loop.rs tests — the review's Blocker B trace):** a camp whose config has `[[order]] on = "event:dispatch.failed"` cooking a one-step formula, with a ROUTING HOLE (no step assignee, no rig or `[dispatch]` default_agent): seed one ready task bead (converge's `prepare` fails routing and appends `dispatch.failed` — no process ever spawns, so the test is process-free), then call `event_loop::settle` directly (it is `pub(super)`). Each outer iteration regenerates one fire through converge; assert: `event_loop::settle` RETURNS; exactly one budget `order.failed`; total events < `FIRE_BUDGET` × 8 + slack; the campd cursor equals the ledger head (true quiescence). Then assert the drip stops across wakes: a second `event_loop::settle` call (fresh budget) appends nothing new — suppressed matches advanced behind the cursor and never re-fire, and each cooked run's step bead already carries its `dispatch.failed` (the Phase 8 `failed` set holds it for this campd lifetime).
- [ ] **Step 4:** confirm test 3's red state with a per-`orders::settle`-call reset (the rejected scoping): the through-converge test never terminates (bounded-timeout harness) — the reviewer's exact trace, pinned.
- [ ] **Step 5:** implement: `fires_this_wake` + `reset_fire_budget` on `OrdersRuntime`; the Decision 11f reset line at `event_loop::settle` entry; budget check in the event-trigger loop.
- [ ] **Step 6:** both tests PASS + full `cargo test -p camp`.
- [ ] **Step 7:** Commit `fix(daemon): per-wake event-order fire budget (closes #17 — in-settle feedback and through-converge regeneration both tested)`.

### Task 10: fake-agent.sh extensions

**Files:**
- Modify: `crates/camp/tests/fake-agent.sh`

New optional env (existing behavior unchanged when unset):
- `FAKE_AGENT_PLAN=<file>`: before closing, atomically pop the first line of the file (`head -1` + `tail -n +2 > tmp && mv`) and use it as the close spec. Line grammar: `pass`, `fail`, `fail-transient`, optionally followed by `output=<path-to-json>`. Empty/missing file → fall through to `FAKE_AGENT_OUTCOME`. (Attempts for one step are strictly sequential — the loop's next attempt exists only after the previous close — so the pop is race-free in these tests.)
- `FAKE_AGENT_OUTPUT_JSON=<path>`: pass `--output-json <path>` on the close (combines with either outcome source).
Close mapping: `fail-transient` → `--outcome fail --transient`.

- [ ] **Step 1:** shellcheck-by-eye + a unit exercise inside Task 11's first integration test (the script has no standalone test harness; its proof is the suite).
- [ ] **Step 2:** Commit `test: fake-agent close plans, transient closes, structured output`.

### Task 11: Integration suite — `crates/camp/tests/daemon_graph.rs`

**Files:**
- Create: `crates/camp/tests/daemon_graph.rs` (harness copied from `daemon_orders.rs`: `camp_cmd`, `init_camp`, `spawn_campd` readiness-line handshake, `stop_campd`, `events_json`, `wait_for` ledger polling — plus `[dispatch] command = <fake-agent.sh>` and `default_agent` in camp.toml, and a minimal `agents/dev.md`, exactly as `daemon_dispatch.rs` does today).

Every test ends with `run_ok(&root, &["doctor", "--refold"])` asserting the output contains `"0 drift rows"` — the master-plan exit criterion, asserted literally.

- [ ] **Test 1 — diamond runs to completion:** write `formulas/diamond4.toml` (4 plain steps, diamond needs, no assignees), `camp sling --formula diamond4`; wait for `run.finalized`; assert: 4 step closes + root close all `pass`; `session.woke` for `implement`/`document` appear only after `design`'s close (ledger seq ordering — **this is also the dispatch-latency functional assertion**: the dependent's dispatch is observed in the wake of the close, no wall-clock number); `run.finalized.final_disposition == "pass"`; refold clean.
- [ ] **Test 2 — check loop passes on the 2nd iteration:** one-step formula with `[steps.check] max_attempts = 3`, script `verify.sh` in the rig: first run writes a marker and exits 1, second run exits 0. `FAKE_AGENT_PLAN` = `pass\npass`. Assert event sequence: attempt-1 close pass → `check.failed {attempt:1, exit_code:1}` → attempt-2 created+dispatched → `check.passed {attempt:2}` → anchor pass → root pass → finalized; two attempt beads exactly; refold clean.
- [ ] **Test 3 — check budget exhaustion fails the run:** same shape, `max_attempts = 2`, script always exits 1. Assert two `check.failed`, anchor `fail`/`hard_fail` reason naming the budget, root fail, `run.finalized.outcome == "fail"`; refold clean.
- [ ] **Test 4 — transient retry exhaustion, hard vs soft table:** `retry max_attempts = 2`; anchor dispositions are asserted on the anchor CLOSE data, run dispositions on `run.finalized` (root closes carry outcome only — Decision 3). (a) `on_exhausted = "hard_fail"`, plan `fail-transient\nfail-transient`: anchor close fail with `final_disposition:"hard_fail"`; root close outcome fail; `run.finalized {outcome:"fail", final_disposition:"hard_fail"}`. (b) `on_exhausted = "soft_fail"` in a two-step formula whose second step is independent and passes: anchor close fail/`soft_fail`; root close outcome **pass**; `run.finalized {outcome:"pass", final_disposition:"soft_fail", soft_failed:["fetch"]}`. (c) soft_fail with a dependent step: dependent closed `skipped`; root close outcome fail; `run.finalized {outcome:"fail", final_disposition:"soft_fail"}`. Refold clean after each.
- [ ] **Test 5 — on_complete fans out 3 bonds:** parent step's agent closes with `FAKE_AGENT_OUTPUT_JSON` pointing at `{"items":[{"name":"a"},{"name":"b"},{"name":"c"}]}`; bond formula `child.toml` has title `"Handle {name}"`, vars `name = "{item.name}"`. (a) parallel: 3 `run.cooked` in the same wake, all complete, child step titles `Handle a/b/c`; (b) `sequential = true`: assert child i+1's `run.cooked` seq > child i's root-close seq, and child roots chain via needs; refold clean.
- [ ] **Test 6 — kill -9 between attempt close and check verdict self-heals:** run Test-2's formula with a `FAKE_AGENT_HOLD_DIR` gate; let attempt 1 close pass, `kill -9` campd before the check completes (SIGKILL the campd child directly), restart campd; assert the check runs and the run completes; exactly one `check.passed` for the final iteration; refold clean.
- [ ] For each test: write it, watch it fail against the unimplemented/partial daemon behavior where applicable, make it pass, commit individually (`test(daemon): …`).

### Task 12: Gates, PR, report

- [ ] `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` — all green locally.
- [ ] Push `phase-9-graph-execution`; open the PR with the exit-criteria evidence table (every §8.2 construct → its test; refold assertions; CI link). `gh pr checks --watch` until the five checks are green.
- [ ] Report to the lead: PR number, CI status, master-plan exit criteria quoted line by line with evidence.

## Self-Review Notes

- **Spec coverage:** §8.2 constructs — steps/needs (Test 1), check (Tests 2/3), retry (Test 4), on_complete (Test 5), assignee routing (attempt beads carry it; Test 4 uses default routing) — each maps to a task. §8.3's dispatcher sentence maps 1:1 onto Tasks 5–8. §13.3 cause chain: `run.finalized.cause_seq` + per-event `actor`.
- **Master-plan test list:** diamond ✓(T1), check-2nd-iteration ✓(T2), check exhaustion ✓(T3), transient hard/soft table ✓(T4), 3-item fan-out parallel+sequential ✓(T5), dispatch-latency functional ✓(inside T1), doctor --refold after every run ✓(all).
- **Type consistency:** `GraphRuntime` signatures pinned in Task 5 and consumed verbatim by Tasks 6–8; `RunContext`/`runtime::*` pinned in Task 2 and consumed by 5–8; `CookOptions` pinned in Task 3 and consumed by 7.
- **No placeholders:** every fold payload, event shape, CLI flag, and test scenario is spelled out above; code blocks are the implementation contract, not sketches.

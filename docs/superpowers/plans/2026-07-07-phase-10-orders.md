# Gas Camp Phase 10 — Orders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Plan approved by Opus 4.8 plan review, 2026-07-07 (automated plan gate per operator directive).** Binding rulings: Decision B ACCEPTED; Decision C ACCEPTED (factor `append_batch`'s per-input body into a shared private `insert_and_fold`); Decision L REJECTED — positional parameters, no `LoopCtx`, Poke-arm swap kept to one line, `CONFIG_WATCH = Token(1)` with connection tokens from 2 (Phase 8 allocates its SIGCHLD token around it); Decision E — ADOPTED by the operator (relayed 2026-07-07): the spec-text edit lands in this PR, spec-first, as the only spec edit authorized in flight. Reviewer corrections applied: Task 10.6's expected cursor values (`process_past_cursor` drains same-transaction appends in the same call: `end == 2`, `cursor == 2`) and Task 10.10's fixpoint wording (appended events drain in the SAME `catch_up` pass).

**Goal:** Spec §9: cron- and event-triggered formulas driven by a timer heap (a timer, never a tick). campd sleeps until the earliest cron deadline (idle heap = infinite poll wait), event orders evaluate on the same post-commit processing path as readiness (zero standing cost), wall-clock jumps recompute deadlines with a bounded catch-up policy, camp.toml is watched via `notify` and hot-reloads with a `config.changed` event, `camp order ls` / `camp order run <name>` expose the surface, and the optional launchd agent ships as an example plist with the honest away-mode limits documented.

**Architecture:** camp-core gains `orders/{mod,parse,cron}.rs`: a self-contained 5-field cron engine on jiff (parse + timezone-aware `next_after`), the `CronHeap` (min-heap of next fire times with the catch-up-window policy), `[[order]]` config compilation with errors that name the order and the field, and the fire pipeline. Every fire — cron, event, or manual — is one durable `order.fired` event; campd's event processor reacts to `order.fired` by cooking the formula (Phase 5's `cook`), so away-mode, event triggers, and `camp order run` are literally one code path. Completion is mechanical: when the root bead of an order-cooked run closes, the processor appends `order.completed`/`order.failed` in the same transaction as the cursor advance. The camp binary gains `daemon/orders.rs` (runtime state: compiled orders, heap, reload) and extends the Phase 7 event loop: `poll_timeout` becomes the heap's next deadline, a notify→mio self-pipe wakes the loop on camp.toml edits, and each wake compares expected vs actual wall time to recompute after jumps.

**Tech Stack:** Rust (edition 2024), jiff 0.2 (already a camp-core dependency — `Timestamp`, `civil`, `tz::TimeZone`, `SignedDuration`), mio (+ `os-ext` feature for the self-pipe), notify 8 (new, camp bin only), rusqlite, serde/serde_json, clap, anyhow (bin) / thiserror (core). No async runtime, no cron crate (Decision A).

## Global Constraints

Copied from AGENTS.md, the master plan, and the operator's standing rules. Every task's requirements implicitly include this section.

- **Spec is authoritative:** `docs/design/2026-07-05-gas-camp-design.md`; its §4 decision record is settled. If implementation reality contradicts the spec, stop and update the spec via PR in the same change (this plan carries one such edit — Decision E; spec edits are serialized through the lead).
- **Master plan contract:** `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md`, section "Phase 10 — Orders (`phase-10-orders`)". Files, interfaces, semantics, test list, and exit criteria are binding. The two documented interface refinements (Decisions B and C) were ACCEPTED at the 2026-07-07 plan review.
- **Invariant 1 — the soul of this phase:** the cron machinery is a timer heap, never a tick. `CronHeap::next_deadline()` is the poll timeout; an empty heap means `None` = infinite wait; no timeout-driven code path exists that isn't an armed timer. No sleeps, no polling loops, anywhere (test harnesses excepted, per Phase 7 precedent).
- **Event orders add no standing cost:** the pattern match runs once per committed event on the same post-commit processing path as readiness (spec §7.3/§9), inside the campd cursor's exactly-once transaction machinery.
- **Every campd action is an event with its cause (spec §13.3):** `order.fired` carries its trigger (`cron` + scheduled time, `event` + cause seq, or `manual`); cook events carry actor `order:<name>:<fired-seq>`; completion events carry the order, the run, and the outcome.
- **Respect merged interfaces:** extend, don't rework. Phase 7's daemon files are merged surface; Phase 5 merged 2026-07-07 (e6a043f) — `crates/camp-core/src/formula/**` and the fixture corpus are consumed, never touched. New event payloads use `#[serde(deny_unknown_fields)]` structs; keep the one-transaction event+state property, the vocab-pin partition tests, and the refold property test green.
- **Vocabulary mirror (spec §15.2):** `order.fired`/`order.completed`/`order.failed` are gc-mirrored (verified present in `tests/fixtures/gc-vocab.json`); `config.changed` is camp-specific (verified absent from the gc pin).
- **Fail fast:** no silent fallbacks, no silenced errors, no panics in library code (`clippy::unwrap_used`/`expect_used`/`panic` denied, `unsafe_code` forbidden). A broken order surfaces as an `order.failed` event in the ledger — evented, never swallowed — and never takes campd down (mirrors Phase 7 Decision H).
- **TDD, strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything.
- **Git:** never commit to main; branch `phase-10-orders`; no co-author lines, no self-mention. Conventional-commit style.
- **Gates before push:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- **Shared-file protocol (siblings phase-6-gc-compat-ci and phase-8-dispatch-workers in flight):** edits to `crates/camp/src/main.rs`, `crates/camp-core/src/{event,vocab,config}.rs`, `crates/camp-core/src/ledger/fold.rs`, both `Cargo.toml`s, and `Cargo.lock` stay minimal and additive. Phase 8 shares the daemon area: per the lead's 2026-07-07 instruction, `event_loop.rs` edits stay additive and minimal — no restructuring — and any change beyond plugging the heap deadline into the poll-timeout seam is flagged to the lead first (Decision L is such a change and is so flagged). When the lead reports a sibling merge: rebase onto current main, resolve, re-run all gates before continuing. Never open/update a PR from a branch not rebased on current main.
- **Nothing is complete until pushed, CI green (`gh pr checks --watch`), and every claim in the PR description verified.**

## Key Paths and Conventions

- Worktree: `/Users/kiener/code/gascamp/.claude/worktrees/agent-aa66a84fa5cefee93`, branch `phase-10-orders` (already created).
- camp-core new: `src/orders/{mod,parse,cron}.rs`. camp-core modified: `src/lib.rs` (one `pub mod orders;` line), `src/event.rs`, `src/vocab.rs`, `src/error.rs` (one variant), `src/config.rs` (one field + one test fix), `src/ledger/{fold,mod}.rs`.
- camp (bin) new: `src/cmd/order.rs`, `src/daemon/orders.rs`, `tests/{cli_order,daemon_orders}.rs`. camp modified: `src/main.rs`, `src/daemon/{mod,event_loop}.rs`, `Cargo.toml` (+ notify, + jiff, mio `os-ext`).
- New top-level content: `contrib/launchd/{com.gascamp.campd.plist.example,README.md}`; one-paragraph README pointer; spec §7.1/§9 edit (Decision E).
- Actor conventions: daemon-appended order events use `actor = "campd"`; CLI-appended use `actor = "cli"` (existing convention); cook events for an order-fired run use `actor = "order:<name>:<fired-seq>"` (Decision J — the cause chain from a run back to its firing, in the mold of the spec §7.2 `session:8f3c2e01` convention).
- Timestamps in event data (`scheduled_ts`) use the canonical spec §7.2 form: RFC3339 UTC whole seconds, `%Y-%m-%dT%H:%M:%SZ` — same as `clock.rs`.
- Integration tests drive the real binary via `env!("CARGO_BIN_EXE_camp")` (Phase 7 harness conventions: `camp_cmd`, `init_camp`, readiness line, `connect_with_retry`-style waits are test-harness-only).
- Phase-5 interfaces consumed: `camp_core::formula::{parse_and_validate, cook, CookedRun}` and the `run.cooked` event — merged main as of e6a043f; the branch was rebased onto it and all gates re-run at plan time.

## Plan-Time Decision Log

Decisions made while writing this plan, with the 2026-07-07 plan-review rulings folded in. **B and C (contract refinements) are ACCEPTED; L's original form is REJECTED (positional-parameter fallback adopted below); E's code convention proceeds with the spec-text edit ON HOLD pending the operator.** The rest are implementation choices inside the contract.

- **A. Cron engine in-house on jiff — no cron crate.** External cron crates (`cron`, `croner`) pull chrono as a second time library next to jiff and don't produce errors that name the cron field. The 5-field engine (minute, hour, day-of-month, month, day-of-week; `*`, lists, ranges, steps; numeric values only — no month/day names in v1, rejected with a clear error) is ~200 lines with an exhaustive table test. Vixie-cron semantics where defined: when **both** day-of-month and day-of-week are restricted, a day matches if **either** matches (the classic OR rule); `7` in day-of-week normalizes to `0` (Sunday).
- **B. `fire_due` returns `Vec<Fire>`, not the master plan's `Vec<&Order>` (operator sign-off).** The `order.fired` event must record the *scheduled* fire time and whether the fire is a late catch-up — an `&Order` alone cannot carry that. `pub struct Fire { pub order: String, pub scheduled: Timestamp, pub catch_up: bool }` preserves the pinned shape's intent (the caller looks the `Order` up by name in O(1)); `recompute → Vec<CatchUp>` stays exactly as pinned. Same information, one added struct, no lost capability.
- **C. Completion tracking is stateless and event-sourced (operator sign-off on the one new ledger API).** `order.completed`/`order.failed` fire when the **root bead of an order-cooked run closes** — a signal that exists today (the test's fake agent closes the root) and stays correct when Phase 9's finalization starts closing roots mechanically. Detection needs no new state table: PR #10's beads rows carry `run_id`/`step_id` (root = `run_id` set, `step_id` null), and the run's `run.cooked` event (indexed by bead) carries the `order:<name>:<fired-seq>` actor. The completion event is appended **inside the processor's transaction** so it commits atomically with the cursor advance — exactly-once by construction, kill -9 safe. That requires one additive camp-core API: `Ledger::append_on(conn, ts, input)` — the existing single-write path factored so a cursor-transaction processor can use it (this is also the mechanism the Phase 7 cursor comment promises Phase 8: "Ledger writes must go through conn").
- **D. One fire pipeline for cron, event, and manual triggers.** A fire is *declared* by appending `order.fired` (cron/catch-up fires from the event loop, event-trigger fires from inside the processor transaction — atomic with the match, manual fires from the CLI). The *cook* is driven by campd **processing** the `order.fired` event: the processor queues `PendingCook{order, fired_seq}`, and the daemon's settle loop (catch-up ⇄ cook, run to fixpoint) executes it — resolve formula → `parse_and_validate` → `cook` with actor `order:<name>:<fired-seq>`. Consequences, all deliberate: `camp order run` appends `order.fired` + pokes, and campd cooks it — manual fire is *literally* the away-mode path; a kill -9 between `order.fired` and the cook self-heals at the next start via reconciliation (`unresponded_fires`: `order.fired` events with no `run.cooked` response actor and no `order.failed` with that `fired_seq` are cooked late — observation over state, in the Phase 11 adopt spirit); cook execution dedupes by checking for an existing response before cooking, so replays never double-cook.
- **E. Formula-name resolution: `<camp>/formulas/<name>.toml` (spec edit, serialize via lead).** Spec §9 orders reference formulas by name (`formula = "triage-inbox"`); Phase 5's `parse_and_validate` takes a path; packs (the eventual layered source, spec §11: "last-wins with local definitions highest") are Phase 12. The camp-local `formulas/` directory is the v1 resolution root and the natural "local definitions" layer for Phase 12 to build under. The spec's §7.1 layout block gains one line (`formulas/`) and §9 one sentence, in this PR. Per the orchestration guide, concurrent spec edits are forbidden — the lead is told at plan approval so this edit can be sequenced.
- **F. Catch-up policy, one rule everywhere.** *(Amended 2026-07-07, PR #13 review — see the post-review amendments section.)* A fire whose lateness (`now − scheduled`) is ≤ `ON_TIME_TOLERANCE` (60 s — one cron granule) is a normal fire. Later than that it is a *missed* fire: it fires once with `catch_up: true` iff the order's window is enabled (> 0) and lateness ≤ window; otherwise it is skipped and the order is rescheduled from `now`. `fire_due` applies this rule to entries that come due while running (covers platforms where the poll timeout keeps counting through a sleep); `recompute(now, last_seen)` applies it after detected jumps, scanning only `(max(last_seen, now − window − tolerance), now]` so the scan is bounded by the window, and returning the **most recent** missed fire per order ("fire once on wake"). At daemon start, `last_seen` = the ledger's last event timestamp *read before appending `campd.started`* — so fires missed while campd was down (powered-off laptop, spec §9) catch up under the same window. Backward jumps need no catch-up (heap deadlines are wall-clock instants and remain valid); `recompute` guards `now ≤ last_seen` by rescheduling only.
- **G. Wall-clock jump detection.** Each wake records `Instant` (monotonic) and `Timestamp` (wall) before and after `poll`; if `|wall_delta − mono_delta| > JUMP_TOLERANCE` (30 s), the wake runs `recompute(now, last_seen)`, else `fire_due(now)`. On platforms where the monotonic clock and poll timeout tick through system sleep, no "jump" is visible — and none is needed: the poll wakes on time and `fire_due` sees the late deadlines, applying the same window rule (Decision F), so both paths converge on identical behavior. `last_seen` updates at every wake. The honest limits stay as spec §9 states them: no wake source, no fire — the launchd README documents this.
- **H. notify → mio self-pipe.** `notify::recommended_watcher`'s callback (its own thread) writes one byte into a `mio::unix::pipe` Sender registered in the poll as `CONFIG_WATCH = Token(1)` (connection tokens start at 2). A full pipe (`WouldBlock`) is fine — the signal coalesces. The watcher watches the **camp root directory** non-recursively (editors rename-replace; a file watch dies with the inode) and the callback filters for paths named `camp.toml`. On wake the loop drains the pipe and calls `reload_if_changed`, which compares raw file bytes against the last applied text: identical → no-op (kills editor double-events without debounce timers); changed and valid → swap config + orders + rebuilt heap (armed at `now`), emit `config.changed {applied: true, orders: N}`; changed and invalid → keep the old config, emit `config.changed {applied: false, error}` (the file *did* change; whether campd applied it is data — spec §13.4 "config changes are themselves events" holds either way, and the error is durable, not just a log line). campd startup with an invalid camp.toml is a hard error (declared automation must parse — fail fast); the reload path is lenient-but-evented only because a running daemon must survive a mid-edit torn write.
- **I. Event-order recursion is documented, not guarded.** An order triggering on an event type its own firing produces (`event:order.fired`, or `event:bead.created` matching cooked beads) recurses; the settle fixpoint would loop, appending events each cycle — visible in the ledger, user-declared, exactly as a `* * * * *` cron on an expensive formula is. campd executes declared structure (spec §8.3); no heuristic cycle-breaker. The hazard is documented in `orders/mod.rs` docs and the contrib README.
- **J. Order names are pinned to `^[a-z0-9][a-z0-9_-]*$`** so the `order:<name>:<fired-seq>` actor encoding parses unambiguously (split on the last `:` for the seq). Validation error names the order and the field.
- **K. Order-level failures are evented and survivable; infrastructure failures are fatal.** `execute_fire` returns `Ok(None)` after appending `order.failed {order, fired_seq, error}` for order-level failures (formula missing/invalid, rig unresolvable, cook error, order removed by a reload before its cook ran) — the daemon logs and continues. Only a failure to *record* the failure (ledger append error) propagates. Timer-path settle errors mirror Phase 7 Decision H: stderr + continue; the cursor holds position and the error resurfaces on the next poke.
- **L. `event_loop::run` gains three positional parameters (plan-review ruling: the `LoopCtx` bundling proposed here was REJECTED — Phase 8 coordination).** The signature becomes `run(listener, socket_path, ledger, processor, runtime: &mut OrdersRuntime, clock: &dyn Clock, config_rx: &mut mio::unix::pipe::Receiver)` — the existing `ledger`/`processor` parameters untouched, three appended. The Poke arm's change is the minimal one-line swap (`cursor::catch_up(...)` → `orders::settle(...)`); `CONFIG_WATCH = Token(1)`, connection tokens start at 2 (the lead is pointing Phase 8's SIGCHLD token around these). No other restructuring of Phase 7 code.
- **M. Rig resolution at fire time mirrors `cmd/create`'s rule** (explicit name, else the sole configured rig, else a hard error naming the fix) but is implemented in `orders/mod.rs` — `cmd/create.rs` is not refactored to share it, deliberately, to keep sibling-phase conflict surface at zero. Flagged as a post-v1 DRY cleanup for the lead.
- **N. Cron next-fire search horizon is 6 years** (`366 × 6` days): covers the worst legal gap (`0 0 29 2 *` ≈ 4 years around a leap cycle; the 2100 century gap is out of v1's service life and documented). An expression with no fire inside the horizon is rejected when the heap arms it (daemon start / reload / `order ls` shows `never`) — a dead order is config junk, fail fast. DST: candidate civil times resolve through jiff's `Disambiguation::Compatible` — nonexistent times (spring-forward gap) shift forward by the gap length and fire once; ambiguous times (fall-back fold) fire at the first occurrence only; a candidate resolving to an instant ≤ `after` (possible in the fold's second pass) is skipped, keeping `next_after` strictly monotonic.

## Post-review amendments (2026-07-07, PR #13 Opus review — all eight findings addressed on the branch)

1. **MEDIUM 1 (amends Decision N's implementation):** `first_fire_on`'s civil-time cut lost the gap-shifted fire when `next_after` was queried from inside a spring-forward gap (an earlier civil time resolves to a LATER instant there). The cut is removed; every candidate resolves to a timestamp and the `ts > after` check is the single authority. Test: `queried_from_inside_the_gap_still_finds_the_shifted_fire` (proven red pre-fix with the reviewer's exact reproduction).
2. **MEDIUM 2 (amends Decision F):** the downtime catch-up anchor is now the ts of the event at campd's CURSOR position (`daemon::orders::catch_up_anchor`) — the last instant campd demonstrably observed — not the ledger's last event of any actor, which let a daemon-less CLI write mask a missed fire in violation of spec §9 (authority order: spec > master plan > this plan). campd's own processed fires still advance the anchor, so nothing refires across restarts; a fresh camp (cursor 0) anchors at `now`. `Ledger::last_event_ts` is superseded and removed. Tests: `downtime_catch_up_survives_an_intervening_cli_write` (includes the masked-anchor counterexample), `catch_up_anchor_is_now_for_an_unprocessed_ledger`.
3. **MEDIUM 3 (amends the settle contract in Task 10.10):** an infrastructure error mid-cook-list now requeues the failing cook and every unexecuted one before surfacing — the cursor is already past their `order.fired` events, so dropped cooks would otherwise wait for a restart's reconciliation. Test: `an_infra_error_mid_cook_list_requeues_the_survivors` (injects a SQL trigger aborting `order.failed` inserts via a second connection; asserts survival and drain-after-recovery).
4. **MEDIUM 4 (amends the Decision L token note):** the authoritative campd token layout is 0 = listener, 1 = config watch, **2 = Phase 8's SIGCHLD self-pipe (reserved)**, connections from 3.
5. **LOW 5:** `fire_response_exists` now runs two targeted existence probes bounded by the `events_type` index (`Ledger::has_event_with_actor`, `Ledger::has_event_with_data_i64`) instead of materializing every response event per cook; the set-based scan remains startup-only for `unresponded_fires`.
6. **LOW 6 (amends Decision F's `fire_due` description):** `fire_due`'s missed branch selects via the same `most_recent_missed` as `recompute` — an oversleep and a detected jump now choose the identical fire with identical `scheduled_ts`. Test: `oversleep_and_jump_agree_on_the_most_recent_missed_fire`.
7. **LOW 7 (amends Task 10.4):** `catch_up_window` is capped at `MAX_CATCH_UP_WINDOW` (7 days); beyond it `compile_orders` rejects naming the order and the field — the window bounds the synchronous missed-fire scan on every startup/jump recompute.
8. **LOW 8 (amends Decision H):** watcher failures are durable: the notify callback stores the error in the runtime's slot (`on_watch_event`) and wakes the loop, which appends `config.changed {applied:false, error:"camp.toml watch error (hot reload degraded): …"}` — invariant 5/spec §13.4, never stderr-only. Tests: `a_watcher_error_becomes_a_rejected_config_changed_event`, `on_watch_event_signals_the_pipe_and_filters_paths`.

## What later phases rely on (interfaces Phase 10 produces)

- **Phase 11 (patrol):** stall timers join the same poll-timeout mechanism — `OrdersRuntime::poll_timeout` is the model: the loop takes `min` of armed deadline sources, and patrol state arrives as further `run` parameters.
- **Phase 12 (packs):** formula resolution goes through one function, `orders::formula_path(camp_root, name)` — pack layering replaces its body, local `formulas/` stays the highest layer. `orders.toml` pack content compiles through the same `compile_orders`.
- **Phase 14 (export bridge):** `[[order]]` tables and `order.*` events are the "city-order declarations" input (spec §15.3).
- **Phase 8 (parallel):** `Ledger::append_on(conn, ts, input)` is the conn-scoped write path the Phase 7 cursor comment promised dispatch; dispatch state arrives as further `run` parameters, and its SIGCHLD token is allocated around `CONFIG_WATCH = Token(1)`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/camp-core/src/orders/cron.rs` (new) | `CronExpr` (parse + `next_after`), `CronHeap`, `Fire`, `CatchUp`, tolerances |
| `crates/camp-core/src/orders/parse.rs` (new) | `OrderConfig` (raw `[[order]]` table), `compile_orders`, trigger/window parsing, name/field-scoped errors |
| `crates/camp-core/src/orders/mod.rs` (new) | `Order`, `Trigger`, `FireCause`, `fired_input`, actor encoding, `event_trigger_matches`, `completion_input`, `execute_fire`, `unresponded_fires`, `formula_path` |
| `crates/camp-core/src/lib.rs` (mod) | `pub mod orders;` |
| `crates/camp-core/src/error.rs` (mod) | `CoreError::Order { order, reason }` |
| `crates/camp-core/src/config.rs` (mod) | `orders: Vec<OrderConfig>` field (`[[order]]`), one struct-literal test fix |
| `crates/camp-core/src/event.rs` (mod) | `OrderFired`, `OrderCompleted`, `OrderFailed`, `ConfigChanged` variants |
| `crates/camp-core/src/vocab.rs` (mod) | three names into `GC_MIRRORED_EVENTS`, `config.changed` into `CAMP_SPECIFIC_EVENTS` |
| `crates/camp-core/src/ledger/fold.rs` (mod) | four log-only arms with validated `deny_unknown_fields` payloads |
| `crates/camp-core/src/ledger/mod.rs` (mod) | `append_on` (factored from `append_batch`), `last_event_ts`, `events_of_type` |
| `crates/camp/src/cmd/order.rs` (new) | `camp order ls [--json]`, `camp order run <name>` |
| `crates/camp/src/main.rs` (mod) | `Order` subcommand wiring |
| `crates/camp/src/daemon/orders.rs` (new) | `OrdersRuntime` (build/reload/poll_timeout/fire_due/recompute), `CampdProcessor`, `settle`, startup reconciliation |
| `crates/camp/src/daemon/event_loop.rs` (mod) | heap deadline → poll timeout, jump detection, fire appends, `CONFIG_WATCH` pipe (three positional params added to `run`) |
| `crates/camp/src/daemon/mod.rs` (mod) | startup order: last-seen read → runtime build → watcher → settle → reconcile → recompute → loop |
| `crates/camp/Cargo.toml` (mod) | + `notify`, + `jiff`, mio `os-ext` |
| `crates/camp/tests/cli_order.rs` (new) | order ls/run against the real binary |
| `crates/camp/tests/daemon_orders.rs` (new) | away-mode cron fire, manual-fire full cycle, event order, hot reload |
| `contrib/launchd/com.gascamp.campd.plist.example` (new) | fire-at-login agent, example only |
| `contrib/launchd/README.md` (new) | install one-liner + honest away-mode limits |
| `README.md` (mod) | one-paragraph orders/launchd pointer |
| `docs/design/2026-07-05-gas-camp-design.md` (mod) | §7.1 `formulas/` line; §9 resolution sentence (Decision E) |

## Watch items

- **PR #10 (phase-5) merged 2026-07-07 (e6a043f)** — the cook provider is on main; this branch is rebased onto it with gates green. Task 10.8's gate step remains as a freshness check only.
- **Phase 8 (dispatch/workers) and Phase 6 (gc-compat CI) run parallel in this window.** Phase 8 shares `event_loop.rs`/`daemon/mod.rs`: keep those diffs surgical and additive, no restructuring, flag anything beyond the poll-timeout seam to the lead (Decision L is flagged); expect a real rebase. Phase 6 owns `ci/gc-compat/**` and `.github/workflows/ci.yml` — this phase touches neither.
- **jiff API details** (`SignedDuration::from_str` friendly format, `tz.to_ambiguous_zoned(...).compatible()`, `Timestamp` subtraction) are asserted by the Task 10.1–10.3 tests first — if any signature differs from this plan, fix the call sites to the real API in that task; the *semantics* pinned here are the contract, and the tests encode semantics, not signatures.
- **DST test dates** assume the standard US 2026 calendar: spring forward Sun 2026-03-08 02:00→03:00, fall back Sun 2026-11-01 02:00→01:00, in `America/New_York` (available on macOS and ubuntu CI via the system tzdb).
- **The cron integration test waits up to ~75 s** for a `* * * * *` minute boundary — the honest away-mode demonstration. It is one test; the suite absorbs it.
- On sibling-merge rebases, the conflict surface is: `event.rs`/`vocab.rs`/`fold.rs` (additive arms), `config.rs` (one field), `main.rs` (one subcommand), `Cargo.toml`/`Cargo.lock`. PR #10 also renames nothing in the files this phase touches; its `vocab.rs`/`fold.rs`/`event.rs` additions (`run.cooked`) merge additively next to ours.

---

### Task 10.0: Commit this plan

- [ ] **Step 1:** `git add docs/superpowers/plans/2026-07-07-phase-10-orders.md && git commit -m "docs: phase 10 execution plan"` on branch `phase-10-orders`. Push (`git push -u origin phase-10-orders`) so the lead/operator can read it. **STOP here until plan approval comes back (master plan decision 10).**

---

### Task 10.1: camp-core — `CronExpr::parse`

**Files:**
- Create: `crates/camp-core/src/orders/cron.rs`, `crates/camp-core/src/orders/mod.rs` (module shell), modify `crates/camp-core/src/lib.rs`
- Test: `cron.rs` `#[cfg(test)]` module

**Interfaces:**
- Consumes: nothing from the repo (pure).
- Produces: `CronExpr::parse(expr: &str) -> Result<CronExpr, String>` (the `String` names the cron field and the offense; `compile_orders` wraps it with the order name in Task 10.4). `CronExpr` is `Debug + Clone + PartialEq`, exposes `pub fn source(&self) -> &str`.

- [ ] **Step 1: Module shell.** `orders/mod.rs` starts as:

```rust
//! Orders (spec §9): cron- and event-triggered formulas. The cron machinery
//! is a timer heap, never a tick (invariant 1). Grows over Phase 10 tasks.
pub mod cron;
```

and `lib.rs` gains `pub mod orders;` (alphabetical placement among the existing `pub mod` lines).

- [ ] **Step 2: Write the failing parse tests** in `cron.rs`:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_section_9_example() {
        // "0 7 * * 1-5" — weekday mornings at 07:00
        let expr = CronExpr::parse("0 7 * * 1-5").unwrap();
        assert_eq!(expr.source(), "0 7 * * 1-5");
    }

    #[test]
    fn accepts_lists_ranges_steps_and_wildcards() {
        for ok in [
            "* * * * *",
            "*/15 * * * *",
            "0,30 8-17 * * *",
            "5 0 1,15 1-6/2 *",
            "0 0 * * 7", // 7 == Sunday, normalized to 0
            "59 23 31 12 6",
        ] {
            CronExpr::parse(ok).unwrap_or_else(|e| panic!("{ok:?} rejected: {e}"));
        }
    }

    #[test]
    fn seven_normalizes_to_sunday() {
        assert_eq!(
            CronExpr::parse("0 0 * * 7").unwrap(),
            CronExpr::parse("0 0 * * 0").unwrap().with_source("0 0 * * 7")
        );
    }

    #[test]
    fn rejects_with_the_field_named() {
        for (expr, field) in [
            ("0 7 * *", "expected 5 fields"),          // arity
            ("0 7 * * 1-5 9", "expected 5 fields"),
            ("60 * * * *", "minute"),
            ("* 24 * * *", "hour"),
            ("* * 0 * *", "day of month"),
            ("* * 32 * *", "day of month"),
            ("* * * 13 *", "month"),
            ("* * * 0 *", "month"),
            ("* * * * 8", "day of week"),
            ("* * * * MON", "day of week"),            // names rejected in v1
            ("*/0 * * * *", "minute"),                 // zero step
            ("5-1 * * * *", "minute"),                 // inverted range
            ("1,,2 * * * *", "minute"),                // empty list item
            ("", "expected 5 fields"),
        ] {
            let err = CronExpr::parse(expr).unwrap_err();
            assert!(err.contains(field), "{expr:?}: error {err:?} must name {field:?}");
        }
    }
}
```

(`with_source` is a `#[cfg(test)]`-only helper returning a clone with the source string replaced, so the `7→0` equivalence can be asserted with `PartialEq`.)

- [ ] **Step 3: Run to verify failure.** `cargo test -p camp-core orders::cron` — expected: compile error (`CronExpr` undefined).

- [ ] **Step 4: Implement.**

```rust
//! Cron expressions (5-field, numeric) and the timer heap (spec §9).
//! Vixie semantics where defined: day-of-month OR day-of-week when both
//! are restricted; `7` == Sunday. Values are numeric only in v1 — names
//! (`MON`, `JAN`) are rejected with an error naming the field.

/// One parsed field: which values are allowed, over `min..=max`.
#[derive(Debug, Clone, PartialEq)]
struct FieldSet {
    min: u8,
    allowed: Vec<bool>, // index = value - min
    restricted: bool,   // false when the field was `*` or `*/1`
}

impl FieldSet {
    fn contains(&self, value: u8) -> bool {
        usize::from(value.wrapping_sub(self.min)) < self.allowed.len()
            && self.allowed[usize::from(value - self.min)]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CronExpr {
    source: String,
    minutes: FieldSet,       // 0-59
    hours: FieldSet,         // 0-23
    days_of_month: FieldSet, // 1-31
    months: FieldSet,        // 1-12
    days_of_week: FieldSet,  // 0-6, 0 = Sunday (7 normalized on parse)
}

impl CronExpr {
    /// Parse a 5-field cron expression. The error string names the field
    /// ("minute", "hour", "day of month", "month", "day of week") and the
    /// offending item; callers add the order context.
    pub fn parse(expr: &str) -> Result<CronExpr, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "expected 5 fields (minute hour day-of-month month day-of-week), got {}",
                fields.len()
            ));
        }
        Ok(CronExpr {
            source: expr.to_owned(),
            minutes: parse_field(fields[0], "minute", 0, 59, false)?,
            hours: parse_field(fields[1], "hour", 0, 23, false)?,
            days_of_month: parse_field(fields[2], "day of month", 1, 31, false)?,
            months: parse_field(fields[3], "month", 1, 12, false)?,
            days_of_week: parse_field(fields[4], "day of week", 0, 7, true)?,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Parse one field: comma-separated items of `*[/step]` or `a[-b][/step]`.
/// `wrap_seven`: day-of-week accepts 7 as an alias for 0 (Sunday).
fn parse_field(
    text: &str,
    name: &str,
    min: u8,
    max: u8,
    wrap_seven: bool,
) -> Result<FieldSet, String> {
    let size = usize::from(max - min) + 1;
    // day-of-week: allowed[] is indexed 0-6 even though input max is 7
    let store = if wrap_seven { size - 1 } else { size };
    let mut set = FieldSet { min, allowed: vec![false; store], restricted: text != "*" };
    for item in text.split(',') {
        if item.is_empty() {
            return Err(format!("{name}: empty list item in {text:?}"));
        }
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => {
                let step: u8 = s
                    .parse()
                    .map_err(|_| format!("{name}: bad step {s:?} in {item:?}"))?;
                if step == 0 {
                    return Err(format!("{name}: step 0 in {item:?}"));
                }
                (r, step)
            }
            None => (item, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else {
            match range.split_once('-') {
                Some((a, b)) => (parse_value(a, name, min, max)?, parse_value(b, name, min, max)?),
                None => {
                    let v = parse_value(range, name, min, max)?;
                    (v, v)
                }
            }
        };
        if lo > hi {
            return Err(format!("{name}: inverted range {range:?}"));
        }
        let mut v = lo;
        loop {
            let normalized = if wrap_seven && v == 7 { 0 } else { v };
            set.allowed[usize::from(normalized - min)] = true;
            match v.checked_add(step) {
                Some(next) if next <= hi => v = next,
                _ => break,
            }
        }
    }
    if wrap_seven {
        // contains() is asked about 0-6 only
    }
    Ok(set)
}

fn parse_value(text: &str, name: &str, min: u8, max: u8) -> Result<u8, String> {
    let v: u8 = text
        .parse()
        .map_err(|_| format!("{name}: {text:?} is not a number (names are not supported)"))?;
    if v < min || v > max {
        return Err(format!("{name}: value {v} out of range {min}-{max}"));
    }
    Ok(v)
}
```

Note the `FieldSet.restricted` flag: `*` (and only `*`) leaves a field unrestricted; `*/n` **is** restricted (matters for the DOM/DOW OR rule in Task 10.2). Adjust `restricted` accordingly: set it `true` unless the whole field text is exactly `"*"`.

- [ ] **Step 5: Run to verify pass.** `cargo test -p camp-core orders::cron` — all green.
- [ ] **Step 6: Commit.** `git commit -m "feat(orders): 5-field cron parser with field-named errors"`

---

### Task 10.2: camp-core — `CronExpr::next_after` (DST-correct next fire)

**Files:**
- Modify: `crates/camp-core/src/orders/cron.rs`

**Interfaces:**
- Produces: `pub fn next_after(&self, after: jiff::Timestamp, tz: &jiff::tz::TimeZone) -> Option<jiff::Timestamp>` — the earliest instant strictly after `after` matching the expression in `tz`, or `None` if none exists within the 6-year horizon (Decision N).

- [ ] **Step 1: Write the failing next-fire table** (append to the tests module):

```rust
    use jiff::tz::TimeZone;
    use jiff::Timestamp;

    fn ny() -> TimeZone {
        TimeZone::get("America/New_York").unwrap()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn next(expr: &str, after: &str, tz: &TimeZone) -> Option<String> {
        CronExpr::parse(expr)
            .unwrap()
            .next_after(ts(after), tz)
            .map(|t| t.to_string())
    }

    #[test]
    fn next_fire_table_utc() {
        let utc = TimeZone::UTC;
        for (expr, after, expect) in [
            // strictly after: an exact hit advances to the next match
            ("0 7 * * *", "2026-07-06T07:00:00Z", "2026-07-07T07:00:00Z"),
            ("0 7 * * *", "2026-07-06T06:59:59Z", "2026-07-06T07:00:00Z"),
            // weekday constraint: Fri 2026-07-10 19:00 → Mon 2026-07-13 07:00
            ("0 7 * * 1-5", "2026-07-10T19:00:00Z", "2026-07-13T07:00:00Z"),
            // dom/dow OR rule (both restricted): the 15th OR a Monday
            ("0 0 15 * 1", "2026-07-10T00:00:00Z", "2026-07-13T00:00:00Z"),
            ("0 0 15 * 1", "2026-07-13T00:00:00Z", "2026-07-15T00:00:00Z"),
            // month-end: only months with a 31st
            ("0 0 31 * *", "2026-01-31T00:00:01Z", "2026-03-31T00:00:00Z"),
            // leap day: next Feb 29 after 2026 is 2028
            ("0 0 29 2 *", "2026-03-01T00:00:00Z", "2028-02-29T00:00:00Z"),
            // steps
            ("*/15 * * * *", "2026-07-06T07:41:00Z", "2026-07-06T07:45:00Z"),
        ] {
            assert_eq!(
                next(expr, after, &utc).as_deref(),
                Some(expect),
                "{expr} after {after}"
            );
        }
    }

    #[test]
    fn spring_forward_gap_fires_once_shifted_compatible() {
        // 2026-03-08 02:30 EST does not exist (02:00→03:00). Compatible
        // disambiguation shifts forward by the gap: fires 03:30 EDT = 07:30Z.
        assert_eq!(
            next("30 2 * * *", "2026-03-08T05:00:00Z", &ny()).as_deref(), // 00:00 EST
            Some("2026-03-08T07:30:00Z")
        );
    }

    #[test]
    fn fall_back_fold_fires_first_occurrence_only() {
        // 2026-11-01 01:30 happens twice (EDT 05:30Z, then EST 06:30Z).
        // Compatible picks the earlier; the second pass is not a fire.
        assert_eq!(
            next("30 1 * * *", "2026-11-01T04:00:00Z", &ny()).as_deref(), // 00:00 EDT
            Some("2026-11-01T05:30:00Z")
        );
        // ...and from within the fold's second pass (01:45 EST = 06:45Z),
        // the next fire is the NEXT day — never an instant ≤ after.
        assert_eq!(
            next("30 1 * * *", "2026-11-01T06:45:00Z", &ny()).as_deref(),
            Some("2026-11-02T06:30:00Z")
        );
    }

    #[test]
    fn impossible_dates_return_none() {
        assert_eq!(next("0 0 30 2 *", "2026-01-01T00:00:00Z", &TimeZone::UTC), None);
    }
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p camp-core orders::cron` — compile error (`next_after` undefined).

- [ ] **Step 3: Implement.**

```rust
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::Timestamp;

/// How far `next_after` searches before declaring an expression dead
/// (Decision N): covers the worst legal gap, `0 0 29 2 *` across a leap
/// cycle. The year-2100 century gap is outside v1's service life.
const SEARCH_HORIZON_DAYS: i32 = 366 * 6;

impl CronExpr {
    /// The earliest instant strictly after `after` matching this expression
    /// in `tz`. Nonexistent civil times (DST gap) resolve forward and fire
    /// once; ambiguous ones (DST fold) fire at the first occurrence only
    /// (jiff `Disambiguation::Compatible`); any resolution ≤ `after` is
    /// skipped so the result is strictly monotonic. `None` = no fire within
    /// the search horizon.
    pub fn next_after(&self, after: Timestamp, tz: &TimeZone) -> Option<Timestamp> {
        let zoned_after = after.to_zoned(tz.clone());
        let start_date = zoned_after.date();
        let mut date = start_date;
        for _ in 0..SEARCH_HORIZON_DAYS {
            if self.day_matches(date) {
                for hour in 0..=23u8 {
                    if !self.hours.contains(hour) {
                        continue;
                    }
                    for minute in 0..=59u8 {
                        if !self.minutes.contains(minute) {
                            continue;
                        }
                        let Ok(time) = civil::Time::new(i8::try_from(hour).ok()?, i8::try_from(minute).ok()?, 0, 0) else {
                            continue; // unreachable by construction
                        };
                        let candidate = civil::DateTime::from_parts(date, time);
                        // Skip candidates at or before `after` on its own day
                        // cheaply, in civil terms first:
                        if date == start_date && candidate <= zoned_after.datetime() {
                            continue;
                        }
                        let Ok(zoned) = tz.to_ambiguous_zoned(candidate).compatible() else {
                            continue; // resolution failed: not a fire
                        };
                        let ts = zoned.timestamp();
                        if ts > after {
                            return Some(ts);
                        }
                        // fold second-pass edge: resolution ≤ after — keep going
                    }
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }

    /// Vixie day rule: month must match; if BOTH dom and dow are restricted,
    /// either may match; if one is restricted, it decides; if neither, all
    /// days match.
    fn day_matches(&self, date: civil::Date) -> bool {
        let month = u8::try_from(date.month()).unwrap_or(0);
        if !self.months.contains(month) {
            return false;
        }
        let dom = u8::try_from(date.day()).unwrap_or(0);
        // jiff: weekday().to_sunday_zero_offset() gives 0=Sunday..6=Saturday
        let dow = u8::try_from(date.weekday().to_sunday_zero_offset()).unwrap_or(0);
        match (self.days_of_month.restricted, self.days_of_week.restricted) {
            (true, true) => self.days_of_month.contains(dom) || self.days_of_week.contains(dow),
            (true, false) => self.days_of_month.contains(dom),
            (false, true) => self.days_of_week.contains(dow),
            (false, false) => true,
        }
    }
}
```

If a jiff signature differs (e.g. `to_sunday_zero_offset`'s exact name, `DateTime::from_parts`), adapt to the real API — the tests pin the semantics. No `unwrap`/`expect` outside `#[cfg(test)]`.

- [ ] **Step 4: Run to verify pass.** `cargo test -p camp-core orders::cron`
- [ ] **Step 5: Commit.** `git commit -m "feat(orders): timezone-aware cron next-fire with DST semantics"`

---

### Task 10.3: camp-core — `CronHeap` with the catch-up window

**Files:**
- Modify: `crates/camp-core/src/orders/cron.rs`, `crates/camp-core/src/orders/mod.rs` (introduce `Order`/`Trigger` — the heap stores them)

**Interfaces:**
- Consumes: `CronExpr` (10.1/10.2); `CoreError::Order` (added here, in `error.rs`).
- Produces (contract-pinned, with Decision B's refinement):

```rust
// orders/mod.rs
pub enum Trigger { Cron { expr: CronExpr }, Event { event_type: String, label: Option<String> } }
pub struct Order {
    pub name: String,
    pub trigger: Trigger,
    pub formula: String,
    pub rig: Option<String>,
    pub catch_up_window: std::time::Duration, // default 2h; ZERO disables
}
// orders/cron.rs
pub struct Fire { pub order: String, pub scheduled: Timestamp, pub catch_up: bool }
pub struct CatchUp { pub order: String, pub scheduled: Timestamp }
impl CronHeap {
    pub fn new(tz: TimeZone) -> Self;
    pub fn arm(&mut self, order: Order, now: Timestamp) -> Result<(), CoreError>;
    pub fn next_deadline(&self) -> Option<Timestamp>;
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<Fire>;
    pub fn recompute(&mut self, now: Timestamp, last_seen: Timestamp) -> Vec<CatchUp>;
}
pub const ON_TIME_TOLERANCE: SignedDuration = SignedDuration::from_secs(60);
```

- [ ] **Step 1: Add `Order`/`Trigger` to `orders/mod.rs`** (both `#[derive(Debug, Clone, PartialEq)]`), and to `error.rs`:

```rust
    #[error("order {order:?}: {reason}")]
    Order { order: String, reason: String },
```

- [ ] **Step 2: Write the failing heap tests** (in `cron.rs` tests; helper builds a cron `Order` with a given window):

```rust
    use std::time::Duration;
    use crate::orders::{Order, Trigger};

    fn cron_order(name: &str, expr: &str, window: Duration) -> Order {
        Order {
            name: name.into(),
            trigger: Trigger::Cron { expr: CronExpr::parse(expr).unwrap() },
            formula: "f".into(),
            rig: None,
            catch_up_window: window,
        }
    }

    const TWO_HOURS: Duration = Duration::from_secs(2 * 60 * 60);

    #[test]
    fn empty_heap_has_no_deadline() {
        assert_eq!(CronHeap::new(TimeZone::UTC).next_deadline(), None);
    }

    #[test]
    fn interleaved_schedules_order_the_heap() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let now = ts("2026-07-06T07:20:00Z");
        heap.arm(cron_order("hourly", "0 * * * *", TWO_HOURS), now).unwrap();
        heap.arm(cron_order("quarter", "*/15 * * * *", TWO_HOURS), now).unwrap();
        // quarter fires 07:30, hourly 08:00
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T07:30:00Z")));
        let fires = heap.fire_due(ts("2026-07-06T07:30:00Z"));
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].order, "quarter");
        assert_eq!(fires[0].scheduled, ts("2026-07-06T07:30:00Z"));
        assert!(!fires[0].catch_up);
        // quarter rescheduled to 07:45, still ahead of hourly
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T07:45:00Z")));
        // both due at 08:00 (07:45 quarter missed-by-15min → within window)
        let fires = heap.fire_due(ts("2026-07-06T08:00:00Z"));
        let names: Vec<&str> = fires.iter().map(|f| f.order.as_str()).collect();
        assert!(names.contains(&"quarter") && names.contains(&"hourly"));
    }

    #[test]
    fn fire_due_is_empty_before_the_deadline() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(cron_order("h", "0 * * * *", TWO_HOURS), ts("2026-07-06T07:20:00Z")).unwrap();
        assert!(heap.fire_due(ts("2026-07-06T07:59:59Z")).is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T08:00:00Z")));
    }

    #[test]
    fn late_fire_within_window_is_a_catch_up_fire() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(cron_order("h", "0 8 * * *", TWO_HOURS), ts("2026-07-06T07:00:00Z")).unwrap();
        // wakes 90 min late (poll timeout ticked through a sleep)
        let fires = heap.fire_due(ts("2026-07-06T09:30:00Z"));
        assert_eq!(fires.len(), 1);
        assert!(fires[0].catch_up);
        assert_eq!(fires[0].scheduled, ts("2026-07-06T08:00:00Z"));
        // rescheduled from now: next fire tomorrow 08:00
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-07T08:00:00Z")));
    }

    #[test]
    fn late_fire_outside_window_is_skipped_and_rescheduled() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(cron_order("h", "0 8 * * *", TWO_HOURS), ts("2026-07-06T07:00:00Z")).unwrap();
        assert!(heap.fire_due(ts("2026-07-06T10:00:01Z")).is_empty()); // 2h1s late
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-07T08:00:00Z")));
    }

    #[test]
    fn zero_window_disables_catch_up_but_not_on_time_fires() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(cron_order("h", "0 8 * * *", Duration::ZERO), ts("2026-07-06T07:00:00Z")).unwrap();
        // 30 s late is within ON_TIME_TOLERANCE: a normal fire
        let fires = heap.fire_due(ts("2026-07-06T08:00:30Z"));
        assert_eq!(fires.len(), 1);
        assert!(!fires[0].catch_up);
        // next day, 10 min late: beyond tolerance, window disabled → skip
        assert!(heap.fire_due(ts("2026-07-07T08:10:00Z")).is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-08T08:00:00Z")));
    }

    #[test]
    fn recompute_fires_once_with_the_most_recent_missed_fire() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let armed = ts("2026-07-06T06:30:00Z");
        heap.arm(cron_order("hourly", "0 * * * *", TWO_HOURS), armed).unwrap();
        // slept 06:30 → 09:30: missed 07:00, 08:00, 09:00; 08:00+09:00 in window
        let catch_ups = heap.recompute(ts("2026-07-06T09:30:00Z"), armed);
        assert_eq!(catch_ups.len(), 1);
        assert_eq!(catch_ups[0].order, "hourly");
        assert_eq!(catch_ups[0].scheduled, ts("2026-07-06T09:00:00Z")); // most recent
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T10:00:00Z")));
    }

    #[test]
    fn recompute_outside_window_and_zero_window_yield_no_catch_ups() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let armed = ts("2026-07-06T06:30:00Z");
        heap.arm(cron_order("daily", "0 7 * * *", TWO_HOURS), armed).unwrap();
        heap.arm(cron_order("zeroed", "0 8 * * *", Duration::ZERO), armed).unwrap();
        // woke at 12:00: 07:00 is 5h late (outside 2h), 08:00 window disabled
        let catch_ups = heap.recompute(ts("2026-07-06T12:00:00Z"), armed);
        assert!(catch_ups.is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-07T07:00:00Z")));
    }

    #[test]
    fn recompute_on_backward_jump_reschedules_without_catch_ups() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let armed = ts("2026-07-06T07:30:00Z");
        heap.arm(cron_order("h", "0 * * * *", TWO_HOURS), armed).unwrap();
        let catch_ups = heap.recompute(ts("2026-07-06T06:00:00Z"), armed); // clock set back
        assert!(catch_ups.is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T07:00:00Z")));
    }

    #[test]
    fn arming_a_never_firing_expression_is_an_error_naming_the_order() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let err = heap
            .arm(cron_order("dead", "0 0 30 2 *", TWO_HOURS), ts("2026-07-06T07:00:00Z"))
            .unwrap_err();
        assert!(err.to_string().contains("dead"), "{err}");
        assert!(err.to_string().contains("never fires"), "{err}");
    }

    #[test]
    fn arming_an_event_order_is_an_error() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let order = Order {
            name: "ev".into(),
            trigger: Trigger::Event { event_type: "bead.closed".into(), label: None },
            formula: "f".into(),
            rig: None,
            catch_up_window: TWO_HOURS,
        };
        assert!(heap.arm(order, ts("2026-07-06T07:00:00Z")).is_err());
    }
```

- [ ] **Step 3: Run to verify failure**, then **Step 4: Implement.**

```rust
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use jiff::SignedDuration;

use crate::error::CoreError;
use crate::orders::{Order, Trigger};

/// A fire within this much of its scheduled time is on-time (one cron
/// granule); later than this it is *missed* and the catch-up window rules
/// (spec §9) decide whether it fires late (once, flagged) or is skipped.
pub const ON_TIME_TOLERANCE: SignedDuration = SignedDuration::from_secs(60);

/// A due fire popped by `fire_due` (Decision B: name + scheduled instant +
/// catch-up flag; the caller resolves the name to its `Order`).
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    pub order: String,
    pub scheduled: Timestamp,
    pub catch_up: bool,
}

/// A missed-while-not-watching fire recovered by `recompute` — always a
/// catch-up (master-plan pinned shape).
#[derive(Debug, Clone, PartialEq)]
pub struct CatchUp {
    pub order: String,
    pub scheduled: Timestamp,
}

/// Min-heap of (next fire, order index). The earliest deadline is campd's
/// poll timeout; an empty heap means infinite wait (invariant 1).
pub struct CronHeap {
    tz: TimeZone,
    orders: Vec<Order>,
    entries: BinaryHeap<Reverse<(Timestamp, usize)>>,
}

impl CronHeap {
    pub fn new(tz: TimeZone) -> Self {
        CronHeap { tz, orders: Vec::new(), entries: BinaryHeap::new() }
    }

    /// Arm a cron order. A non-cron order or an expression with no fire
    /// inside the search horizon is an error naming the order (fail fast:
    /// dead automation is config junk).
    pub fn arm(&mut self, order: Order, now: Timestamp) -> Result<(), CoreError> {
        let Trigger::Cron { expr } = &order.trigger else {
            return Err(CoreError::Order {
                order: order.name.clone(),
                reason: "only cron orders arm the timer heap".into(),
            });
        };
        let next = expr.next_after(now, &self.tz).ok_or_else(|| CoreError::Order {
            order: order.name.clone(),
            reason: format!(
                "cron expression {:?} never fires within the {}-day search horizon",
                expr.source(),
                SEARCH_HORIZON_DAYS
            ),
        })?;
        let idx = self.orders.len();
        self.orders.push(order);
        self.entries.push(Reverse((next, idx)));
        Ok(())
    }

    /// The earliest armed deadline — campd's poll timeout source.
    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.entries.peek().map(|Reverse((t, _))| *t)
    }

    /// Pop everything due at `now`, reschedule each from `now`, and return
    /// the fires the caller must declare. Applies the catch-up rule
    /// (Decision F) so a poll that overslept a system sleep behaves exactly
    /// like a detected jump.
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<Fire> {
        let mut fires = Vec::new();
        while let Some(Reverse((deadline, idx))) = self.entries.peek().copied() {
            if deadline > now {
                break;
            }
            self.entries.pop();
            let order = &self.orders[idx];
            let lateness = now.duration_since(deadline);
            if lateness <= ON_TIME_TOLERANCE {
                fires.push(Fire { order: order.name.clone(), scheduled: deadline, catch_up: false });
            } else if window_allows(order.catch_up_window, lateness) {
                fires.push(Fire { order: order.name.clone(), scheduled: deadline, catch_up: true });
            } // else: missed outside the window — skip, reschedule only
            if let Trigger::Cron { expr } = &self.orders[idx].trigger {
                if let Some(next) = expr.next_after(now, &self.tz) {
                    self.entries.push(Reverse((next, idx)));
                }
                // None: the expression ran off the horizon after years of
                // service — the order goes quiet; `camp order ls` shows
                // "never". Documented, not silent-in-the-dark.
            }
        }
        fires
    }

    /// Wall-clock jump handling (spec §9): reschedule every order from
    /// `now` and return one catch-up per order whose most recent missed
    /// fire in `(last_seen, now]` is within its window. Scans only the
    /// window span, so a year-long gap costs window-sized work.
    pub fn recompute(&mut self, now: Timestamp, last_seen: Timestamp) -> Vec<CatchUp> {
        self.entries.clear();
        let mut catch_ups = Vec::new();
        for (idx, order) in self.orders.iter().enumerate() {
            let Trigger::Cron { expr } = &order.trigger else { continue };
            if now > last_seen {
                if let Ok(window) = SignedDuration::try_from(order.catch_up_window) {
                    if window > SignedDuration::ZERO {
                        // earliest instant that could still be in-window:
                        let floor = now
                            .checked_sub(window)
                            .and_then(|t| t.checked_sub(ON_TIME_TOLERANCE))
                            .unwrap_or(last_seen);
                        let mut cursor = if floor > last_seen { floor } else { last_seen };
                        let mut most_recent = None;
                        while let Some(fire) = expr.next_after(cursor, &self.tz) {
                            if fire > now {
                                break;
                            }
                            if window_allows(order.catch_up_window, now.duration_since(fire))
                                || now.duration_since(fire) <= ON_TIME_TOLERANCE
                            {
                                most_recent = Some(fire);
                            }
                            cursor = fire;
                        }
                        if let Some(scheduled) = most_recent {
                            catch_ups.push(CatchUp { order: order.name.clone(), scheduled });
                        }
                    }
                }
            }
            if let Some(next) = expr.next_after(now, &self.tz) {
                self.entries.push(Reverse((next, idx)));
            }
        }
        catch_ups
    }
}

fn window_allows(window: std::time::Duration, lateness: SignedDuration) -> bool {
    match SignedDuration::try_from(window) {
        Ok(w) => w > SignedDuration::ZERO && lateness <= w,
        Err(_) => false, // a window beyond SignedDuration range: treat as disabled
    }
}
```

(`Timestamp::duration_since` returns `SignedDuration` in jiff 0.2; if the real name differs — e.g. `now - deadline` via `Sub` — use it; tests pin semantics.)

- [ ] **Step 5: Run to verify pass.** `cargo test -p camp-core orders`
- [ ] **Step 6: Commit.** `git commit -m "feat(orders): cron timer heap with catch-up window semantics"`

---

### Task 10.4: camp-core — `[[order]]` config compilation

**Files:**
- Create: `crates/camp-core/src/orders/parse.rs`
- Modify: `crates/camp-core/src/orders/mod.rs` (`pub mod parse;`), `crates/camp-core/src/config.rs`

**Interfaces:**
- Consumes: `CampConfig` (Phase 3), `CronExpr::parse`, `Trigger`/`Order`, `EventType::parse`, `CoreError::Order`.
- Produces:

```rust
pub const DEFAULT_CATCH_UP_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderConfig { pub name: String, pub on: String, pub formula: String,
                         #[serde(default)] pub rig: Option<String>,
                         #[serde(default)] pub catch_up_window: Option<String> }
pub fn compile_orders(config: &CampConfig) -> Result<Vec<Order>, CoreError>;
```

and in `config.rs`, on `CampConfig`:

```rust
    #[serde(default, rename = "order", skip_serializing_if = "Vec::is_empty")]
    pub orders: Vec<crate::orders::parse::OrderConfig>,
```

(The existing `round_trips_through_toml` test constructs `CampConfig` literally — add `orders: vec![]`. That is the entire `config.rs` diff: shared-file minimalism.)

- [ ] **Step 1: Write the failing tests** (in `parse.rs`):

```rust
    // Parsing the spec §9 example verbatim:
    #[test]
    fn compiles_the_spec_section_9_example() {
        let cfg = CampConfig::parse(r#"
[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/code/gascity"
prefix = "gc"

[[order]]
name    = "morning-triage"
on      = "cron:0 7 * * 1-5"
formula = "triage-inbox"
rig     = "gascity"

[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "fix-ci"
"#).unwrap();
        let orders = compile_orders(&cfg).unwrap();
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].name, "morning-triage");
        assert!(matches!(&orders[0].trigger, Trigger::Cron { expr } if expr.source() == "0 7 * * 1-5"));
        assert_eq!(orders[0].rig.as_deref(), Some("gascity"));
        assert_eq!(orders[0].catch_up_window, DEFAULT_CATCH_UP_WINDOW);
        assert!(matches!(&orders[1].trigger,
            Trigger::Event { event_type, label }
            if event_type == "bead.closed" && label.as_deref() == Some("ci-red")));
    }

    #[test]
    fn window_parses_friendly_durations_and_zero_disables() {
        for (text, expect) in [
            ("30m", Duration::from_secs(30 * 60)),
            ("2h", Duration::from_secs(2 * 60 * 60)),
            ("0", Duration::ZERO),
        ] {
            let cfg = one_order_cfg(&format!(
                "name=\"n\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\ncatch_up_window=\"{text}\""
            ));
            assert_eq!(compile_orders(&cfg).unwrap()[0].catch_up_window, expect, "{text}");
        }
    }

    // Every semantic error names the order and the field:
    #[test]
    fn errors_name_the_order_and_the_field() {
        for (body, hits) in [
            ("name=\"x\"\non=\"daily\"\nformula=\"f\"", vec!["x", "on"]),
            ("name=\"x\"\non=\"cron:61 * * * *\"\nformula=\"f\"", vec!["x", "on", "minute"]),
            ("name=\"x\"\non=\"event:bogus.event\"\nformula=\"f\"", vec!["x", "on", "bogus.event"]),
            ("name=\"x\"\non=\"event:campd.started[label=y]\"\nformula=\"f\"", vec!["x", "on", "label"]),
            ("name=\"x\"\non=\"event:bead.closed[label=]\"\nformula=\"f\"", vec!["x", "on", "label"]),
            ("name=\"x\"\non=\"event:bead.closed[color=red]\"\nformula=\"f\"", vec!["x", "on"]),
            ("name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\ncatch_up_window=\"soon\"", vec!["x", "catch_up_window"]),
            ("name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\nrig=\"nope\"", vec!["x", "rig"]),
            ("name=\"Bad Name\"\non=\"cron:0 7 * * *\"\nformula=\"f\"", vec!["name"]),
            ("name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"\"", vec!["x", "formula"]),
        ] {
            let cfg = one_order_cfg(body);
            let err = compile_orders(&cfg).unwrap_err().to_string();
            for hit in hits {
                assert!(err.contains(hit), "error {err:?} must contain {hit:?}");
            }
        }
    }

    #[test]
    fn duplicate_order_names_are_rejected() {
        let cfg = CampConfig::parse(
            "[camp]\nname=\"d\"\n\
             [[order]]\nname=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\n\
             [[order]]\nname=\"x\"\non=\"cron:0 8 * * *\"\nformula=\"g\"\n",
        ).unwrap();
        assert!(compile_orders(&cfg).unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn unknown_order_table_key_is_rejected_at_toml_level() {
        // deny_unknown_fields: the toml error carries line/col — the
        // TOML-syntax layer of "parse errors name the order and the field".
        assert!(CampConfig::parse(
            "[camp]\nname=\"d\"\n[[order]]\nname=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\nbogus=1\n"
        ).is_err());
    }
```

`one_order_cfg(body)` is a test helper wrapping `[camp] name="d"` + one rig `gc` + `[[order]]\n{body}`.

- [ ] **Step 2: Run to verify failure**, then **Step 3: Implement.** Key logic:

```rust
const NAME_PATTERN_DOC: &str = "^[a-z0-9][a-z0-9_-]*$";

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn compile_orders(config: &CampConfig) -> Result<Vec<Order>, CoreError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut orders = Vec::with_capacity(config.orders.len());
    for raw in &config.orders {
        let field_err = |field: &str, reason: String| CoreError::Order {
            order: raw.name.clone(),
            reason: format!("field {field:?}: {reason}"),
        };
        if !valid_name(&raw.name) {
            return Err(CoreError::Order {
                order: raw.name.clone(),
                reason: format!("field \"name\": must match {NAME_PATTERN_DOC} (it becomes part of event actors)"),
            });
        }
        if !seen.insert(raw.name.clone()) {
            return Err(CoreError::Order {
                order: raw.name.clone(),
                reason: "duplicate order name".into(),
            });
        }
        if raw.formula.is_empty() {
            return Err(field_err("formula", "must not be empty".into()));
        }
        if let Some(rig) = &raw.rig {
            config.rig(rig).map_err(|_| field_err("rig", format!("unknown rig {rig:?}")))?;
        }
        let trigger = parse_trigger(&raw.on).map_err(|reason| field_err("on", reason))?;
        let catch_up_window = match raw.catch_up_window.as_deref() {
            None => DEFAULT_CATCH_UP_WINDOW,
            Some("0") => Duration::ZERO,
            Some(text) => parse_window(text).map_err(|reason| field_err("catch_up_window", reason))?,
        };
        orders.push(Order {
            name: raw.name.clone(),
            trigger,
            formula: raw.formula.clone(),
            rig: raw.rig.clone(),
            catch_up_window,
        });
    }
    Ok(orders)
}

fn parse_trigger(on: &str) -> Result<Trigger, String> {
    if let Some(expr) = on.strip_prefix("cron:") {
        return Ok(Trigger::Cron { expr: CronExpr::parse(expr)? });
    }
    if let Some(rest) = on.strip_prefix("event:") {
        let (event_type, label) = match rest.split_once('[') {
            None => (rest, None),
            Some((ty, bracket)) => {
                let inner = bracket
                    .strip_suffix(']')
                    .ok_or_else(|| format!("unterminated filter in {rest:?}"))?;
                let value = inner
                    .strip_prefix("label=")
                    .ok_or_else(|| format!("only [label=…] filters are supported, got {inner:?}"))?;
                if value.is_empty() {
                    return Err("label filter value must not be empty".into());
                }
                (ty, Some(value.to_owned()))
            }
        };
        crate::event::EventType::parse(event_type)
            .map_err(|_| format!("unknown event type {event_type:?}"))?;
        if label.is_some() && !event_type.starts_with("bead.") {
            return Err(format!(
                "label filters match beads; {event_type:?} is not a bead.* event"
            ));
        }
        return Ok(Trigger::Event { event_type: event_type.to_owned(), label });
    }
    Err(format!("expected \"cron:<expr>\" or \"event:<type>[label=<value>]\", got {on:?}"))
}

fn parse_window(text: &str) -> Result<Duration, String> {
    let signed: jiff::SignedDuration = text
        .parse()
        .map_err(|e| format!("{text:?} is not a duration ({e}); use forms like \"2h\", \"30m\", or \"0\" to disable"))?;
    if signed.is_negative() {
        return Err(format!("{text:?} is negative"));
    }
    Duration::try_from(signed).map_err(|e| format!("{text:?}: {e}"))
}
```

- [ ] **Step 4: Fix `config.rs`** (field + `orders: vec![]` in the round-trip test) and run `cargo test -p camp-core` — the whole core suite stays green.
- [ ] **Step 5: Commit.** `git commit -m "feat(orders): [[order]] config tables compile with order- and field-named errors"`

---

### Task 10.5: camp-core — order events, vocab, fold arms

**Files:**
- Modify: `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`
- Test: fold cases in `crates/camp-core/src/ledger/mod.rs` tests module; the existing `tests/vocab_pin.rs` partition tests enforce the vocab side automatically.

**Interfaces:**
- Produces: `EventType::{OrderFired, OrderCompleted, OrderFailed, ConfigChanged}` with names `order.fired`, `order.completed`, `order.failed`, `config.changed`. Payload contracts (each `#[serde(deny_unknown_fields)]`, all four events log-only — no state effect):
  - `order.fired` `{order, trigger: "cron"|"event"|"manual", scheduled_ts?, catch_up?, cause_seq?}` — `cron` requires `scheduled_ts` (and only `cron` may set `catch_up`); `event` requires `cause_seq`; `manual` requires none of them.
  - `order.completed` `{order, fired_seq, root_bead, run_id, outcome: "pass"}`.
  - `order.failed` — either the cook-failure shape `{order, fired_seq, error}` or the run-failure shape `{order, fired_seq, root_bead, run_id, outcome: "fail"}`; exactly one of `error`/`root_bead` present.
  - `config.changed` `{path, applied, orders?, error?}` — `applied: true` requires `orders` and forbids `error`; `applied: false` requires `error`.

- [ ] **Step 1: Write the failing tests** (in `ledger/mod.rs` tests, following the existing `campd_lifecycle_events_are_log_only` pattern and helpers):

```rust
    #[test]
    fn order_events_are_validated_and_log_only() {
        let (_dir, mut ledger) = temp_ledger();
        for data in [
            serde_json::json!({"order":"t","trigger":"cron","scheduled_ts":"2026-07-06T07:00:00Z"}),
            serde_json::json!({"order":"t","trigger":"cron","scheduled_ts":"2026-07-06T07:00:00Z","catch_up":true}),
            serde_json::json!({"order":"t","trigger":"event","cause_seq":4}),
            serde_json::json!({"order":"t","trigger":"manual"}),
        ] {
            ledger.append(input(EventType::OrderFired, None, None, data)).unwrap();
        }
        ledger.append(input(EventType::OrderCompleted, None, None,
            serde_json::json!({"order":"t","fired_seq":1,"root_bead":"gc-1","run_id":"r","outcome":"pass"}))).unwrap();
        ledger.append(input(EventType::OrderFailed, None, None,
            serde_json::json!({"order":"t","fired_seq":1,"error":"formula not found"}))).unwrap();
        ledger.append(input(EventType::OrderFailed, None, None,
            serde_json::json!({"order":"t","fired_seq":1,"root_bead":"gc-1","run_id":"r","outcome":"fail"}))).unwrap();
        ledger.append(input(EventType::ConfigChanged, None, None,
            serde_json::json!({"path":"camp.toml","applied":true,"orders":2}))).unwrap();
        ledger.append(input(EventType::ConfigChanged, None, None,
            serde_json::json!({"path":"camp.toml","applied":false,"error":"unknown field"}))).unwrap();
    }

    #[test]
    fn malformed_order_events_are_rejected() {
        let (_dir, mut ledger) = temp_ledger();
        for (kind, data) in [
            (EventType::OrderFired, serde_json::json!({"order":"t","trigger":"vibes"})),
            (EventType::OrderFired, serde_json::json!({"order":"t","trigger":"cron"})), // no scheduled_ts
            (EventType::OrderFired, serde_json::json!({"order":"t","trigger":"event"})), // no cause_seq
            (EventType::OrderFired, serde_json::json!({"order":"t","trigger":"manual","catch_up":true})),
            (EventType::OrderCompleted, serde_json::json!({"order":"t","fired_seq":1,"root_bead":"gc-1","run_id":"r","outcome":"fail"})),
            (EventType::OrderFailed, serde_json::json!({"order":"t","fired_seq":1})), // neither shape
            (EventType::OrderFailed, serde_json::json!({"order":"t","fired_seq":1,"error":"e","root_bead":"gc-1"})), // both
            (EventType::ConfigChanged, serde_json::json!({"path":"p","applied":true,"error":"e"})),
            (EventType::ConfigChanged, serde_json::json!({"path":"p","applied":false})),
            (EventType::OrderFired, serde_json::json!({"order":"t","trigger":"manual","bogus":1})),
        ] {
            assert!(ledger.append(input(kind, None, None, data.clone())).is_err(), "{kind:?} {data}");
        }
    }
```

- [ ] **Step 2: Run to verify failure** (compile error on the new variants).
- [ ] **Step 3: Implement.** `event.rs`: add the four variants to the enum, `ALL`, `as_str` (names above). `vocab.rs`: `GC_MIRRORED_EVENTS` += `"order.fired"`, `"order.completed"`, `"order.failed"`; `CAMP_SPECIFIC_EVENTS` += `"config.changed"`. `fold.rs`: four arms in `apply`, each a validation-only function in the mold of Phase 5's `run_cooked`, e.g.:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderFired {
    order: String,
    trigger: String,
    #[serde(default)]
    scheduled_ts: Option<String>,
    #[serde(default)]
    catch_up: Option<bool>,
    #[serde(default)]
    cause_seq: Option<i64>,
}

/// `order.fired` is log-only: the durable declaration that a trigger
/// tripped (spec §9). campd cooks the formula in response to *processing*
/// this event, so a fire is never lost to a crash (plan Decision D).
fn order_fired(event: &Event) -> Result<(), CoreError> {
    let p: OrderFired = payload(event)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    if p.order.is_empty() {
        return Err(bad("empty order".into()));
    }
    match p.trigger.as_str() {
        "cron" => {
            if p.scheduled_ts.is_none() {
                return Err(bad("cron trigger requires scheduled_ts".into()));
            }
            if p.cause_seq.is_some() {
                return Err(bad("cron trigger does not carry cause_seq".into()));
            }
        }
        "event" => {
            if p.cause_seq.is_none() {
                return Err(bad("event trigger requires cause_seq".into()));
            }
            if p.scheduled_ts.is_some() || p.catch_up.is_some() {
                return Err(bad("event trigger carries only cause_seq".into()));
            }
        }
        "manual" => {
            if p.scheduled_ts.is_some() || p.catch_up.is_some() || p.cause_seq.is_some() {
                return Err(bad("manual trigger carries no schedule data".into()));
            }
        }
        other => return Err(bad(format!("unknown trigger {other:?}"))),
    }
    Ok(())
}
```

(`order_completed`, `order_failed`, `config_changed` analogous, enforcing the rules from the Interfaces block.)

- [ ] **Step 4: Run the full core suite** — `cargo test -p camp-core`. The `vocab_pin` partition test passes exactly when both vocab lists are updated; the refold property test stays green (log-only events add no state).
- [ ] **Step 5: Commit.** `git commit -m "feat(orders): order.fired/completed/failed (gc-mirrored) and config.changed events"`

---

### Task 10.6: camp-core — ledger plumbing (`append_on`, `last_event_ts`, `events_of_type`)

**Files:**
- Modify: `crates/camp-core/src/ledger/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn append_on(conn: &Connection, ts: &str, input: EventInput) -> Result<Seq, CoreError>` (associated fn on `Ledger`, no `self`): the single write path (insert event row + fold) executed on a caller-provided connection — **must** run inside a transaction the caller commits; documented for exactly that use (`process_past_cursor` processors — Decision C; also the API the Phase 7 cursor comment promised Phase 8).
  - `pub fn last_event_ts(&self) -> Result<Option<String>, CoreError>` — the `ts` of the highest-seq event (startup `last_seen`, Decision F).
  - `pub fn events_of_type(&self, kind: EventType) -> Result<Vec<Event>, CoreError>` — via the existing `events_type` index (fire reconciliation, Decision D).

- [ ] **Step 1: Write the failing tests** (ledger tests module):

```rust
    #[test]
    fn append_on_writes_through_a_processor_transaction_atomically() {
        let (_dir, mut ledger) = temp_ledger();
        ledger.append(input(EventType::CampdStarted, None, None, serde_json::json!({}))).unwrap();
        // A processor that appends a config.changed for every event it sees:
        let end = ledger.process_past_cursor("t", &mut |conn, event| {
            if event.kind == EventType::CampdStarted {
                Ledger::append_on(conn, "2026-07-06T07:00:00Z", EventInput {
                    kind: EventType::ConfigChanged,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({"path":"camp.toml","applied":true,"orders":0}),
                })?;
            }
            Ok(())
        }).unwrap();
        // process_past_cursor drains pages until empty WITHIN one call: the
        // config.changed appended while processing seq 1 lands at seq 2 and
        // is processed by the same call (plan-review correction).
        assert_eq!(end, 2, "the same call drains events appended mid-processing");
        let events = ledger.events_range(1, None).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, EventType::ConfigChanged);
        assert_eq!(ledger.cursor("t").unwrap(), 2);
    }

    #[test]
    fn append_on_rejects_invalid_payloads_like_append() {
        let (_dir, mut ledger) = temp_ledger();
        ledger.append(input(EventType::CampdStarted, None, None, serde_json::json!({}))).unwrap();
        let err = ledger.process_past_cursor("t", &mut |conn, _event| {
            Ledger::append_on(conn, "2026-07-06T07:00:00Z", EventInput {
                kind: EventType::ConfigChanged,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"applied": true}), // missing path/orders
            })?;
            Ok(())
        });
        assert!(err.is_err());
        // the failed processor transaction rolled back: no event, no cursor move
        assert_eq!(ledger.events_range(1, None).unwrap().len(), 1);
        assert_eq!(ledger.cursor("t").unwrap(), 0);
    }

    #[test]
    fn last_event_ts_and_events_of_type() {
        let (_dir, mut ledger) = temp_ledger();
        assert_eq!(ledger.last_event_ts().unwrap(), None);
        ledger.append(input(EventType::CampdStarted, None, None, serde_json::json!({}))).unwrap();
        ledger.append(input(EventType::CampdStopped, None, None, serde_json::json!({}))).unwrap();
        assert!(ledger.last_event_ts().unwrap().is_some());
        assert_eq!(ledger.events_of_type(EventType::CampdStarted).unwrap().len(), 1);
        assert_eq!(ledger.events_of_type(EventType::OrderFired).unwrap().len(), 0);
    }
```

- [ ] **Step 2: Run to verify failure**, then **Step 3: Implement.** Factor the body of `append_batch`'s per-input loop into a private `fn insert_and_fold(conn: &Connection, ts: &str, input: EventInput) -> Result<Seq, CoreError>`; `append_batch` calls it inside its transaction; `append_on` is the `pub` re-export of the same function with the transaction-contract doc comment. `last_event_ts`: `SELECT ts FROM events ORDER BY seq DESC LIMIT 1` (optional). `events_of_type`: `SELECT seq, ts, type, rig, actor, bead, data FROM events WHERE type = ?1 ORDER BY seq` through `row_to_event`.
- [ ] **Step 4: Run** `cargo test -p camp-core` (green, including refold — `append_on` reuses the one fold path).
- [ ] **Step 5: Commit.** `git commit -m "feat(ledger): conn-scoped append_on, last_event_ts, events_of_type"`

---

### Task 10.7: camp-core — fire declarations, trigger matching, completion detection

**Files:**
- Modify: `crates/camp-core/src/orders/mod.rs`

**Interfaces:**
- Consumes: 10.3–10.6.
- Produces:

```rust
pub const ORDER_ACTOR_PREFIX: &str = "order:";
pub enum FireCause { Cron { scheduled: Timestamp, catch_up: bool }, Event { cause_seq: Seq }, Manual }
pub struct PendingCook { pub order: String, pub fired_seq: Seq }

pub fn fired_input(order_name: &str, cause: &FireCause) -> EventInput;      // actor: campd (cron/event), cli (manual)
pub fn cook_actor(order_name: &str, fired_seq: Seq) -> String;              // "order:<name>:<seq>"
pub fn parse_cook_actor(actor: &str) -> Option<(&str, Seq)>;
pub fn pending_cook_from_fired(event: &Event) -> Result<Option<PendingCook>, CoreError>;
pub fn event_trigger_matches(conn: &Connection, order: &Order, event: &Event) -> Result<bool, CoreError>;
pub fn completion_input(conn: &Connection, event: &Event) -> Result<Option<EventInput>, CoreError>;
pub fn formula_path(camp_root: &Path, formula: &str) -> PathBuf;            // <camp>/formulas/<name>.toml
```

- [ ] **Step 1: Write the failing tests** (orders/mod.rs tests module; uses a real `Ledger` in a tempdir and `process_past_cursor` to obtain a live `conn` — the pattern from Task 10.6's tests):

```rust
    #[test]
    fn fired_inputs_carry_the_cause() {
        let cron = fired_input("t", &FireCause::Cron { scheduled: ts("2026-07-06T07:00:00Z"), catch_up: true });
        assert_eq!(cron.kind, EventType::OrderFired);
        assert_eq!(cron.actor, "campd");
        assert_eq!(cron.data["trigger"], "cron");
        assert_eq!(cron.data["scheduled_ts"], "2026-07-06T07:00:00Z");
        assert_eq!(cron.data["catch_up"], true);
        let ev = fired_input("t", &FireCause::Event { cause_seq: 9 });
        assert_eq!(ev.data["trigger"], "event");
        assert_eq!(ev.data["cause_seq"], 9);
        let manual = fired_input("t", &FireCause::Manual);
        assert_eq!(manual.actor, "cli");
        assert_eq!(manual.data["trigger"], "manual");
    }

    #[test]
    fn cook_actor_round_trips() {
        let actor = cook_actor("morning-triage", 412);
        assert_eq!(actor, "order:morning-triage:412");
        assert_eq!(parse_cook_actor(&actor), Some(("morning-triage", 412)));
        assert_eq!(parse_cook_actor("session:8f3c2e01"), None);
        assert_eq!(parse_cook_actor("order:name-without-seq"), None);
    }

    #[test]
    fn on_time_cron_fire_omits_catch_up_flag() {
        let cron = fired_input("t", &FireCause::Cron { scheduled: ts("2026-07-06T07:00:00Z"), catch_up: false });
        assert!(cron.data.get("catch_up").is_none(), "on-time fires carry no catch_up key");
    }

    #[test]
    fn event_trigger_matches_type_and_bead_label() {
        let (_dir, mut ledger) = test_ledger_with_rig(); // helper: camp+rig gc
        // gc-1 labeled ci-red, gc-2 unlabeled
        append_created(&mut ledger, "gc-1", &["ci-red"]);
        append_created(&mut ledger, "gc-2", &[]);
        append_closed(&mut ledger, "gc-1", "pass");
        append_closed(&mut ledger, "gc-2", "pass");
        let labeled = order_on("event:bead.closed[label=ci-red]");
        let unlabeled = order_on("event:bead.closed");
        let mut results = Vec::new();
        ledger.process_past_cursor("t", &mut |conn, event| {
            results.push((
                event.seq,
                event_trigger_matches(conn, &labeled, event).unwrap(),
                event_trigger_matches(conn, &unlabeled, event).unwrap(),
            ));
            Ok(())
        }).unwrap();
        // seq 1/2 = creates (no match: wrong type), 3 = close gc-1 (both match),
        // 4 = close gc-2 (only the unlabeled order matches)
        assert_eq!(results[0].1, false);
        assert_eq!(results[2], (3, true, true));
        assert_eq!(results[3], (4, false, true));
    }

    #[test]
    fn completion_input_fires_only_for_order_cooked_run_roots() {
        let (_dir, mut ledger) = test_ledger_with_rig();
        // Simulate a cooked run the way cook() writes it (run_id set on both,
        // step_id only on the step), with the order cook actor:
        let actor = cook_actor("t", 1);
        append_with(&mut ledger, EventType::BeadCreated, Some("gc-1"), &actor,
            serde_json::json!({"title":"root","run_id":"r1","needs":["gc-2"]}));
        append_with(&mut ledger, EventType::BeadCreated, Some("gc-2"), &actor,
            serde_json::json!({"title":"step","run_id":"r1","step_id":"s1"}));
        append_with(&mut ledger, EventType::RunCooked, Some("gc-1"), &actor,
            serde_json::json!({"run_id":"r1","formula":"f","root":"gc-1","steps":{"s1":"gc-2"}}));
        // a plain bead, closed: no completion
        append_created(&mut ledger, "gc-3", &[]);
        append_closed(&mut ledger, "gc-3", "pass");
        // the STEP closing: no completion (step_id set)
        append_closed(&mut ledger, "gc-2", "pass");
        // the ROOT closing with fail: order.failed with the run shape
        append_closed(&mut ledger, "gc-1", "fail");
        let mut completions = Vec::new();
        ledger.process_past_cursor("t", &mut |conn, event| {
            if let Some(input) = completion_input(conn, event).unwrap() {
                completions.push(input);
            }
            Ok(())
        }).unwrap();
        assert_eq!(completions.len(), 1);
        let c = &completions[0];
        assert_eq!(c.kind, EventType::OrderFailed);
        assert_eq!(c.data["order"], "t");
        assert_eq!(c.data["fired_seq"], 1);
        assert_eq!(c.data["root_bead"], "gc-1");
        assert_eq!(c.data["run_id"], "r1");
        assert_eq!(c.data["outcome"], "fail");
    }
```

**Note:** this test constructs `run.cooked`/`run_id` payloads by hand exactly as Phase 5's `cook` writes them (merged main, e6a043f) — hand-built here so the trigger/completion logic is tested in isolation; Task 10.8's tests then cover the same path with the real `cook`.

- [ ] **Step 2: Run to verify failure**, then **Step 3: Implement.** Highlights:

```rust
pub fn fired_input(order_name: &str, cause: &FireCause) -> EventInput {
    let (actor, data) = match cause {
        FireCause::Cron { scheduled, catch_up } => {
            let mut data = serde_json::json!({
                "order": order_name,
                "trigger": "cron",
                "scheduled_ts": canonical_ts(*scheduled),
            });
            if *catch_up {
                data["catch_up"] = serde_json::json!(true);
            }
            ("campd", data)
        }
        FireCause::Event { cause_seq } => (
            "campd",
            serde_json::json!({"order": order_name, "trigger": "event", "cause_seq": cause_seq}),
        ),
        FireCause::Manual => (
            "cli",
            serde_json::json!({"order": order_name, "trigger": "manual"}),
        ),
    };
    EventInput { kind: EventType::OrderFired, rig: None, actor: actor.into(), bead: None, data }
}

fn canonical_ts(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}
```

`event_trigger_matches`: `Trigger::Event` only; `event.kind.as_str() == event_type`; no label → match; label → `event.bead` must be set and `SELECT labels FROM beads WHERE id = ?1` (optional row; absent → no match) parsed as `Vec<String>` must contain the label. `completion_input`: `BeadClosed` only; `SELECT run_id, step_id FROM beads WHERE id = ?1`; require `run_id` set and `step_id` null; `SELECT actor, data FROM events WHERE bead = ?1 AND type = 'run.cooked' ORDER BY seq DESC LIMIT 1`; `parse_cook_actor` on that actor (non-order runs → `Ok(None)`); outcome from the close event's `data["outcome"]` string; build `order.completed` (pass) or the run-failure `order.failed` (fail) with `actor: "campd"`. `pending_cook_from_fired`: `OrderFired` only → deserialize `order` from data → `PendingCook { order, fired_seq: event.seq }`. Module docs carry the Decision I recursion warning.

- [ ] **Step 4: Run** `cargo test -p camp-core orders`, **Step 5: Commit.** `git commit -m "feat(orders): fire declarations, event-trigger matching, run-root completion detection"`

---

### Task 10.8: REBASE GATE, then `execute_fire` + reconciliation (needs Phase 5's cook)

**Files:**
- Modify: `crates/camp-core/src/orders/mod.rs`

**Gate (satisfied at plan time — kept as a freshness check):**
- [ ] **Step 0:** PR #10 merged 2026-07-07 as e6a043f and this branch was rebased onto it with all gates green before plan approval. At this step, just re-verify freshness: `git fetch origin && git rebase origin/main` (if main moved — e.g. a Phase 6/8 merge — resolve and re-run all gates: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`).

**Interfaces:**
- Consumes: `camp_core::formula::{parse_and_validate, cook, CookedRun}` (merged Phase 5), `CampConfig::rig`, 10.7's helpers.
- Produces:

```rust
pub fn execute_fire(ledger: &mut Ledger, config: &CampConfig, camp_root: &Path,
                    order: &Order, fired_seq: Seq) -> Result<Option<CookedRun>, CoreError>;
pub fn fire_response_exists(ledger: &Ledger, order_name: &str, fired_seq: Seq) -> Result<bool, CoreError>;
pub fn unresponded_fires(ledger: &Ledger) -> Result<Vec<PendingCook>, CoreError>;
```

Semantics (Decisions D and K): `execute_fire` first checks `fire_response_exists` (a `run.cooked` whose actor is `cook_actor(name, fired_seq)`, or an `order.failed` whose `data.fired_seq == fired_seq` and `data.order == name`) → `Ok(None)` if already answered (dedupe; replay-safe). Then: `formula_path(camp_root, &order.formula)` must exist → `parse_and_validate` → resolve rig (explicit `order.rig` via `config.rig`, else the sole configured rig, else error naming the fix) → `cook(ledger, &formula, &camp_root.join("runs"), rig, &cook_actor(...))` → `Ok(Some(run))`. Every order-level failure appends `order.failed {order, fired_seq, error}` and returns `Ok(None)`; only a failure to append that event returns `Err`. `unresponded_fires`: all `events_of_type(OrderFired)` minus those with a response, as `PendingCook`s in seq order.

- [ ] **Step 1: Write the failing tests:**

```rust
    fn write_formula(camp_root: &Path, name: &str) {
        std::fs::create_dir_all(camp_root.join("formulas")).unwrap();
        std::fs::write(
            camp_root.join("formulas").join(format!("{name}.toml")),
            format!("formula = \"{name}\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n"),
        ).unwrap();
    }

    #[test]
    fn execute_fire_cooks_the_formula_with_the_order_actor() {
        let (dir, mut ledger, config) = camp_fixture(); // camp root + camp.toml with rig gc
        write_formula(dir.path(), "one-step");
        let order = cron_order_named("t", "one-step");
        let fired = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        let run = execute_fire(&mut ledger, &config, dir.path(), &order, fired).unwrap().unwrap();
        // cook events carry the order actor; run dir pinned under <camp>/runs/
        let cooked = ledger.events_of_type(EventType::RunCooked).unwrap();
        assert_eq!(cooked.len(), 1);
        assert_eq!(cooked[0].actor, cook_actor("t", fired));
        assert!(dir.path().join("runs").join(&run.run_id).join("manifest.json").exists());
        // dedupe: a second execution for the same fired_seq is a no-op
        assert!(execute_fire(&mut ledger, &config, dir.path(), &order, fired).unwrap().is_none());
        assert_eq!(ledger.events_of_type(EventType::RunCooked).unwrap().len(), 1);
    }

    #[test]
    fn execute_fire_failure_is_evented_not_thrown() {
        let (dir, mut ledger, config) = camp_fixture();
        let order = cron_order_named("t", "missing-formula");
        let fired = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        assert!(execute_fire(&mut ledger, &config, dir.path(), &order, fired).unwrap().is_none());
        let failed = ledger.events_of_type(EventType::OrderFailed).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["fired_seq"], fired);
        assert!(failed[0].data["error"].as_str().unwrap().contains("missing-formula"));
    }

    #[test]
    fn unresponded_fires_reconciles_exactly_the_unanswered_ones() {
        let (dir, mut ledger, config) = camp_fixture();
        write_formula(dir.path(), "one-step");
        let order = cron_order_named("t", "one-step");
        let answered = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        execute_fire(&mut ledger, &config, dir.path(), &order, answered).unwrap();
        let orphaned = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        let failed = ledger.append(fired_input("u", &FireCause::Manual)).unwrap();
        ledger.append(EventInput {
            kind: EventType::OrderFailed, rig: None, actor: "campd".into(), bead: None,
            data: serde_json::json!({"order":"u","fired_seq":failed,"error":"e"}),
        }).unwrap();
        assert_eq!(
            unresponded_fires(&ledger).unwrap(),
            vec![PendingCook { order: "t".into(), fired_seq: orphaned }]
        );
    }
```

- [ ] **Step 2: Run to verify failure**, **Step 3: Implement** per the semantics block, **Step 4: Run** `cargo test -p camp-core`, **Step 5: Commit.** `git commit -m "feat(orders): execute_fire cooks fired orders with dedupe and crash reconciliation"`

---

### Task 10.9: camp — `camp order ls` / `camp order run`

**Files:**
- Create: `crates/camp/src/cmd/order.rs`, `crates/camp/tests/cli_order.rs`
- Modify: `crates/camp/src/main.rs`, `crates/camp/Cargo.toml` (add `jiff = "0.2.31"`)

**Interfaces:**
- Consumes: `compile_orders`, `CronExpr::next_after`, `fired_input`/`FireCause::Manual`, `poke_best_effort`, `CampDir`.
- Produces: CLI surface:
  - `camp order ls [--json]` — one row per order: `NAME  ON  FORMULA  RIG  WINDOW  NEXT`. `NEXT` = next cron fire rendered in the system timezone (`-` for event orders, `never` for an expression with no fire in the horizon). `--json` emits an array of `{name, on, formula, rig, catch_up_window_secs, next_fire}`.
  - `camp order run <name>` — appends `order.fired {trigger:"manual"}` (actor `cli`), pokes campd best-effort, prints `fired order <name> (seq N); campd cooks and dispatches it`. Unknown name: hard error listing configured order names. No configured orders: hard error `no orders configured in camp.toml`.

- [ ] **Step 1: Write the failing CLI tests** (`cli_order.rs`, using the Phase 7 harness conventions — `camp_cmd`, `init_camp`):

```rust
    const ORDERS_TOML: &str = r#"
[[order]]
name    = "morning-triage"
on      = "cron:0 7 * * 1-5"
formula = "triage-inbox"
rig     = "gc"

[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "fix-ci"
"#;

    fn add_orders(root: &Path) {
        let path = root.join("camp.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(ORDERS_TOML);
        std::fs::write(&path, text).unwrap();
    }

    #[test]
    fn order_ls_shows_triggers_and_next_fires() {
        let dir = tempfile::tempdir().unwrap();
        let root = init_camp(dir.path());
        add_orders(&root);
        let out = run_ok(&root, &["order", "ls"]);
        assert!(out.contains("morning-triage") && out.contains("cron:0 7 * * 1-5"));
        assert!(out.contains("ci-red") && out.contains("event:bead.closed[label=ci-red]"));
        let json = run_ok(&root, &["order", "ls", "--json"]);
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert!(rows[0]["next_fire"].is_string(), "cron orders show a next fire");
        assert!(rows[1]["next_fire"].is_null(), "event orders have none");
    }

    #[test]
    fn order_run_appends_a_manual_fire() {
        let dir = tempfile::tempdir().unwrap();
        let root = init_camp(dir.path());
        add_orders(&root);
        let out = run_ok(&root, &["order", "run", "morning-triage"]);
        assert!(out.contains("fired order morning-triage"));
        let events = run_ok(&root, &["events", "--json"]);
        let fired: Vec<serde_json::Value> = events.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .filter(|e: &serde_json::Value| e["type"] == "order.fired")
            .collect();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0]["data"]["trigger"], "manual");
        assert_eq!(fired[0]["actor"], "cli");
    }

    #[test]
    fn order_run_unknown_name_lists_the_options() {
        let dir = tempfile::tempdir().unwrap();
        let root = init_camp(dir.path());
        add_orders(&root);
        let out = camp_cmd(&root).args(["order", "run", "nope"]).output().unwrap();
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("nope") && err.contains("morning-triage"));
    }

    #[test]
    fn order_ls_with_a_broken_order_names_order_and_field() {
        let dir = tempfile::tempdir().unwrap();
        let root = init_camp(dir.path());
        let path = root.join("camp.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[[order]]\nname=\"bad\"\non=\"cron:61 * * * *\"\nformula=\"f\"\n");
        std::fs::write(&path, text).unwrap();
        let out = camp_cmd(&root).args(["order", "ls"]).output().unwrap();
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("bad") && err.contains("on") && err.contains("minute"), "{err}");
    }
```

- [ ] **Step 2: Run to verify failure** (`cargo test -p camp --test cli_order` — unknown subcommand), then **Step 3: Implement** `cmd/order.rs` (`pub fn ls(camp: &CampDir, json: bool) -> Result<()>`, `pub fn run_order(camp: &CampDir, name: &str) -> Result<()>`) and wire `main.rs`:

```rust
    /// Manage orders (scheduled and event-triggered formulas)
    Order {
        #[command(subcommand)]
        command: OrderCommand,
    },
// ...
#[derive(Subcommand)]
enum OrderCommand {
    /// List configured orders with their next fire times
    Ls {
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Fire an order now (manual trigger; campd cooks it)
    Run {
        /// Order name from camp.toml
        name: String,
    },
}
```

`ls` computes `next_fire` with `expr.next_after(jiff::Timestamp::now(), &jiff::tz::TimeZone::system())`, rendered via the zoned display. `run_order` finds the compiled order (error lists names), `ledger.append(fired_input(name, &FireCause::Manual))`, `poke_best_effort`, prints the line.

- [ ] **Step 4: Run** `cargo test -p camp --test cli_order`, **Step 5: Commit.** `git commit -m "feat(cli): camp order ls / camp order run"`

---

### Task 10.10: camp — `OrdersRuntime`, `CampdProcessor`, `settle`

**Files:**
- Create: `crates/camp/src/daemon/orders.rs` (+ `pub mod orders;` in `daemon/mod.rs`)

**Interfaces:**
- Consumes: everything from 10.3–10.8; `ReadinessProcessor`, `cursor::catch_up`, `Ledger::append_on`, `Clock`/`SystemClock`/`FixedClock`.
- Produces:

```rust
pub struct OrdersRuntime { /* camp_root, tz, raw config text, config, orders, heap, pending_cooks */ }
impl OrdersRuntime {
    pub fn build(camp_root: &Path, now: Timestamp, tz: TimeZone) -> anyhow::Result<Self>; // hard error on bad/never-firing config
    pub fn poll_timeout(&self, now: Timestamp) -> Option<std::time::Duration>;
    pub fn order(&self, name: &str) -> Option<&Order>;
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<Fire>;
    pub fn recompute(&mut self, now: Timestamp, last_seen: Timestamp) -> Vec<CatchUp>;
    pub fn reload_if_changed(&mut self, now: Timestamp) -> anyhow::Result<Option<EventInput>>; // Decision H
    pub fn take_pending_cooks(&mut self) -> Vec<PendingCook>;
    pub fn queue_cook(&mut self, cook: PendingCook);
}
pub struct CampdProcessor<'a> {
    pub readiness: &'a mut ReadinessProcessor,
    pub runtime: &'a mut OrdersRuntime,
    pub clock: &'a dyn Clock,
}
impl EventProcessor for CampdProcessor<'_> { /* readiness + orders, see below */ }
pub fn settle(ledger: &mut Ledger, readiness: &mut ReadinessProcessor,
              runtime: &mut OrdersRuntime, clock: &dyn Clock) -> Result<(), CoreError>;
```

Processor semantics (the heart of "same path as readiness"): for each event, in order — (1) delegate to `ReadinessProcessor::process`; (2) if the event is `order.fired`, queue `pending_cook_from_fired`; (3) if `completion_input` yields one, `Ledger::append_on(conn, &clock.now_utc(), input)` (atomic with the cursor); (4) for every event-triggered order where `event_trigger_matches`, `append_on` a `fired_input(name, &FireCause::Event { cause_seq: event.seq })` — the appended `order.fired` lands past the cursor and is drained (and its cook queued) in the SAME `catch_up` call, which pages until no events remain (plan-review correction); the cooks then execute after `catch_up` returns, and the next settle iteration folds the resulting `run.cooked`/`bead.created` events. `settle` loops: `catch_up` → drain `readiness.take_pending()` (Phase 8's hook, kept drained) → `take_pending_cooks`; if empty, done; else for each cook, look up the order by name (`None` → the order was removed by a reload between fire and cook: append `order.failed {order, fired_seq, error:"order no longer configured"}`) and `execute_fire` (its `Ok(None)`/evented-failure contract means settle only propagates infrastructure errors), then loop again.

`poll_timeout`: `next_deadline()` → `None` if no heap entries (idle = infinite wait); else `deadline − now` as a std `Duration`, clamped ≥ `Duration::ZERO`, plus 1 ms (round up so the wake lands at-or-after the deadline, never a hot spin just before it).

`reload_if_changed`: read `<camp_root>/camp.toml` bytes; identical to the last applied text → `Ok(None)`; parse + compile + rebuild a fresh heap (armed at `now`) — success → swap all state, return `config.changed {path:"camp.toml", applied:true, orders:N}` input; failure → keep state, return `config.changed {applied:false, error}` input. (The caller appends; the runtime never holds the ledger.)

- [ ] **Step 1: Write the failing tests** (in-module; a `FixedClock`, a tempdir camp root with `camp.toml` + `formulas/one-step.toml`, a real `Ledger`):

```rust
    #[test]
    fn build_rejects_bad_config_and_never_firing_cron() { /* bad TOML → Err;
        "cron:0 0 30 2 *" order → Err naming the order */ }

    #[test]
    fn poll_timeout_is_none_when_idle_and_deadline_based_when_armed() {
        // no orders → None; one cron order → Some(d) where d ≈ deadline − now (+1ms)
    }

    #[test]
    fn settle_cooks_a_manual_fire_and_completes_on_root_close() {
        // append fired_input(Manual) → settle → run.cooked with order actor,
        // step bead exists; close step (pass) then root (pass) via ledger;
        // settle → order.completed appended with fired_seq/root_bead/outcome pass;
        // cursor caught up; a second settle appends nothing new.
    }

    #[test]
    fn settle_fires_event_orders_on_matching_close_only() {
        // runtime with event:bead.closed[label=ci-red]; close unlabeled bead →
        // settle → no order.fired; close labeled bead → settle → order.fired
        // {trigger:"event", cause_seq:<close seq>} AND its run cooked in the
        // same settle (fixpoint), all in one call.
    }

    #[test]
    fn settle_survives_a_broken_order_and_events_the_failure() {
        // order whose formula file is missing; manual fire; settle → Ok;
        // order.failed present; campd-fatal error NOT raised.
    }

    #[test]
    fn reload_swaps_config_and_reports_rejects() {
        // same content → None; add an order → Some(applied:true, orders:1) and
        // runtime.order("new") is Some; write junk → Some(applied:false, error)
        // and runtime.order("new") STILL Some (old config retained).
    }

    #[test]
    fn startup_reconciliation_cooks_orphaned_fires() {
        // append fired_input(Manual); advance the campd cursor past it WITHOUT
        // cooking (simulate kill -9 after cursor advance): process_past_cursor
        // with a no-op processor; then unresponded_fires → queue → settle-style
        // drain cooks it exactly once; a repeat drains nothing.
    }
```

Write each with full assertions in the style of the earlier tasks (they are compact once the fixture helpers exist — `fixture()` returning `(TempDir, Ledger, OrdersRuntime, FixedClock)`).

- [ ] **Step 2: Run to verify failure**, **Step 3: Implement**, **Step 4: Run** `cargo test -p camp daemon::orders`, **Step 5: Commit.** `git commit -m "feat(campd): orders runtime, processor on the readiness path, settle fixpoint"`

---

### Task 10.11: camp — event-loop integration (heap deadline, jump detection, notify)

**Files:**
- Modify: `crates/camp/src/daemon/event_loop.rs`, `crates/camp/src/daemon/mod.rs`, `crates/camp/Cargo.toml`

**Interfaces:**
- Consumes: 10.10; `mio::unix::pipe` (add `os-ext` to mio features), `notify = "8"`.
- Produces: the assembled daemon. Per the Decision L ruling, `event_loop::run` keeps its existing parameters and appends three positional ones: `run(listener, socket_path, ledger, processor, runtime: &mut OrdersRuntime, clock: &dyn Clock, config_rx: &mut mio::unix::pipe::Receiver)`. No `LoopCtx`.

Loop shape (replacing the current fixed `poll_timeout()` fn — its doc comment moves onto `OrdersRuntime::poll_timeout`, "the only timeout expression in campd" now sourced from the heap):

```rust
const LISTENER: Token = Token(0);
const CONFIG_WATCH: Token = Token(1);
// connection tokens start at 2
const JUMP_TOLERANCE: SignedDuration = SignedDuration::from_secs(30);

let mut last_seen = Timestamp::now();
loop {
    let timeout = runtime.poll_timeout(Timestamp::now());
    let wall_before = Timestamp::now();
    let mono_before = Instant::now();
    poll.poll(&mut events, timeout).context("poll")?;
    let now = Timestamp::now();
    // Wall-clock jump check (spec §9, Decision G): expected vs actual.
    let wall_delta = now.duration_since(wall_before);
    let mono_delta = SignedDuration::try_from(mono_before.elapsed()).unwrap_or(SignedDuration::MAX);
    let fires: Vec<Fire> = if (wall_delta - mono_delta).abs() > JUMP_TOLERANCE {
        runtime.recompute(now, last_seen).into_iter()
            .map(|c| Fire { order: c.order, scheduled: c.scheduled, catch_up: true })
            .collect()
    } else {
        runtime.fire_due(now)
    };
    last_seen = now;
    let mut wake_ledger_work = !fires.is_empty();
    for fire in fires {
        ledger.append(camp_core::orders::fired_input(
            &fire.order,
            &FireCause::Cron { scheduled: fire.scheduled, catch_up: fire.catch_up },
        ))?;
    }
    for event in events.iter() {
        match event.token() {
            LISTENER => { /* unchanged accept loop */ }
            CONFIG_WATCH => {
                drain_pipe(config_rx)?; // read until WouldBlock, discard bytes
                if let Some(input) = runtime.reload_if_changed(now)? {
                    ledger.append(input)?;
                    wake_ledger_work = true;
                }
            }
            token => { /* unchanged connection serving; drain_lines' Poke arm
                          calls orders::settle(...) instead of cursor::catch_up
                          — the one-line swap (Decision L ruling) — and still
                          drains readiness pending for Phase 8 */ }
        }
    }
    if wake_ledger_work {
        // Timer-path settle errors mirror Phase 7 Decision H: surface to
        // stderr, keep serving; the cursor holds and the error re-surfaces.
        if let Err(e) = orders::settle(ledger, processor, runtime, clock) {
            eprintln!("campd: settle failed: {e}");
        }
    }
}
```

`daemon/mod.rs::run` startup order (Decision F — read `last_seen0` before `campd.started`):

```rust
let mut ledger = Ledger::open(&camp.db_path())?;
let clock = SystemClock;
let tz = TimeZone::system();
let last_seen0: Timestamp = match ledger.last_event_ts()? {
    Some(ts) => ts.parse().context("parsing the ledger's last event ts")?,
    None => Timestamp::now(),
};
// ... bind socket (unchanged), append campd.started (unchanged) ...
let mut runtime = OrdersRuntime::build(&camp.root, Timestamp::now(), tz)?; // fail fast on bad config
// notify watcher: camp root dir, non-recursive; callback filters camp.toml
let (sender, receiver) = mio::unix::pipe::new()?;
let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
    match res {
        Ok(ev) if ev.paths.iter().any(|p| p.file_name() == Some(OsStr::new("camp.toml"))) => {
            use std::io::Write as _;
            let _ = (&sender).write(&[1]); // full pipe (WouldBlock) coalesces
        }
        Ok(_) => {}
        Err(e) => eprintln!("campd: camp.toml watch error: {e}"),
    }
})?;
watcher.watch(&camp.root, notify::RecursiveMode::NonRecursive)
    .context("watching camp.toml")?;
let mut readiness = ReadinessProcessor::default();
// startup settle (catch-up + any cooks queued by replayed order.fired):
orders::settle(&mut ledger, &mut readiness, &mut runtime, &clock)?;
// reconcile fires orphaned by a crash between order.fired and its cook:
for cook in camp_core::orders::unresponded_fires(&ledger)? {
    runtime.queue_cook(cook);
}
// cron fires missed while campd was down, under the window (spec §9):
let now = Timestamp::now();
for c in runtime.recompute(now, last_seen0) {
    ledger.append(camp_core::orders::fired_input(
        &c.order, &FireCause::Cron { scheduled: c.scheduled, catch_up: true },
    ))?;
}
orders::settle(&mut ledger, &mut readiness, &mut runtime, &clock)?;
// ... readiness line (unchanged), then (positional params per the ruling):
event_loop::run(
    listener, &socket_path, &mut ledger, &mut readiness,
    &mut runtime, &clock, &mut receiver,
)
```

The watcher handle stays alive in `run`'s scope (drop = watch dies). Startup settle/reconcile failures are fatal (fail fast, Phase 7 precedent).

- [ ] **Step 1: Write the failing unit test first** — the existing in-module `daemon::tests` (which call `run(&camp)` in a thread) must stay green with the new wiring, plus one new in-module test in `event_loop.rs` or `daemon/orders.rs` pinning the timeout conversion:

```rust
    #[test]
    fn poll_timeout_rounds_up_and_never_spins() {
        // deadline 1 µs in the future → timeout ≥ 1 ms; deadline in the past
        // → Some(ZERO); empty heap → None
    }
```

- [ ] **Step 2:** `cargo test -p camp` to watch the new pieces fail / signatures break, **Step 3:** implement as above (Cargo: `notify = "8"`, `jiff` already added in 10.9, mio features `["os-poll", "net", "os-ext"]`), **Step 4:** `cargo test -p camp` — all existing daemon tests green plus the new one. **Step 5: Commit.** `git commit -m "feat(campd): heap deadline drives poll timeout; camp.toml hot-reload via notify"`

---

### Task 10.12: Integration — away-mode, event orders, hot reload (`daemon_orders.rs`)

**Files:**
- Create: `crates/camp/tests/daemon_orders.rs` (reuse the Phase 7 harness helpers: `camp_cmd`, `init_camp`, spawning `BIN daemon` with piped stdout and waiting for `READY_PREFIX`)

Test fixtures: `init_camp` + write `<camp>/formulas/one-step.toml` (the Task 10.8 single-step formula) + append order tables to `camp.toml` before starting the daemon. Helper `events_of(root, ty)` shells `camp events --json` and filters. Helper `wait_for(root, ty, timeout)` polls `camp events --json` every 250 ms (test harness only) up to the deadline.

- [ ] **Step 1: Write the failing tests:**

```rust
    /// Exit criterion: away-mode is the same code path — a cron order fires
    /// with NO user session driving anything, campd cooks it, and the ledger
    /// tells the story. `* * * * *` fires at the next minute boundary
    /// (≤ ~75 s worst case).
    #[test]
    fn a_cron_order_fires_and_cooks_with_no_user_session() {
        // camp.toml: [[order]] name="tick" on="cron:* * * * *" formula="one-step"
        // start campd; do NOT run any camp verbs except read-only `events`
        // wait_for order.fired (90 s): data.trigger == "cron", actor == "campd",
        //   data.scheduled_ts present
        // wait_for run.cooked: actor == "order:tick:<fired seq>"
        // assert a step bead exists (camp ls --json) that nothing dispatched
        //   (Phase 8 not merged) — cooked, ready, waiting: the ledger story is
        //   order.fired → run.cooked → bead.created, every action with a cause
        // stop campd
    }

    /// The full lifecycle on the manual path (same pipeline, fast): fire →
    /// cook → fake-agent closes → order.completed.
    #[test]
    fn a_manual_fire_cooks_and_completes_via_the_fake_agent_contract() {
        // order on="cron:0 0 1 1 *" (far future — never fires in-test)
        // start campd; camp order run one-shot
        // wait_for run.cooked; extract root + step bead ids from its data
        // fake-agent contract via the CLI: camp claim <step> --session s;
        //   camp close <step> --outcome pass; camp close <root> --outcome pass
        // wait_for order.completed: data.outcome == "pass",
        //   data.root_bead == root, data.fired_seq == the order.fired seq
        // camp doctor --refold exits 0 (state == history)
    }

    #[test]
    fn an_event_order_fires_on_matching_close_and_not_otherwise() {
        // order on="event:bead.closed[label=ci-red]" formula="one-step"
        // start campd
        // camp create "plain" + camp close → no order.fired after settle
        //   (poke is synchronous: the close's poke response means processing
        //   ran; assert order.fired count == 0)
        // camp create "red" --label ci-red + camp close → wait_for order.fired:
        //   trigger == "event", cause_seq == the close event's seq; then
        //   run.cooked with the order actor
    }

    #[test]
    fn editing_camp_toml_hot_reloads_with_a_config_changed_event() {
        // start campd with no orders (idle: no timers)
        // append an [[order]] block to camp.toml
        // wait_for config.changed: applied == true, orders == 1
        // camp order run <it> → wait_for run.cooked (proves the reload armed it)
        // write syntactically broken camp.toml → wait_for second config.changed:
        //   applied == false, error non-empty; campd still answers {"op":"status"}
        // restore the valid file → wait_for third config.changed (applied true)
    }

    /// kill -9 between order.fired and the cook self-heals at next start
    /// (Decision D reconciliation), end to end.
    #[test]
    fn an_orphaned_fire_is_cooked_on_restart() {
        // no campd running: camp order run one-shot (order.fired lands, poke
        //   goes nowhere — this IS the orphaned state)
        // start campd; wait_for run.cooked with actor order:one-shot:<seq>
    }
```

- [ ] **Step 2: Run to verify failure** (`cargo test -p camp --test daemon_orders` — orders don't fire yet if any wiring is missing; otherwise these pass immediately and pin the behavior).
- [ ] **Step 3: Fix whatever the integration surfaces.** (Expected wrinkles: poke-path settle needs the runtime — verify the `drain_lines` rework; notify latency on macOS FSEvents — `wait_for` timeouts of 10 s for reload, 90 s for the cron test.)
- [ ] **Step 4: Run the full workspace suite.** `cargo test --workspace`
- [ ] **Step 5: Commit.** `git commit -m "test(campd): away-mode cron fire, event orders, completion, hot reload end to end"`

---

### Task 10.13: launchd example, docs, spec edit

**Files:**
- Create: `contrib/launchd/com.gascamp.campd.plist.example`, `contrib/launchd/README.md`
- Modify: `README.md`, `docs/design/2026-07-05-gas-camp-design.md` (**coordinate with the lead before this edit lands — spec edits are serialized**)

- [ ] **Step 1: The plist example** (example only, never auto-installed — spec §5/§9):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <!-- Gas Camp: OPTIONAL fire-at-login agent (spec §5, §9).
       Edit the two paths below, then see README.md alongside this file.
       Without this agent, campd auto-starts on first `camp` use and orders
       fire only while it runs — that default is fine for most setups. -->
  <key>Label</key>
  <string>com.gascamp.campd</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/camp</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>/Users/YOU/camps/dev/.camp</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <!-- Deliberately NO KeepAlive: `camp stop` must stay stopped. A campd
       that exits is restarted on demand by the next camp verb (spec §5). -->
  <key>StandardErrorPath</key>
  <string>/Users/YOU/camps/dev/.camp/campd.log</string>
  <key>StandardOutPath</key>
  <string>/dev/null</string>
</dict>
</plist>
```

- [ ] **Step 2: `contrib/launchd/README.md`** — the order docs' operational page:

```markdown
# campd at login (optional launchd agent)

Orders (spec §9) fire only while `campd` runs. By default campd starts on
demand — the first `camp` verb after boot brings it up — so a freshly
rebooted, untouched machine fires nothing. If you want orders firing from
login without running a `camp` command first, install this agent:

    sed -e "s|/usr/local/bin/camp|$(command -v camp)|" \
        -e "s|/Users/YOU/camps/dev/.camp|$HOME/camps/dev/.camp|" \
        contrib/launchd/com.gascamp.campd.plist.example \
        > ~/Library/LaunchAgents/com.gascamp.campd.plist \
    && launchctl load ~/Library/LaunchAgents/com.gascamp.campd.plist

(Adjust the camp path; one plist per camp. `launchctl unload …` removes it.
It is an example, never auto-installed — visible automation only.)

## The honest away-mode limits (spec §9)

- An order fires, campd cooks and dispatches, everything lands in the
  ledger; you come back and the ledger tells the story. Same code path as
  attended use — there is no separate "away mode".
- With the default on-demand daemon, orders fire only between the first
  `camp` use and `camp stop`/reboot. This agent closes the from-login gap.
- A powered-off or sleeping laptop fires nothing until wake. On wake,
  fires missed within an order's `catch_up_window` (default "2h"; "0"
  disables) fire once, flagged `catch_up: true`; older ones are skipped.
- campd never guards against self-triggering orders (an order matching an
  event its own formula produces recurses, visibly, in the ledger) — that
  power is yours, like a `* * * * *` cron on an expensive formula.
```

- [ ] **Step 3: README pointer.** After the design-doc paragraph in `README.md`, add: `Orders can fire at login via the optional launchd agent — see [contrib/launchd/](contrib/launchd/README.md).`
- [ ] **Step 4: Spec edit (Decision E — ADOPTED by the operator, 2026-07-07; the only spec edit authorized in flight).** In §7.1's layout block, after the `runs/<run-id>/` line, add `  formulas/                # camp-local formula definitions, resolved by name (§9; packs layer beneath, §11)`. In §9, after the `[[order]]` example block, add the sentence: `An order's formula names <camp>/formulas/<name>.toml; when packs land (§11) they layer beneath these local definitions, last-wins.` Lands in this PR so spec and code agree at merge.
- [ ] **Step 5: Commit.** `git commit -m "docs: launchd fire-at-login example with honest away-mode limits; spec: formulas/ resolution"`

---

### Task 10.14: Gates, PR, CI, exit-criteria evidence

- [ ] **Step 1: Full gates.** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` — all green, zero warnings.
- [ ] **Step 2: Rebase check.** `git fetch origin && git rebase origin/main` (if anything merged since 10.8), re-run gates.
- [ ] **Step 3: Push + PR.** `git push -u origin phase-10-orders` (or `--force-with-lease` after rebase); `gh pr create` targeting main. PR description: summary, the Decision-log deviations (B, C, E) called out, the exit-criteria table below, test inventory.
- [ ] **Step 4: CI.** `gh pr checks --watch` until green.
- [ ] **Step 5: Report to the lead** — PR number, CI status, and the master-plan exit criteria quoted line by line with evidence:
  - *"away-mode is the same code path demonstrably (order fires with no user session; the ledger tells the story)"* → `a_cron_order_fires_and_cooks_with_no_user_session` (no CLI writes after daemon start; asserted trail `order.fired{cron}` → `run.cooked{actor order:…}` → `bead.created`), plus `a_manual_fire_cooks_and_completes_via_the_fake_agent_contract` showing the identical pipeline drives manual fires to `order.completed`.
  - *"no polling introduced (heap deadline = poll timeout, idle heap = infinite wait)"* → `OrdersRuntime::poll_timeout` returns `None` with an empty heap (unit test `poll_timeout_is_none_when_idle_and_deadline_based_when_armed`); the only timeout expression in campd is the heap deadline (code inspection: `event_loop.rs` has exactly one `poll` call whose timeout comes from `poll_timeout`); no `sleep`/interval code outside test harnesses (`grep -rn "sleep" crates/ --include=*.rs` shows tests only).
  - *"CI green"* → `gh pr checks` output.

## Master-plan test-obligation map

| Obligation | Where |
|---|---|
| cron parse/next-fire table (5-field, DST boundaries, month ends) with fixed clocks | 10.1 `rejects_with_the_field_named` etc.; 10.2 `next_fire_table_utc`, `spring_forward_gap_fires_once_shifted_compatible`, `fall_back_fold_fires_first_occurrence_only`, `impossible_dates_return_none` |
| heap ordering under interleaved schedules | 10.3 `interleaved_schedules_order_the_heap` |
| sleep/wake catch-up inside and outside the window | 10.3 `late_fire_within_window_is_a_catch_up_fire`, `late_fire_outside_window_is_skipped_and_rescheduled`, `recompute_fires_once_with_the_most_recent_missed_fire`, `recompute_outside_window_and_zero_window_yield_no_catch_ups` |
| `"0"` disables | 10.3 `zero_window_disables_catch_up_but_not_on_time_fires`; 10.4 window parse |
| event order fires on matching close and not otherwise | 10.7 `event_trigger_matches_type_and_bead_label`; 10.10 `settle_fires_event_orders_on_matching_close_only`; 10.12 `an_event_order_fires_on_matching_close_and_not_otherwise` |
| integration: cron order cooks and completes a formula via fake agent | 10.12 `a_cron_order_fires_and_cooks_with_no_user_session` + `a_manual_fire_cooks_and_completes_via_the_fake_agent_contract` (fake-agent contract = claim/close via the CLI; Phase 8's fake-agent.sh does not exist yet — noted for the lead) |
| config edit hot-reloads with event | 10.10 `reload_swaps_config_and_reports_rejects`; 10.12 `editing_camp_toml_hot_reloads_with_a_config_changed_event` |
| `[[order]]` exactly as spec §9; errors name order and field | 10.4 `compiles_the_spec_section_9_example`, `errors_name_the_order_and_the_field`; 10.9 CLI variant |
| vocab: order.* into GC_MIRRORED_EVENTS, verified against the pin | 10.5 (existing `vocab_pin.rs` partition tests enforce it) |
| launchd plist example + documented limits | 10.13 |

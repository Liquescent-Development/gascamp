# Fix #81 — Patrol Config Hot-Reload Implementation Plan

> **Plan review: APPROVE, 2026-07-13 (Opus 4.8 plan gate).** Config-derived field inventory verified exhaustive (config, camp_config, ladder budget — Decision A's live-timer preservation deliberate); the pack-scoped reproduction verified deterministic-RED against the un-wired arm; Decision L confirmed a plan-time deferral (phase-11 plan :86), not a settled spec decision — issue #81 un-defers it, no escalation. Non-blocking notes: N1 the camp-core module header at `crates/camp-core/src/patrol/mod.rs:3-5` carries the same obsolete Decision-L line as the camp-side header — update it in this PR (Task 1 already edits that file; Decision F commits to code/docs non-divergence); N2 the exhausted-branch reload boundary (unit-level only) accepted as flagged. No deviations accepted.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (a fresh implementer session, per the fix-81 kickoff amendment). Steps use checkbox (`- [ ]`) syntax for tracking. Record the plan-review approval note (date, verdict, non-blocking notes, accepted deviations) at the top of this doc in the first execution commit.

**Goal:** An applied `camp.toml` hot reload must reach the patrol runtime, so a worker dispatched to a freshly added pack/agent is resolved by patrol without a campd restart — killing the spurious `patrol.degraded` "unknown agent" the birth config produces.

**Architecture:** Patrol currently caches the `CampConfig` it was born with and never updates it; the `CONFIG_WATCH` arm of the event loop pushes an applied reload into the dispatcher and the graph runtime but not into patrol (issue #28 wired the first two; #81 is the missing third). Give `PatrolRuntime` an `apply_config` that swaps its config surface — the whole `CampConfig`, the derived `PatrolConfig`, and the ladder's restart-budget ceiling — and call it in the same `if applied { … }` block, mirroring `dispatcher.apply_config` / `graph.apply_config`. Future resolutions and future timer arms see the new config; in-flight timers and per-bead ladder state are left untouched, exactly as the dispatcher leaves in-flight children on their already-resolved spec.

**Tech Stack:** Rust (workspace: `camp-core` library + `camp` binary), `jiff` durations, `notify` file watches, `mio` event loop, SQLite ledger. Tests: `cargo test`, `tempfile`, the `fake-agent.sh` worker fake. No Claude, no network, no new dependencies.

## Global Constraints

Copied verbatim from AGENTS.md and the fix-81 kickoff; every task implicitly includes these.

- **Idle is free — no ticks, no polling loops anywhere.** Components sleep on OS events. This fix adds NO polling; it swaps in-memory config on an event already delivered by the config-watch pipe. (Kickoff acceptance: "no polling introduced anywhere".)
- **Fail fast.** No fallbacks, no silenced errors, no placeholders. Every error surfaces to the caller or lands in the ledger as an event.
- **No panics in library code.** `clippy` denies `unwrap_used`, `expect_used`, `panic`; `unsafe_code` is forbidden. Library code (`crates/**/src/**`) must not `.unwrap()`/`.expect()`/`panic!`. Test modules opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (already present in the files this plan touches).
- **TDD, strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything.
- **Respect merged interfaces — extend, don't rework.** New events use `deny_unknown_fields` payload structs; keep the one-transaction event+state property; satisfy the vocab-pin partition tests; keep the refold property test green. (This fix adds NO new event type — the failure signature is the EXISTING `patrol.degraded`/`agent.stalled` events.)
- **Gates that must be green before push (run all three):**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace`
- **One reviewable PR.** Work is not complete until pushed and CI is green — foreground-watch to the settled result (`gh pr checks --watch`); never report "CI is running".
- **No test may spawn a real claude or spend API money.** Workers are `#!/bin/sh` fakes (`crates/camp/tests/fake-agent.sh`); no network (file:// fixtures only). Test-harness ledger polling (`wait_until`) is explicitly sanctioned — "camp never polls; tests may" (`daemon_patrol.rs` header).
- **House rules:** never add co-authors or mention yourself in commits; never silence errors; never call something complete unless it is actually 100% complete.
- **Owned files for this work stream (fix-81):** `crates/camp/src/daemon/patrol.rs` and the `CONFIG_WATCH` arm of `crates/camp/src/daemon/event_loop.rs`. This plan also makes one small ADDITIVE change to `crates/camp-core/src/patrol/mod.rs` (a new `Ladder::set_restart_budget` method) — that file is not owned by any sibling work stream and the change is purely additive (see Decision C). Do not touch any other sibling's owned files. If a sibling PR merges before this one, the lead will instruct a rebase-onto-main + full-gate re-run before opening/updating the PR.

---

## Root Cause Analysis (systematic-debugging, Phase 1–3)

The issue's own analysis was treated as evidence, not gospel, and confirmed against the code on `main`.

**Symptom (issue #81):** after an applied hot reload, patrol emits `patrol.degraded` with an "unknown agent" error for an agent that dispatch resolved and spawned moments earlier; patrol keeps the config campd was born with.

**Reproduction path traced through the code:**

1. **The reload reaches dispatcher and graph, but not patrol.** `crates/camp/src/daemon/event_loop.rs:293-296`, inside the `CONFIG_WATCH` arm:
   ```rust
   if applied {
       dispatcher.apply_config(runtime.config().clone());
       graph.apply_config(runtime.config());
   }
   ```
   There is no `patrol.apply_config(...)`. The comment above it (lines 287-292, issue #28) says an applied reload "must reach dispatch, not just the order scheduler… so a new pack/agent/rig/default_agent takes effect with no restart" — patrol was simply not added to that list. `patrol` is in scope here (`run(...)` takes `patrol: &mut PatrolRuntime`, `event_loop.rs:102`).

2. **Patrol caches the birth `CampConfig` and never updates it.** `PatrolRuntime::new` stores `camp_config: camp_config.clone()` (`patrol.rs:208-224`). There is no method that mutates `self.camp_config` or `self.config` after construction. The module header states the current (now-obsolete) behavior verbatim: *"Patrol config is read at campd start; hot reload does not re-arm patrol (plan Decision L)"* (`patrol.rs:15-16`).

3. **Agent resolution runs against that stale config, producing the degraded event.** In `apply_tracking`, for every newly tracked worker (`patrol.rs:426-448`):
   ```rust
   let base = match pack::resolve_agent(&self.camp_config, &tracked.agent) {
       Ok(def) => …,
       Err(e) => {
           if tracked.owned == Owned::Child {
               ledger.append(EventInput { kind: EventType::PatrolDegraded, …
                   "error": format!("stall threshold fell back to the camp default: {e}"), … })?;
           }
           self.config.stall_after
       }
   };
   ```
   `pack::resolve_agent` (`crates/camp-core/src/pack.rs`) searches the agent layers built by `layers(cfg)`: one `agents/` dir per entry in **`cfg.packs`** (config-captured) plus the live **`cfg.root/agents`** dir. A campd-spawned worker (`Owned::Child`, actor `campd` on the `session.woke`) whose agent came from a pack **added by the reload** is unresolvable under the birth config — `cfg.packs` is still empty — so `resolve_agent` returns `CoreError::UnknownAgent`, patrol appends `patrol.degraded`, and the stall threshold silently falls back to the camp default instead of the agent's own `stall_after`.

4. **Why a *locally-added* agent does not reproduce the bug (and the test must use a pack).** `layers()` reads `cfg.root/agents` live from disk, and `cfg.root` is unchanged by a reload. An agent added as a plain file under `<root>/agents/` therefore resolves even under the stale birth config. The defect is **pack-scoped**: only agents introduced through `cfg.packs` (or any config-captured layer) are invisible to the birth config. The reproduction adds the agent via a reloaded pack, exactly mirroring the issue #28 dispatcher test `a_hot_reload_updates_dispatch_routing_without_a_restart` (`crates/camp/tests/daemon_orders.rs:382`).

**Root cause (single, confirmed):** the `CONFIG_WATCH` arm never propagates an applied reload to patrol, and `PatrolRuntime` has no way to accept one. Fix at the source: add `PatrolRuntime::apply_config` and call it alongside its two siblings.

**Confirmed non-issues (ruled out during investigation):**
- *Torn state / fallible apply.* An applied reload is pre-validated: `compile_and_arm` → `CampConfig::parse` runs `PatrolConfig::from_section(&cfg.patrol)?` (`crates/camp-core/src/config.rs`, `parse`). So `applied:true` guarantees a valid `[patrol]` section, and re-deriving `PatrolConfig` inside `apply_config` cannot fail for an applied config. The `?` is fail-fast on an impossible torn state, never a silent fallback (Decision D).
- *Spec / settled-decision conflict.* "Decision L" lives in a **plan** doc (`docs/superpowers/plans/2026-07-07-phase-11-patrol-adoption.md:86`) and is an explicit **deferral** — *"extending [the hot-reload promise] to patrol is deferred and documented in the module header"* — not a settled spec §4 decision. Issue #81 is the ticket that un-defers it. The design spec's hot-reload promise and issue #28's "an applied reload must reach dispatch" support applying config broadly. No spec edit and no lead escalation are required (Decision F).

---

## Plan-Time Decisions

- **Decision A — Semantics: swap config for the FUTURE; never re-arm in-flight state.** `apply_config` swaps the config used for future agent/rig/threshold resolutions and future timer arms. It does NOT re-arm already-armed stall timers and does NOT reset per-bead ladder history. This mirrors `dispatcher.apply_config`, whose doc-comment reads *"In-flight children are untouched: each carries its own already-resolved spec, so a reload never disturbs running work"* (`dispatch.rs:174-181`). It also honors the surviving, narrower half of Phase-11 Decision L (no mid-flight re-arm), while un-deferring the config-visibility half that #81 targets.

- **Decision B — Full config surface.** Patrol reads config from three cached places: `self.camp_config` (agent resolution `patrol.rs:426`, rig lookup `:784`, dispatch command `:823/:924/:949/:1000/:1010`, exec_timeout `:919/:995`, root `:799`), `self.config: PatrolConfig` (default `stall_after` `:428/:446/:678/:751/:849`, `release_grace` `:713`), and the ladder's `restart_budget` (seeded at construction `:210`, read live per fire in `Ladder::on_fire`). A complete swap updates all three so `self.config` and the ladder never disagree about the budget. Leaving `restart_budget` stale would reproduce the same "keeps birth config" defect class in a narrower form.

- **Decision C — `Ladder::set_restart_budget` (additive camp-core change).** The ladder's `restart_budget` is the only config-derived value patrol caches in a stateful sub-object. `Ladder::on_fire` reads `self.restart_budget` live (`camp-core/src/patrol/mod.rs:115`), so updating the field takes effect on future fires while preserving every per-bead `LadderState`. Rebuilding the ladder is rejected — it would discard live per-bead restart counts. A new `pub fn set_restart_budget(&mut self, restart_budget: u32)` is the minimal, additive way to keep the ceiling current. `crates/camp-core/src/patrol/mod.rs` is not owned by any sibling work stream and this change is purely additive.

- **Decision D — `apply_config` is fallible (`-> Result<()>`), propagated with `?`.** It re-derives `PatrolConfig::from_section(&config.patrol)?`. For an applied config this cannot fail (see Root Cause "non-issues"); the `?` is fail-fast on an impossible state, never a fallback, and never an `.unwrap()`. The `CONFIG_WATCH` arm lives inside `run() -> Result<()>`, so `patrol.apply_config(...)?` propagates cleanly.

- **Decision E — Reproduction is an end-to-end integration test in `daemon_patrol.rs`.** The bug is a WIRING gap; the only faithful test drives a real reload through the daemon's `CONFIG_WATCH` arm (a pure unit test of the method cannot exercise the inline arm). `daemon_patrol.rs` already spawns a real campd with a hermetic `CLAUDE_CONFIG_DIR` (needed so patrol's transcript paths/watches stay inside the tempdir) and has `wait_until`, `scaffold`, `count`, `events_json`. The test mirrors the issue #28 dispatcher test's structure. A fast method-level unit test in `patrol.rs` (Task 2) additionally isolates `apply_config` for a tight TDD loop.

- **Decision F — No new event type, no spec edit, no escalation.** The failure signature uses the existing `patrol.degraded` and `agent.stalled` events. Decision L is a reversible plan-time deferral, not a spec decision (see Root Cause). The module-header comment that documents the old behavior IS updated in this PR so code and its own docs never diverge.

- **Decision G — restart-budget propagation test scope.** `Ladder::set_restart_budget` gets a direct unit test proving the ceiling changes on future fires while per-bead state is preserved (Task 1). `apply_config`'s one-line delegation to it is a call to that tested function; it is not separately re-exercised end-to-end (reaching the exhausted branch through the daemon needs multiple re-armed fires and adds no confidence over the direct test). This is a deliberate, flagged scope boundary, not an omission.

---

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `crates/camp-core/src/patrol/mod.rs` | Modify: add `Ladder::set_restart_budget` (~line 149, after `restarts`) + a unit test in the existing `mod tests` | Let the escalation ladder accept a hot-reloaded restart-budget ceiling without losing per-bead state. |
| `crates/camp/src/daemon/patrol.rs` | Modify: add `PatrolRuntime::apply_config` (in `impl PatrolRuntime`, near the other config-facing methods) + update the module-header doc comment (lines 15-16) + a unit test in the existing `mod tests` | Swap patrol's whole config surface on an applied reload. |
| `crates/camp/src/daemon/event_loop.rs` | Modify: the `if applied { … }` block in the `CONFIG_WATCH` arm (lines 293-296) | Push the applied reload into patrol alongside dispatcher and graph. |
| `crates/camp/tests/daemon_patrol.rs` | Modify: add one end-to-end reproduction test + a tiny pack-fixture helper | The acceptance test: a reloaded pack agent is resolved by patrol with no restart and no `patrol.degraded`. Dies against main's behavior / the missing wiring. |

---

### Task 1: `Ladder::set_restart_budget` (camp-core)

**Files:**
- Modify: `crates/camp-core/src/patrol/mod.rs` (add method after `restarts`, ~line 149; add a test in `mod tests`, after the existing ladder tests ~line 240)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn Ladder::set_restart_budget(&mut self, restart_budget: u32)` — sets the live ceiling read by `on_fire`; per-bead `LadderState` (restart counts, next step) is preserved.

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)] mod tests` block in `crates/camp-core/src/patrol/mod.rs` (the module already has `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` and `use super::*;`):

```rust
#[test]
fn set_restart_budget_takes_effect_on_future_fires_and_preserves_state() {
    // A budget of 0 would exhaust on the fire after the first nudge…
    let mut ladder = Ladder::new(0);
    assert_eq!(ladder.on_fire("gc-1"), LadderAction::Nudge);
    // …until a hot reload raises it: the very next fire restarts instead.
    ladder.set_restart_budget(1);
    assert_eq!(
        ladder.on_fire("gc-1"),
        LadderAction::Restart,
        "the reloaded budget must govern the next fire"
    );

    // Lowering the ceiling below a bead's existing restart count exhausts
    // it next fire, but the per-bead restart history is NOT reset.
    let mut ladder = Ladder::new(5);
    assert_eq!(ladder.on_fire("gc-2"), LadderAction::Nudge);
    assert_eq!(ladder.on_fire("gc-2"), LadderAction::Restart); // restarts -> 1
    assert_eq!(ladder.on_fire("gc-2"), LadderAction::Nudge);
    assert_eq!(ladder.on_fire("gc-2"), LadderAction::Restart); // restarts -> 2
    assert_eq!(ladder.restarts("gc-2"), 2);
    ladder.set_restart_budget(1); // now below the bead's 2 restarts
    assert_eq!(ladder.on_fire("gc-2"), LadderAction::Nudge);
    assert_eq!(
        ladder.on_fire("gc-2"),
        LadderAction::Exhausted,
        "a lowered budget exhausts the next restart fire"
    );
    assert_eq!(
        ladder.restarts("gc-2"),
        2,
        "the reloaded budget must not reset per-bead restart history"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails.**

Run: `cargo test -p camp-core patrol::tests::set_restart_budget_takes_effect_on_future_fires_and_preserves_state`
Expected: FAIL to compile — `no method named set_restart_budget found for struct Ladder`.

- [ ] **Step 3: Implement the method.** Insert into `impl Ladder` in `crates/camp-core/src/patrol/mod.rs`, immediately after the `restarts` method (after line 149):

```rust
    /// Follow a hot-reloaded `[patrol] restart_budget` (issue #81). The
    /// ceiling is read live per fire (`on_fire`), so future fires honor the
    /// new budget; every per-bead restart count and next-step is preserved
    /// — a reload adjusts the ceiling, it does not rewrite history.
    pub fn set_restart_budget(&mut self, restart_budget: u32) {
        self.restart_budget = restart_budget;
    }
```

- [ ] **Step 4: Run the test to verify it passes.**

Run: `cargo test -p camp-core patrol::tests::set_restart_budget_takes_effect_on_future_fires_and_preserves_state`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/camp-core/src/patrol/mod.rs
git commit -m "feat(patrol): Ladder::set_restart_budget for hot reload (#81)"
```

---

### Task 2: `PatrolRuntime::apply_config` + module-header doc update (camp)

**Files:**
- Modify: `crates/camp/src/daemon/patrol.rs` — add `apply_config` to `impl PatrolRuntime`; update the module-header comment (lines 15-16); add a unit test in the existing `#[cfg(test)] mod tests`.

**Interfaces:**
- Consumes: `Ladder::set_restart_budget` (Task 1); `PatrolConfig::from_section(&PatrolSection) -> Result<PatrolConfig, CoreError>` (`camp-core`, already imported); `camp_core::config::CampConfig`.
- Produces: `pub fn PatrolRuntime::apply_config(&mut self, config: CampConfig) -> anyhow::Result<()>` — swaps `self.camp_config`, `self.config` (derived `PatrolConfig`), and the ladder's restart-budget ceiling. Called by Task 3.

**Notes for the implementer:**
- `PatrolConfig`, `Ladder`, and `parse_duration` are already imported at `patrol.rs:32`; `CampConfig` at `:27`; `anyhow::Result` at `:23`.
- `PatrolRuntime` fields `config`, `camp_config`, and `ladder` are defined at `patrol.rs:126-152`.
- The test module near the bottom of `patrol.rs` already carries the `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` opt-out and `use super::*;` — confirm this before writing the test (search for `mod tests` in the file); if a needed import (`camp_core::ledger::Ledger`, `camp_core::event::EventInput`, `EventType`, `serde_json`) is not already in scope there, add it inside the test.

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)] mod tests` block in `crates/camp/src/daemon/patrol.rs`:

```rust
/// #81: after apply_config swaps in a config whose pack ships an agent the
/// BIRTH config could not see, patrol resolves that agent — no
/// patrol.degraded, and the agent's own stall_after governs the armed
/// timer (proving resolution ran against the reloaded config, not the
/// birth one).
#[test]
fn apply_config_lets_patrol_resolve_a_reloaded_pack_agent() {
    use camp_core::config::CampConfig;
    use camp_core::event::{EventInput, EventType};
    use camp_core::ledger::Ledger;

    let dir = tempfile::tempdir().unwrap();
    // A pack shipping agent "sentry" with a DISTINCT stall_after override.
    let pack = dir.path().join("sentrypack");
    std::fs::create_dir_all(pack.join("agents")).unwrap();
    std::fs::write(
        pack.join("agents/sentry.md"),
        "---\nname: sentry\nisolation: none\nstall_after: 700ms\n---\nWork.\n",
    )
    .unwrap();

    // Birth config: NO packs, a distinct camp-default stall_after of 5s.
    let birth_toml = "[camp]\nname = \"t\"\n\n[patrol]\nstall_after = \"5s\"\n";
    std::fs::write(dir.path().join("camp.toml"), birth_toml).unwrap();
    let birth = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
    let patrol_config =
        camp_core::patrol::PatrolConfig::from_section(&birth.patrol).unwrap();
    let mut patrol = PatrolRuntime::new(patrol_config, &birth);

    // Reloaded config: adds the pack (so "sentry" becomes resolvable).
    let reloaded_toml = format!(
        "packs = [\"{}\"]\n\n[camp]\nname = \"t\"\n\n[patrol]\nstall_after = \"5s\"\n",
        pack.display()
    );
    std::fs::write(dir.path().join("camp.toml"), &reloaded_toml).unwrap();
    let reloaded = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
    patrol.apply_config(reloaded).unwrap();

    // Drive a campd-spawned (Owned::Child) worker for the pack agent through
    // observe -> apply_tracking, exactly as the settle path does. Append the
    // session.woke through the ledger and read it back so the Event has the
    // exact shape the fold produces (mirrors event_loop.rs's test
    // a_due_stall_declares_and_the_settle_executes_the_action).
    let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"title": "t"}),
        })
        .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({
                "name": "t/sentry/1",
                "agent": "sentry",
                "transcript_path": dir.path().join("projects/-p/sid.jsonl"),
                "bead": "gc-1",
            }),
        })
        .unwrap();
    let events = ledger.events_range(1, None).unwrap();
    let woke = events
        .iter()
        .find(|e| e.kind == EventType::SessionWoke)
        .unwrap();
    patrol.observe(woke);
    let now = jiff::Timestamp::now();
    patrol.apply_tracking(&mut ledger, now).unwrap();

    // No unknown-agent degradation: resolution ran against the reloaded config.
    let degraded = ledger.events_of_type(EventType::PatrolDegraded).unwrap();
    assert!(
        degraded.is_empty(),
        "patrol must resolve the reloaded pack agent, got: {degraded:?}"
    );

    // And the arm used the AGENT's 700ms override, not the 5s camp default:
    // fire it just past 700ms and read the declared threshold.
    let later = now
        .checked_add(jiff::SignedDuration::from_millis(750))
        .unwrap();
    let fires = patrol.fire_due(later);
    assert_eq!(fires.len(), 1, "the 700ms agent threshold fired by 750ms");
    patrol.declare_stalls(&mut ledger, &fires, later).unwrap();
    let stalled = ledger.events_of_type(EventType::AgentStalled).unwrap();
    assert_eq!(
        stalled[0].data["threshold"], "700ms",
        "patrol armed at the reloaded agent's stall_after, not the camp default"
    );
}
```

> Implementer note: the `EventInput` / `events_range` / `events_of_type` calls above are the exact APIs the neighboring `patrol.rs` and `event_loop.rs` tests already use. If any signature differs on this branch, match the surrounding tests rather than inventing a call.

- [ ] **Step 2: Run the test to verify it fails.**

Run: `cargo test -p camp patrol::tests::apply_config_lets_patrol_resolve_a_reloaded_pack_agent`
Expected: FAIL to compile — `no method named apply_config found for struct PatrolRuntime`.

- [ ] **Step 3: Implement `apply_config`.** Add to `impl PatrolRuntime` in `crates/camp/src/daemon/patrol.rs` (place it near `stalled_count`/`filter_slot`, after `new`, ~line 235):

```rust
    /// Swap patrol's config on an applied hot reload (issue #81). Patrol
    /// resolves agents, rig lookups, the dispatch command, and stall/
    /// release thresholds against the config it holds; an applied reload
    /// that adds a pack/agent/rig or edits `[patrol]` must reach patrol too,
    /// or a worker dispatched to a freshly added pack agent draws a spurious
    /// `patrol.degraded` "unknown agent" (the birth config cannot see it).
    ///
    /// FUTURE resolutions and future timer arms see the new config; in-flight
    /// timers and tracked workers are NOT re-armed — each armed worker keeps
    /// the threshold it was armed with, exactly as the dispatcher leaves
    /// in-flight children on their already-resolved spec. The ladder's
    /// per-bead restart history is preserved; only its `restart_budget`
    /// ceiling follows the reload.
    ///
    /// An applied config is pre-validated (`CampConfig::parse` runs
    /// `PatrolConfig::from_section`), so the re-derivation cannot fail for an
    /// applied reload; the `?` is fail-fast on an impossible torn state,
    /// never a silent fallback.
    pub fn apply_config(&mut self, config: CampConfig) -> Result<()> {
        let patrol_config = PatrolConfig::from_section(&config.patrol)?;
        self.ladder.set_restart_budget(patrol_config.restart_budget);
        self.config = patrol_config;
        self.camp_config = config;
        Ok(())
    }
```

- [ ] **Step 4: Run the test to verify it passes.**

Run: `cargo test -p camp patrol::tests::apply_config_lets_patrol_resolve_a_reloaded_pack_agent`
Expected: PASS.

- [ ] **Step 5: Update the module-header doc comment.** In `crates/camp/src/daemon/patrol.rs`, replace the now-obsolete lines 15-16:

```rust
//! Patrol config is read at campd start; hot reload does not re-arm
//! patrol (plan Decision L).
```

with:

```rust
//! Patrol config is swapped on an applied hot reload (issue #81,
//! `apply_config`): future agent/rig/threshold resolutions and future
//! timer arms follow the reloaded config with no campd restart. In-flight
//! stall timers are NOT re-armed — each tracked worker keeps the threshold
//! it was armed with (the surviving half of Phase-11 plan Decision L; the
//! config-visibility half is un-deferred by #81).
```

- [ ] **Step 6: Run fmt + clippy on the crate to catch lint issues early.**

Run: `cargo fmt --all --check && cargo clippy -p camp --all-targets --all-features -- -D warnings`
Expected: clean (no diff, no warnings).

- [ ] **Step 7: Commit.**

```bash
git add crates/camp/src/daemon/patrol.rs
git commit -m "feat(patrol): apply_config swaps the runtime's config on hot reload (#81)"
```

---

### Task 3: Wire `apply_config` into `CONFIG_WATCH` + end-to-end reproduction (camp)

**Files:**
- Modify: `crates/camp/src/daemon/event_loop.rs` — the `if applied { … }` block (lines 293-296).
- Modify: `crates/camp/tests/daemon_patrol.rs` — add the reproduction test + a tiny pack-fixture helper.

**Interfaces:**
- Consumes: `PatrolRuntime::apply_config` (Task 2); the existing `daemon_patrol.rs` harness — `scaffold(dir, patrol_toml, agents) -> (root, rig)`, `Daemon::spawn(root, claude_dir, envs)`, `camp_ok(root, args)`, `wait_until(root, what, pred)`, `events_json(root)`, `count(events, kind)`, `fake_agent()`.
- Produces: nothing consumed by later tasks (final code task).

**This task is the acceptance test. It follows TDD at the wiring level: write the end-to-end test, run it and watch it FAIL because the `CONFIG_WATCH` arm does not yet call `patrol.apply_config` (main's behavior — patrol keeps the birth config), then add the one-line wiring and watch it PASS. Reverting only that one line reproduces the failure, proving the wiring is load-bearing.**

- [ ] **Step 1: Add the reproduction test — the failing test.** Append to `crates/camp/tests/daemon_patrol.rs` (the file header already has the clippy opt-out and imports `Path`, `PathBuf`, `Duration`, `Instant`, etc.):

```rust
/// #81: a hot reload that adds a PACK shipping a new agent must reach
/// PATROL, not just the dispatcher. A worker dispatched to the reloaded
/// pack agent (with NO campd restart) is resolved by patrol: the stall it
/// declares carries the AGENT's own stall_after — not the camp default the
/// stale birth config would fall back to — and no patrol.degraded is
/// emitted. Against main (the CONFIG_WATCH arm never calls
/// patrol.apply_config) patrol keeps its birth config, so it cannot see the
/// pack agent: it falls back to the 5s camp default and logs a
/// patrol.degraded "unknown agent" — both assertions fail.
#[test]
fn a_hot_reloaded_pack_agent_is_resolved_by_patrol_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    // Birth camp: a distinct 5s camp-default stall_after, one local "dev"
    // agent so the base config is runnable; no packs yet.
    let (root, rig) = scaffold(
        dir.path(),
        "stall_after = \"5s\"",
        &[("dev", "isolation: none\n")],
    );

    // A throwaway pack shipping agent "sentry" with a DISTINCT 700ms
    // stall_after override. camp's pack loader needs only <pack>/agents/*.md.
    let pack = dir.path().join("sentrypack");
    std::fs::create_dir_all(pack.join("agents")).unwrap();
    std::fs::write(
        pack.join("agents/sentry.md"),
        "---\nname: sentry\nisolation: none\nstall_after: 700ms\n---\nWork.\n",
    )
    .unwrap();

    // FAKE_AGENT_NUDGE_CLOSE=1: the worker goes silent (stalls), then closes
    // on the nudge so no fake-agent process outlives the daemon.
    let _campd = Daemon::spawn(
        &root,
        &dir.path().join("claude-home"),
        &[("FAKE_AGENT_NUDGE_CLOSE", "1")],
    );

    // Hot-add the pack and route new beads to its agent — NO restart. `packs`
    // is a top-level key, so it precedes every [table] header (TOML).
    let reloaded = format!(
        "packs = [\"{}\"]\n\n[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\n\
         path = \"{}\"\nprefix = \"gc\"\n\n[dispatch]\nmax_workers = 4\n\
         command = \"{}\"\ndefault_agent = \"sentry\"\n\n[patrol]\nstall_after = \"5s\"\n",
        pack.display(),
        rig.display(),
        fake_agent(),
    );
    std::fs::write(root.join("camp.toml"), &reloaded).unwrap();
    wait_until(&root, "the applied reload", |e| {
        e.iter()
            .any(|ev| ev["type"] == "config.changed" && ev["data"]["applied"] == true)
    });

    // Dispatch a bead to the freshly added agent.
    camp_ok(&root, &["sling", "watch me"]);

    // Patrol must resolve "sentry" from the reloaded pack: the first stall it
    // declares for this worker carries the agent's 700ms threshold.
    wait_until(&root, "the sentry stall", |e| {
        e.iter()
            .any(|ev| ev["type"] == "agent.stalled" && ev["data"]["agent"] == "sentry")
    });
    let events = events_json(&root);
    let first_stall = events
        .iter()
        .find(|e| e["type"] == "agent.stalled" && e["data"]["agent"] == "sentry")
        .unwrap();
    assert_eq!(
        first_stall["data"]["threshold"], "700ms",
        "patrol must arm at the reloaded pack agent's stall_after, not the camp default; events: {events:#?}"
    );
    assert_eq!(
        count(&events, "patrol.degraded"),
        0,
        "no unknown-agent degradation once the reload reaches patrol; events: {events:#?}"
    );

    // The nudge revives and closes it: clean shutdown, no lingering worker.
    wait_until(&root, "the revived close", |e| {
        e.iter().any(|ev| ev["type"] == "session.stopped")
    });
}
```

- [ ] **Step 2: Run the test to verify it FAILS against the un-wired arm.**

Run: `cargo test -p camp --test daemon_patrol a_hot_reloaded_pack_agent_is_resolved_by_patrol_without_a_restart -- --nocapture`
Expected: FAIL. Patrol keeps the birth config → the first `agent.stalled` `threshold` is `"5s"` (not `"700ms"`) and a `patrol.degraded` "unknown agent" event is present, so both assertions fail. (This is main's behavior — the wiring in Step 3 is what the test is pinning.)

- [ ] **Step 3: Add the one-line wiring.** In `crates/camp/src/daemon/event_loop.rs`, extend the `if applied { … }` block (currently lines 293-296) so patrol joins its two siblings:

```rust
                        if applied {
                            dispatcher.apply_config(runtime.config().clone());
                            graph.apply_config(runtime.config());
                            // Issue #81: patrol resolves agents/rigs/
                            // thresholds against its own cached config too —
                            // an applied reload must reach it, or a worker
                            // dispatched to a freshly added pack agent draws a
                            // spurious patrol.degraded "unknown agent".
                            patrol.apply_config(runtime.config().clone())?;
                        }
```

- [ ] **Step 4: Run the reproduction test to verify it PASSES.**

Run: `cargo test -p camp --test daemon_patrol a_hot_reloaded_pack_agent_is_resolved_by_patrol_without_a_restart -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full patrol + orders integration suites (guard against regressions in the arm and the patrol harness).**

Run: `cargo test -p camp --test daemon_patrol --test daemon_orders`
Expected: PASS (all existing tests, including `a_hot_reload_updates_dispatch_routing_without_a_restart`, stay green).

- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/daemon/event_loop.rs crates/camp/tests/daemon_patrol.rs
git commit -m "fix(campd): an applied hot reload reaches patrol, not just dispatch (#81)"
```

---

### Task 4: Full gates + PR

**Files:** none (verification + PR).

- [ ] **Step 1: Run all three gates from the workspace root.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: fmt clean, clippy clean (no `-D warnings` hits), all workspace tests pass.

- [ ] **Step 2: Push the branch and open the PR.**

```bash
git push -u origin fix-81-patrol-config-reload
gh pr create --fill --base main --head fix-81-patrol-config-reload
```

- [ ] **Step 3: Foreground-watch CI to the settled result.**

Run: `gh pr checks --watch`
Expected: all checks green. Do not report completion until CI has settled green.

- [ ] **Step 4: Report to the lead** — PR number, settled CI status, and each acceptance criterion quoted line by line with its evidence:
  - "The reproduction test fails against main's behavior and passes with the fix" → Task 3 Steps 2 (FAIL: threshold `"5s"` + `patrol.degraded` present) and 4 (PASS), test `a_hot_reloaded_pack_agent_is_resolved_by_patrol_without_a_restart`.
  - "no polling introduced anywhere" → the fix swaps in-memory config inside the existing `CONFIG_WATCH` event arm; no timer, tick, or loop added (Global Constraints); the idle-CPU perf gate is unaffected.
  - "CI green" → Task 4 Step 3 output.

---

## Self-Review

**1. Spec / kickoff coverage.**
- "give PatrolRuntime an apply_config" → Task 2.
- "wire it into event_loop.rs's CONFIG_WATCH arm alongside dispatcher.apply_config / graph.apply_config" → Task 3 Step 3.
- "a test that reproduces the observed failure (an applied reload adds an agent; patrol must resolve it without a campd restart) and dies against the missing wiring" → Task 3 (integration reproduction; fails at Step 2 without the wiring, passes at Step 4 with it).
- "no polling introduced anywhere" → no timers/loops added; verified in Task 4 Step 4.
- "CI green" → Task 4.
- Complete config surface (Decision B) — `restart_budget` propagation → Task 1 + the `apply_config` body in Task 2.
- Code/doc divergence avoided → module-header update, Task 2 Step 5.

**2. Placeholder scan.** No "TBD/handle edge cases/similar to Task N". Every code step shows complete code. The one implementer judgement call — matching `EventInput`/`events_range`/`events_of_type` signatures to this branch — is flagged with the exact neighboring tests to follow, not left vague.

**3. Type consistency.** `set_restart_budget(&mut self, restart_budget: u32)` — same name/signature in Task 1 (definition), Task 2 (`apply_config` call), and Decision C. `apply_config(&mut self, config: CampConfig) -> Result<()>` — same in Task 2 (definition) and Task 3 (`patrol.apply_config(runtime.config().clone())?`). `PatrolConfig::from_section`, `CampConfig::load`, `events_of_type`, `events_range`, `fire_due`, `declare_stalls`, `observe`, `apply_tracking` are all existing APIs used with their real signatures (verified against `patrol.rs` / `pack.rs` / `config.rs` / `patrol/mod.rs` on this branch).

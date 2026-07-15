# Compat Phase 3 — The gc Worker Contract Implementation Plan

## Plan-gate approval (2026-07-14)
APPROVED by the adversarial 4-panelist plan gate. Rounds: R1 REJECT (12 findings B1-B12, all fixed in 606a147) → R2 REJECT (2 critic findings, fixed in 487ccd7) → R3 APPROVE. Final lens verdicts: contract/interface/execution APPROVE (round 2), execution/critic re-audit APPROVE (round 3).
Accepted rulings/deviations (verified by the panels against merged code):
- B1: route ownership is split — cook stamps beads.assignee (the qualified route) at BeadCreated; the claim stamps only work_branch. This satisfies §6.1's "one row, three projections, no second formatter" (all three fields land on the one bead row that bd_metadata projects). The claim must NOT re-stamp route from GC_AGENT env.
- B4: the release event is named worker.drain_acked (additive; NOT a mirror of gc's session.drain_acked_with_assigned_work, which is gc's acked-while-still-holding-work anomaly with no camp counterpart).
- §6.2 reconciled: bead-close only drops stdin + arms the 30s grace (never kills); drain-ack is the PROMPT kill via kill_released on the already-released worker. bd show --json metadata comes from readiness::bead_metadata (the one formatter); new events route through audit::<T> with deny_unknown_fields.
Non-blocking notes to FOLD IN during execution (the reviewer accepted these as cheap improvements — implement them, do not skip):
1. Task 1: add a superset assertion that verbs_static ⊇ the recorded dynamic `verbs`, so an under-capturing grep (a variable- or eval-indirected gc/bd invocation) cannot silently shrink verbs_static and pass reconciliation.
2. Task 11: the §14 gate must keep release_grace (30s default) > the watchdog deadline (~20s) so the grace backstop cannot mask a drain-ack→KillReleased regression; state this and do NOT set release_grace below the deadline.
3. Task 11 RED path: on the deadline/failure path, process-group-kill the `exec sleep 600` worker so a failing test does not orphan a 600s sleep (the worker is campd's grandchild).
4. Heads-up (no change required): beads.work_branch gets a second writer at bead_closed, so gc.work_branch is not durable past a non-shipped close — fine for the fragment contract; note it.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Revision 2 (plan-gate round 1 findings B1–B12 + non-blocking).** The central fix: the claim transaction does **NOT** re-stamp the route — cook already owns `beads.assignee` (= gc's `gc.routed_to`) at `BeadCreated` (cook.rs:407), so re-stamping it from `GC_AGENT` env would make the §6.1 projection equal *by construction* and re-admit the rev-3 bug. The claim stamps only `work_branch`; every projection reads the route from the **bead row** (via `readiness::bead_metadata`, the one formatter), and the guard fixtures set env `GC_AGENT` ≠ the cooked route so an independent re-derivation goes RED.

**Goal:** A downloaded Gas City pack worker closes a Gas City bead end-to-end inside camp — running its own unmodified 140-line bash fragment against `gc`/`bd` shims that campd puts on the worker's PATH — proven by executing the REAL `gc-role-worker` fragment from the corpus at `GCPACKS_REF` and asserting it claims, closes, drain-acks, and exits under a deadline.

**Architecture:** campd installs two `#!/bin/sh` argv-translator shims (`.camp/bin/gc`, `.camp/bin/bd`) that `exec` camp's own absolute binary as `camp gc-shim …` / `camp bd-shim …`, and prepends `.camp/bin` to the worker child's PATH only. The shims are the sole new ledger-touching surface; `camp` stays the one process that writes `camp.db`. The claim invariant lives on the **bead row** (§6.1): cook already stamps `beads.assignee` (the qualified route, projected as `gc.routed_to`, readiness.rs:71-72); the claim transaction stamps `claimed_by` (the session) and `work_branch` (the dispatch branch, projected as `gc.work_branch`). The hook JSON, `bd show --json`, and the worker's environment are three projections of that one row, and every projection of the route/branch reads it back through the single existing formatter `readiness::bead_metadata` (readiness.rs:201). `runtime drain-ack` is campd's prompt **kill** trigger; the bead-close grace timer is the backstop for a worker that never acks.

**Tech Stack:** Rust (workspace crates `camp`, `camp-core`), clap subcommands, rusqlite/SQLite ledger, `serde`/`serde_json`, POSIX `sh` shims, a `ci/gc-compat` Python gate driving the real `camp` binary against the corpus fetched at `GCPACKS_REF`, and `contrib/docker`.

## The camp↔gc column inversion (orientation — read once, it recurs everywhere)

gc and camp name the two identity columns oppositely; every task below depends on getting this right:

| fact | gc name | camp column | how it is read |
|---|---|---|---|
| the qualified agent, e.g. `gc.run-operator` | `gc.routed_to` (metadata) | `beads.assignee` | projected as `gc.routed_to` by `readiness::bead_metadata` (PROJECTED_METADATA, readiness.rs:71) — **set at cook** (cook.rs:407) |
| the worker session, e.g. `t/gc.run-operator/1` | `assignee` | `beads.claimed_by` | read directly — **set at claim** |
| the dispatch branch, e.g. `camp/gc-2` | `gc.work_branch` (metadata) | `beads.work_branch` | projected as `gc.work_branch` by `readiness::bead_metadata` (readiness.rs:72) — **set at claim** (this phase) |

So: **`bd show --json`'s top-level `assignee` field is camp's `claimed_by`; its `metadata."gc.routed_to"` is camp's `assignee` column.** The claim stamps `claimed_by` + `work_branch` and NEVER `assignee` (cook owns it).

## Global Constraints

Every task's requirements implicitly include this section. Values are copied verbatim from the spec and AGENTS.md; do not paraphrase them into code.

- **Fail fast, no fallbacks, no silenced errors.** An unknown shim verb/flag, an unresolved binding, an unspawnable agent, a missing `python3` — each surfaces to the caller AND lands in the ledger. Never a no-op (spec §6: "a silently-ignored `bd update --set-metadata gc.outcome=pass` is a corrupted ledger").
- **No panics in library code.** `clippy::unwrap_used`/`expect_used`/`panic` are denied outside `#[cfg(test)]`; `unsafe_code` forbidden. (The shim `main` arm's `std::process::exit` is not a panic — it is the deliberate, documented exit-code channel; see Task 4.)
- **New event payload structs use `#[serde(deny_unknown_fields)]` and route through `audit::<T>`** (fold.rs:71) so the parse validates the shape at append time — never a bare `=> Ok(())` that drops validation. Extend an existing payload only by adding `#[serde(default)]` fields (backward-readable ledgers — invariant 3).
- **One transaction for event + state.** Every state change is one appended event whose fold is one SQLite transaction; `append` rolls back entirely on `Err` ("rejections appended nothing"); `refold_prop.rs` replays the accepted prefix and must reproduce the state byte-for-byte.
- **Vocabulary mirror (invariant 7).** Every new event name is declared in `crates/camp-core/src/vocab.rs::CAMP_SPECIFIC_EVENTS` and MUST NOT exist in gc's registry (`crates/camp-core/tests/fixtures/gc-vocab.json`) — including as a **truncation** of a gc name. `tests/vocab_pin.rs` enforces exact-string disjointness; the plan additionally avoids near-truncations by construction (Task 2).
- **The shim embeds camp's ABSOLUTE path** (`std::env::current_exe()`), never `exec camp` by bare name (§6.3).
- **`.camp/bin` is gitignored** (§6.3): `gitignore::RUNTIME_DIRS` gains `bin`.
- **Attended sessions get no shims** (§6.3): gc pack agents are campd-dispatch-only.
- **Tests use no network and spend no API.** Git-backed imports run against local `file://` repos in a temp dir; workers are `#!/bin/sh` fakes; never a real `claude`.
- **`python3` is a hard runtime dependency** of the gc worker contract (§6.1) and must be in `contrib/docker/`.
- **Every new test must die against a mutation of the code it guards** (§14). The step says which mutation.
- **Branch:** `compat-3-worker-contract`. Never commit to `main`. No co-author lines.
- **Already merged, do NOT re-do:** #86 (`--verbose`) landed in fix-86 (#88); spawn.rs:199 already emits it. `PROJECTED_METADATA` already maps `gc.routed_to → assignee`, `gc.work_branch → work_branch` (readiness.rs:71-72) and `readiness::bead_metadata` (readiness.rs:201) is the one formatter that emits both. The `beads.work_branch` column exists (schema.rs:32). Cook stamps `assignee = <qualified route>` at `BeadCreated` (cook.rs:407) and carries step `metadata` onto the bead (cook.rs:412). The binding namespace + `resolve_agent(cfg, "<binding>.<agent>")` are merged (pack.rs:251).

## Operator ruling — MEASURE gc, do not infer it

Spec §6.1 quotes a five-line excerpt of the `gc-role-worker` fragment; that excerpt is **not** the contract. The operator ruling is binding: **build the shim first and measure the real fragment's behavior — on ALL its branches, not just the happy path** — and where the plan leans on a gc behavior it says so and cites how it is measured. Task 1 fetches the fragment at `GCPACKS_REF`, drives it under `sh` through its happy, fail-close, AND config-reject branches with a recording stub, and commits the observed contract (the FULL verb/flag set, JSON field names, exit-code expectations) as the fixture every later task asserts against. No task below hard-codes a fragment fact Task 1 did not observe; where a step names a field it is the spec's claim to be **confirmed** by the recording, and the step says so.

---

## File Structure

**New files:**

- `crates/camp/src/cmd/shim/mod.rs` — the shim entry points (`gc-shim`, `bd-shim` dispatch), the `ShimExit` type, the `shim.refused` emitter, the shared refusal helper.
- `crates/camp/src/cmd/shim/install.rs` — `.camp/bin` generation (absolute-path `sh` scripts), PATH-prepend helper.
- `crates/camp/src/cmd/shim/project.rs` — `claim_projection(conn, bead) -> ClaimProjection { assignee, route, work_branch }`: reads `claimed_by` directly and `route`/`work_branch` **through `readiness::bead_metadata`** (the one formatter — no second projection).
- `crates/camp/src/cmd/shim/hook.rs` — `gc hook --claim --json`.
- `crates/camp/src/cmd/shim/bd.rs` — `bd show/update/close/list/ready/create`.
- `crates/camp/src/cmd/shim/runtime.rs` — `runtime drain-ack` + `convoy status --json`.
- `crates/camp/tests/worker_contract.rs` — the hermetic Rust integration test (real campd, fake claude running the fragment under `sh`, real ledger, real shims) with a deadline watchdog, plus the byte-projection equality test.
- `crates/camp/tests/fixtures/gc-fragment.sh` — a FAITHFUL synthetic fragment built from Task 1's recording (happy + fail-close branches).
- `crates/camp-core/tests/claim_invariant.rs` — Task 3's integration tests (cook a bead, claim it, assert the columns), using the cook-then-append harness that already exists in `cook.rs`/`refold_prop.rs`.
- `ci/gc-compat/worker_contract.py` — THE §14 gate.
- `ci/gc-compat/fixtures/gc-role-worker.observed.json` — Task 1's committed measurement (our derived facts + `fragment_path` + `gcpacks_ref`; NOT the fragment's copyrighted source — §10).

**Modified files:**

- `crates/camp/src/main.rs` — two `Command` variants `GcShim`/`BdShim` + two bespoke arms that convert `ShimExit` to a process exit code, bypassing `report()`. **Guaranteed-contention file: additive only.**
- `crates/camp/src/daemon/spawn.rs` — `build_spec` env + PATH; shim install at dispatch.
- `crates/camp/src/daemon/dispatch.rs` — install shims in `launch` (before spawn).
- `crates/camp/src/daemon/patrol.rs` — `worker.drain_acked` → prompt `kill_released`; bead-close release stays as the grace backstop. (The observation goes in `patrol::observe`, patrol.rs:301 — the same place that owns the `BeadClosed → PendingAction::Release` at patrol.rs:328.)
- `crates/camp/src/gitignore.rs` — `RUNTIME_DIRS += "bin"`.
- `crates/camp-core/src/event.rs` — `ShimRefused`, `WorkerDrainAcked` variants. **Additive only.**
- `crates/camp-core/src/vocab.rs` — the two names in `CAMP_SPECIFIC_EVENTS`, with the justification. **Additive only.**
- `crates/camp-core/src/ledger/fold.rs` — `BeadClaimed` gains `work_branch` (only); two audit-arm payload structs. **Additive only.**
- `contrib/docker/Dockerfile` — `python3` in the runtime apt install.
- `.github/workflows/ci.yml` — one new step in the `gc-compat` job.

---

## Task 1: Measure the real fragment on ALL its branches (SHIM FIRST — no Rust yet)

**Files:**
- Create: `ci/gc-compat/fixtures/gc-role-worker.observed.json`
- Create (scratch, not committed): a recording stub `gc`/`bd`

**Interfaces:**
- Produces: `gc-role-worker.observed.json` with `{ "gcpacks_ref", "fragment_path", "verbs": {"gc":[...], "bd":[...]}, "hook_json_fields":[...], "bd_show_json_fields":[...], "branches": {"happy":[...calls...], "fail_close":[...], "config_reject":[...]}, "exit_contract": {...}, "env_read":[...] }`. Every later task that names a fragment fact cites this file.

- [ ] **Step 1: Fetch the corpus at the pinned ref and locate the fragment**

```bash
REF=$(cat ci/gc-compat/GCPACKS_REF)
git clone https://github.com/gastownhall/gascity-packs.git /tmp/gcpacks
git -C /tmp/gcpacks checkout "$REF"
find /tmp/gcpacks -name '*role-worker*' | sort
grep -rl 'runtime drain-ack' /tmp/gcpacks/gascity /tmp/gcpacks/gascity/roles 2>/dev/null
```

Expected: one `gc-role-worker` fragment under `gascity/roles/template-fragments/` (and `gascity/`). Record its repo-relative path.

- [ ] **Step 2: Run the fragment under `sh` with a RECORDING stub, on EVERY branch (B2)**

Write a scratch recording `gc`/`bd` (each appends its full argv to a log and returns a canned response chosen by an env-scripted sequence). Set the §6.1 env, put the stubs on PATH, run the rendered fragment under `sh` with a short deadline. Exercise, at minimum, THREE branches by scripting different response sequences:
1. **happy:** `hook`→work, `bd show`→claimed, `bd close`→ok, `hook`→drain, `runtime drain-ack`. Records the claim→close→drain-ack→exit path.
2. **fail-close:** the work step fails, so the fragment takes its `bd close --status fail` (or `--set-metadata gc.outcome=fail`) branch and any failure-reporting call (`mail send human`? `bd update`?) BEFORE draining. This is the branch B2 warns about — a verb here that phase-3 would refuse HANGS in production while happy-path tests stay green.
3. **config-reject:** `BEADS_ACTOR` unset (or `python3` absent) → the fragment prints `CONFIG_REJECTED`, runs `gc runtime drain-ack`, exits 0. Records the early-exit verb set.

Record, per branch, into the fixture: the exact `gc`/`bd` verbs+flags, the JSON fields the fragment's inline `python3` parses out of `hook --claim --json` and `bd show --json`, the exit-code contract (`hook` work=0/drain=1, `drain-ack` exit), and every env var read.

- [ ] **Step 3: EXHAUSTIVE static extraction — every verb in the fragment TEXT, not just the branches you drove (R2-B1)**

The 3-branch dynamic drive (Step 2) proves no-hang on the paths it scripts, but a verb reachable only on a FOURTH branch nobody scripted (a `mail send human` on some non-fail-close escalation, a `bd`/`convoy` call on a re-hook or progress path) never enters that recording — and in production it is refused, eaten by `set +e; sleep 2; continue`, and the worker hangs while every test stays green. So add an **exhaustive-by-construction** net that does not depend on which branch runs: statically extract EVERY `gc`/`bd` invocation from the fragment source and dedupe it:

```bash
FRAG=/tmp/gcpacks/<fragment_path>   # from Step 1
grep -oE '\b(gc|bd) [a-z][a-z-]*' "$FRAG" | sort -u
```

Record this deduped set in the fixture under `verbs_static` (distinct from the per-branch `verbs`). It is the ground truth the reconciliation checks against.

- [ ] **Step 4: Reconcile the static verb set against phase-3's served set**

The served set is: `gc hook --claim --json`, `gc runtime drain-ack`, `gc convoy status --json`, `bd show/update/close/list/ready/create`. Cross-check **`verbs_static`** (Step 3 — exhaustive over the whole fragment, every branch) — NOT only the three driven branches — against that served set. If ANY statically-extracted verb falls outside it (`prime`, `mail send`, a `bd`/`convoy` verb not listed): **STOP and report to the lead** — refusing it would hang the fragment, and moving it into phase-3 or accepting a documented refusal is a scope decision, not a guess. (Practical exposure is narrow — the served `bd` set is broad and §2 records `bd mol`/`ready`/`gate` as prohibitions-only — but exhaustive-by-construction is the requirement, not "the branches I thought to script".) Record the finding in the fixture either way.

- [ ] **Step 5: Commit** (the fragment text is NOT committed — §10; only our derived facts, incl. `verbs_static`)

```bash
git add ci/gc-compat/fixtures/gc-role-worker.observed.json
git commit -m "compat(worker): measure the real gc-role-worker fragment (all branches + static verb set)"
```

---

## Task 2: The two new ledger events, with validated audit arms

**Files:**
- Modify: `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`
- Test: `crates/camp-core/src/event.rs` (inline), `crates/camp-core/tests/vocab_pin.rs`, `crates/camp-core/src/ledger/fold.rs` (a new inline test module — see Step 5)

**Interfaces:**
- Produces: `EventType::ShimRefused` (`"shim.refused"`), `EventType::WorkerDrainAcked` (`"worker.drain_acked"`). Payloads `ShimRefused { binding: Option<String>, agent: Option<String>, verb: String, detail: String }` and `WorkerDrainAcked { session: String }`, both `#[serde(deny_unknown_fields)]`, routed through `audit::<T>` (fold.rs:71) — the parse IS the append-time validation (B7). Consumed by Tasks 5–8 (emit) and Task 10 (release trigger).

**Naming justification (B4 — record verbatim in a vocab.rs doc comment):** gc's registry carries `session.drain_acked_with_assigned_work`, `session.draining`, `session.undrained` (gc-vocab.json:22,15,16). camp's release signal is a **distinct concept**: camp truncates gc's continuation loop (§6.2), so a worker is one-bead-per-session and has **no assigned work remaining** at drain-ack — `worker.drain_acked` is campd's internal *release trigger*, not gc's *session-still-holding-work* state. The `worker.*` namespace is camp's (`worker.milestone`), and gc's only `worker.*` name is `worker.operation` (gc-vocab.json:63) — so `worker.drain_acked` is neither a gc name nor a prefix-truncation of one. `shim.refused` follows the merged `import.refused`/`formula.refused` precedent; gc has no `shim.*`.

- [ ] **Step 1: Write the failing test** (append to `event.rs` tests)

```rust
#[test]
fn shim_and_drain_ack_events_roundtrip_and_are_camp_specific() {
    for (variant, name) in [
        (EventType::ShimRefused, "shim.refused"),
        (EventType::WorkerDrainAcked, "worker.drain_acked"),
    ] {
        assert_eq!(variant.as_str(), name);
        assert_eq!(EventType::parse(name).unwrap(), variant);
        assert!(EventType::ALL.contains(&variant));
        assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&name));
        assert!(!crate::vocab::GC_MIRRORED_EVENTS.contains(&name));
    }
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p camp-core --lib event::tests::shim_and_drain_ack -- --nocapture`
Expected: FAIL — `no variant named ShimRefused`.

- [ ] **Step 3: Implement** — add both variants to `EventType`, `ALL`, `as_str`, and the two names to `CAMP_SPECIFIC_EVENTS` (with the justification doc comment above). In `fold.rs`, add the two payload structs (`deny_unknown_fields`) near the cp-1 audit structs, and two arms to `apply` (fold.rs:17): `EventType::ShimRefused => audit::<ShimRefused>(event),` and `EventType::WorkerDrainAcked => audit::<WorkerDrainAcked>(event),` — the exhaustive `match` will not compile until both arms exist, which is the guard that neither is forgotten.

- [ ] **Step 4: Run the event test + the vocab partition gate**

Run: `cargo test -p camp-core --lib event:: && cargo test -p camp-core --test vocab_pin`
Expected: PASS. Mutation caught: dropping either name from `CAMP_SPECIFIC_EVENTS` fails the partition; routing an arm to bare `=> Ok(())` would let a malformed event append (caught by Step 5).

- [ ] **Step 5: Prove the audit arms VALIDATE** (new inline `#[cfg(test)] mod tests` in `fold.rs` — the file has none today, B9; add one with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` per repo test convention)

```rust
#[test]
fn shim_refused_with_an_unknown_field_is_refused_at_append() {
    // build an in-memory Ledger; append ShimRefused with data {verb:"x", detail:"y", bogus:1};
    // assert the append returns Err (deny_unknown_fields), and nothing was stored.
    // Mutation caught: an arm written as `=> Ok(())` (accepts the malformed event).
}
```

Run: `cargo test -p camp-core --lib fold::tests::shim_refused_with_an_unknown_field` → PASS.

- [ ] **Step 6: Refold** — `cargo test -p camp-core --test refold_prop` → PASS (audit events fold to a state no-op and refold identically).

- [ ] **Step 7: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs
git commit -m "compat(worker): shim.refused + worker.drain_acked, validated audit arms"
```

---

## Task 3: The claim invariant — `BeadClaimed` stamps work_branch (NOT route)

**Files:**
- Modify: `crates/camp-core/src/ledger/fold.rs` (`BeadClaimed` struct fold.rs:269, `bead_claimed` fold.rs:273)
- Create: `crates/camp-core/tests/claim_invariant.rs`

**Interfaces:**
- Produces: `BeadClaimed { session, work_branch: Option<String> }`. When `work_branch` is `Some`, the SAME `UPDATE` that sets `claimed_by`/`status='in_progress'` sets `beads.work_branch`. **`beads.assignee` (the route → `gc.routed_to`) is NEVER touched — cook owns it (cook.rs:407).** Consumed by Task 6 (hook emits `{session, work_branch}`), Task 6's `claim_projection` (reads the columns back). `camp claim` (claim.rs) keeps emitting `{session}` only (`work_branch` `None` → column untouched).

**Durability note (heads-up, not a required change):** `beads.work_branch` now has a SECOND writer — `bead_closed` sets it on the `shipped` path (fold.rs:490). So a claim-stamped `gc.work_branch` is not durable past a `no-op`/`blocked`/`abandoned` (non-shipped) close, which leaves it as the claim value. This is fine for the fragment contract (the branch matters during the claim→close window, which is when the worker reads it); recorded so a later phase that needs post-close durability of `gc.work_branch` knows the two writers exist.

**Why not `route` (B1 — the central fix):** cook stamps `beads.assignee = <qualified route>` at `BeadCreated`. If the claim re-stamped it from `GC_AGENT` env via `COALESCE`, the bead's `gc.routed_to`, the hook's printed route, and env `GC_AGENT` would be equal *by construction* — the §6.1 byte-projection guard would confirm equality only when everything is equal by construction, re-admitting the exact rev-3 bug (a projection deriving the route from env instead of the bead). So the claim leaves `assignee` alone; every projection reads the route from the bead row, and Task 6/11 fixtures set `GC_AGENT` ≠ the cooked route to make a re-derivation observable.

- [ ] **Step 1: Write the failing tests** (`crates/camp-core/tests/claim_invariant.rs` — uses the cook-then-append harness that already exists in `cook.rs`; open a `Ledger`, `cook_with` a one-step formula whose step routes to `gc.publisher`, then append `BeadClaimed`)

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// harness mirrors crates/camp-core/tests/cook.rs (Ledger::open in a tempdir, cook_with a fixture formula)

#[test]
fn claim_stamps_session_and_work_branch_and_leaves_the_cooked_route_intact() {
    // cook a step bead "gc-2" whose assignee column = "gc.publisher" (the route).
    ledger.append(EventInput {
        kind: EventType::BeadClaimed, rig: None, actor: "gc-shim".into(),
        bead: Some("gc-2".into()),
        data: serde_json::json!({ "session": "t/gc.publisher/1", "work_branch": "camp/gc-2" }),
    }).unwrap();
    let row = read_bead(&ledger, "gc-2");
    assert_eq!(row.status, "in_progress");
    assert_eq!(row.claimed_by.as_deref(), Some("t/gc.publisher/1"));   // gc's assignee
    assert_eq!(row.assignee.as_deref(), Some("gc.publisher"));          // UNCHANGED — cook owns it
    assert_eq!(row.work_branch.as_deref(), Some("camp/gc-2"));          // → gc.work_branch
    // and the projection reads back the cooked route:
    let meta = camp_core::readiness::bead_metadata(conn, "gc-2").unwrap();
    assert_eq!(meta.get("gc.routed_to").map(String::as_str), Some("gc.publisher"));
    assert_eq!(meta.get("gc.work_branch").map(String::as_str), Some("camp/gc-2"));
}

#[test]
fn claim_without_work_branch_leaves_the_column_untouched() {
    // camp's own `camp claim {session}` path. Mutation caught: an UPDATE that always
    // nulls work_branch.
}

#[test]
fn bead_claimed_rejects_unknown_fields() {
    // {session, route:"x"} must Err at append — `route` is not a field (deny_unknown_fields).
    // This is the guard that the route CANNOT be smuggled in via the claim.
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p camp-core --test claim_invariant`
Expected: FAIL — the `work_branch` assertion fails (column stays NULL; the struct has no `work_branch` field yet, so it is ignored). NOTE: `bead_claimed_rejects_unknown_fields` currently PASSES for `route` (already unknown) — keep it; after Step 3 it still guards that `route` was not added.

- [ ] **Step 3: Implement** — extend the struct and fold arm:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadClaimed {
    session: String,
    /// compat §6.1 — the dispatch branch, projected as gc.work_branch (beads.work_branch).
    /// The route is NOT here: cook owns beads.assignee (= gc.routed_to) and the claim
    /// must not re-derive it (round-1 B1).
    #[serde(default)]
    work_branch: Option<String>,
}
```

In `bead_claimed`'s `Some("open")` arm, add `work_branch = COALESCE(?N, work_branch)` to the existing UPDATE (do NOT touch `assignee`):

```rust
conn.execute(
    "UPDATE beads SET status = 'in_progress', claimed_by = ?1,
                      work_branch = COALESCE(?2, work_branch),
                      updated_ts = ?3
     WHERE id = ?4",
    params![p.session, p.work_branch, event.ts, id],
)?;
```

Keep the existing `dispatch_failure = NULL` clear that follows.

- [ ] **Step 4: Run, watch pass** — `cargo test -p camp-core --test claim_invariant` → PASS.

- [ ] **Step 5: Refold** — `cargo test -p camp-core --test refold_prop` → PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/ledger/fold.rs crates/camp-core/tests/claim_invariant.rs
git commit -m "compat(worker): the claim stamps session + work_branch, never the cooked route"
```

---

## Task 4: Shim scaffolding — subcommands, exit-code channel, absolute-path install, gitignore

**Files:**
- Create: `crates/camp/src/cmd/shim/mod.rs`, `crates/camp/src/cmd/shim/install.rs`
- Modify: `crates/camp/src/main.rs` (two variants + two bespoke arms), `crates/camp/src/gitignore.rs`, `crates/camp/src/cmd/mod.rs`
- Test: `install.rs` (inline), `gitignore.rs` (inline)

**Interfaces (B8):**
- `pub struct ShimExit(pub u8);` — the process exit code the shim intends. `0` = success/work; `1` = drain (a NORMAL outcome, not an error). Returned by `gc_shim`/`bd_shim`.
- `pub fn gc_shim(camp: &CampDir, args: Vec<String>) -> anyhow::Result<ShimExit>` / `bd_shim(...)`. A genuine error is `Err` (the `main` arm prints + exits 1); a drain is `Ok(ShimExit(1))` (no print).
- `shim::install::write_shims(camp_root: &Path, camp_exe: &Path) -> Result<()>`; `shim::install::prepend_bin_path(camp_root: &Path, existing: Option<&str>) -> String`.

- [ ] **Step 1: Write the failing tests** (`install.rs` inline)

```rust
#[test]
fn shims_embed_the_absolute_camp_path_not_a_bare_name() {
    let dir = tempfile::tempdir().unwrap();
    write_shims(dir.path(), Path::new("/opt/camp/bin/camp")).unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("bin/gc")).unwrap(),
        "#!/bin/sh\nexec /opt/camp/bin/camp gc-shim \"$@\"\n");
    assert_eq!(std::fs::read_to_string(dir.path().join("bin/bd")).unwrap(),
        "#!/bin/sh\nexec /opt/camp/bin/camp bd-shim \"$@\"\n");
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(std::fs::metadata(dir.path().join("bin/gc")).unwrap().permissions().mode() & 0o111, 0o111); }
}
#[test]
fn prepend_bin_path_puts_camp_bin_first() {
    let dir = tempfile::tempdir().unwrap();
    let p = prepend_bin_path(dir.path(), Some("/usr/bin:/bin"));
    assert!(p.starts_with(&format!("{}/bin:", dir.path().display())) && p.ends_with("/usr/bin:/bin"));
}
```

`gitignore.rs`:
```rust
#[test]
fn bin_is_a_runtime_dir() { assert!(RUNTIME_DIRS.contains(&"bin"), "the shim bindir must be gitignored (§6.3)"); }
```

- [ ] **Step 2: Run, watch fail** — `cargo test -p camp --lib cmd::shim::install:: gitignore::tests::bin_is_a_runtime_dir` → FAIL.

- [ ] **Step 3: Implement**
  - `gitignore.rs`: `RUNTIME_DIRS = &["runs","sessions","worktrees","imports","bin"]`.
  - `shim/install.rs`: `write_shims` (create `<root>/bin`, write both scripts with the exact bytes, chmod 0755 on unix), `prepend_bin_path`.
  - `shim/mod.rs`: `pub struct ShimExit(pub u8);`, `pub mod install;`, and `gc_shim`/`bd_shim` returning `Result<ShimExit>` — for now the verb match routes everything to the Task-5 refusal (they may return the refusal `Err` until later tasks fill verbs).
  - `main.rs` — two variants:

```rust
#[command(hide = true)]  // gc pack worker shim (spec §6); installed into .camp/bin, not for humans
GcShim { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
#[command(hide = true)]
BdShim { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
```

  and TWO BESPOKE ARMS that bypass `report()` (B8 — the exit-code channel `report` cannot express). Because `run()` returns `Result<()>` and `main` wraps it in `report`, handle these by exiting directly (the shim is a short-lived leaf; its ledger writes are already committed before return, so `std::process::exit` skipping Drop is safe and documented):

```rust
Command::GcShim { args } => {
    match cmd::shim::gc_shim(&camp, args) {
        Ok(code) => std::process::exit(code.0 as i32),          // drain → exit 1, no error print
        Err(e) => { eprintln!("camp: {e:#}"); std::process::exit(1); }
    }
}
Command::BdShim { args } => { /* identical, bd_shim */ }
```

(`trailing_var_arg + allow_hyphen_values` keep gc/bd's own arg grammar out of clap's hands.)

- [ ] **Step 4: Run, watch pass** — PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/shim/ crates/camp/src/main.rs crates/camp/src/gitignore.rs crates/camp/src/cmd/mod.rs
git commit -m "compat(worker): shim scaffolding — ShimExit channel, absolute-path .camp/bin, gitignored"
```

---

## Task 5: The refusal path — unknown verbs/flags fail fast AND event `shim.refused`

**Files:** Modify `crates/camp/src/cmd/shim/mod.rs`; Test inline.

**Interfaces:**
- `shim::refuse(camp, verb, detail) -> Result<ShimExit>` — reads `binding`/`agent` from `$GC_TEMPLATE`/`$GC_AGENT`/`$CAMP_SESSION`, appends `EventType::ShimRefused { binding, agent, verb, detail }` (poke best-effort), and returns `Err` (the `main` arm prints + exits nonzero). Consumed by every unhandled verb/flag in Tasks 6–8.

- [ ] **Step 1: Write the failing tests** (inline, driving `gc_shim`/`bd_shim` against a temp `CampDir`)

```rust
#[test]
fn unknown_gc_verb_fails_fast_and_events_shim_refused() {
    let camp = temp_camp();
    let err = gc_shim(&camp, vec!["mol".into(), "list".into()]).unwrap_err();
    assert!(format!("{err:#}").contains("mol"), "names the refused verb");
    assert!(read_events(&camp).iter().any(|e| e.kind == EventType::ShimRefused && e.data["verb"] == "mol"));
}
#[test]
fn unknown_bd_flag_is_refused_not_silently_ignored() {
    // `bd update gc-1 --set-metadata gc.outcome=pass --frobnicate` → Err naming --frobnicate + shim.refused.
    // Mutation caught: a fall-through that no-ops an unknown flag (a corrupted ledger, §6).
}
```

- [ ] **Step 2: Run, watch fail** — FAIL (unimplemented).
- [ ] **Step 3: Implement** the verb match + `refuse` (append `ShimRefused`, `bail!`). A served verb with an unknown flag calls `refuse` from its own handler (Tasks 6–8).
- [ ] **Step 4: Run, watch pass** — PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): shim refusals are loud AND evented (shim.refused)"
```

---

## Task 6: `gc hook --claim --json` — discovery, claim flip, drain

**Files:**
- Create: `crates/camp/src/cmd/shim/hook.rs`, `crates/camp/src/cmd/shim/project.rs`
- Modify: `crates/camp/src/cmd/shim/mod.rs`
- Test: `hook.rs` (inline)

**Interfaces (B5/B6 — one formatter, no second projection):**
- `project.rs`: `pub struct ClaimProjection { pub assignee: Option<String>, pub route: Option<String>, pub work_branch: Option<String> }` and `pub fn claim_projection(conn: &Connection, bead: &str) -> Result<ClaimProjection>` where `assignee = claimed_by` (a direct `SELECT claimed_by FROM beads`), and `route`/`work_branch` = `readiness::bead_metadata(conn, bead)?["gc.routed_to"|"gc.work_branch"]`. **The "assignee column IS gc.routed_to" mapping is NOT re-hand-rolled here — it stays owned by `readiness::PROJECTED_METADATA`/`bead_metadata` (readiness.rs:71,201).** (B6: this avoids `BeadRow`, which has no `work_branch` field — readiness.rs:23-24 has `assignee`/`claimed_by` but no `work_branch`, and `BEAD_COLS` at readiness.rs:51 does not select it; `bead_metadata` does its own dedicated `SELECT assignee, work_branch` at readiness.rs:212.)
- `hook --claim --json` prints, on WORK: `{"schema_version":1,"ok":true,"action":"work","reason":null,"bead_id":"…","assignee":"<session>","route":"<qualified>"}` and returns `Ok(ShimExit(0))`; on DRAIN: `{…,"action":"drain","reason":"<why>","assignee":null,"route":null}` and returns `Ok(ShimExit(1))` (or `ShimExit(0)` with `--drain-ack`). **The field names/shape are the spec's claim — confirm against Task 1's `hook_json_fields` before implementing; a code comment cites the fixture.**

- [ ] **Step 1: Confirm the shape** against `gc-role-worker.observed.json` (`hook_json_fields`, exit contract work=0/drain=1). If the recording differs, the JSON and assertions change to match it.

- [ ] **Step 2: Write the failing tests** (`hook.rs` inline; the fixtures set `GC_AGENT` ≠ the cooked route so a re-derivation is observable — B1)

```rust
#[test]
fn hook_claim_returns_work_and_projects_the_BEAD_route_not_the_env() {
    // ledger: cooked open bead "gc-2", assignee col = "gc.publisher" (the cooked route).
    // env: CAMP_BEAD=gc-2, CAMP_SESSION="t/gc.publisher/1", GC_AGENT="gc.WRONG"  <-- deliberately ≠ route
    let out = run_hook(&camp, &["--claim","--json"]);
    assert_eq!(out.exit, 0);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["action"], "work");
    assert_eq!(v["bead_id"], "gc-2");
    assert_eq!(v["assignee"], "t/gc.publisher/1");   // the session (claimed_by)
    assert_eq!(v["route"], "gc.publisher");          // the BEAD's route — NOT "gc.WRONG"
    // Mutation caught: hook re-deriving route from GC_AGENT env → would print "gc.WRONG" → RED.
    let row = read_bead(&camp, "gc-2");
    assert_eq!(row.status, "in_progress");
    assert_eq!(row.work_branch.as_deref(), Some("camp/gc-2"));
    assert_eq!(row.assignee.as_deref(), Some("gc.publisher"));   // claim left the route intact
}
#[test]
fn hook_claim_on_a_closed_bead_returns_drain_exit_1() {
    let out = run_hook(&camp, &["--claim","--json"]);   // bead already closed
    assert_eq!(out.exit, 1);
    assert_eq!(json(&out.stdout)["action"], "drain");
}
#[test]
fn hook_claim_drain_with_drain_ack_flag_exits_0() { /* `--drain-ack` → exit 0 */ }
```

- [ ] **Step 3: Run, watch fail** — FAIL.

- [ ] **Step 4: Implement** — `project.rs::claim_projection` (as above). `hook.rs`: parse `--claim`/`--json`/`--drain-ack` (unknown flag → `refuse`); read `CAMP_BEAD`, `CAMP_SESSION`; load the bead status.
  - closed / not-`open`-and-not-`in_progress`-by-this-session → build the drain JSON, return `Ok(ShimExit(if --drain-ack {0} else {1}))`.
  - `open` → append `BeadClaimed { session: CAMP_SESSION, work_branch: format!("camp/{CAMP_BEAD}") }` (NO route field), poke; then read `claim_projection(conn, bead)` and print the work JSON from IT (route comes from the bead), return `Ok(ShimExit(0))`.
  - already `in_progress` by this session → print work again from `claim_projection` (idempotent re-hook).
  **Do not route the drain exit through `bail!`** — it is a normal outcome carried by `ShimExit(1)`.

- [ ] **Step 5: Run, watch pass** — PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/shim/hook.rs crates/camp/src/cmd/shim/project.rs crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): gc hook --claim --json — claim flip + drain, route read from the bead"
```

---

## Task 7: `bd` data plane — projecting the claimed row through the one formatter

**Files:** Create `crates/camp/src/cmd/shim/bd.rs`; Modify `mod.rs`; Test inline.

**Interfaces (B5):**
- `bd show <bead> --json` → JSON `{ "id":…, "status":…, "assignee": <claimed_by>, …, "metadata": <readiness::bead_metadata(conn, bead)> }`. **The `metadata` map MUST come from `readiness::bead_metadata` (readiness.rs:201), which already emits `gc.routed_to`, `gc.work_branch`, and every non-projected `bead_meta` key — it is NOT re-derived in shim code.** The top-level `assignee` is `claimed_by` (the session). §6.1: `bd show`'s assignee/route overwrite hook's in the fragment before it compares.
- `bd update <bead> --set-metadata k=v …` → `BeadUpdated { metadata }`; `bd close <bead> --status …` → camp's close (outcome/work-outcome, vocab.rs:58,68); `bd create`/`list`/`ready` → camp equivalents. Flags outside Task 1's `verbs.bd` recording → `refuse`.

**Acknowledged out-of-fragment-scope edges (map only what Task 1 records; do not gold-plate).** The corpus fragment's happy/fail paths use `bd close` with `pass`/`fail` only — `blocked`/`abandoned`/`no-op` need mapping ONLY if Task 1's recording shows the fragment emitting them (map exactly what it uses; refuse the rest). `bd update --set-metadata` on a **projected key** (`gc.work_branch`/`gc.routed_to`) would try to write `bead_meta` while reads project from the column — `write_meta` already REFUSES this (fold.rs:221-233, "a key with a dedicated column is refused"), so it surfaces as a loud shim error, not a silent divergence; the fragment does not do it. A **double-claim of one open bead** cannot occur (the hook is pinned to `CAMP_BEAD`, one bead per session, §6.2) and `bead_claimed` rejects a non-`open` claim anyway. Each is a one-line acknowledgement, not new machinery.

- [ ] **Step 1: Confirm** the `bd` subcommand+flag set and the `bd_show_json_fields` against `gc-role-worker.observed.json` (the fragment reads `assignee` + `gc.routed_to` from `bd show` at fragment lines 127-133). Cite the fixture.

- [ ] **Step 2: Write the failing tests**

```rust
#[test]
fn bd_show_json_assignee_is_the_session_and_metadata_comes_from_bead_metadata() {
    // after a hook claim: claimed_by="t/gc.publisher/1", assignee col="gc.publisher".
    let v = run_bd_json(&camp, &["show","gc-2","--json"]);
    assert_eq!(v["assignee"], "t/gc.publisher/1");                 // gc's assignee = the session
    assert_eq!(v["metadata"]["gc.routed_to"], "gc.publisher");     // from readiness::bead_metadata
    assert_eq!(v["metadata"]["gc.work_branch"], "camp/gc-2");
    // Mutation caught: a second hand-rolled projection that reads the wrong column.
}
#[test]
fn bd_update_set_metadata_writes_through_bead_updated() { /* gc.custom=x lands in bead_meta */ }
#[test]
fn bd_close_maps_to_camps_close_vocabulary() { /* --status pass → closed, outcome pass */ }
#[test]
fn bd_unknown_subcommand_is_refused() { /* `bd mol` → shim.refused (Task 5) */ }
```

- [ ] **Step 3: Run, watch fail** — FAIL.
- [ ] **Step 4: Implement** `bd.rs`: verb match; `show --json` = the bead's scalar fields + `assignee = claimed_by` + `metadata = readiness::bead_metadata(conn, bead)`; `update`/`close`/`create` reuse the existing `cmd::*`/`EventInput` paths (do NOT re-implement close's shipped-commit gate); map gc's `--status`/`--set-metadata gc.outcome=…` to camp's `--outcome`/`--work-outcome` per Task 1. Unknown flags → `refuse`.
- [ ] **Step 5: Run, watch pass** — PASS.
- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/shim/bd.rs crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): bd data plane — show projects via readiness::bead_metadata (one formatter)"
```

---

## Task 8: `runtime drain-ack` (release signal) + `convoy status --json`

**Files:** Create `crates/camp/src/cmd/shim/runtime.rs`; Modify `mod.rs`; Test inline.

**Interfaces:**
- `runtime drain-ack` → append `WorkerDrainAcked { session: CAMP_SESSION }`, poke campd (`poke_best_effort` — invariant 1, no new poll), return `Ok(ShimExit(0))`. `convoy status --json` → worker-facing read of the session's bead (shape per Task 1). Other `runtime`/`convoy` verbs → `refuse`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn runtime_drain_ack_appends_worker_drain_acked_and_exits_0() {
    let code = run_gc(&camp, &["runtime","drain-ack"]).unwrap();
    assert_eq!(code.0, 0);
    assert!(read_events(&camp).iter().any(|e| e.kind == EventType::WorkerDrainAcked
        && e.data["session"] == "t/gc.publisher/1"));
}
#[test]
fn convoy_status_json_reports_the_sessions_bead() { /* fields per Task 1 */ }
#[test]
fn runtime_unknown_subcommand_is_refused() { /* `runtime foo` → shim.refused */ }
```

- [ ] **Step 2: Run, watch fail** — FAIL.
- [ ] **Step 3: Implement** `runtime.rs`.
- [ ] **Step 4: Run, watch pass** — PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/shim/runtime.rs crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): runtime drain-ack (release signal) + convoy status --json"
```

---

## Task 9: The worker environment + shim install at dispatch (§6.1 projection #3)

**Files:** Modify `crates/camp/src/daemon/spawn.rs`, `crates/camp/src/daemon/dispatch.rs`; Test in `spawn.rs` (inline) + a launch-level assertion.

**Interfaces:**
- `build_spec`'s `env` additionally carries `BEADS_ACTOR = GC_SESSION_NAME = GC_SESSION_ID = session_name`; `GC_AGENT = GC_TEMPLATE = agent.name` (qualified); `PATH = prepend_bin_path(camp_root, inherited PATH)`. In production `GC_AGENT` equals the cooked route (both from `resolve_agent`), so the three projections agree; the guard fixtures (Task 6/11) mismatch them on purpose.
- `dispatch.rs::launch` calls `shim::install::write_shims(&self.camp.root, &std::env::current_exe()?)` before `spawn` — on error, append `dispatch.failed` and return `Ok(())` (never a silent skip). `current_exe()` is the §6.3 absolute path (NOT `[dispatch].command`, which is `claude`).

- [ ] **Step 1: Write the failing tests** (`spawn.rs` inline; set the fixture agent's `name = "gc.run-operator"`)

```rust
#[test]
fn build_spec_exports_the_gc_worker_environment() {
    let spec = build_spec(Path::new("claude"), &full_agent(/*name="gc.run-operator"*/), Path::new("/camps/dev"),
        "gc-142", "dev/gc.run-operator/1", "sid", Path::new("/h/.claude/x.jsonl"),
        Path::new("/code/gc"), StdinMode::HeldStream);
    let env: std::collections::BTreeMap<_,_> = spec.env.iter().cloned().collect();
    for k in ["BEADS_ACTOR","GC_SESSION_NAME","GC_SESSION_ID"] { assert_eq!(env[k], "dev/gc.run-operator/1"); }
    for k in ["GC_AGENT","GC_TEMPLATE"] { assert_eq!(env[k], "gc.run-operator"); }
    assert!(env["PATH"].starts_with("/camps/dev/bin:"));
    assert_eq!(env["CAMP_BEAD"], "gc-142");   // the four CAMP_* still present
}
```

Plus a launch-level assertion (B12) — sited where `daemon_drain.rs`/the dispatch tests drive a real `launch`:
```rust
#[test]
fn a_dispatch_installs_the_absolute_path_shims_into_camp_bin() {
    // after launch(): <camp>/bin/gc exists, is 0755, and contains the absolute camp exe path
    // (not "exec camp "). Mutation caught: deleting the write_shims call in launch().
}
```

- [ ] **Step 2: Run, watch fail** — FAIL. NOTE: the existing `argv_matches_the_fixture_facts_for_a_fully_pinned_agent` test asserts `spec.env == vec![…4 CAMP_*…]` exactly (spawn.rs:683-693) — it WILL fail; UPDATE that assertion to include the five gc vars + `PATH` (appended after the four `CAMP_*`).

- [ ] **Step 3: Implement** — extend `build_spec`'s `env` vec after the four `CAMP_*` entries; add `write_shims` to `launch` before `spawn`.

- [ ] **Step 4: Run, watch pass** — `cargo test -p camp --lib daemon::spawn::` and the launch test → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/spawn.rs crates/camp/src/daemon/dispatch.rs
git commit -m "compat(worker): worker env (BEADS_ACTOR/GC_*) + absolute-path shim install on dispatch"
```

---

## Task 10: Session lifecycle §6.2 — drain-ack is the prompt KILL; grace is the backstop

**Files:** Modify `crates/camp/src/daemon/patrol.rs` (add a `WorkerDrainAcked` arm to `patrol::observe`, patrol.rs:301, queuing a `PendingAction::KillReleased`); Test in `patrol.rs` (inline).

**Interfaces / Decision (B3 — reconciled, record; do not re-litigate):** The merged path is: bead-close → `patrol::observe` queues `PendingAction::Release` (patrol.rs:333) → `release_worker` drops stdin + sets the released reason + the executor arms `TimerKind::Release` for `release_grace` (patrol.rs:751) → the timer fires → `KillReleased` → `kill_released` (patrol.rs:774). **Bead-close never KILLS; it only drops stdin and arms a timer.** §6.2's "release on drain-ack, not bead-close" is implemented by making **drain-ack the prompt KILL trigger**, which is NOT redundant with bead-close (bead-close does not kill):

- On `worker.drain_acked`, `patrol::observe` queues `PendingAction::KillReleased { session }` for the acking session; `execute_pending` (its existing `KillReleased` arm, patrol.rs:773) calls `kill_released(session)` **immediately** — the worker was already released-but-live from bead-close, so `kill_released`'s `released.is_some()` guard passes — killing it now instead of waiting the full `release_grace`. The stop reason stays the bead-close reason ("released after bead close") — accurate: the release WAS caused by close; drain-ack only makes the kill prompt. (Round-1's "the reason names the drain" assertion was wrong and is dropped; the observable is the **timing**.)
- The `release_grace` timer remains the **backstop** for a worker that dies/hangs and never acks. **The race §6.2 kills** (a live worker SIGKILLed mid-handshake) does not bite, because camp truncates gc's continuation loop: the hook returns `drain` immediately post-close (§6.2, Task 6), so the worker's post-close path is `hook → drain → drain-ack → exit` — a couple of shim calls, no `sleep`. Post-close drain time ≪ `release_grace` (default 30s). The backstop only fires when a worker is genuinely stuck (its own drain path failing in `sleep 2; continue`), which is exactly when killing is correct. Native (non-gc) workers never ack and are killed by the same grace as today — no regression.

- [ ] **Step 1: Write the failing tests** (`patrol.rs` inline, following `release_arms_the_grace_and_kill_released_stops_with_reason`, patrol.rs:2824)

```rust
#[test]
fn drain_ack_kills_the_released_worker_promptly_before_the_grace() {
    // bead closes → observe → Release (stdin dropped, grace armed). Then observe a
    // worker.drain_acked{session}. Assert kill_released runs at drain-ack time — the
    // worker is reaped WITHOUT the release_grace timer having fired.
}
#[test]
fn a_slow_drain_that_acks_before_the_grace_is_not_killed_early() {
    // model a worker that acks at t < release_grace: no grace-timeout kill occurs; the
    // drain-ack path reaps it. Guards §6.2's kill-before-drain-ack race for a live worker.
}
#[test]
fn a_worker_that_never_acks_is_killed_by_the_grace_backstop() {
    // bead closes, no drain-ack; the TimerKind::Release grace fires → kill_released.
    // Mutation caught: removing the bead-close grace arm (a crashed-mid-drain worker leaks).
}
#[test]
fn a_drain_ack_at_the_grace_boundary_still_reaps_via_the_ack_not_a_double_kill() {
    // NB2 — pin the "post-close drain << grace" claim instead of deriving it. Configure a
    // SHORT release_grace and drive the ack right AT/just-before it: assert the drain-ack
    // KillReleased reaps the worker exactly once (idempotent with the grace fire that lands
    // in the same wake) — no double kill_released, no panic. This is the boundary where the
    // §6.2 race would reappear if the two paths were not idempotent.
}
```

- [ ] **Step 2: Run, watch fail** — FAIL.
- [ ] **Step 3: Implement** — add the `EventType::WorkerDrainAcked` arm to `patrol::observe` (queue `PendingAction::KillReleased { session }` for the acking session, resolved via `event.data["session"]`); the existing `execute_pending` `KillReleased` arm (patrol.rs:773) already calls `kill_released`. Ensure idempotency: a session killed by drain-ack must no-op the later grace fire (the reap untracks it; the timer fire finds no worker).
- [ ] **Step 4: Run, watch pass** — `cargo test -p camp --lib daemon::patrol::` → PASS (existing release tests unregressed).
- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/patrol.rs
git commit -m "compat(worker): drain-ack is the prompt kill trigger; the bead-close grace is the backstop (§6.2)"
```

---

## Task 11: THE UNSKIPPABLE §14 real-fragment gate + the projection property

**Files:** Create `crates/camp/tests/worker_contract.rs`, `crates/camp/tests/fixtures/gc-fragment.sh`, `ci/gc-compat/worker_contract.py`; Modify `.github/workflows/ci.yml`, `ci/gc-compat/README.md`.

- [ ] **Step 1: The byte-projection property (hermetic Rust) — with an env≠bead mismatch (B1)**

```rust
#[test]
fn hook_bd_show_and_env_project_the_same_bead_row() {
    // dispatch a gc bead whose COOKED route is "gc.publisher"; set env GC_AGENT/GC_TEMPLATE
    // to a DIFFERENT value on purpose. Then:
    //   env session == hook.assignee == bd.assignee, and
    //   hook.route == bd.metadata.gc.routed_to == "gc.publisher" (the BEAD), NOT env GC_AGENT.
    // Mutation caught: any projection deriving route from GC_AGENT env → RED (the rev-3 bug).
}
```

**Independence note (NB1 — state explicitly):** `hook.route` and `bd.metadata.gc.routed_to` are NOT independent legs — both call the single `readiness::bead_metadata` formatter (that reuse is the B5 fix), so their mutual equality is tautological and only proves the formatter is called, not that the route is right. The genuinely independent leg is **env `GC_AGENT` == the cooked route** — the equality the REAL fragment depends on to not spin (it compares `EXPECTED_ROUTE` from env against `bd show`'s route). That leg is pinned ONLY by the real-campd Task 11 §14 gate (Step 2/4), because only there does the env come from a real `build_spec` and the route from a real cook. **This is why closing R2-B2's lingering fixture matters: the §14 gate is the sole honest guard of the env leg, so it must actually run the integrated path to the end.**

- [ ] **Step 2: The hermetic loop test with a REAL watchdog AND a LINGERING worker (B11 happy+fail-close B2; R2-B2 lifecycle)** — write `crates/camp/tests/fixtures/gc-fragment.sh`: a FAITHFUL synthetic fragment from `gc-role-worker.observed.json` — the `EXPECTED_ASSIGNEE`/`python3` guard, the `set +e … while true` claim loop (`gc hook --claim --json` → `bd show` → `bd close` → on drain, `gc runtime drain-ack`). **After `drain-ack` the fragment MUST LINGER, NOT `exit 0`** — e.g. `exec sleep 600` (well past the deadline). Rationale (R2-B2): in production the worker is a `claude -p` whose held stdin does NOT exit on EOF (P3, spawn.rs:98) — that is the entire reason `kill_released` exists (dispatch.rs:341). A fragment that self-exits ends only its bash subshell and makes campd's kill a no-op, so the drain-ack → `poke` → `observe(WorkerDrainAcked)` → `KillReleased` → `kill_released` wiring — the single §6.2 behavior camp owns — is NEVER exercised in the integrated path (Task 10's `cat`-based unit test is the only guard, and it is time-simulated in isolation). A lingering fragment forces **campd's kill** to be what reaps the worker.

  Provide a `FAIL` mode that takes the `bd close --status fail` branch and any failure-report verb Task 1 recorded, so the fail-close verb set is DRIVEN, not just grepped. In `worker_contract.rs`, drive REAL campd with `[dispatch].command` = a fake `claude` that `exec`s `sh gc-fragment.sh`, real ledger, real shims (installed by `launch` — Task 9). Watchdog: a poll loop `while !reaped { if start.elapsed() > DEADLINE { child.kill(); panic!("campd did not reap the drained worker — the drain-ack→KillReleased wiring regressed, or the fragment hung in sleep 2; continue") } try_wait(); sleep(50ms) }` (the `daemon_drain.rs` pattern — `Instant`/`Duration`/poll, never a blocking `child.wait()`). Assert BOTH: the bead reached `closed`, AND campd reaped the worker (a `session.stopped` for it) **while the fragment was still in its `sleep 600`** — i.e. the reap came from campd's kill, not the fragment self-exiting. Because the fragment sleeps 600 s but the deadline is ~20 s, the ONLY way `reaped` becomes true in time is campd's KillReleased firing; if that wiring regressed, the fragment sleeps past the deadline → the watchdog fails RED. Run BOTH happy and fail-close modes.

```rust
#[test]
fn campd_reaps_a_lingering_gc_worker_via_drain_ack_after_it_closes_the_bead() { /* happy; DEADLINE 20s; worker sleeps 600s */ }
#[test]
fn the_fail_close_branch_also_closes_and_is_reaped_without_a_hang() { /* fail mode; DEADLINE 20s */ }
```

- [ ] **Step 3: Run the hermetic tests** — `cargo test -p camp --test worker_contract` → PASS.

- [ ] **Step 4: The §14 CI gate (real corpus fragment)** — write `ci/gc-compat/worker_contract.py <corpus-checkout> <camp-binary>`, mirroring `e2e_corpus.py`:
  1. `camp init`; `camp import add <corpus>/gascity/roles --name gc` (the deployment recipe, §3/§7.3); set `[agent_defaults].tools`.
  2. Create/route a bead to a real `gc.<agent>` (so `gc.routed_to` is stamped from cook).
  3. Real campd with `[dispatch].command` = a fake claude that runs `sh` on the REAL rendered `gc-role-worker` fragment and then **LINGERS** (`sh <fragment>; exec sleep 600`) — NOT exits when the fragment's bash returns. Same rationale as Step 2 (R2-B2): a real `claude -p` does not exit on task completion or EOF (P3), so the fake claude must linger for campd's drain-ack→KillReleased to be the thing that reaps it; a self-exiting wrapper makes the kill a no-op and leaves the one camp-owned lifecycle decision unexercised even here. **The renderer is THIS Python harness** (it substitutes the fragment's Go-template with the recorded env/values) — NOT a camp "prime" (prime is phase-4; `parse_agent_dir` reads the prompt raw) (NB1).
  4. Assert under a wall deadline: the bead reaches `closed`, a `worker.drain_acked` appears, and **campd reaps the lingering worker** (`session.stopped`) — since the wrapper sleeps 600 s, a reap within the deadline can only be campd's KillReleased; a hang (wiring regressed) fails the gate. Drive the fail-close branch too.
  5. Re-derive `gc-role-worker.observed.json` from the live fragment and fail on drift; add this to the README "Moving GCPACKS_REF" procedure.

  **What green proves, stated plainly (NB2):** a passing §14 gate proves the SHIM CONTRACT (the fragment's verbs are served, the bead-side projection holds, the drain handshake completes). It does NOT prove production dispatch of a gc agent's *rendered prompt to a real claude* — that is a phase-4/§5.1 (`prime`) concern the fake-claude gate deliberately does not exercise.

- [ ] **Step 5: Wire into CI** — after the `e2e_corpus.py` step in the `gc-compat` job:

```yaml
      - name: "phase-3 WORKER CONTRACT gate — the real gc-role-worker fragment closes a gc bead (§14)"
        run: python3 ci/gc-compat/worker_contract.py gcpacks-src target/debug/camp
```

- [ ] **Step 6: Run the gate locally** — `python3 ci/gc-compat/worker_contract.py /tmp/gcpacks target/debug/camp` → PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/tests/worker_contract.rs crates/camp/tests/fixtures/gc-fragment.sh \
        ci/gc-compat/worker_contract.py ci/gc-compat/README.md .github/workflows/ci.yml
git commit -m "compat(worker): THE §14 gate — the real gc-role-worker fragment closes a gc bead"
```

---

## Task 12: `python3` in the container

**Files:** Modify `contrib/docker/Dockerfile`; guard in `ci/gc-compat/worker_contract.py`.

- [ ] **Step 1: Write the failing guard** (in `worker_contract.py`)

```python
assert "python3" in open("contrib/docker/Dockerfile").read(), \
    "python3 is a hard gc-worker dependency (§6.1) and must be in the runtime image"
```
Run → FAIL.

- [ ] **Step 2: Implement** — add `python3` to the runtime stage's `apt-get install`, with the comment:

```dockerfile
# python3         a HARD runtime dependency of the gc worker contract (compat §6.1):
#                 every gc pack agent's shared fragment parses `hook --claim --json`
#                 with an inline `python3`, and refuses (CONFIG_REJECTED, exit 0
#                 doing nothing) if it is absent. No python3, no gc worker.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini python3 \
 && rm -rf /var/lib/apt/lists/*
```

- [ ] **Step 3: Run the guard, watch pass** → PASS.
- [ ] **Step 4: Commit**

```bash
git add contrib/docker/Dockerfile ci/gc-compat/worker_contract.py
git commit -m "compat(worker): python3 in the reference container (§6.1 gc-worker dependency)"
```

---

## Task 13: Full-gate green + self-review

- [ ] **Step 1: Merge gates** — `cargo fmt --all --check` · `cargo clippy --workspace --all-targets --all-features -- -D warnings` · `cargo test --workspace`. All green; fix any `unwrap_used`/`expect_used`/`panic` at the source.

- [ ] **Step 2: Compat gates against a fetched corpus**

```bash
python3 ci/gc-compat/load_corpus_packs.py /tmp/gcpacks target/debug/camp
python3 ci/gc-compat/formula_gate.py    /tmp/gcpacks target/debug/camp
python3 ci/gc-compat/e2e_corpus.py      /tmp/gcpacks target/debug/camp
python3 ci/gc-compat/worker_contract.py /tmp/gcpacks target/debug/camp
```

- [ ] **Step 3: Exit-criteria checklist** — a gc worker closes a gc bead via the REAL fragment (Task 11); every §6 verb served or refused loudly (Tasks 5–8; the served set covers Task 1's FULL branch recording); `.camp/bin` absolute-path/gitignored/dispatch-only (Tasks 4/9); the bead-side claim invariant + byte-projection (Tasks 3/6/9/11); `python3` in the container (Task 12); CI green.

- [ ] **Step 4: Rebase discipline** — if `main` advanced, rebase and re-run Steps 1–2. The guaranteed-contention files (`main.rs`, `event.rs`, `vocab.rs`, `fold.rs`, `Cargo.toml`, `Cargo.lock`) are additive here.

---

## Self-Review

**Spec coverage (§6 in full, §12.3, §14):**
- §6 shims (argv translators, `camp` sole writer) → Tasks 4–8. §6 verb table → Tasks 6/7/8; `prime`/`mail` (phase 4, §12.4) refused loudly (Task 5), with Task 1 confirming the real fragment does not need them on ANY branch (else escalate — B2).
- §6 "FAIL FAST + `shim.refused`" → Task 5. §6.1 claim invariant (one row, three projections, route from the bead) → Task 3 (the row), Task 6 (hook + `claim_projection` via `bead_metadata`), Task 7 (bd-show via `bead_metadata`), Task 9 (env), Task 11 (byte-for-byte with an env≠bead mismatch). `python3` → Task 12.
- §6.2 lifecycle → Task 6 (drain post-close) + Task 10 (drain-ack prompt kill; grace backstop).
- §6.3 shims (absolute path, gitignored, dispatch-only) → Tasks 4/9.
- §12.3 + §14 → Task 11.

**Round-1 findings, each addressed:** B1 claim no longer stamps route + env≠bead mismatch fixtures (Tasks 3/6/11); B2 full-branch measurement + fail-close driven (Tasks 1/11); B3 drain-ack = prompt kill (not redundant — bead-close does not kill), grace-race defused by continuation truncation, timing-not-reason assertion, slow-drain test (Task 10); B4 `worker.drain_acked` with justification (Task 2); B5 `bd show` metadata from `readiness::bead_metadata` (Task 7); B6 `claim_projection(conn, bead)` avoids `BeadRow` (Task 6); B7 `deny_unknown_fields` payloads via `audit::<T>` + a validation test (Task 2); B8 `ShimExit` + bespoke `main` arms (Task 4); B9 tests sited in `tests/claim_invariant.rs` with the cook harness + corrected expected-fail (Task 3); B10 `--test refold_prop` (Tasks 2/3); B11 poll-loop watchdog, no blocking `wait()` (Task 11); B12 launch-level shim-install assertion (Task 9); execNB drain-ack observed in `patrol::observe` + column-inversion note (Tasks 3/6/10 + the orientation table); NB1/NB2 renderer named, §14-scope stated (Task 11).

**Round-2 findings, each addressed:** R2-B1 exhaustive static verb extraction (`grep -oE '\b(gc|bd) [a-z][a-z-]*'`) cross-checked against the served set, not just the 3 driven branches (Task 1 Steps 3–4); R2-B2 the §14 fixture (both hermetic and the Python gate) LINGERS after `drain-ack` (`exec sleep 600`) so campd's drain-ack→KillReleased is what reaps the worker — the integrated path now exercises the one camp-owned §6.2 decision, and a wiring regression hangs the deadline RED (Task 11 Steps 2/4); NB1 the env-leg independence dependency stated explicitly (Task 11 Step 1 note); NB2 a grace-boundary idempotency test (Task 10); the optional edges (`bd close` outcome space, `--set-metadata` on a projected key, double-claim) acknowledged as out-of-fragment-scope (Task 7); the `beads.work_branch` second-writer durability heads-up recorded (Task 3).

**Type consistency:** `BeadClaimed{session, work_branch}` (Task 3) = what the hook emits (Task 6). `claim_projection(conn, bead) -> {assignee, route, work_branch}` (Task 6) is used by hook (Task 6) and — via `readiness::bead_metadata` for the metadata map — by bd-show (Task 7), and asserted against env in Task 11. `ShimExit` (Task 4) is returned by every shim verb and consumed by the `main` arms. `EventType::ShimRefused`/`WorkerDrainAcked` (Task 2) are emitted by Tasks 5/8 and observed by Task 10.

**Known lean-on-gc points, measured not inferred:** the fragment's FULL branch/verb set, the `hook`/`bd show` JSON field names, and the exit-code contract — all from Task 1's live multi-branch recording, re-derived by the Task 11 gate on every `GCPACKS_REF` move. The one camp-owned lifecycle decision (§6.2) is fully specified in Task 10.

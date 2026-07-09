# Dispatch Lifecycle Phase 2 — Isolation Default = Worktree (#31) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **APPROVAL NOTE (recorded in the first execution commit):**
> - Date: 2026-07-09
> - Verdict: **APPROVE** (Opus 4.8 plan reviewer, relayed by the team lead 2026-07-09). Reviewer verified the plan against the §9 Phase 2 contract, spec §12, and every source file/symbol it touches; all load-bearing claims (pack.rs:107 parse arm, spawn.rs create_worktree error string, dispatch.rs launch() dispatch.failed path, event/vocab/fold positions) match the checkout. Contract coverage, the `dispatch.live_tree` event design (additive, log-only fold, partition/refold safe, no gc collision), the re-pin list (independently verified complete), and the TDD ordering confirmed sound.
> - Reviewer-accepted deviations: (a) re-pinning `isolation: none` on live-tree-subject tests does not mask the default — default coverage lives in the Task 5/6 tests; (b) git-initializing the daemon_orders hot-reload rig is correct and necessary — confirmed the only daemon_orders test that dispatches a real worker; (c) perf pinning to `isolation: none` is defensible and honestly recorded.
> - Non-blocking notes (applied with judgment, do not gate): (1) Task 3 Step 3 — the `.clone()` hedge on `prep.agent_name` should be unnecessary: `serde_json::json!` serializes by reference (see the existing `json!(wt)` with `wt: &PathBuf` at dispatch.rs:528); "add clone only on compile error" stands, expect no clone. (2) Task 6 Step 6 — `Command` is already imported in daemon_orders.rs (line 9); the fully-qualified form is redundant, either compiles. (3) Perf now measures only the opt-out path; measuring the default worktree dispatch path's latency is a follow-up scoping decision for the operator, out of this phase.

**Goal:** Flip autonomous dispatch to worktree isolation by default (design Q1, APPROVED 2026-07-09), add the explicit and LOUD `isolation = "none"` opt-out, prove the fail-fast contract on un-worktree-able rigs, and land the spec §12 amendment in the same PR — fixing #31, closing #47.

**Architecture:** The worktree machinery already exists and is correct (`create_worktree`/`ensure_worktree`/`remove_worktree` in `crates/camp/src/daemon/spawn.rs`; `launch()`'s worktree-failure → `dispatch.failed` path in `crates/camp/src/daemon/dispatch.rs`). This phase changes *which agents get it* (all of them, unless they opt out) and makes the opt-out visible: a new camp-specific, log-only ledger event `dispatch.live_tree` fires on every live-tree dispatch. The three §9 Phase 2 test obligations are proven with the existing fake-agent integration harness (`crates/camp/tests/daemon_dispatch.rs`), extended with one new fake-agent env (`FAKE_AGENT_RECORD_BRANCH`) so the *worker itself* records `git branch --show-current` from inside its own cwd.

**Tech Stack:** Rust (no async runtime), rusqlite ledger, serde/serde_json (`deny_unknown_fields` payloads), bash fake-agent harness, git worktrees.

**Tracking:** Issue #47 (fixes #31). Branch: `phase-2-isolation-default`. PR description MUST contain the literal lines `Fixes #31` and `Closes #47`.

## Global Constraints

- Never commit to main; all work on branch `phase-2-isolation-default`; one reviewable PR targeting main (AGENTS.md).
- No co-author lines and no self-mention in commits (user CLAUDE.md).
- Gates before push: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` (AGENTS.md).
- No panics/unwrap/expect in library code (clippy denies them); `unsafe_code` forbidden; fail fast, no fallbacks, no silenced errors (AGENTS.md invariant 5).
- Vocabulary mirror (invariant 7): new event names go into `CAMP_SPECIFIC_EVENTS` (additive, must not exist in Gas City source — `ci/gc-compat/check_vocab.sh` assertion (b) validates this in CI); the `tests/vocab_pin.rs` partition tests must stay green.
- Every event's fold effect runs in the same transaction as its insert (one-transaction event+state property); the refold property test (`crates/camp-core/tests/refold_prop.rs`) must stay green.
- New event payloads use `#[serde(deny_unknown_fields)]` structs in `crates/camp-core/src/ledger/fold.rs` (kickoff rule).
- TDD strictly: write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything.
- SCOPE EXCLUSION (design §9 Phase 3): NO delivery semantics — no `WorkOutcome` axis, no worker-contract text changes (`plugin/skills/worker/SKILL.md` and `WORKER_CONTRACT` in `spawn.rs` stay untouched), no committer pack content, no definition of "landed".
- Sibling ownership: do NOT touch `plugin/commands/sling.md`, the converse verb, or spec §8.4 (Phase 1 owns them). Do NOT rewrite `worktree_isolation_creates_then_reaps_on_pass` or `a_dead_reader_nudge_fails_loudly` beyond what the default flip mechanically requires (issue #44 de-flake agent owns their timing fixes). `a_dead_reader_nudge_fails_loudly` is a unit test inside `crates/camp/src/daemon/dispatch.rs` that uses `test_insert_held_cat` — the flip does not touch it; expected change: none.
- Known flakes (issue #44, fix in flight): `worktree_isolation_creates_then_reaps_on_pass`, `a_dead_reader_nudge_fails_loudly` may spuriously fail — rerun to confirm, never silence.
- If a sibling PR merges to main mid-work, rebase onto main when the lead instructs, resolve, and re-run the full gates before opening/updating the PR.

## Design decisions locked in this plan

1. **The loud opt-out is a ledger event** (design §9 Phase 2: "an event / prominent doc"): new camp-specific, log-only event **`dispatch.live_tree`** (naming style mirrors `dispatch.failed`), appended by campd immediately BEFORE the `session.woke` registry row on every dispatch whose agent declared `isolation = "none"`. Payload: `{"path": <canonical worker cwd>, "agent": <agent name>}`, bead and rig on the envelope. Plus the spec §12 prose (Task 7). Nothing-hidden (invariant 3): the choice to run on the live tree is a ledger fact with its cause.
2. **`Isolation` default flips in the enum** (`#[default]` moves from `None` to `Worktree`) and the frontmatter parse maps: missing key → `Worktree`, `"worktree"` → `Worktree`, `"none"` → `None`, anything else → hard parse error naming both accepted values.
3. **Existing tests whose subject is live-tree worker mechanics get the explicit opt-out** (`isolation: none` in their agent frontmatter) rather than being rewritten around worktrees — they now exercise the opt-out parse and stay true to what they test. Tests whose subject IS isolation keep/gain worktree setups. The one test that uses the starter pack's `dev` agent (which must follow the flipped default) gets a git rig instead (`a_hot_reload_updates_dispatch_routing_without_a_restart`).
4. **Starter pack agents change not at all**: `packs/starter/agents/dev.md` and `reviewer.md` declare no `isolation` key, so they now inherit the worktree default — exactly the #31 fix. Delivery-aware prompt text is Phase 3.
5. **e2e (`crates/camp/tests/e2e.rs`, local-only `make e2e`) pins `isolation: none`** on its two agents with a comment: e2e Tier-0 asserts the work lands in the rig's live tree (`toy ls --json` run in the rig), which is delivery — Phase 3 moves e2e onto the default path when "landed" is defined. Without the pin, the flip would break e2e for reasons that are Phase 3's contract, not Phase 2's.
6. **perf suite (`crates/camp/tests/perf_daemon.rs`, local-only `make perf`) pins `isolation: none`**: it measures the spec §14 dispatch-latency floor; adding `git worktree add` to the measured path would change what the numbers mean. Recorded as a non-blocking note for the operator: post-flip, perf measures the opt-out path.

## File Structure (all files that change)

| File | Change |
|---|---|
| `crates/camp-core/src/event.rs` | Add `EventType::DispatchLiveTree` (variant, `ALL`, `as_str` = `"dispatch.live_tree"`) |
| `crates/camp-core/src/vocab.rs` | Add `"dispatch.live_tree"` to `CAMP_SPECIFIC_EVENTS` |
| `crates/camp-core/src/ledger/fold.rs` | `apply` arm + log-only `dispatch_live_tree` fold with `deny_unknown_fields` payload |
| `crates/camp-core/src/ledger/mod.rs` | Unit test `dispatch_live_tree_is_log_only_and_validates_payload` |
| `crates/camp-core/src/pack.rs` | Accept `isolation: none`; flip `#[default]` to `Worktree`; unit tests |
| `crates/camp/src/daemon/dispatch.rs` | Emit `dispatch.live_tree` in `launch()` when `make_worktree == false` |
| `crates/camp/tests/fake-agent.sh` | New optional env `FAKE_AGENT_RECORD_BRANCH` |
| `crates/camp/tests/daemon_dispatch.rs` | Scaffold opt-out re-pin; 5 new integration tests (loud opt-out, default worktree, 2× fail-fast, concurrency) |
| `crates/camp/tests/daemon_patrol.rs` | Re-pin `("dev", "")` → `("dev", "isolation: none\n")` at 5 call sites |
| `crates/camp/tests/daemon_graph.rs` | Re-pin scaffold `dev.md` with `isolation: none` |
| `crates/camp/tests/daemon_orders.rs` | Git-init the rig in the hot-reload dispatch test (starter pack agent follows the default) |
| `crates/camp/tests/perf_daemon.rs` | Re-pin scaffold `dev` with `isolation: none` (+ comment) |
| `crates/camp/tests/e2e.rs` | Re-pin `dev.md`/`reviewer.md` with `isolation: none` (+ Phase 3 comment) |
| `docs/design/2026-07-05-gas-camp-design.md` | §12 amendment (same PR as the flip) |
| `docs/superpowers/plans/2026-07-09-dispatch-phase-2-isolation-default.md` | This plan; approval note added in first execution commit |

Files deliberately NOT touched: `plugin/commands/sling.md`, `plugin/skills/worker/SKILL.md`, spec §8.4, `packs/starter/**`, `crates/camp/src/daemon/spawn.rs` (`WORKER_CONTRACT` and worktree helpers unchanged), `crates/camp-core/tests/fixtures/gc-vocab.json` (that fixture pins gc's side only; camp-specific names live in `vocab.rs`).

---

### Task 0: Record plan approval

**Files:**
- Modify: `docs/superpowers/plans/2026-07-09-dispatch-phase-2-isolation-default.md` (the APPROVAL NOTE block at the top)

- [ ] **Step 1: Fill in the approval note** with the date, verdict, and any non-blocking notes / reviewer-accepted deviations relayed by the team lead. Do not begin Task 1 before approval arrives.

- [ ] **Step 2: Commit (this is the first execution commit)**

```bash
git add docs/superpowers/plans/2026-07-09-dispatch-phase-2-isolation-default.md
git commit -m "docs: phase-2 isolation-default plan with approval note (#47)"
```

---

### Task 1: The `dispatch.live_tree` event (vocabulary + fold)

**Files:**
- Modify: `crates/camp-core/src/event.rs` (EventType enum ~line 14, `ALL` ~line 43, `as_str` ~line 73)
- Modify: `crates/camp-core/src/vocab.rs` (`CAMP_SPECIFIC_EVENTS`, ~line 23)
- Modify: `crates/camp-core/src/ledger/fold.rs` (`apply` match ~line 16; new payload struct + handler near `DispatchFailed` ~line 527)
- Test: `crates/camp-core/src/ledger/mod.rs` (tests module, next to `worktree_events_are_log_only_and_validate_payloads` ~line 1030)

**Interfaces:**
- Consumes: existing fold helpers `required_bead`, `known_bead`, `non_empty`, `payload` (all already in `fold.rs`); test helpers `temp_ledger()`, `seeded_bead()` in `ledger/mod.rs` tests.
- Produces: `EventType::DispatchLiveTree` with wire name `"dispatch.live_tree"`, log-only fold requiring a known bead and non-empty `path` and `agent` payload fields, rejecting unknown fields. Task 3 emits it; Task 3's integration test asserts it.

- [ ] **Step 1: Write the failing unit test** in the tests module of `crates/camp-core/src/ledger/mod.rs`, immediately after `worktree_events_are_log_only_and_validate_payloads`:

```rust
    /// Phase 2 (dispatch-lifecycle Q1): `dispatch.live_tree` is the LOUD
    /// marker that campd dispatched a worker onto the rig's live tree
    /// because the agent explicitly declared `isolation = "none"`.
    /// Log-only, but the payload is validated like every other event.
    #[test]
    fn dispatch_live_tree_is_log_only_and_validates_payload() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        // the happy shape appends
        l.append(EventInput {
            kind: EventType::DispatchLiveTree,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"path": "/code/rig", "agent": "dev"}),
        })
        .unwrap();
        // missing bead is an error
        assert!(
            l.append(EventInput {
                kind: EventType::DispatchLiveTree,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"path": "/p", "agent": "dev"}),
            })
            .is_err(),
            "dispatch.live_tree without a bead must fail"
        );
        // empty path, empty agent, and unknown fields are all rejected
        for data in [
            serde_json::json!({"path": "", "agent": "dev"}),
            serde_json::json!({"path": "/p", "agent": ""}),
            serde_json::json!({"path": "/p", "agent": "dev", "extra": 1}),
        ] {
            assert!(
                l.append(EventInput {
                    kind: EventType::DispatchLiveTree,
                    rig: Some("gc".into()),
                    actor: "campd".into(),
                    bead: Some("gc-1".into()),
                    data: data.clone(),
                })
                .is_err(),
                "invalid payload must be rejected: {data}"
            );
        }
    }
```

- [ ] **Step 2: Run it to watch it fail (compile error — the variant does not exist)**

Run: `cargo test -p camp-core --lib dispatch_live_tree_is_log_only_and_validates_payload`
Expected: FAIL to compile with `no variant or associated item named 'DispatchLiveTree' found for enum 'EventType'`.

- [ ] **Step 3: Add the variant.** In `crates/camp-core/src/event.rs`:

In the `pub enum EventType` body, immediately after the `DispatchFailed` variant (~line 33), add:

```rust
    DispatchLiveTree,
```

In `EventType::ALL`, immediately after `EventType::DispatchFailed,` (~line 63), add:

```rust
        EventType::DispatchLiveTree,
```

In `as_str`, immediately after the `EventType::DispatchFailed => "dispatch.failed",` arm (~line 92), add:

```rust
            EventType::DispatchLiveTree => "dispatch.live_tree",
```

- [ ] **Step 4: Declare the vocabulary.** In `crates/camp-core/src/vocab.rs`, inside `CAMP_SPECIFIC_EVENTS`, immediately after `"dispatch.failed",`:

```rust
    "dispatch.live_tree",
```

- [ ] **Step 5: Add the fold.** In `crates/camp-core/src/ledger/fold.rs`:

In the `apply` match, immediately after `EventType::DispatchFailed => dispatch_failed(conn, event),`:

```rust
        EventType::DispatchLiveTree => dispatch_live_tree(conn, event),
```

Immediately after the `dispatch_failed` function (~line 540), add:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchLiveTree {
    path: String,
    agent: String,
}

/// `dispatch.live_tree` is log-only (spec §12, dispatch-lifecycle Q1):
/// campd dispatched an autonomous worker onto the rig's live tree because
/// the agent explicitly declared `isolation = "none"`. The opt-out is
/// LOUD — running on the live tree is a ledger fact, never silent.
fn dispatch_live_tree(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: DispatchLiveTree = payload(event)?;
    non_empty(event, "path", &p.path)?;
    non_empty(event, "agent", &p.agent)?;
    Ok(())
}
```

- [ ] **Step 6: Run the unit test and the vocab/refold guards**

Run: `cargo test -p camp-core --lib dispatch_live_tree_is_log_only_and_validates_payload && cargo test -p camp-core --test vocab_pin && cargo test -p camp-core --test refold_prop`
Expected: all PASS. (`vocab_pin::every_event_type_is_declared_mirrored_or_camp_specific_never_both` proves the partition still holds with the new name; `camp_specific_names_do_not_collide_with_gc` proves `dispatch.live_tree` is absent from the pinned gc event list. The refold property is untouched — the event is log-only.)

- [ ] **Step 7: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat(core): dispatch.live_tree event — loud marker for live-tree dispatch (#47)"
```

---

### Task 2: `isolation: "none"` parses as an explicit opt-out (default unchanged yet)

**Files:**
- Modify: `crates/camp-core/src/pack.rs` (`parse_agent_file` isolation match, ~line 107; tests module)

**Interfaces:**
- Consumes: `Isolation` enum as it exists (`None` is still the default in this task).
- Produces: `parse_agent_file` accepting `isolation: none` → `Isolation::None`; the rejection message now names both accepted values. Task 6 flips the default on top of this.

- [ ] **Step 1: Write the failing test** in the `tests` module of `crates/camp-core/src/pack.rs`, after `tools_accepts_a_yaml_list_and_isolation_worktree_parses`:

```rust
    #[test]
    fn isolation_none_is_an_accepted_explicit_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "live.md",
            "---\nname: live\nisolation: none\n---\nWork on the live tree.\n",
        );
        let def = parse_agent_file(&dir.path().join("live.md")).unwrap();
        assert_eq!(def.isolation, Isolation::None);
    }
```

- [ ] **Step 2: Run it to watch it fail**

Run: `cargo test -p camp-core --lib isolation_none_is_an_accepted_explicit_opt_out`
Expected: FAIL — the parse error `frontmatter key "isolation" accepts only "worktree", got "none"` propagates and the `unwrap()` panics.

- [ ] **Step 3: Implement.** In `parse_agent_file`, replace:

```rust
    let isolation = match get_str("isolation")?.as_deref() {
        None => Isolation::None,
        Some("worktree") => Isolation::Worktree,
        Some(other) => {
            return Err(pack_err(
                path,
                format!("frontmatter key \"isolation\" accepts only \"worktree\", got {other:?}"),
            ));
        }
    };
```

with:

```rust
    let isolation = match get_str("isolation")?.as_deref() {
        None => Isolation::default(),
        Some("worktree") => Isolation::Worktree,
        // The explicit opt-out (spec §12, dispatch-lifecycle Q1): the
        // agent intentionally runs on the rig's live tree; dispatch makes
        // that loud (`dispatch.live_tree`).
        Some("none") => Isolation::None,
        Some(other) => {
            return Err(pack_err(
                path,
                format!(
                    "frontmatter key \"isolation\" accepts only \"worktree\" or \"none\", got {other:?}"
                ),
            ));
        }
    };
```

(`Isolation::default()` is deliberate: Task 6 flips the enum's `#[default]` and this line follows it without a second edit.)

- [ ] **Step 4: Run the pack tests**

Run: `cargo test -p camp-core --lib pack`
Expected: all PASS (including `malformed_files_fail_naming_the_file_and_problem` — its `badiso.md` case asserts the message names `isolation` and the file, both still true).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/pack.rs
git commit -m "feat(core): accept isolation \"none\" as the explicit live-tree opt-out (#47)"
```

---

### Task 3: campd events `dispatch.live_tree` on every live-tree dispatch

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` (`launch()`, ~line 497: the `else { None }` branch of the worktree resolution)
- Test: `crates/camp/tests/daemon_dispatch.rs` (new integration test)

**Interfaces:**
- Consumes: `EventType::DispatchLiveTree` (Task 1); `Prep { spec, agent_name, .. }` — `prep.spec.cwd` is the already-canonicalized worker cwd (the rig path in the non-worktree branch of `prepare()`); test helpers `scaffold`, `write_agent`, `camp_ok`, `wait_until`, `count`, `seq_of`, `events_json`, `Daemon::spawn` (all already in `daemon_dispatch.rs`).
- Produces: every `launch()` with `prep.make_worktree == false` appends `dispatch.live_tree` (bead + rig on the envelope, `{"path", "agent"}` payload) before the `session.woke` registry row. NOTE: until Task 6 flips the default, ALL existing dispatch tests emit this event too (their agents still default to `Isolation::None`) — that is correct interim behavior, and no existing assertion counts it (verified: assertions are per-event-type).

- [ ] **Step 1: Write the failing integration test** in `crates/camp/tests/daemon_dispatch.rs`, after `a_spawn_failure_with_isolation_keeps_the_worktree`:

```rust
/// Phase 2 (dispatch-lifecycle Q1, spec §12): running on the rig's live
/// tree is an explicit opt-out and it is LOUD — every isolation="none"
/// dispatch appends dispatch.live_tree naming the path and agent, before
/// the worker's registry row. Never silent.
#[test]
fn an_isolation_none_dispatch_is_loud_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    write_agent(&root, "dev", "isolation: none\n");
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "live tree work"]).trim().to_owned();
    wait_until(&root, "the live-tree worker to stop", |e| {
        count(e, "session.stopped") == 1
    });

    let events = events_json(&root);
    let live = events
        .iter()
        .find(|e| e["type"] == "dispatch.live_tree")
        .expect("isolation=none dispatch must event dispatch.live_tree");
    assert_eq!(live["bead"], bead.as_str());
    assert_eq!(live["actor"], "campd");
    assert_eq!(live["data"]["agent"], "dev");
    // the recorded path is the worker's cwd — the CANONICAL rig path
    let canon_rig = std::fs::canonicalize(&rig).unwrap();
    assert_eq!(live["data"]["path"], canon_rig.to_str().unwrap());
    // loud BEFORE the worker exists: live_tree precedes the registry row
    let live_seq = seq_of(&events, |e| e["type"] == "dispatch.live_tree");
    let woke_seq = seq_of(&events, |e| e["type"] == "session.woke");
    assert!(
        live_seq < woke_seq,
        "dispatch.live_tree must precede session.woke: {events:#?}"
    );
    // and no worktree machinery ran
    assert_eq!(count(&events, "bead.worktree.reaped"), 0);
    assert!(!root.join("worktrees").join(&bead).exists());
}
```

- [ ] **Step 2: Run it to watch it fail**

Run: `cargo test -p camp --test daemon_dispatch an_isolation_none_dispatch_is_loud_in_the_ledger`
Expected: FAIL at the `.expect("isolation=none dispatch must event dispatch.live_tree")` — the event is never emitted.

- [ ] **Step 3: Implement.** In `crates/camp/src/daemon/dispatch.rs` `launch()`, replace the worktree-resolution `else` branch:

```rust
        } else {
            None
        };
```

with:

```rust
        } else {
            // The explicit live-tree opt-out (spec §12, dispatch-lifecycle
            // Q1): make it LOUD — a ledger fact before the registry row,
            // never a silent default (invariant 3).
            ledger.append(EventInput {
                kind: EventType::DispatchLiveTree,
                rig: Some(bead.rig.clone()),
                actor: "campd".into(),
                bead: Some(bead.id.clone()),
                data: serde_json::json!({
                    "path": prep.spec.cwd,
                    "agent": prep.agent_name,
                }),
            })?;
            None
        };
```

NOTE: `prep.agent_name` is moved into the `woke` JSON later in `launch()` — it is used there as `prep.agent_name` inside a `serde_json::json!` macro which borrows. Confirm the borrow order compiles; if the existing code moves `prep.agent_name` into `woke`, emit the live-tree event BEFORE building `woke` (it already is — the worktree resolution precedes the `woke` construction) and pass `"agent": prep.agent_name.clone()` only if the compiler demands it. Prefer the non-clone form; add `.clone()` only on a compile error, never silently restructure.

- [ ] **Step 4: Run the new test and the whole daemon_dispatch suite**

Run: `cargo test -p camp --test daemon_dispatch`
Expected: all PASS — including every pre-existing test (their agents currently default to live-tree, so each now ALSO emits `dispatch.live_tree`; no existing assertion is violated because all are per-event-type). If any test fails on an exact-count or exact-sequence assertion, STOP and re-read the failing assertion — fix the test only if the new event is genuinely orthogonal to what it asserts, and record the change in the PR description.

- [ ] **Step 5: Run the full workspace suite to catch cross-file assumptions**

Run: `cargo test --workspace`
Expected: all PASS (the flip has not happened yet; every agent still defaults to live-tree). Known flakes (issue #44): `worktree_isolation_creates_then_reaps_on_pass`, `a_dead_reader_nudge_fails_loudly` — rerun on spurious failure.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/daemon/dispatch.rs crates/camp/tests/daemon_dispatch.rs
git commit -m "feat(campd): event dispatch.live_tree on every live-tree dispatch (#47)"
```

---

### Task 4: Fake-agent branch evidence (`FAKE_AGENT_RECORD_BRANCH`)

**Files:**
- Modify: `crates/camp/tests/fake-agent.sh` (new env block after the `FAKE_AGENT_TOUCH` block; new doc line in the header comment)

**Interfaces:**
- Consumes: nothing new.
- Produces: `FAKE_AGENT_RECORD_BRANCH=<file>` — the fake worker writes the output of `git branch --show-current`, run in its OWN cwd, to `<file>` (relative to cwd) right after claiming. Task 5's obligation-(i) test reads it. Under `set -euo pipefail`, a non-git cwd makes the worker crash loudly — correct fail-fast behavior; only git-rig tests set this env.

- [ ] **Step 1: Add the env documentation line** in the header comment of `crates/camp/tests/fake-agent.sh`, after the `FAKE_AGENT_TOUCH` line:

```bash
#   FAKE_AGENT_RECORD_BRANCH  write `git branch --show-current` (as seen
#                         from the worker's own cwd) to this file — the
#                         Phase 2 isolation evidence
```

- [ ] **Step 2: Add the behavior block**, immediately after the `FAKE_AGENT_TOUCH` block:

```bash
if [[ -n "${FAKE_AGENT_RECORD_BRANCH:-}" ]]; then
  # Isolation evidence (Phase 2, dispatch-lifecycle §9 obligation i): the
  # WORKER records the branch of its own cwd — not the test guessing.
  git branch --show-current > "$FAKE_AGENT_RECORD_BRANCH"
fi
```

- [ ] **Step 3: Sanity-run the script's syntax**

Run: `bash -n crates/camp/tests/fake-agent.sh`
Expected: exit 0, no output.

- [ ] **Step 4: Commit** (with Task 5 — the env is exercised by the obligation-(i) test; committing here is also fine if preferred, but the test in Task 5/6 is its verification. Fold this file into Task 6's commit if executing strictly task-by-task.)

```bash
git add crates/camp/tests/fake-agent.sh
git commit -m "test: fake-agent records its cwd branch (FAKE_AGENT_RECORD_BRANCH) (#47)"
```

---

### Task 5: Write the three obligation tests (they FAIL red — the default is not flipped yet)

**Files:**
- Test: `crates/camp/tests/daemon_dispatch.rs` (4 new tests, appended after `an_isolation_none_dispatch_is_loud_in_the_ledger`)

**Interfaces:**
- Consumes: `scaffold`, `write_agent`, `git_rig`, `camp_ok`, `camp`, `wait_until`, `count`, `events_json`, `Daemon::spawn`, `FAKE_AGENT_HOLD_DIR`, `FAKE_AGENT_TOUCH`, `FAKE_AGENT_RECORD_BRANCH` (Task 4). `write_agent(&root, "dev", "")` OVERWRITES the scaffold's dev.md with a no-isolation-key agent — the flipped default under test.
- Produces: the design §9 Phase 2 binding test obligations (i), (ii), (iii) as executable tests. They go green in Task 6.

- [ ] **Step 1: Write obligation (i)** — default puts the worker on `camp/<bead>`, never the rig's branch:

```rust
/// Phase 2 test obligation (i) (dispatch-lifecycle §9): an autonomous
/// worker's cwd is a camp worktree on camp/<bead> BY DEFAULT — the agent
/// declares no isolation key — and never the rig's live branch. The
/// branch evidence is recorded by the WORKER from inside its own cwd
/// (`git branch --show-current`, FAKE_AGENT_RECORD_BRANCH).
#[test]
fn default_isolation_puts_the_worker_on_a_worktree_branch_never_the_rigs() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", ""); // NO isolation key: the DEFAULT under test
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap()),
            ("FAKE_AGENT_RECORD_BRANCH", "branch.txt"),
        ],
    );

    let bead = camp_ok(&root, &["sling", "default isolation"])
        .trim()
        .to_owned();
    wait_until(&root, "the default-isolated worker to claim", |e| {
        count(e, "bead.claimed") == 1
    });

    // The worker recorded its own branch from inside its own cwd; wait for
    // the file (the claim precedes the write by a subprocess tick).
    let wt = root.join("worktrees").join(&bead);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !wt.join("branch.txt").exists() {
        assert!(
            Instant::now() < deadline,
            "worker never recorded its branch; worktree dir: {}",
            wt.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let worker_branch = std::fs::read_to_string(wt.join("branch.txt"))
        .unwrap()
        .trim()
        .to_owned();
    let out = Command::new("git")
        .arg("-C")
        .arg(&rig)
        .args(["branch", "--show-current"])
        .output()
        .unwrap();
    let rig_branch = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(worker_branch, format!("camp/{bead}"));
    assert_eq!(rig_branch, "main");
    assert_ne!(
        worker_branch, rig_branch,
        "obligation (i): the worker's branch must never be the rig's checked-out branch"
    );
    assert!(
        !rig.join("branch.txt").exists(),
        "nothing may leak onto the rig's live tree"
    );
    // the default is not the opt-out: no live-tree event fired
    assert_eq!(count(&events_json(&root), "dispatch.live_tree"), 0);

    // release: a clean pass reaps the worktree (spec §12)
    std::fs::write(hold.join(&bead), "go").unwrap();
    wait_until(&root, "the worktree reap", |e| {
        count(e, "bead.worktree.reaped") == 1
    });
    assert!(!wt.exists());
}
```

- [ ] **Step 2: Write obligation (ii), git-init-only rig** — fail fast, no worker, nothing stranded:

```rust
/// Phase 2 test obligation (ii) (dispatch-lifecycle §9, §4.2.2): a rig
/// that cannot host a worktree — git-init-only, NO base commit — fails
/// fast at dispatch: dispatch.failed evented, no worker spawned, no
/// registry row, nothing stranded. The bead stays open and ready for
/// after the operator prepares the rig.
#[test]
fn a_baseless_rig_fails_fast_at_dispatch_with_no_worker_and_nothing_stranded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    // git init but NO commit: no base for a worktree branch
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    write_agent(&root, "dev", ""); // default isolation = worktree
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "cannot isolate here"])
        .trim()
        .to_owned();
    wait_until(&root, "the fail-fast dispatch", |e| {
        count(e, "dispatch.failed") == 1
    });

    let events = events_json(&root);
    let failed = events
        .iter()
        .find(|e| e["type"] == "dispatch.failed")
        .unwrap();
    assert_eq!(failed["bead"], bead.as_str());
    assert!(
        failed["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("git worktree add failed"),
        "reason must carry the git failure: {failed}"
    );
    // no worker was ever spawned: no registry row, no claim, no session end
    for kind in [
        "session.woke",
        "bead.claimed",
        "session.stopped",
        "session.crashed",
    ] {
        assert_eq!(count(&events, kind), 0, "{kind} must not fire");
    }
    // nothing stranded: no commit, no camp/<bead> branch, no worktree dir
    let revs = Command::new("git")
        .arg("-C")
        .arg(&rig)
        .args(["rev-list", "--all", "--count"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&revs.stdout).trim(), "0");
    let branches = Command::new("git")
        .arg("-C")
        .arg(&rig)
        .args(["branch", "--list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&branches.stdout).trim(), "");
    assert!(!root.join("worktrees").join(&bead).exists());
    // the bead is still open and ready — nothing lost
    let ls = camp_ok(&root, &["ls", "--ready", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&ls).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == bead.as_str()),
        "the bead must remain ready: {rows}"
    );
}
```

- [ ] **Step 3: Write obligation (ii), non-git rig** — the emptier case:

```rust
/// Obligation (ii), the emptier case: a rig directory that is not a git
/// repository at all. Same fail-fast contract, same ledger evidence.
#[test]
fn a_non_git_rig_fails_fast_at_dispatch_under_default_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, ""); // plain dir, no git
    write_agent(&root, "dev", ""); // default isolation = worktree
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "no repo here"]).trim().to_owned();
    wait_until(&root, "the fail-fast dispatch", |e| {
        count(e, "dispatch.failed") == 1
    });

    let events = events_json(&root);
    let failed = events
        .iter()
        .find(|e| e["type"] == "dispatch.failed")
        .unwrap();
    assert_eq!(failed["bead"], bead.as_str());
    assert!(
        failed["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("git worktree add failed"),
        "reason: {failed}"
    );
    assert_eq!(count(&events, "session.woke"), 0, "no worker spawned");
    assert!(!rig.join(".git").exists(), "the rig stays untouched");
    assert!(!root.join("worktrees").join(&bead).exists());
}
```

- [ ] **Step 4: Write obligation (iii)** — two concurrent workers, distinct worktrees:

```rust
/// Phase 2 test obligation (iii) (dispatch-lifecycle §9): two concurrent
/// autonomous workers on ONE rig get DISTINCT worktrees on distinct
/// camp/<bead> branches — no shared-tree collision — and the rig's live
/// tree is untouched.
#[test]
fn two_concurrent_default_workers_get_distinct_worktrees() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", ""); // default isolation = worktree
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap()),
            ("FAKE_AGENT_TOUCH", "proof.txt"),
        ],
    );

    let b1 = camp_ok(&root, &["sling", "first"]).trim().to_owned();
    let b2 = camp_ok(&root, &["sling", "second"]).trim().to_owned();
    wait_until(&root, "both workers to claim", |e| {
        count(e, "bead.claimed") == 2
    });

    let wt1 = root.join("worktrees").join(&b1);
    let wt2 = root.join("worktrees").join(&b2);
    assert_ne!(wt1, wt2, "distinct beads must get distinct worktrees");
    // both workers ran in their OWN worktree (proof.txt written by each)
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(wt1.join("proof.txt").exists() && wt2.join("proof.txt").exists()) {
        assert!(
            Instant::now() < deadline,
            "both workers must run in their own worktrees ({} / {})",
            wt1.display(),
            wt2.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // each worktree sits on its own camp/<bead> branch
    for (bead, wt) in [(&b1, &wt1), (&b2, &wt2)] {
        let out = Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("camp/{bead}")
        );
    }
    assert!(
        !rig.join("proof.txt").exists(),
        "the rig's live tree stays untouched"
    );

    std::fs::write(hold.join(&b1), "go").unwrap();
    std::fs::write(hold.join(&b2), "go").unwrap();
    wait_until(&root, "both reaps", |e| {
        count(e, "bead.worktree.reaped") == 2
    });
}
```

- [ ] **Step 5: Run all four and watch them fail RED (the default is still live-tree)**

Run: `cargo test -p camp --test daemon_dispatch -- default_isolation_puts_the_worker_on_a_worktree_branch_never_the_rigs a_baseless_rig_fails_fast_at_dispatch_with_no_worker_and_nothing_stranded a_non_git_rig_fails_fast_at_dispatch_under_default_isolation two_concurrent_default_workers_get_distinct_worktrees`
Expected: 4 FAILURES — obligation (i)/(iii) time out waiting for worktree evidence (workers run on the rig); the two fail-fast tests time out waiting for `dispatch.failed` (dispatch succeeds on the live tree). This red run is the proof the tests bite; capture the output for the PR evidence. Do NOT commit yet — the commit lands with the flip in Task 6 so every commit on the branch is green.

---

### Task 6: FLIP the default to Worktree + mechanically re-pin existing live-tree tests

**Files:**
- Modify: `crates/camp-core/src/pack.rs` (enum `#[default]`, ~line 18; two unit tests)
- Modify: `crates/camp/tests/daemon_dispatch.rs` (scaffold `write_agent` line ~60; `rig_default_agent_routes_dispatch` ~line 682; `worker_cwd_is_canonicalized_so_patrol_watches_the_real_transcript_path` ~line 776)
- Modify: `crates/camp/tests/daemon_patrol.rs` (agent tuples at lines ~194, ~245, ~306, ~357, ~472)
- Modify: `crates/camp/tests/daemon_graph.rs` (scaffold `dev.md` write, ~line 63)
- Modify: `crates/camp/tests/daemon_orders.rs` (`a_hot_reload_updates_dispatch_routing_without_a_restart`, rig setup ~line 386)
- Modify: `crates/camp/tests/perf_daemon.rs` (scaffold `write_agent`, ~line 56)
- Modify: `crates/camp/tests/e2e.rs` (`dev.md`/`reviewer.md` frontmatter, ~lines 334–350)

**Interfaces:**
- Consumes: `Isolation::default()` in `parse_agent_file` (Task 2) — flipping the enum default flips the parse default; the four red tests from Task 5.
- Produces: `Isolation::Worktree` as the enum + frontmatter default. Existing tests whose SUBJECT is live-tree worker mechanics carry `isolation: none` explicitly (they now also exercise the opt-out); the starter-pack-driven hot-reload test gets a git rig.

- [ ] **Step 1: Update the pack.rs default expectation tests FIRST (red).** In `crates/camp-core/src/pack.rs` tests: in `parses_a_claude_code_agent_file`, change

```rust
        assert_eq!(def.isolation, Isolation::None);
```

to

```rust
        // Phase 2 (dispatch-lifecycle Q1): no isolation key = the DEFAULT,
        // which is worktree.
        assert_eq!(def.isolation, Isolation::Worktree);
```

and add a dedicated default test after `isolation_none_is_an_accepted_explicit_opt_out`:

```rust
    #[test]
    fn isolation_defaults_to_worktree_when_undeclared() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(dir.path(), "d.md", "---\nname: d\n---\nWork.\n");
        let def = parse_agent_file(&dir.path().join("d.md")).unwrap();
        assert_eq!(def.isolation, Isolation::Worktree);
    }
```

Run: `cargo test -p camp-core --lib pack`
Expected: 2 FAILURES (`parses_a_claude_code_agent_file`, `isolation_defaults_to_worktree_when_undeclared`) — the default is still `None`.

- [ ] **Step 2: Flip the enum default.** In `crates/camp-core/src/pack.rs`, replace:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Isolation {
    #[default]
    None,
    Worktree,
}
```

with:

```rust
/// Where a dispatched worker's tree lives (spec §12). Worktree is the
/// DEFAULT for autonomous dispatch (dispatch-lifecycle Q1, approved
/// 2026-07-09): workers never run on the rig's live branch unless the
/// agent explicitly declares `isolation = "none"` — and that opt-out is
/// loud (`dispatch.live_tree`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Isolation {
    None,
    #[default]
    Worktree,
}
```

Run: `cargo test -p camp-core --lib pack`
Expected: all PASS.

- [ ] **Step 3: Re-pin the daemon_dispatch scaffold and the two live-tree-mechanics tests.** In `crates/camp/tests/daemon_dispatch.rs`:

(a) In `scaffold()` (~line 60), change

```rust
    write_agent(&root, "dev", "");
```

to

```rust
    // Post-flip (spec §12) the scaffold's dev agent PINS the live-tree
    // opt-out: these tests exercise worker mechanics (crash, cap, routing,
    // canonicalization) on the rig cwd, not the isolation contract — which
    // has its own tests below. Tests about the DEFAULT overwrite dev.md
    // with write_agent(&root, "dev", "").
    write_agent(&root, "dev", "isolation: none\n");
```

(b) In `rig_default_agent_routes_dispatch` (~line 683), change

```rust
    write_agent(&root, "rigger", "");
```

to

```rust
    write_agent(&root, "rigger", "isolation: none\n");
```

(c) In `worker_cwd_is_canonicalized_so_patrol_watches_the_real_transcript_path` (~line 776), change

```rust
    write_agent(&root, "dev", "");
```

to

```rust
    // this test asserts the LIVE-TREE (rig cwd) canonicalization branch
    write_agent(&root, "dev", "isolation: none\n");
```

- [ ] **Step 4: Re-pin daemon_patrol.** In `crates/camp/tests/daemon_patrol.rs`, change every `("dev", "")` agent tuple to `("dev", "isolation: none\n")` — call sites at ~lines 194, 245, 306, 357, 472 (the line-357 site is `&[("iso", "isolation: worktree\n"), ("dev", "isolation: none\n")]`; leave the `iso` half untouched). Verify no site is missed:

Run: `grep -n '("dev", "")' crates/camp/tests/daemon_patrol.rs`
Expected: no output.

- [ ] **Step 5: Re-pin daemon_graph.** In `crates/camp/tests/daemon_graph.rs` `scaffold()` (~line 63), change

```rust
    std::fs::write(agents.join("dev.md"), "---\nname: dev\n---\nDo the work.\n").unwrap();
```

to

```rust
    // graph tests exercise check loops / fan-out mechanics on the rig cwd;
    // the isolation contract (spec §12 default) has its own tests in
    // daemon_dispatch.rs — pin the opt-out explicitly.
    std::fs::write(
        agents.join("dev.md"),
        "---\nname: dev\nisolation: none\n---\nDo the work.\n",
    )
    .unwrap();
```

- [ ] **Step 6: Git-init the rig in the starter-pack hot-reload test.** In `crates/camp/tests/daemon_orders.rs` `a_hot_reload_updates_dispatch_routing_without_a_restart` (~line 386), after

```rust
    let rig = dir.path().join("repo");
    std::fs::create_dir_all(&rig).unwrap();
```

add:

```rust
    // The starter pack's dev agent follows the flipped worktree DEFAULT
    // (spec §12), so the rig must be able to host a worktree.
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
```

(Adjust to a bare `Command::new` if `std::process::Command` is already imported in that file — check the file's `use` list first.)

- [ ] **Step 7: Re-pin perf_daemon.** In `crates/camp/tests/perf_daemon.rs` `scaffold()` (~line 56), change

```rust
    write_agent(&root, "dev", "");
```

to

```rust
    // perf measures the spec §14 dispatch floor; `git worktree add` in the
    // measured path would change what the numbers mean. Pin the live-tree
    // opt-out — post-flip, perf measures the opt-out path (recorded as a
    // non-blocking note in the Phase 2 plan).
    write_agent(&root, "dev", "isolation: none\n");
```

- [ ] **Step 8: Re-pin e2e agents.** In `crates/camp/tests/e2e.rs` (~lines 334–350), add `isolation: none\n` to both agent frontmatters, with a comment above the `dev.md` write:

```rust
    // Phase 2 note: e2e Tier-0 asserts the work lands in the RIG's live
    // tree (`toy ls --json` run in the rig) — that is delivery, which is
    // Phase 3's contract. Until Phase 3 defines "landed" for the worktree
    // path, e2e pins the explicit live-tree opt-out (spec §12).
```

`dev.md` frontmatter becomes:

```rust
        "---\nname: dev\nmodel: sonnet\npermissionMode: bypassPermissions\n\
         isolation: none\n\
         tools: Read, Edit, Write, Bash, Grep, Glob\n---\n\
```

`reviewer.md` frontmatter becomes:

```rust
        "---\nname: reviewer\nmodel: sonnet\npermissionMode: bypassPermissions\n\
         isolation: none\n\
         tools: Read, Bash, Grep, Glob\n---\n\
```

(Only the frontmatter line is added; prompt bodies are untouched.)

- [ ] **Step 9: Run the four Task 5 tests — now GREEN**

Run: `cargo test -p camp --test daemon_dispatch -- default_isolation_puts_the_worker_on_a_worktree_branch_never_the_rigs a_baseless_rig_fails_fast_at_dispatch_with_no_worker_and_nothing_stranded a_non_git_rig_fails_fast_at_dispatch_under_default_isolation two_concurrent_default_workers_get_distinct_worktrees`
Expected: 4 PASS.

- [ ] **Step 10: Run the full workspace suite and triage**

Run: `cargo test --workspace`
Expected: all PASS. Triage policy for any failure: (a) the test's subject is live-tree worker mechanics → pin `isolation: none` in its agent; (b) the test's subject is isolation/worktrees → give it a git rig and the worktree expectations; (c) known #44 flakes → rerun to confirm; (d) anything else → STOP, apply superpowers:systematic-debugging, do not paper over. Record every re-pinned file beyond the list above in the PR description.

- [ ] **Step 11: Commit (tests + flip + sweep — one green commit)**

```bash
git add crates/camp-core/src/pack.rs crates/camp/tests/daemon_dispatch.rs crates/camp/tests/daemon_patrol.rs crates/camp/tests/daemon_graph.rs crates/camp/tests/daemon_orders.rs crates/camp/tests/perf_daemon.rs crates/camp/tests/e2e.rs
git commit -m "feat!: autonomous dispatch defaults to worktree isolation (#31, #47)"
```

(If Task 4's fake-agent.sh change was not committed separately, include `crates/camp/tests/fake-agent.sh` here.)

---

### Task 7: Spec §12 amendment (same PR — spec and code never diverge)

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md` §12 "Multi-rig and worktrees" (lines ~585–590: the "Dispatch sets the worker's cwd…" bullet). Do NOT touch §8.4 (Phase 1 owns it).

**Interfaces:**
- Consumes: the shipped behavior from Tasks 1–6 (default worktree; `isolation = "none"` opt-out; `dispatch.live_tree`; fail-fast `dispatch.failed`).
- Produces: the documented working-tree contract the kickoff requires: autonomous = worktree on `camp/<bead>`, reaped on clean pass, kept on failure; attended = supervised live tree (A2).

- [ ] **Step 1: Replace the third §12 bullet.** Replace exactly this text:

```markdown
- Dispatch sets the worker's cwd to the rig — or to a camp-managed worktree
  under `<camp>/worktrees/` when the agent definition sets
  `isolation = "worktree"`. Worktrees are removed on clean close and kept
  (with an event) on failure for forensics; the Gas Town worktree-cleanup
  lessons (leaked worktrees from crashed agents) are addressed by adoption
  (§8.5) sweeping orphaned worktrees against the registry.
```

with:

```markdown
- Dispatch sets the worker's cwd to a camp-managed worktree under
  `<camp>/worktrees/<bead>` on a fresh `camp/<bead>` branch — worktree
  isolation is the DEFAULT for autonomous dispatch (decision 2026-07-09,
  dispatch-lifecycle design Q1): an autonomous worker never runs on the
  rig's live branch. Worktrees are removed on clean close and kept (with
  an event) on failure for forensics; the Gas Town worktree-cleanup
  lessons (leaked worktrees from crashed agents) are addressed by adoption
  (§8.5) sweeping orphaned worktrees against the registry.
- The opt-out is explicit and loud: an agent that intentionally wants the
  live tree declares `isolation = "none"`, and every live-tree dispatch
  appends a `dispatch.live_tree` event naming the path and agent — running
  on the live tree is always visible in the ledger, never silent.
- Fail fast when a rig cannot host a worktree (not a git repository, or no
  base commit): dispatch appends `dispatch.failed` with the git error, no
  worker is spawned, and nothing is stranded — the operator prepares the
  rig (a base commit) before dispatching code work.
- The working-tree contract in one line: autonomous work happens on
  `camp/<bead>`, reaped on clean pass, kept on failure; attended work —
  the operator driving from their own session — is the documented standing
  exception (assumption A2, §17: a teammate's cwd is pinned to the parent
  session's directory, so worktree isolation is structurally unavailable
  there), supervised on the operator's live tree, where the operator owns
  integration (dispatch-lifecycle design §4.4).
```

- [ ] **Step 2: Proofread the section in context** (read §12 top to bottom; confirm the first two bullets and the final cross-rig bullet still read coherently; confirm no §8.4 text was touched):

Run: `git diff docs/design/2026-07-05-gas-camp-design.md`
Expected: one bullet replaced by four; no other hunks.

- [ ] **Step 3: Commit**

```bash
git add docs/design/2026-07-05-gas-camp-design.md
git commit -m "docs(spec): §12 — worktree isolation is the autonomous default; loud none opt-out; A2 attended exception (#31, #47)"
```

---

### Task 8: Gates, perf, push, PR, CI to a terminal result

**Files:** none new.

- [ ] **Step 1: Full gates**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all three exit 0. On a #44 flake, rerun the single test to confirm; never silence.

- [ ] **Step 2: Local perf suite** (dispatch behavior default changed; per AGENTS.md run perf before merging perf-relevant PRs — the perf fixture pins the opt-out, so the numbers must be unchanged):

Run: `make perf`
Expected: exit 0, spec §14 numbers hold. If it fails, STOP and debug — do not push.

- [ ] **Step 3: Rebase check.** Ask the lead / check whether a sibling PR (Phase 1 or issue-44) merged to main since branching. If yes: `git fetch origin && git rebase origin/main`, resolve (expected hot spots: `crates/camp/src/main.rs`, `Cargo.toml`/`Cargo.lock`, `event.rs`/`vocab.rs`, `fold.rs`, `daemon_dispatch.rs`), then re-run Step 1 (and Step 2 if dispatch paths were touched by the rebase). Never open the PR from a branch not rebased on current main.

- [ ] **Step 4: Push and open the PR**

```bash
git push -u origin phase-2-isolation-default
gh pr create --title "Dispatch Phase 2: worktree isolation is the autonomous default (#31)" --body "$(cat <<'EOF'
Implements dispatch-lifecycle Phase 2 (docs/design/2026-07-09-dispatch-lifecycle.md §9, Q1 APPROVED 2026-07-09).

Fixes #31
Closes #47

## What
- `Isolation` default flipped to `Worktree` (crates/camp-core/src/pack.rs); `isolation = "none"` is the explicit opt-out.
- The opt-out is LOUD: new camp-specific log-only event `dispatch.live_tree` (path + agent) on every live-tree dispatch, appended before the worker's registry row. Vocab partition + gc-collision guards green; fold payload is deny_unknown_fields; refold property green.
- Fail-fast proven: a rig that cannot host a worktree (non-git, or git-init-only with no base commit) → `dispatch.failed`, no worker spawned, no registry row, nothing stranded; the bead stays ready.
- Spec §12 amended IN THIS PR: default, opt-out, fail-fast, and the working-tree contract (autonomous = worktree on camp/<bead>, reaped on pass, kept on fail; attended = supervised live tree, A2). §8.4 untouched (Phase 1 owns it).

## Test obligations (design §9 Phase 2, BINDING)
- (i) `default_isolation_puts_the_worker_on_a_worktree_branch_never_the_rigs` — the WORKER records `git branch --show-current` from its own cwd (`camp/<bead>`), asserted ≠ the rig's checked-out branch (`main`).
- (ii) `a_baseless_rig_fails_fast_at_dispatch_with_no_worker_and_nothing_stranded` + `a_non_git_rig_fails_fast_at_dispatch_under_default_isolation` — `dispatch.failed` evented, zero session.woke/claim/session-end, zero commits/branches/worktrees, bead still ready.
- (iii) `two_concurrent_default_workers_get_distinct_worktrees` — two live workers, distinct worktrees on distinct `camp/<bead>` branches, rig tree untouched.
- Plus `an_isolation_none_dispatch_is_loud_in_the_ledger` — the opt-out events `dispatch.live_tree` before `session.woke`.

## Existing-test re-pins (mechanical consequence of the flip)
Tests whose subject is live-tree worker mechanics (crash/cap/routing/canonicalization/patrol/graph/perf/e2e) now declare `isolation: none` explicitly — they exercise the opt-out parse and keep their original assertions. The starter-pack hot-reload test got a git rig (the starter dev agent follows the new default). e2e pins the opt-out until Phase 3 defines "landed" for the worktree path. Scope exclusions honored: no delivery semantics, no WorkOutcome, no worker-contract text.
EOF
)"
```

- [ ] **Step 5: Watch CI to a TERMINAL result**

Run: `gh pr checks --watch`
Expected: all checks pass (terminal). On a #44-flake failure, rerun the job and note it; on any other failure, fix on the branch and re-run the gates before pushing again. Never stop with "CI is running".

- [ ] **Step 6: Report to the team lead** — PR number, CI status, and the exit criteria / test obligations quoted line by line with evidence (test names + command outputs, including the Task 5 RED run and the Task 6 GREEN run).

---

## Self-review (performed at plan-writing time)

1. **Spec coverage** against the kickoff + design §9 Phase 2: default flip (Task 6), explicit loud opt-out (Tasks 1–3), fail-fast confirmed and tested (Task 5/6), working-tree contract documented + spec §12 in the same PR (Task 7), obligations (i)/(ii)/(iii) (Tasks 5/6), one PR with `Fixes #31`/`Closes #47` + gates + CI watched (Task 8). Scope exclusions and sibling ownership honored (Global Constraints).
2. **Placeholder scan:** all steps carry complete code/commands; no TBDs.
3. **Type consistency:** `EventType::DispatchLiveTree` / `"dispatch.live_tree"` used identically in Tasks 1, 3, and the tests; `Isolation::Worktree` default consistent between Task 2 (`Isolation::default()`) and Task 6 (enum flip); `FAKE_AGENT_RECORD_BRANCH` name identical in Task 4 and Task 5.

## Known risks / notes for the reviewer

- **Interim state after Task 3:** until the flip (Task 6), every dispatch emits `dispatch.live_tree` (all agents still default to live-tree). Verified: no existing assertion counts that event type; the suite is run at that checkpoint to prove it.
- **`prep.agent_name` borrow in Task 3:** the live-tree event is appended before `woke` is built; if the compiler flags a move conflict, clone into the event payload — noted in the step, no silent restructuring.
- **Non-blocking note for the operator:** post-flip, the local perf suite measures the opt-out (live-tree) dispatch path by design; measuring the worktree default would change the spec §14 numbers' meaning and is a separate scoping decision.
- **e2e stays on the live tree** (explicit opt-out + comment) until Phase 3 defines "landed" for the worktree path — otherwise Phase 2 would break e2e for a reason that is Phase 3's contract.
- **`isolation` is a real Claude Code agent-file key** (documented value: `worktree`). Camp accepting `"none"` is a camp-read semantic over the same file; Claude Code tolerates unknown values in keys it does not act on, and the operator approved this exact opt-out spelling (design Q1) — recorded here, not re-litigated.

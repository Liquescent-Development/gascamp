# Failed-Dispatch Recovery (issue #83) Implementation Plan

> **Plan review: APPROVE, 2026-07-13 (Opus 4.8 plan gate).** One-transaction loop-termination argument, restart inversion, camp-specific vocab partition, and the fix-82 boundary (dispatch.rs:615 reason wrap untouched) all verified against code. Non-blocking notes: N1 Task 5 Step 7's git-add lists crates/camp/src/cmd/mod.rs which does not exist — drop that path from the add line (submodules are declared in main.rs, as the plan's own prose says); N2 refold_prop's generator never emits dispatch.rearmed, so the new fold arm has no property-level coverage — determinism is by construction and the focused idempotency test covers it, acceptable as planned; N3 the top-level HashSet import likely goes unused after deleting the failed set (known_pids uses the fully-qualified form) — the clippy gate catches it as the plan defers. No deviations accepted.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (this work stream runs planning and execution as separate sessions — a fresh implementer session executes this doc on branch `fix-83-failed-dispatch-recovery` after the plan gate's APPROVE). Steps use checkbox (`- [ ]`) syntax for tracking. Strict TDD: write the failing test, run it, watch it fail, implement, watch it pass, commit.

**Goal:** Make a failed dispatch recoverable — the failed state becomes ledger-visible and persists across restart, `camp top` stops advertising a runtime-unreachable bead as `ready`, and a new `camp retry <bead>` verb re-arms the existing bead (keeping its id and history) with no automatic retry loop.

**Architecture:** Today re-dispatch is suppressed by an in-memory `Dispatcher.failed: HashSet<String>` (hidden runtime state, lost on restart) while the ledger's `beads.dispatch_failure` column records the reason but does not gate dispatchability. This plan **unifies the two onto the ledger marker**: `dispatchable_beads` (and the `camp top` `ready` count) exclude beads whose `dispatch_failure` is set, so the in-memory set is deleted entirely. A new camp-specific event `dispatch.rearmed` clears the marker; the new `camp retry <bead>` CLI verb appends it and pokes campd (the same pure-client shape as `camp sling`). campd's existing `converge` then re-dispatches, because dispatchability is now derived from ledger truth alone.

**Tech Stack:** Rust (workspace crates `camp-core` and `camp`), SQLite ledger (rusqlite), event-sourced fold, clap CLI, Unix-socket campd protocol (newline-delimited JSON).

## Global Constraints

Copied verbatim from AGENTS.md and the kickoff — every task's requirements implicitly include these:

- **Invariant 1 (Idle is free):** No ticks, no polling loops. The recovery path is operator-triggered state + an explicit re-arm; there is NO timer-driven re-attempt.
- **Invariant 3 (Nothing hidden):** All durable truth is the ledger. The failed state and its clearing are both events; no runtime-only suppression set may remain.
- **Invariant 4 (Six primitives, zero roles):** No role names or judgment calls in Rust. `camp retry` mechanically re-arms; it never reasons about the work.
- **Invariant 5 (Fail fast):** No fallbacks, no silenced errors, no placeholders. No panics in library code (`clippy::unwrap_used`/`expect_used`/`panic` are denied outside `#[cfg(test)]` blocks that already `#[allow(...)]` them; `unsafe_code` forbidden).
- **Invariant 7 (Vocabulary mirror):** New event names are camp-specific and additive — they must NOT exist in gc's registry (`crates/camp-core/tests/fixtures/gc-vocab.json`). New payload structs use `#[serde(deny_unknown_fields)]`.
- **One-transaction event+state:** `Ledger::append` folds synchronously inside the write transaction (verified: `append` → `append_batch` → `insert_and_fold`), so a CLI-appended event's state effect is visible to the next state-table query with no campd round-trip.
- **Gates before every push (all must be green):** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`. Run every new or changed test and watch it fail before implementing.
- **No test may spawn a real `claude` or spend API money.** Workers are `#!/bin/sh` fakes (`crates/camp/tests/fake-agent.sh`). No network in tests.
- **House rules:** never add co-authors or mention yourself in commits; never commit to main (work lands on `fix-83-failed-dispatch-recovery`).

## Coordination with sibling fix-82

fix-82 changes *when* `dispatch.failed` fires (the branch-collision path in `crates/camp/src/daemon/spawn.rs`, around `create_worktree`/`remove_worktree`). This plan's design is **robustly additive** to that: whatever new site appends `dispatch.failed`, the fold sets `beads.dispatch_failure`, the ledger-derived exclusion suppresses re-dispatch, and `camp retry` recovers it — no coordination on the *trigger* is needed. Expect a small rebase. **Do not touch `spawn.rs`** (fix-82 and fix-86 own it). If a rebase against fix-82 lands a new `dispatch.failed` call site, no change here is required; just re-run the gates.

---

## Root Cause (confirmed against the code, not just the issue's analysis)

The issue names three symptoms (bead stays open/ready forever; the failed set is invisible; no retry verb). The single underlying root cause is a **split-brain between two suppression mechanisms**:

1. **In-memory** — `Dispatcher.failed: HashSet<String>` (`crates/camp/src/daemon/dispatch.rs:36`), consulted in `converge` at `dispatch.rs:432` (`.find(|b| !self.failed.contains(&b.id))`). Hidden (invariant 3), and per-process — a restart rebuilds it empty, so the bead is silently re-attempted once (the "restart clears it" secret state the issue documents).
2. **Ledger marker** — `beads.dispatch_failure`, folded from `dispatch.failed` (`crates/camp-core/src/ledger/fold.rs:612`), cleared only by `session.woke`/`bead.claimed` (`fold.rs:186`, `fold.rs:879`). Already shown by `camp ls` (`open:dispatch-failed`) and `camp show`, but it does **not** gate dispatchability.

Because `dispatchable_beads` (`crates/camp-core/src/readiness.rs:133`) and `ready_task_count` (`readiness.rs:153`) are pure state-table queries that ignore `dispatch_failure`, the ledger says "ready/dispatchable" while the in-memory set says "suppressed" — that disagreement is the `ready: 1` lie. `camp adopt` reconciles the *session registry* (`crates/camp/src/daemon/patrol.rs:1068`, iterates `live_sessions()`); a dispatch-failed bead has no session row, so adopt can never reach it.

**The fix collapses the two onto the ledger marker.** After that, deleting `self.failed` is safe and loop-termination is preserved: within one `converge`, after `dispatch_one` appends `dispatch.failed`, the fold sets the marker in the same transaction, so the loop's next `dispatchable_beads()` already excludes that bead (no hot-loop). Across restarts the marker persists in the ledger, so nothing is silently re-attempted.

## Design decisions and justification

- **Verb choice — `camp retry <bead>`, not overloading `camp adopt`.** `adopt` reconciles the *session registry* against process reality (it iterates live sessions); a dispatch-failed bead has no session, so folding this into adopt would blur its single responsibility (SOLID). `camp retry <bead>` names exactly the operation the issue's operator reached for, targets one named bead, and keeps the bead's id and history. It re-arms the *existing* bead — never a close + re-sling.
- **`camp retry` is a pure client that appends + pokes (the `camp sling` shape), not a new socket request.** Because dispatchability is now ledger-derived, re-arming is just "clear the marker (an event) + poke a running campd." This needs no new `Request`/`Response` wire variant (DRY, minimal protocol surface) and keeps campd the sole dispatcher (one dispatch path). The event (`dispatch.rearmed`) is appended by the CLI with `actor: "cli"`, exactly like `bead.created`/`bead.closed`/`bead.claimed`.
- **`camp top` truth — exclude stuck beads from `ready` AND surface them as `stuck`.** `ready` must mean "something will run it," so it drops beads with a set marker. To satisfy invariant 3 ("names them"), a new `stuck` count (open task beads with a set marker) is added to the status summary and rendered by `camp top`. The compact `--statusline` badge (`▲live ●ready ✖red`) is left unchanged: `ready` there is now truthful, and the badge is a widely-pinned minimal format; the stuck detail belongs in the full snapshot.
- **Out of scope (do not expand):** `ready_task_count` also over-counts ever-*sessioned* crashed beads that are open+unblocked but excluded from `dispatchable_beads` by the sessions clause. That is a pre-existing, separate discrepancy not named by issue #83 and not addressed here.

## File structure

| File | Change | Responsibility |
| --- | --- | --- |
| `crates/camp-core/src/event.rs` | modify (shared) | Add `EventType::DispatchRearmed` → `"dispatch.rearmed"` (enum, `ALL`, `as_str`). |
| `crates/camp-core/src/vocab.rs` | modify (shared) | Declare `"dispatch.rearmed"` camp-specific. |
| `crates/camp-core/src/ledger/fold.rs` | modify (shared) | Fold `dispatch.rearmed`: clear `beads.dispatch_failure`. Update `dispatch_failed` doc comment. |
| `crates/camp-core/src/readiness.rs` | modify | `dispatchable_beads` + `ready_task_count` exclude a set marker; add `stuck_task_count`; update `BeadRow.dispatch_failure` doc. |
| `crates/camp-core/src/ledger/mod.rs` | modify | `StatusSummary.stuck`; `status_summary` populates it. |
| `crates/camp/src/daemon/dispatch.rs` | modify (owned) | Delete `Dispatcher.failed`; `converge`/`dispatch_one`/`dispatch_bead` use the ledger-derived set. |
| `crates/camp/src/daemon/socket.rs` | modify | Update pinned wire-format test + `fake_campd::status` for the new `stuck` field. |
| `crates/camp/src/cmd/top.rs` | modify | Render the `stuck:` line; update render unit test. |
| `crates/camp/src/cmd/retry.rs` | create | `camp retry <bead>`: validate, append `dispatch.rearmed`, poke. |
| `crates/camp/src/main.rs` | modify (shared) | Declare `pub mod retry;` inside the inline `mod cmd { ... }` block (there is no `cmd/mod.rs` — the submodules are declared in `main.rs`); add `Command::Retry { bead }` + dispatch arm. |
| `crates/camp/src/cmd/show.rs` | modify | Replace the "restart campd" guidance with `camp retry <bead>`. |
| `crates/camp/tests/daemon_dispatch.rs` | modify | Two integration tests: recovery-with-retry, and restart-no-silent-retry. |

---

### Task 1: Add the `dispatch.rearmed` event type and vocabulary

**Files:**
- Modify: `crates/camp-core/src/event.rs` (enum `EventType`, `ALL`, `as_str`)
- Modify: `crates/camp-core/src/vocab.rs` (`CAMP_SPECIFIC_EVENTS`)
- Test: `crates/camp-core/src/event.rs` (existing `mod tests`), `crates/camp-core/tests/vocab_pin.rs` (existing partition/collision tests exercise the new entry)

**Interfaces:**
- Produces: `EventType::DispatchRearmed`, `EventType::DispatchRearmed.as_str() == "dispatch.rearmed"`. Consumed by Tasks 2, 3, 5, 6.

- [ ] **Step 1: Write the failing test** — add to the `mod tests` block in `crates/camp-core/src/event.rs`:

```rust
    #[test]
    fn dispatch_rearmed_round_trips_through_its_name() {
        assert_eq!(EventType::DispatchRearmed.as_str(), "dispatch.rearmed");
        assert_eq!(
            EventType::parse("dispatch.rearmed").unwrap(),
            EventType::DispatchRearmed
        );
        assert!(EventType::ALL.contains(&EventType::DispatchRearmed));
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p camp-core --lib event::tests::dispatch_rearmed_round_trips_through_its_name`
Expected: FAIL to compile — `no variant named DispatchRearmed`.

- [ ] **Step 3: Implement** — in `crates/camp-core/src/event.rs`, add the variant next to `DispatchFailed` in all three places:
  - In `enum EventType` (after `DispatchLiveTree`): `DispatchRearmed,`
  - In `EventType::ALL` (after `EventType::DispatchLiveTree,`): `EventType::DispatchRearmed,`
  - In `as_str` (after the `DispatchLiveTree => "dispatch.live_tree",` arm): `EventType::DispatchRearmed => "dispatch.rearmed",`

  In `crates/camp-core/src/vocab.rs`, add to `CAMP_SPECIFIC_EVENTS` (after `"dispatch.live_tree",`): `"dispatch.rearmed",`

- [ ] **Step 4: Run the event + vocab tests and watch them pass**

Run: `cargo test -p camp-core --lib event:: && cargo test -p camp-core --test vocab_pin`
Expected: PASS — including `every_event_type_round_trips_through_its_name`, `every_event_type_is_declared_mirrored_or_camp_specific_never_both`, and `camp_specific_names_do_not_collide_with_gc` (verified: `"dispatch.rearmed"` is absent from `gc-vocab.json`).

Note: `crates/camp-core/src/ledger/fold.rs`'s `fold` match is exhaustive (its only catch-all is the explicit `CampdStarted | CampdStopped` arm), so the crate will NOT compile until Task 2 adds the fold arm. That is expected; commit Tasks 1 and 2 together if the intermediate state does not build.

- [ ] **Step 5: Commit** (only if the crate builds; otherwise proceed to Task 2 and commit together)

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs
git commit -m "feat(events): add dispatch.rearmed event type and vocabulary"
```

---

### Task 2: Fold `dispatch.rearmed` — clear the marker

**Files:**
- Modify: `crates/camp-core/src/ledger/fold.rs` (add fold arm + handler; update `dispatch_failed` doc comment)
- Test: `crates/camp-core/src/ledger/fold.rs` (fold has an inline `mod tests`; if not, add the tests to `crates/camp-core/src/readiness.rs` tests instead — see Task 3, which also exercises the clear). Prefer a focused fold-level test here.

**Interfaces:**
- Consumes: `EventType::DispatchRearmed` (Task 1); `required_bead`, `known_bead`, `non_empty`, `payload` helpers (already in `fold.rs`).
- Produces: folding a `dispatch.rearmed` event sets `beads.dispatch_failure = NULL` for the named bead (idempotent when already clear).

- [ ] **Step 1: Write the failing test.** First confirm where fold tests live:

Run: `grep -n "mod tests" crates/camp-core/src/ledger/fold.rs`

If there is an inline `#[cfg(test)] mod tests`, add the test there. If there is not, place this test in `crates/camp-core/src/readiness.rs`'s `mod tests` (it already has `ledger()`, `create`, and event-append helpers). Written for the readiness test module (adapt the helper names if placing in fold.rs):

```rust
    #[test]
    fn dispatch_rearmed_clears_the_failure_marker() {
        use crate::event::{EventInput, EventType};
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        l.append(EventInput {
            kind: EventType::DispatchFailed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "reason": "rig path is not a directory" }),
        })
        .unwrap();
        assert_eq!(
            l.get_bead("gc-1").unwrap().unwrap().dispatch_failure.as_deref(),
            Some("rig path is not a directory")
        );

        l.append(EventInput {
            kind: EventType::DispatchRearmed,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "previous_reason": "rig path is not a directory" }),
        })
        .unwrap();
        assert_eq!(
            l.get_bead("gc-1").unwrap().unwrap().dispatch_failure,
            None,
            "re-arm clears the marker"
        );

        // idempotent: re-arming an already-clear bead is a harmless no-op
        l.append(EventInput {
            kind: EventType::DispatchRearmed,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "previous_reason": "rig path is not a directory" }),
        })
        .unwrap();
        assert_eq!(l.get_bead("gc-1").unwrap().unwrap().dispatch_failure, None);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p camp-core --lib dispatch_rearmed_clears_the_failure_marker`
Expected: FAIL — the fold has no `DispatchRearmed` arm, so `append` errors (or the crate does not build if Task 1 was committed without the arm).

- [ ] **Step 3: Implement.** In `crates/camp-core/src/ledger/fold.rs`, add the match arm next to `DispatchFailed` (around line 35):

```rust
        EventType::DispatchRearmed => dispatch_rearmed(conn, event),
```

Add the payload struct + handler next to `dispatch_failed` (after its function, around line 622):

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchRearmed {
    /// The dispatch.failed reason being cleared — carried so the ledger
    /// history is self-describing ("re-armed after: <reason>").
    previous_reason: String,
}

/// `dispatch.rearmed`: an operator re-armed a bead whose dispatch failed
/// (`camp retry`, issue #83). Clears `beads.dispatch_failure` so the bead
/// re-enters the dispatchable set on the next converge — the explicit
/// re-arm path (invariant 1: no automatic retry). Idempotent (like the
/// session.woke/claim clears): a bead whose marker is already clear is a
/// harmless no-op, which keeps refold deterministic.
fn dispatch_rearmed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: DispatchRearmed = payload(event)?;
    non_empty(event, "previous_reason", &p.previous_reason)?;
    conn.execute(
        "UPDATE beads SET dispatch_failure = NULL, updated_ts = ?2
         WHERE id = ?1 AND dispatch_failure IS NOT NULL",
        params![bead, event.ts],
    )?;
    Ok(())
}
```

Also update the `dispatch_failed` doc comment (around `fold.rs:611`): change "Cleared by a later session.woke/claim." to "Cleared by a later session.woke/claim, or by `dispatch.rearmed` (`camp retry`)."

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p camp-core --lib dispatch_rearmed_clears_the_failure_marker`
Expected: PASS.

- [ ] **Step 5: Run the refold property test** (a new fold arm must stay deterministic under replay):

Run: `cargo test -p camp-core --test refold_prop`
Expected: PASS (the generator does not emit `dispatch.rearmed`, and the fold is deterministic, so this stays green).

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs
git commit -m "feat(fold): dispatch.rearmed clears the dispatch_failure marker"
```

---

### Task 3: Make dispatchability ledger-derived; delete the in-memory `failed` set

**Files:**
- Modify: `crates/camp-core/src/readiness.rs` (`dispatchable_beads` SQL; `BeadRow.dispatch_failure` doc)
- Modify: `crates/camp/src/daemon/dispatch.rs` (delete `Dispatcher.failed` and its every use; `converge`; `dispatch_one`; `dispatch_bead`)
- Test: `crates/camp-core/src/readiness.rs` `mod tests`; existing `crates/camp/src/daemon/dispatch.rs` tests must stay green; existing `crates/camp/tests/daemon_dispatch.rs::an_unroutable_bead_lands_dispatch_failed_and_campd_survives` must stay green.

**Interfaces:**
- Consumes: `EventType::DispatchRearmed` fold (Task 2); `beads.dispatch_failure` column.
- Produces: `dispatchable_beads` excludes any bead whose `dispatch_failure IS NOT NULL`. `Dispatcher` no longer has a `failed` field. `converge` dispatches the head of the (ledger-derived) dispatchable set until the cap or the well runs dry.

- [ ] **Step 1: Write the failing test** — add to `crates/camp-core/src/readiness.rs`'s `mod tests` (after `dispatchable_still_excludes_after_bound_session_ends`):

```rust
    #[test]
    fn dispatchable_excludes_a_dispatch_failed_bead_until_rearmed() {
        use crate::event::{EventInput, EventType};
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        // a failed dispatch marks the bead — it drops out of the set
        l.append(EventInput {
            kind: EventType::DispatchFailed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "reason": "no agent to dispatch to" }),
        })
        .unwrap();
        assert!(
            l.dispatchable_beads().unwrap().is_empty(),
            "a dispatch-failed bead is not dispatchable"
        );
        // re-arming clears the marker and the bead is dispatchable again
        l.append(EventInput {
            kind: EventType::DispatchRearmed,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "previous_reason": "no agent to dispatch to" }),
        })
        .unwrap();
        let ids: Vec<String> = l
            .dispatchable_beads()
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ids, vec!["gc-1"], "a re-armed bead is dispatchable again");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p camp-core --lib readiness::tests::dispatchable_excludes_a_dispatch_failed_bead_until_rearmed`
Expected: FAIL on the first assertion — `dispatchable_beads` currently ignores the marker, so the bead is still returned.

- [ ] **Step 3: Implement the ledger-derived exclusion.** In `crates/camp-core/src/readiness.rs`, add one clause to `dispatchable_beads` (the SQL around line 133). The updated `sql` string:

```rust
    let sql = format!(
        "SELECT {BEAD_COLS} FROM beads b
         WHERE b.status = 'open' AND {TASK}
           AND b.dispatch_failure IS NULL
           AND NOT (b.run_id IS NOT NULL AND b.step_id IS NULL)
           AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.bead = b.id)
           AND NOT EXISTS (
             SELECT 1 FROM deps d LEFT JOIN beads t ON t.id = d.needs_id
             WHERE d.bead_id = b.id AND {UNMET_DEP})
         ORDER BY b.created_ts, b.id"
    );
```

Update the `dispatchable_beads` doc comment to note the new exclusion: after "never dispatched before" add ", and not currently dispatch-failed (`dispatch_failure` clear — a failed dispatch is suppressed until `camp retry` re-arms it, invariant 3/1)".

Update the `BeadRow.dispatch_failure` field doc (readiness.rs:28-34): replace the "Retry semantics ... campd's in-memory failed set suppresses re-dispatch for its lifetime ... a campd restart retries (once per restart)" sentences with:

```rust
    /// Retry semantics (issue #83): while this is set the bead is excluded
    /// from `dispatchable_beads` and from the `ready` count, and it persists
    /// across a campd restart (no silent re-attempt). `camp retry <bead>`
    /// appends `dispatch.rearmed`, which clears this — the explicit,
    /// operator-visible re-arm path. `camp show` states this next to the
    /// reason.
```

- [ ] **Step 4: Run the readiness test and watch it pass**

Run: `cargo test -p camp-core --lib readiness::tests`
Expected: PASS — the new test and all existing readiness tests.

- [ ] **Step 5: Delete the in-memory `failed` set in `crates/camp/src/daemon/dispatch.rs`.** Make these edits:

  1. Remove the `failed` field from `struct Dispatcher` (lines ~34-36):
  ```rust
      /// Beads that failed to dispatch this campd lifetime (plan decision
      /// F): one dispatch.failed each, retried once per restart (crash-only).
      failed: HashSet<String>,
  ```
  Delete those three lines.

  2. In `Dispatcher::new`, remove the `failed: HashSet::new(),` initializer line.

  3. If `HashSet` is now unused, drop it from the `use std::collections::{HashMap, HashSet};` import at the top — change to `use std::collections::HashMap;`. (Verify with clippy in Step 7; `HashSet` is also used by `known_pids` via `std::collections::HashSet` fully-qualified, so the top-level import may already be unused after this change — let clippy decide.)

  4. In `converge` (lines ~429-434), replace the failed-filter with the head of the ledger-derived set:
  ```rust
          loop {
              if self.children.len() >= self.config.dispatch.max_workers {
                  return Ok(());
              }
              let Some(bead) = ledger.dispatchable_beads()?.into_iter().next() else {
                  return Ok(());
              };
              self.dispatch_one(ledger, &bead)?;
          }
  ```
  Update the `converge` doc comment: the phrase "the ledger is the only bookkeeping" is now literally true; add "Dispatch failures suppress re-dispatch through the ledger's `dispatch_failure` marker (invariant 3), not an in-memory set — a marked bead leaves `dispatchable_beads`, so the loop advances and never hot-loops the same failure."

  5. In `dispatch_one` (line ~495), delete the `self.failed.insert(bead.id.clone());` line — the appended `dispatch.failed` already sets the marker via the fold. The block becomes:
  ```rust
          let prep = match self.prepare(ledger, bead) {
              Ok(prep) => prep,
              Err(reason) => {
                  ledger.append(EventInput {
                      kind: EventType::DispatchFailed,
                      rig: Some(bead.rig.clone()),
                      actor: "campd".into(),
                      bead: Some(bead.id.clone()),
                      data: serde_json::json!({ "reason": reason }),
                  })?;
                  return Ok(());
              }
          };
  ```

  6. In `launch` (the worktree-creation failure path, line ~609), delete the `self.failed.insert(bead.id.clone());` line the same way (keep the `ledger.append(DispatchFailed ...)`).

  7. In `dispatch_bead` (lines ~483-485), delete the two lines:
  ```rust
          // a patrol respawn supersedes an earlier same-life dispatch failure
          self.failed.remove(bead_id);
  ```
  so the tail becomes just `self.dispatch_one(ledger, &bead)`. (A patrol respawn targets an ever-sessioned bead whose marker was already cleared by its original `session.woke`; the removed line is now a no-op.)

- [ ] **Step 6: Run the dispatcher's own tests and watch them pass**

Run: `cargo test -p camp --lib daemon::dispatch`
Expected: PASS — including `queued_patrol_respawn...` (its beads are ever-sessioned, still excluded by the sessions clause and re-hooked via the targeted queue, unaffected by the new clause).

- [ ] **Step 7: Run fmt + clippy to catch the possibly-unused `HashSet` import**

Run: `cargo fmt --all --check && cargo clippy -p camp --all-targets --all-features -- -D warnings`
Expected: PASS. If clippy flags an unused import, apply the minimal fix (drop `HashSet` from the top-of-file `use`).

- [ ] **Step 8: Run the dispatch integration suite (proves one-per-lifetime still holds)**

Run: `cargo test -p camp --test daemon_dispatch an_unroutable_bead_lands_dispatch_failed_and_campd_survives`
Expected: PASS — the marker now provides the "exactly once per bead per campd lifetime" suppression the in-memory set used to provide.

- [ ] **Step 9: Commit**

```bash
git add crates/camp-core/src/readiness.rs crates/camp/src/daemon/dispatch.rs
git commit -m "fix(dispatch): derive dispatchability from the ledger marker, delete the in-memory failed set (#83)"
```

---

### Task 4: `camp top` truth — `ready` excludes stuck beads; add a `stuck` count

**Files:**
- Modify: `crates/camp-core/src/readiness.rs` (`ready_task_count`; new `stuck_task_count`)
- Modify: `crates/camp-core/src/ledger/mod.rs` (`StatusSummary.stuck`; `status_summary`; test literals)
- Modify: `crates/camp/src/daemon/socket.rs` (pinned wire test; `fake_campd::status`)
- Modify: `crates/camp/src/cmd/top.rs` (render + render test)
- Test: as above (unit tests in each file)

**Interfaces:**
- Consumes: `beads.dispatch_failure` marker.
- Produces: `readiness::stuck_task_count(conn) -> Result<u64, CoreError>`; `StatusSummary { live_sessions, ready, open, stuck }` (field order: `stuck` after `open`); `ready_task_count` no longer counts a marked bead.

- [ ] **Step 1: Write the failing count test** — add to `crates/camp-core/src/ledger/mod.rs`'s status tests (in `mod tests`, after `status_summary_counts_only_task_beads`):

```rust
    #[test]
    fn status_summary_moves_a_dispatch_failed_bead_from_ready_to_stuck() {
        use crate::event::{EventInput, EventType};
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({ "title": "one" })))
            .unwrap();
        // ready before the failure
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 1,
                open: 1,
                stuck: 0,
            }
        );
        // a dispatch failure: no longer ready, now stuck (still open)
        ledger
            .append(EventInput {
                kind: EventType::DispatchFailed,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({ "reason": "no agent" }),
            })
            .unwrap();
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 0,
                open: 1,
                stuck: 1,
            }
        );
    }
```

Note: `created(...)` / `temp_ledger()` are existing helpers in this test module (used by the neighbouring `status_summary_*` tests).

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p camp-core --lib status_summary_moves_a_dispatch_failed_bead_from_ready_to_stuck`
Expected: FAIL to compile — `StatusSummary` has no field `stuck`.

- [ ] **Step 3: Implement the counts.** In `crates/camp-core/src/readiness.rs`:
  - In `ready_task_count` (line ~153), add `AND b.dispatch_failure IS NULL` to the WHERE:
  ```rust
      let sql = format!(
          "SELECT count(*) FROM beads b
           WHERE b.status = 'open' AND {TASK}
             AND b.dispatch_failure IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM deps d LEFT JOIN beads t ON t.id = d.needs_id
               WHERE d.bead_id = b.id AND {UNMET_DEP})"
      );
      count_nonneg(conn, &sql, "ready-task")
  ```
  Update its doc comment: after "narrowed to plain work (`TASK`)" add "and excluding beads whose dispatch failed (`dispatch_failure` set), which are counted as `stuck` instead (issue #83) — so `ready` means campd will actually pick it up".
  - Add a new function next to it:
  ```rust
  /// The number of stuck TASK beads — open plain work whose dispatch failed
  /// and has not been re-armed (`beads.dispatch_failure` set). The status
  /// surface's `stuck` count (issue #83): a bead that is open in the ledger
  /// but unreachable in the runtime until `camp retry` re-arms it. Task-scoped
  /// like the other status counts.
  pub fn stuck_task_count(conn: &Connection) -> Result<u64, CoreError> {
      let sql = format!(
          "SELECT count(*) FROM beads b
           WHERE b.status = 'open' AND {TASK} AND b.dispatch_failure IS NOT NULL"
      );
      count_nonneg(conn, &sql, "stuck-task")
  }
  ```

  In `crates/camp-core/src/ledger/mod.rs`:
  - Add `pub stuck: u64,` to `struct StatusSummary` (after `pub open: u64,`).
  - In `status_summary` (line ~325), populate it:
  ```rust
          let ready = crate::readiness::ready_task_count(&self.conn)?;
          let open = crate::readiness::open_task_count(&self.conn)?;
          let stuck = crate::readiness::stuck_task_count(&self.conn)?;
          Ok(StatusSummary {
              live_sessions,
              ready,
              open,
              stuck,
          })
  ```
  - Add `stuck: 0` (or the right value) to every existing `StatusSummary { ... }` literal in this file's tests: the assertions at lines ~2220, ~2258, ~2291, ~2307, ~2324. Every one of those neighbours expects no stuck bead, so `stuck: 0` in each.

- [ ] **Step 4: Update the other `StatusSummary` construction sites and pinned wire format.**
  - `crates/camp/src/cmd/top.rs` render test (`render_is_plain_text_and_stable`, lines ~76 and ~85): add `stuck: 0` to the `empty` literal and `stuck: 0` to the `busy` literal, and update the two expected render strings (see Step 5 for the format).
  - `crates/camp/src/daemon/socket.rs`:
    - `fake_campd::status` (line ~613): add `stuck: 0,` to its `StatusSummary { ... }`.
    - `response_wire_format_is_pinned` (line ~1055): add `stuck: 0,` to the `StatusSummary` literal, and update the expected JSON string. `StatusSummary` is `#[serde(flatten)]`ed into `Response::Status`, and serde emits fields in declaration order, so `stuck` lands between `open` and `red`:
    ```rust
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                r#"{"ok":true,"live_sessions":["camp/dev/1"],"ready":1,"open":2,"stuck":0,"red":1,"campd_pid":4242}"#
            );
    ```
    - In the same test, the `Response::Status` round-trip parse assertion (the `serde_json::from_str::<Response>(...)` for a Status line, line ~1100) must include `"stuck":0`:
    ```rust
            assert!(matches!(
                serde_json::from_str::<Response>(
                    r#"{"ok":true,"live_sessions":[],"ready":0,"open":0,"stuck":0,"red":0,"campd_pid":1}"#
                )
                .unwrap(),
                Response::Status { .. }
            ));
    ```

- [ ] **Step 5: Render the `stuck` line in `camp top`.** In `crates/camp/src/cmd/top.rs`, update `render` to add a `stuck:` line after `open:`:

```rust
    format!(
        "campd pid: {campd_pid}\nlive sessions: {sessions}\nready: {}\nopen: {}\nstuck: {}\nred: {red}\n",
        summary.ready, summary.open, summary.stuck
    )
```

Update the two expected strings in `render_is_plain_text_and_stable`:
```rust
        assert_eq!(
            render(&empty, 0, 4242),
            "campd pid: 4242\nlive sessions: 0\nready: 0\nopen: 0\nstuck: 0\nred: 0\n"
        );
        // ... busy: live_sessions ["camp/dev/1","camp/dev/2"], ready 1, open 3, stuck 0, red 1, pid 7
        assert_eq!(
            render(&busy, 1, 7),
            "campd pid: 7\nlive sessions: 2 (camp/dev/1, camp/dev/2)\nready: 1\nopen: 3\nstuck: 0\nred: 1\n"
        );
```
(The `busy` literal must set `stuck: 0` — add it in Step 4. The compact `statusline` badge `▲{live} ●{ready} ✖{red}` is intentionally left unchanged.)

- [ ] **Step 6: Run all touched tests and watch them pass**

Run: `cargo test -p camp-core --lib status_summary && cargo test -p camp-core --lib readiness && cargo test -p camp --lib cmd::top && cargo test -p camp --lib daemon::socket`
Expected: PASS.

- [ ] **Step 7: Confirm no other test constructs `StatusSummary` or pins `camp top` text**

Run: `grep -rn "StatusSummary {" crates/ ; grep -rn "ready:\|open:\|stuck:" crates/camp/tests/`
Expected: every `StatusSummary { ... }` now carries `stuck`. `crates/camp/tests/daemon_lifecycle.rs` uses `out.contains("ready: 0")` / `out.contains("open: 0")` — a substring match, unaffected by an added `stuck:` line. If any literal was missed, add `stuck: <value>`.

- [ ] **Step 8: Commit**

```bash
git add crates/camp-core/src/readiness.rs crates/camp-core/src/ledger/mod.rs crates/camp/src/daemon/socket.rs crates/camp/src/cmd/top.rs
git commit -m "feat(top): count dispatch-stuck beads and drop them from ready (#83)"
```

---

### Task 5: `camp retry <bead>` CLI verb (+ updated `camp show` guidance)

**Files:**
- Create: `crates/camp/src/cmd/retry.rs`
- Modify: `crates/camp/src/main.rs` (declare `pub mod retry;` inside the inline `mod cmd { ... }` block — there is NO `cmd/mod.rs`; the submodules are declared in `main.rs` at lines 7-31; then add `Command::Retry { bead }` + dispatch arm)
- Modify: `crates/camp/src/cmd/show.rs` (guidance text)
- Test: `crates/camp/src/cmd/retry.rs` (inline unit tests for the validation branches, using an in-repo ledger — no campd needed)

**Interfaces:**
- Consumes: `EventType::DispatchRearmed` (Task 1); `beads.dispatch_failure` marker; `socket::{require, Request::Poke}` (the `camp sling` pattern); `Ledger::{open, get_bead, append}`; `CampDir`.
- Produces: `crate::cmd::retry::run(camp: &CampDir, bead: String) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing unit tests.** Create `crates/camp/src/cmd/retry.rs` with the module skeleton and its tests. First inspect an existing verb that opens the ledger and validates a bead (e.g. `crates/camp/src/cmd/close.rs`) to mirror import style. The tests exercise the two validation branches that do NOT need a running campd (bail before the poke):

```rust
use anyhow::{Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request};

/// `camp retry <bead>` (issue #83): re-arm a bead whose dispatch failed,
/// keeping its id and history. A PURE CLIENT (design §4.3) in the `camp
/// sling` shape — append the durable `dispatch.rearmed` fact (which the fold
/// uses to clear `beads.dispatch_failure`), then poke a RUNNING campd so it
/// re-dispatches. campd is the sole dispatcher; there is no second path.
///
/// The re-arm is an EXPLICIT operator action, never a timer (invariant 1).
/// A bead with no failed dispatch is a loud, actionable error — re-arming
/// clean work would be a lie about state (invariant 5).
pub fn run(camp: &CampDir, bead: String) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let row = ledger
        .get_bead(&bead)?
        .ok_or_else(|| anyhow::anyhow!("no such bead: {bead}"))?;
    let Some(previous_reason) = row.dispatch_failure.clone() else {
        bail!(
            "{bead} has no failed dispatch to retry (its dispatch_failure marker is clear). \
             `camp show {bead}` shows its current state; `camp top` counts stuck beads."
        );
    };
    let seq = ledger.append(EventInput {
        kind: EventType::DispatchRearmed,
        rig: Some(row.rig.clone()),
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data: serde_json::json!({ "previous_reason": previous_reason }),
    })?;
    drop(ledger); // campd may need the write lock immediately

    // The re-arm is DURABLE now: print it before the poke, so a campd that
    // cannot serve us costs the operator the dispatch, never the re-arm.
    println!("re-armed {bead} (was: {previous_reason})");
    socket::require(camp, &Request::Poke { seq }).map_err(|e| {
        e.context(format!(
            "{bead} is re-armed and durable, but NOT dispatched — only a healthy, running \
             campd dispatches; it runs as soon as one is (campd catches up from its cursor \
             on start)"
        ))
    })?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn camp_with_ledger() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        // touch the ledger so it exists
        drop(Ledger::open(&camp.db_path()).unwrap());
        (dir, camp)
    }

    #[test]
    fn retry_on_an_unknown_bead_bails() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let (_d, camp) = camp_with_ledger();
        let err = run(&camp, "gc-404".into()).unwrap_err();
        assert!(
            format!("{err:#}").contains("no such bead"),
            "err was: {err:#}"
        );
    }

    #[test]
    fn retry_on_a_bead_with_no_failure_bails_before_any_event() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let (_d, camp) = camp_with_ledger();
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({ "title": "healthy work" }),
            })
            .unwrap();
        drop(ledger);

        let err = run(&camp, "gc-1".into()).unwrap_err();
        assert!(
            format!("{err:#}").contains("no failed dispatch to retry"),
            "err was: {err:#}"
        );
        // no dispatch.rearmed was appended
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert_eq!(
            ledger
                .events_of_type(EventType::DispatchRearmed)
                .unwrap()
                .len(),
            0
        );
    }
}
```

Before writing, verify the helper names exist as used:
- `Run: grep -n "spawn_probe_guard" crates/camp/src/daemon/mod.rs` — confirms the test guard that blocks accidental process spawns (used across these tests).
- `Run: grep -n "fn events_of_type" crates/camp-core/src/ledger/mod.rs` — confirms the read helper the second test uses. If it is named differently, use the equivalent (`events_named`/`events_for_bead`) — adapt the assertion accordingly.
- `Run: grep -n "pub fn db_path" crates/camp/src/campdir.rs` — confirms `CampDir::db_path`.

- [ ] **Step 2: Wire the module and CLI.**
  - In `crates/camp/src/main.rs`, add `pub mod retry;` to the inline `mod cmd { ... }` block (lines 7-31, keep it alphabetical — between `pub mod remember;` and `pub mod rig;`).
  - In `crates/camp/src/main.rs`, add a variant to `enum Command` (place it near `Adopt`/`Nudge` — recovery verbs):
  ```rust
      /// Re-arm a bead whose dispatch failed, keeping its id and history
      /// (campd must be running). See `camp show <bead>` / `camp top`.
      Retry {
          /// Bead id (the one shown as dispatch-failed by `camp show`/`camp ls`)
          bead: String,
      },
  ```
  - In `main.rs`'s command dispatch `match`, add the arm (mirror the `Command::Adopt` / `Command::Sling` arms — they resolve `camp` the same way):
  ```rust
          Command::Retry { bead } => cmd::retry::run(&camp, bead),
  ```
  Inspect the neighbouring arms first (`grep -n "Command::Adopt\|Command::Sling" crates/camp/src/main.rs`) to match exactly how `camp` (the `CampDir`) is obtained and how the arm returns.

- [ ] **Step 3: Run the unit tests and watch them fail, then pass**

Run: `cargo test -p camp --lib cmd::retry`
Expected: first run FAILs (module/verb not present) → after Steps 1-2, PASS.

- [ ] **Step 4: Update `camp show` guidance.** In `crates/camp/src/cmd/show.rs` (lines ~203-212), replace the "restart campd" note with the `camp retry` path:

```rust
    if let Some(df) = &row.dispatch_failure {
        // issue #83: the marker persists across restart and suppresses
        // re-dispatch; `camp retry` is the explicit, evented re-arm path.
        println!("dispatch-failed  {df}");
        println!(
            "                 (won't dispatch until re-armed — after fixing the cause, run `camp retry {}`)",
            row.id
        );
    }
```

- [ ] **Step 5: Confirm nothing else pins the old show guidance text**

Run: `grep -rn "retries once per restart\|restart campd" crates/camp`
Expected: no matches remain (the old sentence is gone). If `crates/camp/tests/cli_show.rs` asserted the old text, update that assertion to expect `camp retry`.

- [ ] **Step 6: Run fmt + clippy + the show/retry tests**

Run: `cargo fmt --all --check && cargo clippy -p camp --all-targets --all-features -- -D warnings && cargo test -p camp --lib cmd::retry cmd::show && cargo test -p camp --test cli_show`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/cmd/retry.rs crates/camp/src/cmd/mod.rs crates/camp/src/main.rs crates/camp/src/cmd/show.rs
git commit -m "feat(cli): add camp retry to re-arm a failed dispatch (#83)"
```

---

### Task 6: End-to-end acceptance tests (the issue's session, with a fake agent)

**Files:**
- Modify: `crates/camp/tests/daemon_dispatch.rs` (two new `#[test]` functions using the existing `scaffold`/`Daemon`/`camp_ok`/`wait_until`/`count` harness)

**Interfaces:**
- Consumes: everything from Tasks 1-5. The failure mode is a **missing rig directory** (`prepare` fails `rig.path.is_dir()` at `dispatch.rs:520`), which is fixable from the test WITHOUT a config hot-reload: recreate the directory, then `camp retry`. The scaffold's `dev` agent is `isolation: none`, so a plain (non-git) rig directory dispatches fine once it exists (`spawn::rig_base` returns `Ok(None)` for a non-repo dir).

- [ ] **Step 1: Write the recovery acceptance test** — append to `crates/camp/tests/daemon_dispatch.rs`:

```rust
/// issue #83: a failed dispatch is recoverable. The rig directory is missing
/// at dispatch time, so dispatch fails; `camp top` stops calling the bead
/// `ready` and names it `stuck`; `camp show` points at `camp retry`; after
/// the cause is fixed, `camp retry` re-dispatches the SAME bead (id and
/// history intact) — never a close + re-sling. Fully evented.
#[test]
fn a_failed_dispatch_is_recoverable_with_camp_retry_keeping_the_bead_id() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    // break the cause: remove the rig directory so prepare() fails is_dir
    std::fs::remove_dir_all(&rig).unwrap();

    let _campd = Daemon::spawn(&root, &[]);
    let bead = camp_ok(&root, &["sling", "recover me"]).trim().to_owned();
    wait_until(&root, "the dispatch failure", |e| {
        count(e, "dispatch.failed") == 1
    });

    // camp top: not counted ready; counted stuck
    let top = camp_ok(&root, &["top"]);
    assert!(top.contains("ready: 0"), "top: {top}");
    assert!(top.contains("stuck: 1"), "top: {top}");

    // camp show: the reason and the recovery verb
    let show = camp_ok(&root, &["show", &bead]);
    assert!(show.contains("dispatch-failed"), "show: {show}");
    assert!(
        show.contains(&format!("camp retry {bead}")),
        "show must name the recovery verb: {show}"
    );

    // fix the cause, then re-arm the EXISTING bead
    std::fs::create_dir_all(&rig).unwrap();
    let retry_out = camp_ok(&root, &["retry", &bead]);
    assert!(retry_out.contains(&bead), "retry out: {retry_out}");

    // it re-dispatches: a session.woke names the same bead
    wait_until(&root, "the re-dispatch", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.woke" && ev["data"]["bead"] == bead.as_str()
        })
    });

    let events = events_json(&root);
    // exactly one re-arm, keyed to the bead
    let rearms: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "dispatch.rearmed" && e["bead"] == bead.as_str())
        .collect();
    assert_eq!(rearms.len(), 1, "one dispatch.rearmed: {events:#?}");
    assert!(
        rearms[0]["data"]["previous_reason"]
            .as_str()
            .unwrap()
            .contains("directory"),
        "the re-arm records the prior reason: {}",
        rearms[0]["data"]["previous_reason"]
    );
    // the bead was NEVER closed — recovery keeps its id and history
    assert_eq!(
        count(&events, "bead.closed"),
        0,
        "recovery must not close and re-sling: {events:#?}"
    );
}
```

- [ ] **Step 2: Run it and watch it fail (then pass after Tasks 1-5 are in)**

Run: `cargo test -p camp --test daemon_dispatch a_failed_dispatch_is_recoverable_with_camp_retry_keeping_the_bead_id -- --nocapture`
Expected: with Tasks 1-5 implemented, PASS. (If run before them, it fails on the missing `camp retry` verb / missing `stuck:` line — that is the red state; do not implement past what the earlier tasks specify.)

- [ ] **Step 3: Write the restart test — the secret state is gone.** Append:

```rust
/// issue #83: the failed state is ledger-durable — a campd RESTART no longer
/// silently re-attempts the bead (the old in-memory failed set was rebuilt
/// empty on restart). Proven deterministically: bead A fails; after a restart
/// a second bead B is created and also fails (a positive sync that campd has
/// converged at least once) — and A's dispatch.failed count is still exactly
/// one, so the restart did not re-attempt A.
#[test]
fn a_dispatch_failure_survives_a_campd_restart_without_a_silent_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    std::fs::remove_dir_all(&rig).unwrap();

    let bead_a = {
        let _campd = Daemon::spawn(&root, &[]);
        let a = camp_ok(&root, &["sling", "bead A"]).trim().to_owned();
        wait_until(&root, "A's dispatch failure", |e| {
            e.iter().any(|ev| {
                ev["type"] == "dispatch.failed" && ev["bead"] == a.as_str()
            })
        });
        a
        // _campd dropped here: campd is killed and reaped (restart)
    };

    let _campd2 = Daemon::spawn(&root, &[]);
    // positive sync: create B (rig still missing), wait for ITS failure —
    // guarantees campd2 has run a converge that scanned the dispatchable set.
    let bead_b = camp_ok(&root, &["sling", "bead B"]).trim().to_owned();
    wait_until(&root, "B's dispatch failure", |e| {
        e.iter().any(|ev| {
            ev["type"] == "dispatch.failed" && ev["bead"] == bead_b.as_str()
        })
    });

    // A was NOT silently re-attempted across the restart.
    let events = events_json(&root);
    let a_failures = events
        .iter()
        .filter(|e| e["type"] == "dispatch.failed" && e["bead"] == bead_a.as_str())
        .count();
    assert_eq!(
        a_failures, 1,
        "the restart must not silently re-attempt A: {events:#?}"
    );
}
```

Note on `camp sling` here: `sling` validates routing client-side and pokes a running campd; the scaffold's `[dispatch] default_agent = "dev"` makes routing succeed, so the failure occurs inside campd at `prepare()` (missing rig dir), exactly the intended path. (If `sling`'s client-side rig check ever rejected a missing rig directory, switch these two tests to `camp create` — which does not validate the rig — and drive the wake with the poke `camp create` already emits. Verify with `grep -n "is_dir\|path.exists" crates/camp/src/cmd/sling.rs crates/camp/src/cmd/create.rs`; as of this writing neither checks the rig directory's existence, so `sling` is correct.)

- [ ] **Step 4: Run the restart test and watch it pass**

Run: `cargo test -p camp --test daemon_dispatch a_dispatch_failure_survives_a_campd_restart_without_a_silent_retry -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the whole dispatch integration file**

Run: `cargo test -p camp --test daemon_dispatch`
Expected: PASS (all pre-existing tests plus the two new ones).

- [ ] **Step 6: Commit**

```bash
git add crates/camp/tests/daemon_dispatch.rs
git commit -m "test(dispatch): failed dispatch recovers via camp retry and survives restart (#83)"
```

---

### Task 7: Full-suite verification and push

**Files:** none (verification only).

- [ ] **Step 1: Format gate**

Run: `cargo fmt --all --check`
Expected: no output (clean).

- [ ] **Step 2: Clippy gate (the invariant-5 denies live here)**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Full workspace test suite**

Run: `cargo test --workspace`
Expected: all green. Pay attention to `camp-core` (`vocab_pin`, `refold_prop`, `export_golden`, `ledger`, `readiness`) and `camp` (`daemon_dispatch`, `cmd::top`, `daemon::socket`, `cmd::retry`, `cli_show`). If `export_golden` fails because the fixture camp's output changed (it should NOT — this plan adds no event to that fixture), read the diff before regenerating; regenerate only if the change is intentional and expected (`UPDATE_EXPORT_GOLDEN=1 cargo test -p camp-core --test export_golden`).

- [ ] **Step 4: Push and watch CI settle**

```bash
git push -u origin fix-83-failed-dispatch-recovery
```
Then open the PR and foreground-watch to the settled result: `gh pr checks --watch`. Work is not complete until CI is green. Never report "CI is running".

---

## Acceptance criteria (from the kickoff) → evidence

- **"the issue's observed session works end to end with the fake agent — dispatch fails, the operator sees why and recovers WITHOUT closing the bead and re-slinging under a new id — fully evented with causes"** → Task 6 `a_failed_dispatch_is_recoverable_with_camp_retry_keeping_the_bead_id`: asserts `dispatch.failed` fires, `camp show` names the reason and `camp retry <bead>`, `camp retry` produces exactly one `dispatch.rearmed` (with `previous_reason`), a `session.woke` re-dispatches the same bead id, and `bead.closed` count is 0.
- **"camp top no longer counts runtime-unreachable beads as plain ready (or names them as blocked)"** → Task 4 `status_summary_moves_a_dispatch_failed_bead_from_ready_to_stuck` + Task 6 asserting `ready: 0` / `stuck: 1` in real `camp top` output. Both: `ready` drops the bead AND it is named (`stuck`).
- **Invariant 3 (ledger-visible failed state; no secret restart behavior)** → Task 3 (dispatchability derived from `beads.dispatch_failure`, in-memory set deleted) + Task 6 `a_dispatch_failure_survives_a_campd_restart_without_a_silent_retry` (A is not re-attempted across a restart).
- **Invariant 1 (no automatic retry loop)** → recovery is the explicit, operator-run `camp retry`; no timer added. `camp retry` appends one event and pokes; campd converges once.
- **Additive to fix-82** → any new `dispatch.failed` call site flows through the same fold marker; no interface here depends on where/when it fires.

---

## Self-review

- **Spec coverage:** Every issue-#83 ask maps to a task — ledger-visible failed state (Task 3), `camp top` truth (Task 4), a first-class retry verb keeping id+history (Task 5), no automatic retry (Tasks 3+5 design), restart no longer secret (Tasks 3+6). The "or terminal state" alternative in the issue is intentionally not taken; the plan restores "`ready` means something will run it" via re-arm, justified above.
- **Placeholder scan:** No TBD/TODO/"handle edge cases"/"similar to Task N"/"add tests for the above" — every code step carries the actual code, and every command carries its expected result.
- **Type consistency:** `EventType::DispatchRearmed` / `"dispatch.rearmed"` (Task 1) is used verbatim in Tasks 2, 3, 5, 6. Payload field `previous_reason` is defined in Task 2's `DispatchRearmed` struct and produced by Task 5's `camp retry` and asserted in Task 6. `StatusSummary.stuck` (Task 4) is populated by `stuck_task_count` and consumed by `camp top` render and the socket wire test. `crate::cmd::retry::run(camp, bead)` signature matches the `main.rs` dispatch arm.
- **Verify-before-use hooks:** Steps that depend on helper names not fully shown here (`spawn_probe_guard`, `events_of_type`, `db_path`, the fold test module location, `sling`/`create` rig checks) include an explicit `grep` to confirm before writing, with a stated fallback — so the implementer never guesses.

---

## Execution handoff

Per the fix-83 kickoff amendment, this planning session ends at the committed-and-pushed plan doc. Execution runs in a **separate, fresh session** dispatched with **superpowers:executing-plans** against this doc, on the same branch, after the plan gate's APPROVE. That implementer session records the approval note (date, verdict, non-blocking notes, accepted deviations) at the top of this plan doc in its first execution commit.

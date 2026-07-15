//! compat §6.1 — the claim invariant. `bead.claimed` stamps the session
//! (`claimed_by`) AND the dispatch branch (`work_branch`, projected as
//! `gc.work_branch`) in the ONE update. It NEVER touches `assignee` — cook
//! owns that column (the qualified route, projected as `gc.routed_to`), and a
//! claim that re-derived the route from `GC_AGENT` env would make the §6.1
//! byte-projection equal by construction and re-admit the rev-3 bug (round-1
//! B1). Every projection reads the route back from the bead row.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use camp_core::clock::FixedClock;
use camp_core::config::RigConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::formula::{Formula, Step, cook};
use camp_core::ledger::Ledger;

fn temp_ledger() -> (tempfile::TempDir, Ledger) {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open_with_clock(
        &dir.path().join("camp.db"),
        Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
    )
    .unwrap();
    (dir, ledger)
}

fn rig() -> RigConfig {
    RigConfig {
        name: "gascity".into(),
        path: "/code/gascity".into(),
        prefix: "gc".into(),
        default_agent: None,
    }
}

/// Cook a single-step formula whose step routes to `route`; returns the step
/// bead id (`gc-2`; the root is `gc-1`). The step bead's `assignee` column IS
/// the route (cook.rs:407) — projected as `gc.routed_to`.
fn cook_one_step_routed_to(dir: &std::path::Path, ledger: &mut Ledger, route: &str) -> String {
    let formula = Formula {
        name: "one".into(),
        vars: Default::default(),
        description: None,
        requires: None,
        steps: vec![Step {
            id: "only".into(),
            title: "work".into(),
            description: None,
            needs: vec![],
            assignee: Some(route.into()),
            metadata: Default::default(),
            timeout: None,
            check: None,
            retry: None,
            on_complete: None,
            drain: None,
        }],
        source: String::new(),
    };
    let cooked = cook(ledger, &formula, &dir.join("runs"), &rig(), "cli").unwrap();
    cooked.step_beads["only"].clone()
}

#[test]
fn claim_stamps_session_and_work_branch_and_leaves_the_cooked_route_intact() {
    let (dir, mut ledger) = temp_ledger();
    let bead = cook_one_step_routed_to(dir.path(), &mut ledger, "gc.publisher");
    assert_eq!(bead, "gc-2");

    ledger
        .append(EventInput {
            kind: EventType::BeadClaimed,
            rig: None,
            actor: "gc-shim".into(),
            bead: Some("gc-2".into()),
            data: serde_json::json!({ "session": "t/gc.publisher/1", "work_branch": "camp/gc-2" }),
        })
        .unwrap();

    let row = ledger.get_bead("gc-2").unwrap().unwrap();
    assert_eq!(row.status, "in_progress");
    assert_eq!(row.claimed_by.as_deref(), Some("t/gc.publisher/1")); // gc's assignee
    assert_eq!(row.assignee.as_deref(), Some("gc.publisher")); // UNCHANGED — cook owns it

    // The projection reads back the cooked route AND the claim-stamped branch.
    let meta = ledger.bead_metadata("gc-2").unwrap();
    assert_eq!(
        meta.get("gc.routed_to").map(String::as_str),
        Some("gc.publisher")
    );
    assert_eq!(
        meta.get("gc.work_branch").map(String::as_str),
        Some("camp/gc-2")
    );

    assert!(ledger.refold_check().unwrap().drift.is_empty());
}

#[test]
fn claim_without_work_branch_succeeds_and_stamps_no_branch() {
    // camp's own `camp claim {session}` path: no work_branch field. The claim
    // succeeds, flips the bead, and leaves the column ABSENT from the
    // projection (it does not invent a branch).
    //
    // NOTE: this does NOT catch the `work_branch = ?2` (COALESCE-dropped)
    // mutation — on a FRESH bead the column is already NULL, so writing NULL is
    // indistinguishable from leaving it absent. That mutant is EQUIVALENT on
    // every state reachable through `Ledger::append` (an open bead never
    // carries a work_branch), so it is killed only by the directly-seeded
    // in-crate test `fold::tests::claim_with_no_branch_preserves_a_pre_existing_\
    // work_branch_coalesce`. This test guards the no-error / no-stamp behavior.
    let (dir, mut ledger) = temp_ledger();
    cook_one_step_routed_to(dir.path(), &mut ledger, "gc.publisher");

    ledger
        .append(EventInput {
            kind: EventType::BeadClaimed,
            rig: None,
            actor: "cli".into(),
            bead: Some("gc-2".into()),
            data: serde_json::json!({ "session": "t/gc.publisher/1" }),
        })
        .unwrap();

    let meta = ledger.bead_metadata("gc-2").unwrap();
    assert_eq!(
        meta.get("gc.routed_to").map(String::as_str),
        Some("gc.publisher")
    );
    assert!(
        !meta.contains_key("gc.work_branch"),
        "a claim with no work_branch must not stamp the column"
    );
}

#[test]
fn bead_claimed_rejects_unknown_fields() {
    // The guard that the route CANNOT be smuggled in via the claim: `route` is
    // not a field of BeadClaimed (deny_unknown_fields), so an append carrying
    // it Errs. This stays RED for any future edit that adds a `route` field.
    let (dir, mut ledger) = temp_ledger();
    cook_one_step_routed_to(dir.path(), &mut ledger, "gc.publisher");

    let err = ledger
        .append(EventInput {
            kind: EventType::BeadClaimed,
            rig: None,
            actor: "gc-shim".into(),
            bead: Some("gc-2".into()),
            data: serde_json::json!({ "session": "t/gc.publisher/1", "route": "gc.WRONG" }),
        })
        .unwrap_err();
    assert!(
        matches!(err, camp_core::error::CoreError::InvalidEventData { .. }),
        "unexpected error: {err:?}"
    );
    // Rejections appended nothing: the bead is still open, unclaimed.
    let row = ledger.get_bead("gc-2").unwrap().unwrap();
    assert_eq!(row.status, "open");
    assert!(row.claimed_by.is_none());
}

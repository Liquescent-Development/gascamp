#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 9 vocabulary and fold contract (plan Task 1): the extended
//! `bead.closed` payload, the `skipped` outcome, and the three new
//! camp-specific log events, all validated by the fold so a malformed
//! event fails fast (invariant 5).

use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

fn ledger() -> (tempfile::TempDir, Ledger) {
    let dir = tempfile::tempdir().unwrap();
    let l = Ledger::open_with_clock(
        &dir.path().join("camp.db"),
        Box::new(FixedClock::new("2026-07-07T12:00:00Z")),
    )
    .unwrap();
    (dir, l)
}

fn create(l: &mut Ledger, id: &str) {
    l.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some("gc".into()),
        actor: "test".into(),
        bead: Some(id.into()),
        data: serde_json::json!({"title": id}),
    })
    .unwrap();
}

fn close_with(
    l: &mut Ledger,
    id: &str,
    data: serde_json::Value,
) -> Result<i64, camp_core::error::CoreError> {
    l.append(EventInput {
        kind: EventType::BeadClosed,
        rig: Some("gc".into()),
        actor: "test".into(),
        bead: Some(id.into()),
        data,
    })
}

#[test]
fn close_payload_accepts_phase9_fields_and_validates_them() {
    let (_d, mut l) = ledger();
    create(&mut l, "gc-1");
    // transient requires outcome fail
    let err = close_with(
        &mut l,
        "gc-1",
        serde_json::json!({"outcome":"pass","failure_class":"transient"}),
    );
    assert!(err.is_err(), "transient on a pass close must be rejected");
    // unknown failure_class rejected
    let err = close_with(
        &mut l,
        "gc-1",
        serde_json::json!({"outcome":"fail","failure_class":"flaky"}),
    );
    assert!(err.is_err(), "unknown failure_class must be rejected");
    // a close never carries a "pass" disposition (plan Decision 3 / review Blocker A)
    let err = close_with(
        &mut l,
        "gc-1",
        serde_json::json!({"outcome":"pass","final_disposition":"pass"}),
    );
    assert!(
        err.is_err(),
        "the run-level pass disposition lives only in run.finalized"
    );
    // disposition requires outcome fail
    let err = close_with(
        &mut l,
        "gc-1",
        serde_json::json!({"outcome":"pass","final_disposition":"soft_fail"}),
    );
    assert!(
        err.is_err(),
        "a disposition on a pass close must be rejected"
    );
    // legal: fail + transient + output + disposition
    close_with(
        &mut l,
        "gc-1",
        serde_json::json!({"outcome":"fail","failure_class":"transient",
            "final_disposition":"soft_fail","output":{"items":[1,2]}}),
    )
    .unwrap();
    let bead = l.get_bead("gc-1").unwrap().unwrap();
    assert_eq!(bead.status, "closed");
    assert_eq!(bead.outcome.as_deref(), Some("fail"));
}

#[test]
fn skipped_is_a_legal_outcome() {
    let (_d, mut l) = ledger();
    create(&mut l, "gc-1");
    close_with(
        &mut l,
        "gc-1",
        serde_json::json!({"outcome":"skipped","reason":"needs cannot be satisfied"}),
    )
    .unwrap();
    assert_eq!(
        l.get_bead("gc-1").unwrap().unwrap().outcome.as_deref(),
        Some("skipped")
    );
    // a skipped dependency still blocks dependents (decision 6: only pass unblocks)
    l.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some("gc".into()),
        actor: "test".into(),
        bead: Some("gc-2".into()),
        data: serde_json::json!({"title": "dependent", "needs": ["gc-1"]}),
    })
    .unwrap();
    assert!(!l.is_ready("gc-2").unwrap());
    // refold property holds with the new outcome in play
    assert!(l.refold_check().unwrap().drift.is_empty());
}

#[test]
fn phase9_log_events_validate_their_payloads() {
    let (_d, mut l) = ledger();
    create(&mut l, "gc-1");
    // check.passed happy path (bead = the attempt bead)
    l.append(EventInput {
        kind: EventType::CheckPassed,
        rig: Some("gc".into()),
        actor: "campd".into(),
        bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r1","step_id":"s1","attempt":2}),
    })
    .unwrap();
    // check.passed requires a known bead
    assert!(
        l.append(EventInput {
            kind: EventType::CheckPassed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-404".into()),
            data: serde_json::json!({"run_id":"r1","step_id":"s1","attempt":1}),
        })
        .is_err(),
        "check.passed on an unknown bead must be rejected"
    );
    // check.failed requires exit evidence (exit_code/signal/timed_out/error)
    assert!(
        l.append(EventInput {
            kind: EventType::CheckFailed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"run_id":"r1","step_id":"s1","attempt":1}),
        })
        .is_err(),
        "check.failed without exit evidence must be rejected"
    );
    l.append(EventInput {
        kind: EventType::CheckFailed,
        rig: Some("gc".into()),
        actor: "campd".into(),
        bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r1","step_id":"s1","attempt":1,
            "exit_code":1,"log":"runs/r1/checks/s1-attempt-1.log"}),
    })
    .unwrap();
    // run.finalized happy path (bead = root, disposition from CAMP_RUN_DISPOSITIONS)
    l.append(EventInput {
        kind: EventType::RunFinalized,
        rig: Some("gc".into()),
        actor: "campd".into(),
        bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r1","root":"gc-1","outcome":"fail",
            "final_disposition":"hard_fail","cause_seq":3,
            "soft_failed":[],"skipped":["s2"]}),
    })
    .unwrap();
    // run.finalized accepts the run-level "pass" disposition (close events never do)
    l.append(EventInput {
        kind: EventType::RunFinalized,
        rig: Some("gc".into()),
        actor: "campd".into(),
        bead: Some("gc-1".into()),
        data: serde_json::json!({"run_id":"r2","root":"gc-1","outcome":"pass",
            "final_disposition":"pass","cause_seq":4,
            "soft_failed":[],"skipped":[]}),
    })
    .unwrap();
    // run.finalized rejects an unknown disposition
    assert!(
        l.append(EventInput {
            kind: EventType::RunFinalized,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"run_id":"r1","root":"gc-1","outcome":"fail",
                "final_disposition":"exploded","cause_seq":3,
                "soft_failed":[],"skipped":[]}),
        })
        .is_err()
    );
    // log-only: no state effect on the bead; refold stays clean
    assert_eq!(l.get_bead("gc-1").unwrap().unwrap().status, "open");
    assert!(l.refold_check().unwrap().drift.is_empty());
}

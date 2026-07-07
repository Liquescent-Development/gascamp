//! Cook-side ledger behavior: the run.cooked event, run-aware bead.created,
//! and the cook() transaction itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
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

#[test]
fn run_cooked_round_trips_and_is_log_only() {
    let (_dir, mut ledger) = temp_ledger();
    assert_eq!(
        EventType::parse("run.cooked").unwrap(),
        EventType::RunCooked
    );
    ledger
        .append(EventInput {
            kind: EventType::RunCooked,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({
                "run_id": "20260705T211403Z-a1b2c3",
                "formula": "minimal",
                "root": "gc-1",
                "steps": {"only": "gc-2"}
            }),
        })
        .unwrap();
    // log-only: no bead rows appear
    let beads = ledger.list_beads(&Default::default()).unwrap();
    assert!(beads.is_empty());
    let events = ledger.events_range(1, None).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn run_cooked_payload_is_validated_and_rejects_unknown_fields() {
    let (_dir, mut ledger) = temp_ledger();
    for bad in [
        serde_json::json!({"formula": "m", "root": "gc-1", "steps": {}}), // missing run_id
        serde_json::json!({"run_id": "", "formula": "m", "root": "gc-1", "steps": {}}), // empty
        serde_json::json!({"run_id": "r", "formula": "m", "root": "gc-1", "steps": {}, "extra": 1}),
    ] {
        assert!(
            ledger
                .append(EventInput {
                    kind: EventType::RunCooked,
                    rig: Some("gc".into()),
                    actor: "cli".into(),
                    bead: None,
                    data: bad.clone(),
                })
                .is_err(),
            "must reject {bad}"
        );
    }
    assert!(ledger.events_range(1, None).unwrap().is_empty());
}

#[test]
fn bead_created_accepts_run_and_step_ids_and_refolds_exactly() {
    let (_dir, mut ledger) = temp_ledger();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({
                "title": "implement",
                "run_id": "20260705T211403Z-a1b2c3",
                "step_id": "implement"
            }),
        })
        .unwrap();
    let report = ledger.refold_check().unwrap();
    assert!(report.drift.is_empty(), "{:?}", report.drift);
}

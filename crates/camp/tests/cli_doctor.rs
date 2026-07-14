#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(non_snake_case)]

use assert_cmd::Command;
use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn seeded_camp(dir: &std::path::Path) -> std::path::PathBuf {
    camp()
        .current_dir(dir)
        .args(["init", "--no-service"])
        .assert()
        .success();
    let camp_root = dir.join(".camp");
    let mut ledger = Ledger::open_with_clock(
        &camp_root.join("camp.db"),
        Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
    )
    .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"title": "one"}),
        })
        .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"outcome": "pass"}),
        })
        .unwrap();
    camp_root
}

fn tamper(camp_root: &std::path::Path) {
    let conn = rusqlite::Connection::open(camp_root.join("camp.db")).unwrap();
    conn.execute(
        "UPDATE beads SET status = 'open', outcome = NULL WHERE id = 'gc-1'",
        [],
    )
    .unwrap();
}

#[test]
fn doctor_refold_reports_clean_on_a_healthy_ledger() {
    let dir = tempfile::tempdir().unwrap();
    seeded_camp(dir.path());
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("replayed 2 events; 0 drift rows"));
}

#[test]
fn doctor_refold_detects_drift_and_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    tamper(&camp_root);
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("gc-1"))
        .stderr(predicates::str::contains("drift"));
}

#[test]
fn doctor_refold_repair_rebuilds_and_subsequent_check_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    tamper(&camp_root);
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold", "--repair"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success();
}

#[test]
fn doctor_refold_repair_restores_BEAD_METADATA_including_a_drain_reservation() {
    // The `bead_meta` refold entry was defended by exactly ONE proptest: deleting its
    // `TableSpec` failed only `refold_prop`, and no CLI-level `doctor --refold` test
    // carried metadata at all. `--refold` is the operator's integrity surface; if it
    // cannot see a reservation, `--repair` silently drops every one of them (and, with
    // `foreign_keys = ON`, hard-fails on the FK if the spec is ordered wrong).
    //
    // This drives the REAL binary end to end: metadata (incl. gc's reservation key)
    // is written, corrupted in the state table, DETECTED as drift, and RESTORED.
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());

    {
        let mut ledger = Ledger::open_with_clock(
            &camp_root.join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({"title": "member"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadUpdated,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({
                    "metadata": {
                        camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION: "gc-3",
                        "gc.run_target": "superpowers.implementer",
                    }
                }),
            })
            .unwrap();
    }

    // A healthy ledger refolds CLEAN — the metadata round-trips through the shadow.
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success();

    // CORRUPT the state table behind the fold's back.
    {
        let conn = rusqlite::Connection::open(camp_root.join("camp.db")).unwrap();
        conn.execute(
            "UPDATE bead_meta SET value = 'gc-WRONG' WHERE bead_id = 'gc-9' AND key = ?1",
            [camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION],
        )
        .unwrap();
    }

    // …the refold must SEE it (a state table it does not diff is a state table it
    // cannot repair).
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("bead_meta"));

    // …and --repair must RESTORE it.
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold", "--repair"])
        .assert()
        .success();

    let ledger = Ledger::open_read_only(&camp_root.join("camp.db")).unwrap();
    let md = ledger.bead_metadata("gc-9").unwrap();
    assert_eq!(
        md.get(camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION)
            .map(String::as_str),
        Some("gc-3"),
        "the reservation is restored from the event log"
    );
    assert_eq!(
        md.get("gc.run_target").map(String::as_str),
        Some("superpowers.implementer")
    );
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    camp().current_dir(dir).arg("init").assert().success();
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

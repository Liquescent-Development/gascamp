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

/// Init a camp, then seed beads directly through camp-core with a fixed clock
/// so `--json` output is byte-deterministic (the binary uses SystemClock).
fn seeded(dir: &std::path::Path) {
    camp().current_dir(dir).arg("init").assert().success();
    let mut ledger = Ledger::open_with_clock(
        &dir.join(".camp/camp.db"),
        Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
    )
    .unwrap();
    // gc-1 open (ready), gc-2 needs gc-1 (blocked)
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gascity".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"title": "one"}),
        })
        .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gascity".into()),
            actor: "cli".into(),
            bead: Some("gc-2".into()),
            data: serde_json::json!({"title": "two", "needs": ["gc-1"]}),
        })
        .unwrap();
}

const READY_JSON: &str = r#"[{"id":"gc-1","rig":"gascity","type":"task","title":"one","status":"open","assignee":null,"claimed_by":null,"outcome":null,"labels":[],"created_ts":"2026-07-05T21:14:03Z","updated_ts":"2026-07-05T21:14:03Z"}]"#;

#[test]
fn ls_ready_json_is_exactly_the_unblocked_bead() {
    let dir = tempfile::tempdir().unwrap();
    seeded(dir.path());
    camp()
        .current_dir(dir.path())
        .args(["ls", "--ready", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::diff(format!("{READY_JSON}\n")));
}

#[test]
fn ls_all_lists_both_beads() {
    let dir = tempfile::tempdir().unwrap();
    seeded(dir.path());
    let out = camp()
        .current_dir(dir.path())
        .args(["ls", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(r#""id":"gc-1""#));
    assert!(out.contains(r#""id":"gc-2""#));
}

#[test]
fn ls_rig_filter_scopes_results() {
    let dir = tempfile::tempdir().unwrap();
    seeded(dir.path());
    camp()
        .current_dir(dir.path())
        .args(["ls", "--rig", "nonesuch", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]\n"));
}

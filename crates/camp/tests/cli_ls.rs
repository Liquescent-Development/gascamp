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

const READY_JSON: &str = r#"[{"id":"gc-1","rig":"gascity","type":"task","title":"one","status":"open","assignee":null,"claimed_by":null,"outcome":null,"work_outcome":null,"dispatch_failure":null,"labels":[],"created_ts":"2026-07-05T21:14:03Z","updated_ts":"2026-07-05T21:14:03Z"}]"#;

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

/// #48 finding 2 + the obligation surface: blocked/un-shipped work is
/// visible at the LIST level — closed beads show their work outcome; an
/// open bead whose dispatch failed fast shows the marker instead of a
/// clean `open`. The dispatch.failed fixture is appended straight through
/// camp-core's Ledger (the camp crate depends on camp-core; campd is the
/// only real writer of this event and needs a baseless rig to produce it).
#[test]
fn ls_surfaces_work_outcomes_and_dispatch_failures() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    {
        let mut ledger = Ledger::open_with_clock(
            &dir.path().join(".camp/camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        for (id, title) in [("gc-1", "one"), ("gc-2", "two"), ("gc-3", "three")] {
            ledger
                .append(EventInput {
                    kind: EventType::BeadCreated,
                    rig: Some("gascity".into()),
                    actor: "cli".into(),
                    bead: Some(id.into()),
                    data: serde_json::json!({"title": title}),
                })
                .unwrap();
        }
    }
    // gc-1: blocked
    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "fail",
            "--work-outcome",
            "blocked",
            "--reason",
            "cannot land",
        ])
        .assert()
        .success();
    // gc-2: the v1 shape — unchanged rendering
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-2", "--outcome", "pass"])
        .assert()
        .success();
    // gc-3: a fail-fast dispatch record
    {
        let mut ledger = Ledger::open(&dir.path().join(".camp/camp.db")).unwrap();
        ledger
            .append(EventInput {
                kind: EventType::DispatchFailed,
                rig: Some("gascity".into()),
                actor: "campd".into(),
                bead: Some("gc-3".into()),
                data: serde_json::json!({"reason": "rig cannot host a worktree (no base commit)"}),
            })
            .unwrap();
    }
    let out = camp()
        .current_dir(dir.path())
        .args(["ls"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("gc-1\tclosed:blocked\t"), "{text}");
    assert!(text.contains("gc-2\tclosed\t"), "{text}");
    assert!(text.contains("gc-3\topen:dispatch-failed\t"), "{text}");

    let out = camp()
        .current_dir(dir.path())
        .args(["ls", "--json"])
        .output()
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    let row = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(row("gc-1")["work_outcome"], "blocked");
    assert!(
        row("gc-3")["dispatch_failure"]
            .as_str()
            .unwrap()
            .contains("worktree")
    );
}

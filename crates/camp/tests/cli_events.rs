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

/// Init a camp in `dir` and append two events through camp-core with a fixed
/// clock, so CLI output is byte-deterministic.
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
            actor: "session:8f3c2e01".into(),
            bead: Some("gc-142".into()),
            data: serde_json::json!({"title": "add a --json flag"}),
        })
        .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "session:8f3c2e01".into(),
            bead: Some("gc-142".into()),
            data: serde_json::json!({"outcome": "pass"}),
        })
        .unwrap();
    camp_root
}

#[test]
fn events_json_emits_canonical_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    seeded_camp(dir.path());

    let expected_line_2 = r#"{"seq":2,"ts":"2026-07-05T21:14:03Z","type":"bead.closed","rig":"gc","actor":"session:8f3c2e01","bead":"gc-142","data":{"outcome":"pass"}}"#;
    let output = camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with(r#"{"seq":1,"#), "line 1: {}", lines[0]);
    assert_eq!(lines[1], expected_line_2);
}

#[test]
fn events_range_flags_bound_the_output() {
    let dir = tempfile::tempdir().unwrap();
    seeded_camp(dir.path());

    let output = camp()
        .current_dir(dir.path())
        .args(["events", "--json", "--from", "2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains(r#""seq":2"#));

    let output = camp()
        .current_dir(dir.path())
        .args(["events", "--json", "--to", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains(r#""seq":1"#));
}

#[test]
fn events_on_empty_log_prints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn camp_dir_resolution_walks_up_from_nested_cwd() {
    let dir = tempfile::tempdir().unwrap();
    seeded_camp(dir.path());
    let nested = dir.path().join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    camp()
        .current_dir(&nested)
        .args(["events", "--json"])
        .assert()
        .success();
}

#[test]
fn camp_dir_resolution_honors_env_var() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    let elsewhere = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env("CAMP_DIR", &camp_root)
        .current_dir(elsewhere.path())
        .args(["events", "--json"])
        .assert()
        .success();
}

#[test]
fn no_camp_found_is_a_clean_error() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("no camp found; run camp init"));
}

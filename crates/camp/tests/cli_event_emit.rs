#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! camp event emit (master plan Phase 8): the worker contract's milestone
//! verb.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn camp(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn init_camp_with_rig(dir: &Path) -> PathBuf {
    let status = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir)
        .arg("init")
        .status()
        .unwrap();
    assert!(status.success());
    let root = dir.join(".camp");
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let out = camp(
        &root,
        &[
            "rig",
            "add",
            rig.to_str().unwrap(),
            "--prefix",
            "gc",
            "--name",
            "gc",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    root
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    let out = camp(root, &["events", "--json"]);
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn emit_appends_a_milestone_with_session_actor_and_bead_rig() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());
    let out = camp(&root, &["create", "work it"]);
    assert!(out.status.success());
    let bead = String::from_utf8(out.stdout).unwrap().trim().to_owned();

    let out = camp(
        &root,
        &[
            "event",
            "emit",
            "tests passing",
            "--bead",
            &bead,
            "--session",
            "t/dev/1",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let events = events_json(&root);
    let milestone = events
        .iter()
        .find(|e| e["type"] == "worker.milestone")
        .expect("worker.milestone event");
    assert_eq!(milestone["actor"], "t/dev/1");
    assert_eq!(milestone["bead"], bead.as_str());
    assert_eq!(milestone["rig"], "gc");
    assert_eq!(milestone["data"]["text"], "tests passing");
}

#[test]
fn emit_without_bead_or_session_defaults_to_cli_actor() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());
    let out = camp(&root, &["event", "emit", "general note"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = events_json(&root);
    let milestone = events
        .iter()
        .find(|e| e["type"] == "worker.milestone")
        .unwrap();
    assert_eq!(milestone["actor"], "cli");
    assert!(milestone.get("bead").is_none());
    assert!(milestone.get("rig").is_none());
}

#[test]
fn emit_for_an_unknown_bead_fails_and_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());
    let before = events_json(&root).len();
    let out = camp(&root, &["event", "emit", "x", "--bead", "gc-999"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("gc-999"));
    assert_eq!(events_json(&root).len(), before);
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! camp session register/end (Phase 12, Decision D1): the hook-facing
//! session-lifecycle verbs. They append the existing session.woke /
//! session.stopped event types — no new vocabulary.

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
        &["rig", "add", rig.to_str().unwrap(), "--prefix", "gc", "--name", "gc"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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
fn register_appends_a_hook_registered_session_woke_then_end_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());

    let out = camp(
        &root,
        &[
            "session",
            "register",
            "--name",
            "attended/S-1",
            "--agent",
            "attended",
            "--session-id",
            "S-1",
            "--transcript",
            "/tmp/S-1.jsonl",
        ],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let events = events_json(&root);
    let woke = events
        .iter()
        .find(|e| e["type"] == "session.woke" && e["data"]["name"] == "attended/S-1")
        .expect("a hook-registered session.woke");
    assert_eq!(woke["actor"], "hook:session-start");
    assert_eq!(woke["data"]["agent"], "attended");
    assert_eq!(woke["data"]["claude_session_id"], "S-1");
    assert_eq!(woke["data"]["transcript_path"], "/tmp/S-1.jsonl");

    let out = camp(
        &root,
        &["session", "end", "--name", "attended/S-1", "--reason", "user quit"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let events = events_json(&root);
    let stopped = events
        .iter()
        .find(|e| e["type"] == "session.stopped" && e["data"]["name"] == "attended/S-1")
        .expect("a session.stopped");
    assert_eq!(stopped["data"]["reason"], "user quit");
}

#[test]
fn ending_an_unknown_session_fails_and_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());
    let before = events_json(&root).len();
    let out = camp(&root, &["session", "end", "--name", "attended/nobody"]);
    assert!(!out.status.success());
    assert_eq!(events_json(&root).len(), before);
}

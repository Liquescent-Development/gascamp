#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 10 CLI integration: `camp order ls` / `camp order run` against the
//! real binary (master plan Phase 10; spec §9).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn camp_cmd(root: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_remove("CAMP_DIR").arg("--camp").arg(root);
    cmd
}

fn run_ok(root: &Path, args: &[&str]) -> String {
    let out = camp_cmd(root).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "camp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// camp init + one rig; returns the camp root (<tempdir>/.camp).
fn init_camp(dir: &Path) -> PathBuf {
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
    let out = camp_cmd(&root)
        .args(["rig", "add"])
        .arg(&rig)
        .args(["--prefix", "gc", "--name", "gc"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    root
}

const ORDERS_TOML: &str = r#"
[[order]]
name    = "morning-triage"
on      = "cron:0 7 * * 1-5"
formula = "triage-inbox"
rig     = "gc"

[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "fix-ci"
"#;

fn add_orders(root: &Path) {
    let path = root.join("camp.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(ORDERS_TOML);
    std::fs::write(&path, text).unwrap();
}

#[test]
fn order_ls_shows_triggers_and_next_fires() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root);
    let out = run_ok(&root, &["order", "ls"]);
    assert!(
        out.contains("morning-triage") && out.contains("cron:0 7 * * 1-5"),
        "{out}"
    );
    assert!(
        out.contains("ci-red") && out.contains("event:bead.closed[label=ci-red]"),
        "{out}"
    );
    let json = run_ok(&root, &["order", "ls", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert!(
        rows[0]["next_fire"].is_string(),
        "cron orders show a next fire: {rows}"
    );
    assert!(
        rows[1]["next_fire"].is_null(),
        "event orders have none: {rows}"
    );
    assert_eq!(rows[0]["catch_up_window_secs"], 7200);
}

#[test]
fn order_run_appends_a_manual_fire() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root);
    let out = run_ok(&root, &["order", "run", "morning-triage"]);
    assert!(out.contains("fired order morning-triage"), "{out}");
    let events = run_ok(&root, &["events", "--json"]);
    let fired: Vec<serde_json::Value> = events
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|e: &serde_json::Value| e["type"] == "order.fired")
        .collect();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0]["data"]["trigger"], "manual");
    assert_eq!(fired[0]["actor"], "cli");
}

#[test]
fn order_run_unknown_name_lists_the_options() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root);
    let out = camp_cmd(&root)
        .args(["order", "run", "nope"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("nope") && err.contains("morning-triage"),
        "{err}"
    );
}

#[test]
fn order_ls_with_a_broken_order_names_order_and_field() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let path = root.join("camp.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[[order]]\nname=\"bad\"\non=\"cron:61 * * * *\"\nformula=\"f\"\n");
    std::fs::write(&path, text).unwrap();
    let out = camp_cmd(&root).args(["order", "ls"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("bad") && err.contains("on") && err.contains("minute"),
        "{err}"
    );
}

#[test]
fn order_ls_with_no_orders_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let out = run_ok(&root, &["order", "ls"]);
    assert!(out.contains("no orders configured"), "{out}");
}

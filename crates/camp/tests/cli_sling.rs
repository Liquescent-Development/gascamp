#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! camp sling (spec §8.1 Tier 0; master plan Phase 8). The daemon-side
//! dispatch behavior lives in daemon_dispatch.rs; this file covers the
//! CLI surface: routing resolution, fail-fast messages, assignee stamping,
//! and the auto-start poke.

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

/// A camp with one rig and a config we control completely. `command` is
/// `true` so an auto-started daemon's dispatch spawn is harmless.
fn scaffold(dir: &Path, dispatch_default: Option<&str>, rig_default: Option<&str>) -> PathBuf {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let rig_line = rig_default
        .map(|a| format!("default_agent = \"{a}\"\n"))
        .unwrap_or_default();
    let dispatch_line = dispatch_default
        .map(|a| format!("default_agent = \"{a}\"\n"))
        .unwrap_or_default();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n{rig_line}\n[dispatch]\ncommand = \"true\"\n{dispatch_line}",
            rig.display()
        ),
    )
    .unwrap();
    camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
    root
}

fn write_agent(root: &Path, name: &str) {
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join(format!("{name}.md")),
        format!("---\nname: {name}\n---\nDo the work.\n"),
    )
    .unwrap();
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

fn stop_campd(root: &Path) {
    // sling auto-starts campd; leave nothing running behind the test
    let _ = camp(root, &["stop"]);
}

#[test]
fn sling_with_no_route_fails_naming_all_three_fixes_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), None, None);
    let out = camp(&root, &["sling", "add a flag"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in ["--agent", "default_agent", "[dispatch]", "[[rigs]]"] {
        assert!(
            stderr.contains(needle),
            "stderr must name {needle}: {stderr}"
        );
    }
    assert!(events_json(&root).is_empty(), "no bead may be created");
    assert!(
        !root.join("campd.sock").exists(),
        "no daemon may be started"
    );
}

#[test]
fn sling_with_an_unresolvable_agent_fails_before_creating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    // no agents/ dir at all: routing picks "dev" but no layer defines it
    let out = camp(&root, &["sling", "add a flag"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dev"));
    assert!(events_json(&root).is_empty());
}

#[test]
fn sling_stamps_the_dispatch_default_agent_and_autostarts_campd() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    let out = camp(&root, &["sling", "add a flag"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bead = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    assert_eq!(bead, "gc-1");
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "dev");
    assert_eq!(created["data"]["title"], "add a flag");
    assert!(
        events.iter().any(|e| e["type"] == "campd.autostarted"),
        "sling must bring the daemon up (spec §5): {events:?}"
    );
    stop_campd(&root);
}

#[test]
fn rig_default_agent_outranks_the_camp_wide_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    let out = camp(&root, &["sling", "review it", "--rig", "gc"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "rigger");
    stop_campd(&root);
}

#[test]
fn explicit_agent_flag_outranks_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    write_agent(&root, "special");
    let out = camp(&root, &["sling", "x", "--agent", "special"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "special");
    stop_campd(&root);
}

// ---- Phase 9 Task 4: sling --formula (spec §8.2 cooking surface) ----------

#[test]
fn sling_formula_cooks_a_run_and_pins_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::write(
        root.join("formulas/one-step.toml"),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
    )
    .unwrap();
    let out = camp(&root, &["sling", "--formula", "one-step"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    // prints "<run_id> root <root-bead>"
    let mut words = stdout.split_whitespace();
    let run_id = words.next().unwrap().to_owned();
    assert_eq!(words.next(), Some("root"));
    let root_bead = words.next().unwrap().to_owned();
    assert!(root_bead.starts_with("gc-"), "{stdout}");
    // pinned run dir exists with manifest + formula copy
    assert!(
        root.join("runs")
            .join(&run_id)
            .join("manifest.json")
            .exists()
    );
    assert!(
        root.join("runs")
            .join(&run_id)
            .join("one-step.toml")
            .exists()
    );
    // run.cooked landed with actor cli
    let events = camp(&root, &["events", "--json"]);
    let cooked = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|e| e["type"] == "run.cooked")
        .expect("run.cooked event");
    assert_eq!(cooked["actor"], "cli");
    assert_eq!(cooked["data"]["run_id"], run_id.as_str());
}

#[test]
fn sling_formula_errors_name_the_formula() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    // missing file
    let out = camp(&root, &["sling", "--formula", "nope"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("nope"), "must name the formula: {err}");
    // invalid formula (city-only construct)
    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::write(
        root.join("formulas/bad.toml"),
        "formula = \"bad\"\npour = true\n\n[[steps]]\nid = \"s\"\ntitle = \"t\"\n",
    )
    .unwrap();
    let out = camp(&root, &["sling", "--formula", "bad"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("bad"), "must name the formula: {err}");
}

#[test]
fn sling_rejects_formula_combined_with_a_title() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    let out = camp(&root, &["sling", "some title", "--formula", "one-step"]);
    assert!(!out.status.success());
}

/// Test obligation (iv), dispatch-lifecycle Phase 1: no reservation state.
/// A sling writes ONE bead.created whose payload is exactly {title,
/// assignee} — no dispatch/reserved/attended key — and the bead is born
/// open and unclaimed (claim-at-creation was the DEPRECATED design).
#[test]
fn sling_creates_an_open_unclaimed_bead_with_no_reservation_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    let out = camp(&root, &["sling", "reservation guard"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bead = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    stop_campd(&root);

    let events = events_json(&root);
    let created = events
        .iter()
        .find(|e| e["type"] == "bead.created")
        .expect("sling appends bead.created");
    let keys: std::collections::BTreeSet<&str> = created["data"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["assignee", "title"].into_iter().collect(),
        "payload is exactly title+assignee"
    );
    // The event log is append-only truth: no reservation event may exist.
    for e in &events {
        let ty = e["type"].as_str().unwrap();
        assert!(
            !ty.contains("reserv") && !ty.contains("attended"),
            "no reservation vocabulary may appear in the ledger: {ty}"
        );
    }
    // Born open and unclaimed: the only claim path is a worker's own
    // `camp claim` (the scaffold's command is `true` — it never claims).
    let ledger = camp_core::ledger::Ledger::open_read_only(&root.join("camp.db")).unwrap();
    let row = ledger.get_bead(&bead).unwrap().expect("bead exists");
    assert_eq!(row.status, "open");
    assert_eq!(row.claimed_by, None);
}

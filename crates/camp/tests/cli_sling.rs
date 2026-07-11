#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! camp sling (spec §8.1 Tier 0; master plan Phase 8). The daemon-side
//! dispatch behavior lives in daemon_dispatch.rs; this file covers the
//! CLI surface: routing resolution, fail-fast messages, assignee stamping,
//! and the poke to a running campd — sling is a PURE CLIENT (design §4.3):
//! it never starts a daemon, and a campd that is down fails it loudly.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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
/// `true`, so when a test spawns a real campd its dispatch spawn is harmless.
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

const READY_PREFIX: &str = "campd listening on ";

/// A real campd child. `spawn` blocks on the readiness line (deterministic —
/// no connect polling); `Drop` SIGKILLs and reaps it (crash-only: a kill -9 is
/// a supported shutdown, spec §5). `sling` is a pure client now, so every test
/// whose sling must SUCCEED needs one of these up first.
///
/// This works in a `scaffold`-built camp even though `scaffold` never shells
/// out to `camp init` (it writes `camp.toml` and opens the ledger directly):
/// `camp daemon --camp <root>` is exactly the command the removed CLI-spawn
/// path used to run, in exactly these scaffolded camps.
struct Daemon {
    child: Child,
}

impl Daemon {
    fn spawn(root: &Path) -> Daemon {
        let mut child = Command::new(BIN)
            .env_remove("CAMP_DIR")
            .args(["daemon", "--camp"])
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon { child }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Design §4.3 + §9's test obligation. `sling` promises dispatch, and campd is
/// the only dispatcher — so a campd that is down FAILS it, loudly. It does not
/// spawn one. What it does NOT do is lose the operator's work: the bead is
/// created (the write is durable — spec §7.2: campd catches up from its cursor
/// on start), its id still reaches stdout, and the error says precisely what
/// did and did not happen.
#[test]
fn sling_with_campd_down_creates_the_bead_prints_it_and_fails_loudly_without_spawning_a_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");

    let out = camp(&root, &["sling", "no daemon here"]);

    assert!(
        !out.status.success(),
        "sling promises dispatch: a down campd must fail it"
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        "gc-1",
        "the durable bead id still reaches stdout — a down campd costs the dispatch, not the id"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in [
        "gc-1",
        "NOT dispatched",
        "campd is not running",
        "camp service status",
        "camp daemon",
    ] {
        assert!(
            stderr.contains(needle),
            "the error must name {needle:?}: {stderr}"
        );
    }
    // the write is durable and honest…
    let events = events_json(&root);
    assert!(
        events.iter().any(|e| e["type"] == "bead.created"),
        "the bead must exist: {events:?}"
    );
    // …and NO daemon came up (the same three tripwires as daemon_lifecycle's
    // assert_no_campd_came_up: the log the removed path opened BEFORE it
    // spawned, the socket a live campd binds, and the campd.started it appends).
    assert!(
        !root.join("campd.log").exists(),
        "campd.log is created only by a CLI about to spawn a daemon"
    );
    assert!(
        !root.join("campd.sock").exists(),
        "the CLI must never start campd"
    );
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "campd.started" || e["type"] == "campd.autostarted"),
        "no campd may have come up: {events:?}"
    );
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
fn sling_stamps_the_dispatch_default_agent_and_pokes_a_running_campd() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    let _campd = Daemon::spawn(&root);

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
        !events.iter().any(|e| e["type"] == "campd.autostarted"),
        "the CLI is a pure client: no campd.autostarted may ever be recorded: {events:?}"
    );
}

#[test]
fn rig_default_agent_outranks_the_camp_wide_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    let _campd = Daemon::spawn(&root);

    let out = camp(&root, &["sling", "review it", "--rig", "gc"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "rigger");
}

#[test]
fn explicit_agent_flag_outranks_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    write_agent(&root, "special");
    let _campd = Daemon::spawn(&root);

    let out = camp(&root, &["sling", "x", "--agent", "special"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "special");
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
    let _campd = Daemon::spawn(&root);

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
    let _campd = Daemon::spawn(&root);

    let out = camp(&root, &["sling", "reservation guard"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bead = String::from_utf8(out.stdout).unwrap().trim().to_owned();

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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 10 integration: orders against the real campd binary (spec §9,
//! master plan Phase 10). The star witness is away-mode: a cron order
//! fires with NO user session driving anything, campd cooks it, and the
//! ledger tells the whole story.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

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

/// camp init + one rig + a one-step formula; returns the camp root.
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
    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::write(
        root.join("formulas/one-step.toml"),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
    )
    .unwrap();
    root
}

fn add_order(root: &Path, table: &str) {
    let path = root.join("camp.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(table);
    std::fs::write(&path, text).unwrap();
}

/// Spawn campd and block on its readiness line (an OS pipe read).
fn spawn_campd(root: &Path) -> Child {
    let mut child = camp_cmd(root)
        .arg("daemon")
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
        "campd did not come up: {line:?}"
    );
    child
}

fn stop_campd(root: &Path, mut child: Child) {
    let out = camp_cmd(root).arg("stop").output().unwrap();
    assert!(
        out.status.success(),
        "camp stop failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = child.wait().unwrap();
    assert!(status.success(), "campd exited nonzero: {status:?}");
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    run_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn events_of(root: &Path, ty: &str) -> Vec<serde_json::Value> {
    events_json(root)
        .into_iter()
        .filter(|e| e["type"] == ty)
        .collect()
}

/// Test-harness-only wait: poll the ledger (read-only) until an event of
/// the type appears or the deadline passes. The DAEMON never polls; the
/// test does, like a human running `camp events` would.
fn wait_for(root: &Path, ty: &str, timeout: Duration) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let hits = events_of(root, ty);
        if !hits.is_empty() {
            return hits;
        }
        assert!(
            Instant::now() < deadline,
            "no {ty} event within {timeout:?}; ledger: {:?}",
            events_json(root)
                .iter()
                .map(|e| e["type"].as_str().unwrap_or("?").to_owned())
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Exit criterion (master plan Phase 10): away-mode is demonstrably the
/// same code path — a cron order fires with no user session, campd cooks
/// the formula, and the ledger tells the story. `* * * * *` fires at the
/// next minute boundary (≤ ~75 s worst case).
#[test]
fn a_cron_order_fires_and_cooks_with_no_user_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_order(
        &root,
        "\n[[order]]\nname=\"tick\"\non=\"cron:* * * * *\"\nformula=\"one-step\"\n",
    );
    let child = spawn_campd(&root);
    // From here on, nothing writes but campd: the test only reads.
    let fired = wait_for(&root, "order.fired", Duration::from_secs(90));
    assert_eq!(fired[0]["data"]["trigger"], "cron");
    assert_eq!(fired[0]["actor"], "campd");
    assert!(fired[0]["data"]["scheduled_ts"].is_string());
    let fired_seq = fired[0]["seq"].as_i64().unwrap();

    let cooked = wait_for(&root, "run.cooked", Duration::from_secs(10));
    assert_eq!(
        cooked[0]["actor"],
        format!("order:tick:{fired_seq}"),
        "the run's cause chain names its firing"
    );
    // The cooked step bead exists and is ready — nothing dispatched it
    // (Phase 8's job), which is exactly the Phase 10 boundary.
    let ls = run_ok(&root, &["ls", "--ready", "--json"]);
    let beads: serde_json::Value = serde_json::from_str(&ls).unwrap();
    assert!(
        beads
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["title"] == "one step"),
        "{beads}"
    );
    stop_campd(&root, child);
}

/// The full lifecycle on the manual path — the SAME pipeline, fast: fire →
/// cook → fake-agent-contract closes → order.completed; refold clean.
#[test]
fn a_manual_fire_cooks_and_completes_via_the_fake_agent_contract() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_order(
        &root,
        "\n[[order]]\nname=\"one-shot\"\non=\"cron:0 0 1 1 *\"\nformula=\"one-step\"\n",
    );
    let child = spawn_campd(&root);
    run_ok(&root, &["order", "run", "one-shot"]);

    let cooked = wait_for(&root, "run.cooked", Duration::from_secs(10));
    let root_bead = cooked[0]["data"]["root"].as_str().unwrap().to_owned();
    let step_bead = cooked[0]["data"]["steps"]["s1"]
        .as_str()
        .unwrap()
        .to_owned();
    let fired_seq = events_of(&root, "order.fired")[0]["seq"].as_i64().unwrap();

    // The fake-agent contract, spoken through the camp CLI:
    run_ok(&root, &["claim", &step_bead, "--session", "fake-agent"]);
    run_ok(&root, &["close", &step_bead, "--outcome", "pass"]);
    run_ok(&root, &["close", &root_bead, "--outcome", "pass"]);

    let completed = wait_for(&root, "order.completed", Duration::from_secs(10));
    assert_eq!(completed[0]["data"]["order"], "one-shot");
    assert_eq!(completed[0]["data"]["fired_seq"], fired_seq);
    assert_eq!(completed[0]["data"]["root_bead"], root_bead);
    assert_eq!(completed[0]["data"]["outcome"], "pass");

    stop_campd(&root, child);
    // state == history after the whole dance
    let out = camp_cmd(&root)
        .args(["doctor", "--refold"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "doctor --refold: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_event_order_fires_on_matching_close_and_not_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_order(
        &root,
        "\n[[order]]\nname=\"ci-red\"\non=\"event:bead.closed[label=ci-red]\"\nformula=\"one-step\"\n",
    );
    let child = spawn_campd(&root);

    // A plain bead closing must NOT fire. The close's poke is answered
    // only after campd settles, so the absence check is race-free.
    let plain = run_ok(&root, &["create", "plain"]).trim().to_owned();
    run_ok(&root, &["close", &plain, "--outcome", "pass"]);
    assert!(
        events_of(&root, "order.fired").is_empty(),
        "an unlabeled close must not fire the order"
    );

    // A matching close fires, and the fire cooks.
    let red = run_ok(&root, &["create", "red", "--label", "ci-red"])
        .trim()
        .to_owned();
    run_ok(&root, &["close", &red, "--outcome", "pass"]);
    let fired = wait_for(&root, "order.fired", Duration::from_secs(10));
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0]["data"]["trigger"], "event");
    assert_eq!(fired[0]["actor"], "campd");
    // cause_seq points at the matching close event
    let cause_seq = fired[0]["data"]["cause_seq"].as_i64().unwrap();
    let cause = events_json(&root)
        .into_iter()
        .find(|e| e["seq"] == cause_seq)
        .unwrap();
    assert_eq!(cause["type"], "bead.closed");
    assert_eq!(cause["bead"], red);
    wait_for(&root, "run.cooked", Duration::from_secs(10));
    stop_campd(&root, child);
}

#[test]
fn editing_camp_toml_hot_reloads_with_a_config_changed_event() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let child = spawn_campd(&root);
    let good = std::fs::read_to_string(root.join("camp.toml")).unwrap();

    // 1. a new order appears → applied
    std::fs::write(
        root.join("camp.toml"),
        format!("{good}\n[[order]]\nname=\"late\"\non=\"cron:0 0 1 1 *\"\nformula=\"one-step\"\n"),
    )
    .unwrap();
    let changed = wait_for(&root, "config.changed", Duration::from_secs(15));
    assert_eq!(changed[0]["data"]["applied"], true);
    assert_eq!(changed[0]["data"]["orders"], 1);
    // the reloaded order is live: fire it through the daemon
    run_ok(&root, &["order", "run", "late"]);
    wait_for(&root, "run.cooked", Duration::from_secs(10));

    // 2. a broken edit → rejected, campd still serves
    std::fs::write(root.join("camp.toml"), "junk [[[").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let rejected = loop {
        let all = events_of(&root, "config.changed");
        if let Some(r) = all.iter().find(|e| e["data"]["applied"] == false) {
            break r.clone();
        }
        assert!(Instant::now() < deadline, "no rejected config.changed");
        std::thread::sleep(Duration::from_millis(250));
    };
    assert!(!rejected["data"]["error"].as_str().unwrap().is_empty());
    // note: camp.toml is currently junk, so plain CLI verbs would fail —
    // campd itself keeps running on the last applied config (the order
    // from step 1 is still known; the ledger read below needs no config).

    // 3. restore → applied again
    std::fs::write(root.join("camp.toml"), &good).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let applied: Vec<_> = events_of(&root, "config.changed")
            .into_iter()
            .filter(|e| e["data"]["applied"] == true)
            .collect();
        if applied.len() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "no re-applied config.changed");
        std::thread::sleep(Duration::from_millis(250));
    }
    stop_campd(&root, child);
}

/// kill -9 between order.fired and its cook self-heals at the next start
/// (plan Decision D): with no campd running, `camp order run` leaves an
/// orphaned fire; startup reconciliation cooks it.
#[test]
fn an_orphaned_fire_is_cooked_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_order(
        &root,
        "\n[[order]]\nname=\"one-shot\"\non=\"cron:0 0 1 1 *\"\nformula=\"one-step\"\n",
    );
    // no campd: the fire lands, the poke goes nowhere — the orphaned state
    run_ok(&root, &["order", "run", "one-shot"]);
    assert!(events_of(&root, "run.cooked").is_empty());

    let child = spawn_campd(&root);
    let cooked = wait_for(&root, "run.cooked", Duration::from_secs(10));
    let fired_seq = events_of(&root, "order.fired")[0]["seq"].as_i64().unwrap();
    assert_eq!(cooked[0]["actor"], format!("order:one-shot:{fired_seq}"));
    stop_campd(&root, child);
}

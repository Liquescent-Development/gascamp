#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn camp_with_bead() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["create", "do the thing", "--rig", "gascity"])
        .assert()
        .success();
    dir
}

#[test]
fn show_reports_state_and_history() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1"))
        .stdout(predicates::str::contains("in_progress"))
        .stdout(predicates::str::contains("bead.created"))
        .stdout(predicates::str::contains("bead.claimed"));
}

/// Phase 3 (#48 finding 2): `camp show` prints the work axis on a closed
/// bead — the honest record of what became of the work itself.
#[test]
fn show_prints_the_work_outcome() {
    let dir = camp_with_bead();
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
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("work     blocked"));
}

/// A shipped bead promotes branch/commit to first-class fields plus a
/// copy-paste pointer, in BOTH renderings — no git archaeology (design §6).
#[test]
fn show_promotes_shipped_deliverable_coordinates() {
    let dir = camp_with_bead();
    // Append a shipped close directly — the fold records the coordinates;
    // the git gate lives in `camp close`, not the fold.
    {
        let mut ledger =
            camp_core::ledger::Ledger::open(&dir.path().join(".camp/camp.db")).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::BeadClosed,
                rig: Some("gascity".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({
                    "outcome": "pass",
                    "work_outcome": "shipped",
                    "work_branch": "camp/gc-1",
                    "work_commit": "b1d59a2df83a060382ee78b5546cd2f858e3702f",
                }),
            })
            .unwrap();
    }
    // Human rendering: branch + commit + the "see:" pointer to the rig.
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("branch   camp/gc-1"))
        .stdout(predicates::str::contains(
            "commit   b1d59a2df83a060382ee78b5546cd2f858e3702f",
        ))
        .stdout(predicates::str::contains("see: git -C "))
        .stdout(predicates::str::contains(
            "show b1d59a2df83a060382ee78b5546cd2f858e3702f",
        ));
    // JSON rendering: branch + commit are first-class.
    let out = camp()
        .current_dir(dir.path())
        .args(["show", "gc-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["branch"], "camp/gc-1");
    assert_eq!(v["commit"], "b1d59a2df83a060382ee78b5546cd2f858e3702f");
    assert_eq!(v["work_outcome"], "shipped");
}

#[test]
fn show_of_unknown_bead_errors() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-999"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("no such bead"));
}

/// `--json` emits ONE object: the bead's state fields plus a `history`
/// array — the operator's machine read (design §5).
#[test]
fn show_json_emits_state_and_history() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    let out = camp()
        .current_dir(dir.path())
        .args(["show", "gc-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["bead"], "gc-1");
    assert_eq!(v["title"], "do the thing");
    assert_eq!(v["status"], "in_progress");
    assert_eq!(v["ready"], false);
    let kinds: Vec<&str> = v["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"bead.created"), "history kinds: {kinds:?}");
    assert!(kinds.contains(&"bead.claimed"), "history kinds: {kinds:?}");
    // Not shipped → no deliverable coordinates yet.
    assert!(v["branch"].is_null());
    assert!(v["commit"].is_null());
}

/// PR #54 assessment finding A (operator UX): the dispatch-failed marker
/// must tell the operator HOW to retry — campd's in-memory failed set
/// suppresses re-dispatch for its lifetime (plan decision F, by design),
/// so fixing the rig alone does nothing until campd restarts. The show
/// rendering states that, right where the reason is read.
#[test]
fn show_prints_the_dispatch_failure_with_the_retry_hint() {
    let dir = camp_with_bead();
    {
        let mut ledger =
            camp_core::ledger::Ledger::open(&dir.path().join(".camp/camp.db")).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::DispatchFailed,
                rig: Some("gascity".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({
                    "reason": "rig repo cannot host a worktree (no base commit)"
                }),
            })
            .unwrap();
    }
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "dispatch-failed  rig repo cannot host a worktree (no base commit)",
        ))
        .stdout(predicates::str::contains(
            "campd retries once per restart — after fixing the cause, restart campd",
        ));
}

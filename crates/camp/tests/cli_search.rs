#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// Init a camp and register one rig `gascity` (prefix `gc`).
fn camp_with_rig() -> tempfile::TempDir {
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
    dir
}

/// The worker-skill contract: remember a fact at close, recall it in the
/// next session. Memory is beads (bead.created, type=memory), so the
/// ledger must also stay refold-exact afterwards.
#[test]
fn remember_recall_round_trip_stays_refold_clean() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["remember", "the staging deploy needs the legacy token"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));

    camp()
        .current_dir(dir.path())
        .args(["recall", "staging deploy"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1\tbody\t"))
        .stdout(predicates::str::contains("staging deploy"));

    // The memory bead is a real bead: type=memory, visible via show.
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("type     memory"));

    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
}

#[test]
fn recall_filters_to_memory_but_search_sees_everything() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "upgrade tokio to 2.0"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args(["remember", "tokio upgrade blocked on the tracing crate"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-2\n"));

    // recall: only the memory bead.
    let recall = camp()
        .current_dir(dir.path())
        .args(["recall", "tokio"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-2"))
        .get_output()
        .stdout
        .clone();
    assert!(
        !String::from_utf8(recall).unwrap().contains("gc-1"),
        "recall must not surface non-memory beads"
    );

    // search: both.
    camp()
        .current_dir(dir.path())
        .args(["search", "tokio"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1"))
        .stdout(predicates::str::contains("gc-2"));
}

#[test]
fn close_notes_are_searchable_from_the_cli() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "chase the flaky test"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--reason",
            "root cause was a stale dispatcher cache",
        ])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["search", "dispatcher"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1\tclose\t"));
}

/// Rig scoping (master-plan test obligation): memories land in the rig
/// they were remembered against — per-rig id prefix and beads.rig — and
/// with several rigs configured, remember requires --rig (same rule as
/// create).
#[test]
fn remember_scopes_memories_to_the_named_rig() {
    let dir = camp_with_rig();
    let second = dir.path().join("toolbox");
    std::fs::create_dir_all(&second).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&second)
        .args(["--prefix", "tb", "--name", "toolbox"])
        .assert()
        .success();

    camp()
        .current_dir(dir.path())
        .args([
            "remember",
            "gascity pins the gc compiler ref",
            "--rig",
            "gascity",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args([
            "remember",
            "toolbox releases cut from main",
            "--rig",
            "toolbox",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("tb-1\n"));

    // Ambiguous rig fails fast, exactly like create.
    camp()
        .current_dir(dir.path())
        .args(["remember", "an orphan fact"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("multiple rigs configured"));

    // Both memories are recallable; the rig is on the bead row.
    camp()
        .current_dir(dir.path())
        .args(["recall", "toolbox OR gascity"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1"))
        .stdout(predicates::str::contains("tb-1"));
    camp()
        .current_dir(dir.path())
        .args(["ls", "--rig", "toolbox"])
        .assert()
        .success()
        .stdout(predicates::str::contains("tb-1"));
}

#[test]
fn malformed_fts_query_is_a_clean_exit_1() {
    let dir = camp_with_rig();
    for verb in ["search", "recall"] {
        let assert = camp()
            .current_dir(dir.path())
            .args([verb, "("])
            .assert()
            .failure()
            .code(1)
            .stderr(predicates::str::contains("invalid search query"));
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            !stderr.contains("panicked"),
            "{verb} must fail cleanly, got: {stderr}"
        );
    }
}

#[test]
fn no_hits_is_success_with_empty_output() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["search", "zeppelin"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn search_limit_caps_output_lines() {
    let dir = camp_with_rig();
    for i in 1..=3 {
        camp()
            .current_dir(dir.path())
            .args(["create", &format!("widget number {i}")])
            .assert()
            .success();
    }
    let out = camp()
        .current_dir(dir.path())
        .args(["search", "widget", "--limit", "2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 2);
}

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
        .args(["init", "--no-service"])
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

#[test]
fn create_allocates_prefixed_ids_and_stays_refold_clean() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "add a --json flag", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args(["create", "second task", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-2\n"));

    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
}

#[test]
fn create_defaults_to_the_only_rig() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "no --rig needed"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
}

#[test]
fn create_with_no_rigs_errors() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .args(["init", "--no-service"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["create", "orphan"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("no rigs configured"));
}

/// The ids `camp ls --ready --json` reports, in order.
fn ready_ids(dir: &std::path::Path) -> Vec<String> {
    let out = camp()
        .current_dir(dir)
        .args(["ls", "--ready", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn create_needs_wires_dependency_edges_and_gates_readiness() {
    let dir = camp_with_rig();
    // A is ready; B needs A, so B is blocked.
    camp()
        .current_dir(dir.path())
        .args(["create", "task A", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args(["create", "task B", "--rig", "gascity", "--needs", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-2\n"));

    // Only A is ready — the --needs edge gates B.
    assert_eq!(ready_ids(dir.path()), vec!["gc-1"]);

    // Closing A (pass) unblocks B.
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass"])
        .assert()
        .success();
    assert_eq!(ready_ids(dir.path()), vec!["gc-2"]);
}

#[test]
fn create_label_and_type_round_trip_through_show() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args([
            "create", "a memory", "--rig", "gascity", "--type", "memory", "--label", "note",
            "--label", "idea",
        ])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("type     memory"))
        .stdout(predicates::str::contains("labels   note, idea"));
}

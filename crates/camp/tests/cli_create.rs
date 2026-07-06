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
    camp().current_dir(dir.path()).arg("init").assert().success();
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
    camp().current_dir(dir.path()).arg("init").assert().success();
    camp()
        .current_dir(dir.path())
        .args(["create", "orphan"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("no rigs configured"));
}

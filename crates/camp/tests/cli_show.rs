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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn camp_with_bead() -> tempfile::TempDir {
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
    camp()
        .current_dir(dir.path())
        .args(["create", "do the thing", "--rig", "gascity"])
        .assert()
        .success();
    dir
}

#[test]
fn claim_then_close_runs_the_full_lifecycle() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "shipped"])
        .assert()
        .success();
    // ledger stays refold-clean across the whole lifecycle
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
}

#[test]
fn double_claim_fails_fast() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/2"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn close_rejects_a_non_subset_outcome() {
    let dir = camp_with_bead();
    // clap constrains --outcome to pass|fail (usage error, exit 2)
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "skipped"])
        .assert()
        .failure()
        .code(2);
}

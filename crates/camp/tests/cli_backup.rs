//! Phase 13: the `camp backup` verb (VACUUM INTO + integrity_check). This is
//! the CI-safe coverage of the verb; the 1M-event volume backup lives in the
//! #[ignore]d perf suite (`make perf`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::*;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

#[test]
fn backup_writes_an_integrity_checked_copy_and_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    camp()
        .arg("--camp")
        .arg(root)
        .arg("init")
        .assert()
        .success();

    let dest = root.join("snapshot.db");
    camp()
        .arg("--camp")
        .arg(root)
        .arg("backup")
        .arg(&dest)
        .assert()
        .success()
        .stdout(predicate::str::contains("integrity_check ok"));
    assert!(dest.exists());

    // Fail fast on an existing destination.
    camp()
        .arg("--camp")
        .arg(root)
        .arg("backup")
        .arg(&dest)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn backup_without_a_camp_fails() {
    let dir = tempfile::tempdir().unwrap();
    // no `camp init` here: no camp.toml -> resolve fails
    camp()
        .arg("--camp")
        .arg(dir.path())
        .arg("backup")
        .arg(dir.path().join("x.db"))
        .assert()
        .failure();
}

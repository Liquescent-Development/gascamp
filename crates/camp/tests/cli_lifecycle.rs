#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// A bead's whole Tier-0 life through the CLI, with `doctor --refold` clean at
/// every stage (the Phase 3 exit criterion).
#[test]
fn create_claim_close_stays_refold_clean_throughout() {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();

    let refold_clean = |label: &str| {
        camp()
            .current_dir(dir.path())
            .args(["doctor", "--refold"])
            .assert()
            .success()
            .stdout(predicates::str::contains("0 drift rows"));
        let _ = label;
    };

    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    refold_clean("after rig add");

    camp()
        .current_dir(dir.path())
        .args(["create", "the whole life", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    refold_clean("after create");

    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    refold_clean("after claim");

    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "done"])
        .assert()
        .success();
    refold_clean("after close");

    // final state: closed + passed, out of the ready set
    camp()
        .current_dir(dir.path())
        .args(["ls", "--ready", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]\n"));
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("status   closed"))
        .stdout(predicates::str::contains("outcome  pass"));
}

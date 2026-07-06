#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

#[test]
fn init_creates_dot_camp_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicates::str::contains(".camp"));

    assert!(dir.path().join(".camp/camp.toml").exists());
    assert!(dir.path().join(".camp/camp.db").exists());
    let config = std::fs::read_to_string(dir.path().join(".camp/camp.toml")).unwrap();
    assert!(config.contains("[camp]"), "camp.toml was: {config}");
    assert!(config.contains("name = "), "camp.toml was: {config}");
}

#[test]
fn init_with_explicit_camp_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("camps").join("dev");
    camp()
        .current_dir(dir.path())
        .arg("--camp")
        .arg(&target)
        .arg("init")
        .assert()
        .success();

    assert!(target.join("camp.toml").exists());
    assert!(target.join("camp.db").exists());
}

#[test]
fn reinit_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("already"));
}

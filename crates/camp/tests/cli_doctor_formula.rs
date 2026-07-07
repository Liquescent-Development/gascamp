#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn write(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn doctor_formula_exits_0_on_a_valid_formula() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    let f = write(
        dir.path(),
        "minimal.toml",
        "formula = \"minimal\"\n\n[[steps]]\nid = \"only\"\ntitle = \"t\"\n",
    );
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--formula"])
        .arg(&f)
        .assert()
        .success()
        .stdout(predicates::str::contains("formula ok: minimal (1 step(s))"));
}

#[test]
fn doctor_formula_exits_1_listing_every_violation() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    let f = write(
        dir.path(),
        "broken.toml",
        "formula = \"wrong-name\"\npour = true\n\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntags = [\"x\"]\nneeds = [\"ghost\"]\n",
    );
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--formula"])
        .arg(&f)
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("pour"))
        .stdout(predicates::str::contains("tags"))
        .stdout(predicates::str::contains("file stem"))
        .stdout(predicates::str::contains("ghost"))
        .stderr(predicates::str::contains("violation"));
}

#[test]
fn doctor_requires_exactly_one_of_refold_or_formula() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .failure()
        .code(2); // clap usage error
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold", "--formula", "x.toml"])
        .assert()
        .failure()
        .code(2);
}

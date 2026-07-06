#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

#[test]
fn version_prints_name_and_semver() {
    Command::cargo_bin("camp")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::is_match(r"^camp \d+\.\d+\.\d+\n$").unwrap());
}

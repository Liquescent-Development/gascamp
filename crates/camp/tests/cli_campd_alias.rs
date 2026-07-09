#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Issue #33: `campd` vs `camp daemon` — two names for one thing.
//!
//! `camp campd` must be a true clap alias of `camp daemon`, not a second,
//! separately-defined subcommand: same enum variant, same handler, same
//! generated help text. `camp daemon` must keep working exactly as before.

use assert_cmd::Command;

fn camp() -> Command {
    Command::cargo_bin("camp").unwrap()
}

#[test]
fn daemon_help_still_works() {
    camp()
        .args(["daemon", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Run the daemon in the foreground",
        ));
}

#[test]
fn campd_is_a_recognized_alias_for_daemon() {
    camp()
        .args(["campd", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Run the daemon in the foreground",
        ));
}

/// A clap alias reuses the aliased subcommand's own definition, so its
/// `--help` output is identical byte-for-byte regardless of which name
/// invoked it. Asserting equality proves `campd` and `daemon` parse to the
/// same command, not two independently-defined commands that merely look
/// alike.
#[test]
fn campd_and_daemon_help_are_identical() {
    let daemon_help = camp()
        .args(["daemon", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let campd_help = camp()
        .args(["campd", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        String::from_utf8(daemon_help).unwrap(),
        String::from_utf8(campd_help).unwrap(),
        "camp campd --help and camp daemon --help must describe the same command"
    );
}

/// Top-level `camp --help` should cross-reference the two names so a user
/// scanning the command list can see `campd` without guessing.
#[test]
fn top_level_help_cross_references_campd_alias() {
    camp()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("campd"));
}

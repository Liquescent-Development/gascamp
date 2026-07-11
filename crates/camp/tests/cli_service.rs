#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 2 (campd service management): the `camp service` control surface.
//!
//! The tests in unit CI are READ-ONLY and must pass on a host with a service
//! manager (macOS) and one without (a Linux CI runner): they never install,
//! start or remove a unit. The full lifecycle against the host's REAL service
//! manager is the `#[ignore]`d, CAMP_SERVICE_E2E-gated test added in Task 6.

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// `camp service list` is a pure query over the unit directory — the one
/// `camp service` verb that needs no camp at all (design §5: it is the
/// "manage everything" view across every managed camp). It must succeed
/// everywhere, mutate nothing, and print SOMETHING (an answer, never silence).
#[test]
fn service_list_is_a_read_only_query_that_needs_no_camp() {
    let dir = tempfile::tempdir().unwrap();
    let out = camp()
        .current_dir(dir.path())
        .args(["service", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        !String::from_utf8_lossy(&out).trim().is_empty(),
        "list must answer the query (managed units, or why there are none)"
    );
}

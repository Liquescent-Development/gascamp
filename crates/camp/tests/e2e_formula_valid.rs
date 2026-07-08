#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 15: CI-safe proof that the e2e-guarded formula fixture is a valid
//! formula-v2 subset (spec §8.2). No claude — pure `camp doctor --formula`.
use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

#[test]
fn e2e_guarded_formula_is_a_valid_subset() {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();

    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/formulas/e2e-guarded.toml"
    );
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--formula", fixture])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "formula ok: e2e-guarded (2 step(s))",
        ));
}

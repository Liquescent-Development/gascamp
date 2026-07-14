#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The starter pack is content, not machinery (Phase 12, spec §11). Its
//! `guarded-change.toml` is a symlink into the gc-validated corpus (single
//! source of truth, Decision D3): the Phase 6 gc gate already validates
//! that directory, so the pack formula passes it transitively.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn starter_formula_is_the_corpus_file_and_doctor_accepts_it() {
    let pack_formula = repo_root().join("packs/starter/formulas/guarded-change.toml");
    let corpus =
        repo_root().join("crates/camp-core/tests/fixtures/formulas/valid/guarded-change.toml");

    assert!(
        std::fs::symlink_metadata(&pack_formula)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the starter formula must be a symlink into the gc-validated corpus (one source of truth)"
    );
    assert_eq!(
        std::fs::canonicalize(&pack_formula).unwrap(),
        std::fs::canonicalize(&corpus).unwrap(),
        "the symlink must resolve to the corpus file"
    );

    // `doctor --formula` COMPILES THROUGH THE LAYERS now (compat §9): an imported
    // formula's `extends`, `description_file` and routes only resolve against a
    // real camp, so the verb needs one — as every other verb already does.
    let dir = tempfile::tempdir().unwrap();
    Command::new(BIN)
        .current_dir(dir.path())
        .args(["init", "--no-service"])
        .status()
        .unwrap();
    let out = Command::new(BIN)
        .current_dir(dir.path())
        .args(["doctor", "--formula"])
        .arg(&pack_formula)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "camp doctor --formula must accept the starter formula: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn starter_pack_ships_agent_definitions() {
    // compat §5.1: an agent is a directory (agent.toml + prompt.md).
    for a in ["dev", "reviewer"] {
        let dir = repo_root().join(format!("packs/starter/agents/{a}"));
        let toml = std::fs::read_to_string(dir.join("agent.toml"))
            .unwrap_or_else(|_| panic!("agents/{a}/agent.toml must exist"));
        assert!(
            toml.contains("description"),
            "agents/{a}/agent.toml must carry a description: {toml}"
        );
        assert!(
            dir.join("prompt.md").is_file(),
            "agents/{a}/prompt.md must exist"
        );
    }
}

#[test]
fn starter_dev_agent_scopes_test_first_to_code_changes() {
    // Regression guard for issue #30: the dev worker's prompt used to
    // hardcode a blanket "test-first" mandate onto every task, even
    // non-code ones (e.g. "give this repo a proper README.md"), pushing
    // the worker to invent tests for documentation.
    let p = repo_root().join("packs/starter/agents/dev/prompt.md");
    let s = std::fs::read_to_string(&p).expect("dev/prompt.md must exist");
    let lower = s.to_lowercase();

    assert!(
        !s.contains("implement the change test-first"),
        "dev prompt must not hardcode a blanket, unconditional test-first mandate: {s}"
    );
    assert!(
        lower.contains("code") && s.contains("test-first"),
        "dev prompt must scope the test-first guidance to code changes"
    );
    assert!(
        lower.contains("docs") || lower.contains("documentation"),
        "dev prompt must call out non-code changes (docs/config) as a distinct case"
    );
    assert!(
        lower.contains("verify") || lower.contains("verif"),
        "dev prompt must instruct the worker to verify non-code changes appropriately"
    );
}

#[test]
fn starter_dev_agent_carries_the_delivery_contract() {
    let dev =
        std::fs::read_to_string(repo_root().join("packs/starter/agents/dev/prompt.md")).unwrap();
    for needle in ["camp/", "work outcome", "shipped", "blocked", "never push"] {
        assert!(dev.contains(needle), "dev agent must state `{needle}`");
    }
}

#[test]
fn starter_pack_ships_a_committer_role_and_the_plugin_still_ships_none() {
    // compat §5.1: identity is the directory name; the prompt carries the role.
    let dir = repo_root().join("packs/starter/agents/committer");
    assert!(dir.is_dir(), "agents/committer/ must exist");
    let committer = std::fs::read_to_string(dir.join("prompt.md")).unwrap();
    assert!(committer.contains("git"));
    // the role-free-plugin policy is enforced by plugin_policy.rs; this is
    // the positive control that the new role landed in the PACK.
}

#[test]
fn starter_pack_orders_example_exists() {
    // compat: pack orders live in an `orders/` directory (gc `[order]` shape).
    let orders_dir = repo_root().join("packs/starter/orders");
    assert!(orders_dir.is_dir(), "packs/starter/orders/ must exist");
    let nightly = std::fs::read_to_string(orders_dir.join("morning-triage.toml"))
        .expect("orders/morning-triage.toml must exist");
    assert!(
        nightly.contains("[order]"),
        "orders/morning-triage.toml must use the gc [order] form"
    );
}

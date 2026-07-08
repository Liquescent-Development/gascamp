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

    let out = Command::new(BIN)
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
    for a in ["dev", "reviewer"] {
        let p = repo_root().join(format!("packs/starter/agents/{a}.md"));
        let s = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("{a}.md must exist"));
        assert!(
            s.starts_with("---") && s.contains("description:"),
            "{a}.md must be a Claude Code agent definition with frontmatter"
        );
    }
}

#[test]
fn starter_pack_orders_example_exists() {
    let orders = repo_root().join("packs/starter/orders.toml");
    let s = std::fs::read_to_string(&orders).expect("packs/starter/orders.toml must exist");
    assert!(
        s.contains("[[order]]"),
        "orders.toml must use the §9 [[order]] form"
    );
}

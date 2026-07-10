#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The worker skill IS the lifecycle contract (Phase 12). This test pins
//! that the shipped SKILL.md documents every contract verb, so the contract
//! text can never silently drop a step.

use std::path::PathBuf;

fn worker_skill() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin/skills/worker/SKILL.md");
    std::fs::read_to_string(&p).expect("plugin/skills/worker/SKILL.md must exist")
}

#[test]
fn worker_skill_documents_every_lifecycle_verb() {
    let s = worker_skill();
    for needle in [
        "camp recall",
        "camp claim",
        "camp event emit",
        "camp remember",
        "camp close",
        "exit",
    ] {
        assert!(s.contains(needle), "worker skill must document `{needle}`");
    }
}

/// Dispatch-lifecycle Phase 3 (#34): the unified contract carries the
/// delivery semantics — commit to the bead branch, record the WorkOutcome
/// axis (gc vocabulary verbatim), no remote in v1.
#[test]
fn worker_skill_carries_the_delivery_contract() {
    let s = worker_skill();
    for needle in [
        "camp/<bead>",
        "--work-outcome",
        "--work-commit",
        "--work-branch",
        "shipped",
        "no-op",
        "blocked",
        "abandoned",
        "never push",
    ] {
        assert!(s.contains(needle), "worker skill must state `{needle}`");
    }
}

#[test]
fn worker_skill_has_skill_frontmatter() {
    let s = worker_skill();
    assert!(s.starts_with("---"), "must open with YAML frontmatter");
    assert!(
        s.contains("name: worker"),
        "frontmatter must set name: worker"
    );
    assert!(
        s.contains("description:"),
        "frontmatter must have a description"
    );
}

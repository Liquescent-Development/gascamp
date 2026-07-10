#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The operator skill IS the control-plane contract (mirror of the worker
//! skill test). This pins that the shipped SKILL.md keeps the mental model,
//! the delivery model, the output discipline, and the don't-poll rule — so
//! the contract can never silently lose a load-bearing line.

use std::path::PathBuf;

fn operator_skill() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin/skills/operator/SKILL.md");
    std::fs::read_to_string(&p).expect("plugin/skills/operator/SKILL.md must exist")
}

#[test]
fn operator_skill_has_skill_frontmatter() {
    let s = operator_skill();
    assert!(s.starts_with("---"), "must open with YAML frontmatter");
    assert!(
        s.contains("name: operator"),
        "frontmatter must set name: operator"
    );
    assert!(
        s.contains("description:"),
        "frontmatter must have a description"
    );
}

#[test]
fn operator_skill_states_the_mental_model() {
    let s = operator_skill();
    for needle in [
        "campd",       // the sole dispatcher
        "enqueue",     // sling only enqueues
        "camp/<bead>", // the branch is the deliverable
        "no remote",   // v1 has no remote/PR/merge
        "shipped",     // shipped is mechanically verified already
    ] {
        assert!(s.contains(needle), "operator skill must state `{needle}`");
    }
}

#[test]
fn operator_skill_carries_the_output_and_polling_discipline() {
    let s = operator_skill();
    for needle in [
        "never paste", // read-and-summarize, don't dump raw output
        "--json",      // machine read
        "poll",        // don't poll
        "--wait",      // the awaitable read
    ] {
        assert!(s.contains(needle), "operator skill must state `{needle}`");
    }
}

#[test]
fn operator_skill_lists_the_operator_verbs() {
    let s = operator_skill();
    for needle in ["camp sling", "camp show", "camp nudge", "camp top"] {
        assert!(
            s.contains(needle),
            "operator skill must reference `{needle}`"
        );
    }
}

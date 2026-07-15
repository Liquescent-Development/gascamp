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

#[test]
fn operator_skill_names_the_control_plane_verbs() {
    let s = operator_skill();
    for needle in [
        "camp sessions",  // §5.4 list sessions (sessions.list)
        "camp attach",    // §5.4 read their streams (session.subscribe)
        "camp nudge",     // §5.4 send them turns (session.send_turn)
        "camp interrupt", // §5.4 interrupt them (session.interrupt)
        "camp decide",    // §5.3 answer a permission (session.permission_decision)
    ] {
        assert!(
            s.contains(needle),
            "operator skill must name the control-plane verb `{needle}`"
        );
    }
}

#[test]
fn operator_skill_states_the_no_private_paths_discipline() {
    let s = operator_skill();
    // Whitespace-normalized so markdown line-wrapping cannot hide a phrase that
    // straddles a newline ("never\n  tail ...").
    let norm = s.split_whitespace().collect::<Vec<_>>().join(" ");

    // The reach-a-worker-only-through-the-socket rule (§4): the skill must NAME
    // the socket as the only path.
    assert!(
        norm.contains("socket"),
        "operator skill must name the socket as the only path to a worker"
    );

    // NEGATIVE FORM (CP5-2): substring-presence of the nouns is NOT enough — a
    // SKILL reworded to ENDORSE private paths ("you may tail a worker's stream
    // file and reach it by pid") keeps every noun. The guard must require the
    // FORBIDDING phrasing itself, so the endorsing rewording reddens it.
    for phrase in ["never tail a worker's stream file", "never reach it by pid"] {
        assert!(
            norm.contains(phrase),
            "operator skill must FORBID private paths in words — missing `{phrase}` (§4)"
        );
    }

    // ...and it must carry no imperative that ENDORSES a private path. This is
    // the direct catch for the mutation the reviewer proved slips past a
    // noun-only guard.
    for endorsement in [
        "may tail a worker's stream file",
        "can tail a worker's stream file",
        "you may reach it by pid",
    ] {
        assert!(
            !norm.contains(endorsement),
            "operator skill must not ENDORSE a private path — found `{endorsement}` (§4)"
        );
    }
}

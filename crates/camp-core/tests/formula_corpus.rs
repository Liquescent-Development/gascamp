//! Table-driven acceptance/rejection over the fixture corpus (master-plan
//! Phase 5). Every valid fixture must parse clean; every invalid fixture
//! must fail with a violation naming the expected construct; and the table
//! must cover exactly the files on disk so a fixture can never silently
//! drop out of coverage. Phase 6 revalidates valid/ with the real gc
//! compiler.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(non_snake_case)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use camp_core::formula::parse_and_validate;

fn corpus(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formulas")
        .join(kind)
}

fn toml_files(kind: &str) -> BTreeSet<String> {
    std::fs::read_dir(corpus(kind))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| p.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect()
}

#[test]
fn every_valid_fixture_is_accepted() {
    let files = toml_files("valid");
    // INVARIANT 6 — every one of these is compiled by the REAL gc compiler in
    // CI's `gc-compat` job (`camp_corpus_validate.go`). A FAIL there means camp
    // accepts what gc rejects, and the subset property is broken.
    //
    // Naming rules the validator imposes, and they are not cosmetic:
    //   * NEVER `*.formula.toml` here — it derives the gc name as
    //     `TrimSuffix(basename, ".toml")`, so gc would look up `"x.formula"`.
    //   * NO expansion fixture — the validator compiles each file STANDALONE,
    //     and an expansion formula is not directly runnable (§9).
    //   * An `extends` CHILD needs a parent LAYER the validator does not
    //     provide, so the child lives in `tests/fixtures/compose/` and only the
    //     PARENT is here.
    assert_eq!(
        files,
        [
            "diamond",
            "drain-separate", // rung 2e
            "extends-parent", // rung 2c (the child needs a layer; it lives in compose/)
            "fan-out",
            "guarded-change",
            "minimal",
            "retry-fetch",
            "vars-condition", // rung 2b
        ]
        .into_iter()
        .map(String::from)
        .collect::<BTreeSet<_>>(),
        "valid corpus drifted from the plan"
    );
    for stem in files {
        let path = corpus("valid").join(format!("{stem}.toml"));
        match parse_and_validate(&path) {
            Ok(f) => assert_eq!(f.name, stem),
            Err(e) => panic!("{stem} must be valid:\n{e}"),
        }
    }
}

/// filename stem -> the construct or key the verdict must name.
///
/// **16 fixtures were DELETED with their rows** (`extends`, `vars`,
/// `type-top-level`, `contract`, `catalog`, `template`, `drain`, `expand`,
/// `expand-vars`, `children`, `condition`, `metadata`, `description-file`,
/// `priority`, `tags`, `notes`). Camp used to reject every one of those keys by
/// name — they are the constructs the real Gas City corpus is BUILT from, and
/// refusing them is what held camp to 5 of 100 formulas. They are now accepted
/// (compat §4/§9) and their fixtures would assert the opposite of the truth.
///
/// What is left is what camp still refuses ON PURPOSE (§4 rule 1 — 0 corpus uses
/// each), what it rejects as MALFORMED, and — D2′ — unrecognised keys in the
/// operator's own `<root>/formulas/`, which stay fatal.
const REJECTIONS: &[(&str, &str)] = &[
    // §4 rule 1 refusals, top level
    ("phase", "phase"),
    ("pour", "pour"),
    ("compose", "compose"),
    ("advice", "advice"),
    ("pointcuts", "pointcuts"),
    // §4 rule 1 refusals, step level
    ("gate", "gate"),
    ("loop", "loop"),
    ("waits-for", "waits_for"),
    ("tally", "tally"),
    ("depends-on", "depends_on"),
    // D2′ — unrecognised keys stay FATAL in the camp-local tier
    ("unknown-key", "dependson"),
    ("nested-unknown-key", "steps.a.check.retries"),
    ("type-step-level", "type"),
    // semantic
    ("dup-step-id", "steps.a.id"),
    ("unknown-needs-id", "steps.a.needs"),
    ("cycle", "steps"),
    ("bad-semver", "requires.formula_compiler"),
    ("caret-requirement", "requires.formula_compiler"),
    ("bare-version-requirement", "requires.formula_compiler"),
    ("spaced-requirement", "requires.formula_compiler"),
    ("unsatisfied-requirement", "requires.formula_compiler"),
    ("check-without-requires", "requires"),
    ("check-with-retry", "steps.a.check"),
    ("check-with-assignee", "steps.a.check"),
    ("retry-with-on-complete", "steps.a.retry"),
    ("for-each-not-output", "steps.a.on_complete.for_each"),
    ("on-complete-missing-bond", "steps.a.on_complete"),
    ("parallel-and-sequential", "steps.a.on_complete"),
    ("timeout-without-check", "steps.a.timeout"),
    ("check-mode-not-exec", "steps.a.check.check.mode"),
    ("check-zero-attempts", "steps.a.check.max_attempts"),
    ("retry-zero-attempts", "steps.a.retry.max_attempts"),
    ("bad-on-exhausted", "steps.a.retry.on_exhausted"),
    ("name-stem-mismatch", "formula"),
    ("missing-title", "steps.a.title"),
    ("multi-violation", "pour"),
];

#[test]
fn every_invalid_fixture_is_rejected_naming_the_construct() {
    let on_disk = toml_files("invalid");
    let in_table: BTreeSet<String> = REJECTIONS.iter().map(|(f, _)| (*f).to_owned()).collect();
    assert_eq!(
        on_disk, in_table,
        "invalid corpus and rejection table must match"
    );

    for (stem, construct) in REJECTIONS {
        let path = corpus("invalid").join(format!("{stem}.toml"));
        let err = parse_and_validate(&path).expect_err(&format!("{stem} must be rejected"));
        // `names`, not `violations` — a refusal is not a violation, and half
        // this table is refusals now.
        assert!(
            err.names(construct),
            "{stem}: nothing names {construct:?} — got:\n{err}"
        );
    }
}

#[test]
fn multi_violation_fixture_reports_every_problem_at_once() {
    let err = parse_and_validate(&corpus("invalid").join("multi-violation.toml"))
        .expect_err("multi-violation must be rejected");
    for construct in [
        "pour",
        "steps.a.gate",
        "formula",
        "steps.a.needs",
        "steps.a.timeout",
    ] {
        assert!(err.names(construct), "missing {construct:?} in:\n{err}");
    }
    // The two buckets, counted separately — the fixture exists to prove camp
    // reports EVERY problem at once, and BD6 is the proof that it stopped: the
    // fixture's old `tags = ["x"]` became an accepted annotation, so it silently
    // produced one fewer finding than its own assertion demanded.
    assert_eq!(
        err.violations.len(),
        3,
        "formula-stem, needs, timeout:\n{err}"
    );
    assert_eq!(err.refusals.len(), 2, "pour, gate:\n{err}");
}

#[test]
fn a_refused_formula_is_refused_by_a_REFUSAL_not_a_violation() {
    // The distinction the whole key table turns on: `phase` is well-formed Gas
    // City that camp DECLINES (§4 rule 1) — permanent, and it names its key.
    // `unknown-key` is MALFORMED in camp's own tier (D2′) — a different thing.
    let err = parse_and_validate(&corpus("invalid").join("phase.toml"))
        .expect_err("phase must be refused");
    assert!(err.violations.is_empty(), "not a violation:\n{err}");
    assert_eq!(err.refusals.len(), 1, "{err}");
    assert_eq!(err.refusals[0].key, "phase");
    assert_eq!(
        err.refusals[0].step, None,
        "formula-scoped: nothing prunes it"
    );

    let err = parse_and_validate(&corpus("invalid").join("unknown-key.toml"))
        .expect_err("unknown key must be fatal in the camp-local tier");
    assert!(err.refusals.is_empty(), "not a refusal:\n{err}");
    assert!(!err.violations.is_empty(), "{err}");
}

#[test]
fn a_step_scoped_refusal_carries_its_step_id() {
    // BD2: a step's refusals must be attributable to the step, because
    // condition-pruning (rung 2b) DISCARDS them with it. Without the step id
    // there is nothing to prune them by, and 19 corpus formulas with a
    // conditional shared-drain arm refuse at parse — a ceiling of 76, not 95.
    let err =
        parse_and_validate(&corpus("invalid").join("gate.toml")).expect_err("gate must refuse");
    assert_eq!(err.refusals.len(), 1, "{err}");
    assert_eq!(err.refusals[0].key, "gate");
    assert_eq!(err.refusals[0].step.as_deref(), Some("a"), "{err}");
}

#[test]
fn guarded_change_fixture_is_the_spec_example_verbatim() {
    let text = std::fs::read_to_string(corpus("valid").join("guarded-change.toml")).unwrap();
    // Anchor lines from spec §8.2 — if the spec changes, this fixture must
    // change with it (spec and code never silently diverge).
    for anchor in [
        "formula = \"guarded-change\"",
        "formula_compiler = \">=2.0.0\"",
        "max_attempts = 3",
        "path = \"scripts/verify.sh\"",
        "timeout = \"5m\"",
        "needs = [\"implement\"]",
    ] {
        assert!(text.contains(anchor), "spec anchor missing: {anchor}");
    }
}

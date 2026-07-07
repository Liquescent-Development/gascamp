//! Table-driven acceptance/rejection over the fixture corpus (master-plan
//! Phase 5). Every valid fixture must parse clean; every invalid fixture
//! must fail with a violation naming the expected construct; and the table
//! must cover exactly the files on disk so a fixture can never silently
//! drop out of coverage. Phase 6 revalidates valid/ with the real gc
//! compiler.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    assert_eq!(
        files,
        [
            "diamond",
            "fan-out",
            "guarded-change",
            "minimal",
            "retry-fetch"
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

/// filename stem -> the construct a violation must name.
const REJECTIONS: &[(&str, &str)] = &[
    // city-only, top level
    ("extends", "extends"),
    ("vars", "vars"),
    ("type-top-level", "type"),
    ("phase", "phase"),
    ("pour", "pour"),
    ("contract", "contract"),
    ("catalog", "catalog"),
    ("template", "template"),
    ("compose", "compose"),
    ("advice", "advice"),
    ("pointcuts", "pointcuts"),
    // city-only, step level
    ("drain", "drain"),
    ("gate", "gate"),
    ("loop", "loop"),
    ("expand", "expand"),
    ("expand-vars", "expand_vars"),
    ("children", "children"),
    ("waits-for", "waits_for"),
    ("condition", "condition"),
    ("tally", "tally"),
    ("metadata", "metadata"),
    ("depends-on", "depends_on"),
    ("type-step-level", "type"),
    ("priority", "priority"),
    ("tags", "tags"),
    ("description-file", "description_file"),
    ("notes", "notes"),
    // stricter-than-gc
    ("unknown-key", "dependson"),
    ("nested-unknown-key", "steps.a.check.retries"),
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
        assert!(
            err.violations.iter().any(|v| v.construct == *construct),
            "{stem}: no violation names {construct:?} — got:\n{err}"
        );
    }
}

#[test]
fn multi_violation_fixture_reports_every_problem_at_once() {
    let err = parse_and_validate(&corpus("invalid").join("multi-violation.toml"))
        .expect_err("multi-violation must be rejected");
    for construct in [
        "pour",
        "tags",
        "formula",
        "steps.a.needs",
        "steps.a.timeout",
    ] {
        assert!(
            err.violations.iter().any(|v| v.construct == construct),
            "missing {construct:?} in:\n{err}"
        );
    }
    assert!(err.violations.len() >= 5, "{err}");
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

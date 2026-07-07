//! Semantic validation for the camp formula subset (rules S1–S13 in the
//! Phase 5 plan). Pure functions over the raw walk output; every rule
//! records a Violation — the caller reports all of them at once.

use std::collections::{BTreeMap, BTreeSet};

use crate::formula::ast::{Disposition, Formula, Retry, Step, Violation};
use crate::formula::parse::{RawFormula, RawStep};

/// Camp's formula-compiler capability. Mirrors gc's v2 host capability
/// (gc formula-spec-v2 §5); `[requires] formula_compiler` comparators are
/// checked against this version.
pub const FORMULA_COMPILER_CAPABILITY: &str = "2.0.0";

fn violation(out: &mut Vec<Violation>, construct: impl Into<String>, message: impl Into<String>) {
    out.push(Violation {
        construct: construct.into(),
        message: message.into(),
    });
}

/// Location prefix for a step: its id, else its index.
fn step_loc(step: &RawStep) -> String {
    match &step.id {
        Some(id) => format!("steps.{id}"),
        None => format!("steps[{}]", step.index),
    }
}

/// Run rules S1–S13. Appends to `out`; the caller already holds the walk's
/// shape violations.
pub(crate) fn check(raw: &RawFormula, stem: Option<&str>, out: &mut Vec<Violation>) {
    // S1/S2 — header name.
    match raw.name.as_deref() {
        None | Some("") => violation(out, "formula", "the `formula` name is required"),
        Some(name) => {
            if let Some(stem) = stem
                && name != stem
            {
                violation(
                    out,
                    "formula",
                    format!(
                        "formula name {name:?} must equal the file stem {stem:?} \
                         (camp enforces gc's name-is-the-lookup-key convention)"
                    ),
                );
            }
        }
    }

    // S3 — at least one step.
    if raw.steps.is_empty() {
        violation(out, "steps", "a camp formula must declare at least one step");
    }

    // S4 — ids: required, non-empty, unique.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for step in &raw.steps {
        match step.id.as_deref() {
            None | Some("") => {
                violation(out, format!("{}.id", step_loc(step)), "step `id` is required")
            }
            Some(id) => {
                if !seen.insert(id) {
                    violation(out, format!("steps.{id}.id"), format!("duplicate step id {id:?}"));
                }
            }
        }
    }
    let known: BTreeSet<&str> = raw.steps.iter().filter_map(|s| s.id.as_deref()).collect();

    for step in &raw.steps {
        let loc = step_loc(step);

        // S5 — title.
        if step.title.as_deref().is_none_or(str::is_empty) {
            violation(out, format!("{loc}.title"), "step `title` is required");
        }

        // S6 — needs reference known, non-self, non-duplicate ids.
        let mut seen_needs: BTreeSet<&str> = BTreeSet::new();
        for need in &step.needs {
            if Some(need.as_str()) == step.id.as_deref() {
                violation(out, format!("{loc}.needs"), format!("step {need:?} needs itself"));
            } else if !known.contains(need.as_str()) {
                violation(
                    out,
                    format!("{loc}.needs"),
                    format!("needs unknown step id {need:?}"),
                );
            }
            if !seen_needs.insert(need) {
                violation(
                    out,
                    format!("{loc}.needs"),
                    format!("duplicate needs entry {need:?}"),
                );
            }
        }

        // S8 — timeout requires check (gc formula-spec-v2 §1.3).
        if step.timeout.is_some() && step.check.is_none() {
            violation(
                out,
                format!("{loc}.timeout"),
                "step `timeout` bounds the check script and requires `check` \
                 (gc formula-spec-v2 §1.3)",
            );
        }

        // S9 — combination rules (gc formula-spec-v2 §3.1/§3.2).
        if step.check.is_some() && step.retry.is_some() {
            violation(
                out,
                format!("{loc}.check"),
                "`check` must not be combined with `retry` (gc formula-spec-v2 §3.1)",
            );
        }
        if step.check.is_some() && step.assignee.is_some() {
            violation(
                out,
                format!("{loc}.check"),
                "`check` must not be combined with `assignee` (gc formula-spec-v2 §3.1)",
            );
        }
        if step.retry.is_some() && step.on_complete.is_some() {
            violation(
                out,
                format!("{loc}.retry"),
                "`retry` must not be combined with `on_complete` (gc formula-spec-v2 §3.2)",
            );
        }

        // S10 — retry.on_exhausted vocabulary.
        if let Some(retry) = &step.retry
            && let Some(value) = retry.on_exhausted.as_deref()
            && !crate::vocab::CAMP_FINAL_DISPOSITIONS.contains(&value)
        {
            violation(
                out,
                format!("{loc}.retry.on_exhausted"),
                format!("on_exhausted {value:?} is not legal; use \"hard_fail\" or \"soft_fail\""),
            );
        }

        // S13 — for_each path shape.
        if let Some(oc) = &step.on_complete
            && !oc.for_each.starts_with("output.")
        {
            violation(
                out,
                format!("{loc}.on_complete.for_each"),
                format!("for_each {:?} must start with \"output.\"", oc.for_each),
            );
        }
    }

    // S7 — acyclic needs graph (unknown ids were already S6).
    check_cycles(raw, out);

    // S11 — the explicit-declaration rule (gc compile.go:51 concept).
    let uses_graph_only = raw
        .steps
        .iter()
        .any(|s| s.check.is_some() || s.retry.is_some() || s.on_complete.is_some());
    if uses_graph_only && raw.formula_compiler.is_none() {
        violation(
            out,
            "requires",
            "formulas that use graph-only constructs must declare \
             [requires] formula_compiler = \">=2.0.0\" (gc formula-spec-v2 §5)",
        );
    }

    // S12 — the comparator itself.
    if let Some(req) = raw.formula_compiler.as_deref() {
        match semver::VersionReq::parse(req) {
            Err(e) => violation(
                out,
                "requires.formula_compiler",
                format!(
                    "formula_compiler must be a semver comparator, for example \">=2.0.0\": {e}"
                ),
            ),
            Ok(parsed) => {
                // The capability constant is a literal; a broken constant
                // must fail loudly, not silently pass.
                match semver::Version::parse(FORMULA_COMPILER_CAPABILITY) {
                    Err(e) => violation(
                        out,
                        "requires.formula_compiler",
                        format!("internal: capability constant unparseable: {e}"),
                    ),
                    Ok(capability) => {
                        if !parsed.matches(&capability) {
                            violation(
                                out,
                                "requires.formula_compiler",
                                format!(
                                    "formula requires formula_compiler {req:?}, but camp's \
                                     capability is {FORMULA_COMPILER_CAPABILITY}"
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Iterative-state DFS cycle detection; reports one violation per distinct
/// cycle found, with the cycle's path in the message.
fn check_cycles(raw: &RawFormula, out: &mut Vec<Violation>) {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        InStack,
        Done,
    }

    fn dfs<'a>(
        node: &'a str,
        edges: &BTreeMap<&'a str, Vec<&'a str>>,
        state: &mut BTreeMap<&'a str, State>,
        stack: &mut Vec<&'a str>,
        out: &mut Vec<Violation>,
        reported: &mut BTreeSet<String>,
    ) {
        state.insert(node, State::InStack);
        stack.push(node);
        for &next in edges.get(node).map(Vec::as_slice).unwrap_or(&[]) {
            match state.get(next) {
                Some(State::InStack) => {
                    let start = stack.iter().position(|&n| n == next).unwrap_or(0);
                    let mut cycle: Vec<&str> = stack[start..].to_vec();
                    cycle.push(next);
                    // Canonical form so the same cycle is reported once.
                    let mut canonical = cycle.clone();
                    canonical.pop();
                    canonical.sort_unstable();
                    if reported.insert(canonical.join(",")) {
                        out.push(Violation {
                            construct: "steps".to_owned(),
                            message: format!("dependency cycle: {}", cycle.join(" -> ")),
                        });
                    }
                }
                Some(State::Unvisited) => {
                    dfs(next, edges, state, stack, out, reported);
                }
                _ => {}
            }
        }
        stack.pop();
        state.insert(node, State::Done);
    }

    let edges: BTreeMap<&str, Vec<&str>> = raw
        .steps
        .iter()
        .filter_map(|s| {
            s.id.as_deref()
                .map(|id| (id, s.needs.iter().map(String::as_str).collect()))
        })
        .collect();
    let mut state: BTreeMap<&str, State> = edges.keys().map(|&k| (k, State::Unvisited)).collect();
    let mut reported: BTreeSet<String> = BTreeSet::new();
    let nodes: Vec<&str> = edges.keys().copied().collect();
    for node in nodes {
        if state.get(node) == Some(&State::Unvisited) {
            let mut stack = Vec::new();
            dfs(node, &edges, &mut state, &mut stack, out, &mut reported);
        }
    }
}

/// Convert a violation-free RawFormula into the public Formula. Only call
/// after `check` reported no violations (parse_and_validate enforces this).
pub(crate) fn assemble(raw: RawFormula, source: String) -> Formula {
    Formula {
        name: raw.name.unwrap_or_default(),
        description: raw.description,
        requires: raw
            .formula_compiler
            .map(|formula_compiler| crate::formula::ast::Requires { formula_compiler }),
        steps: raw
            .steps
            .into_iter()
            .map(|s| Step {
                id: s.id.unwrap_or_default(),
                title: s.title.unwrap_or_default(),
                description: s.description,
                needs: s.needs,
                assignee: s.assignee,
                timeout: s.timeout,
                check: s.check,
                retry: s.retry.map(|r| Retry {
                    max_attempts: r.max_attempts,
                    on_exhausted: match r.on_exhausted.as_deref() {
                        Some("soft_fail") => Disposition::SoftFail,
                        _ => Disposition::HardFail, // gc default
                    },
                }),
                on_complete: s.on_complete,
            })
            .collect(),
        source,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::formula::ast::Violation;
    use crate::formula::parse::walk;

    fn violations_for(text: &str, stem: &str) -> Vec<Violation> {
        let (raw, mut v) = walk(text);
        super::check(&raw, Some(stem), &mut v);
        v
    }

    fn has(v: &[Violation], construct: &str, needle: &str) -> bool {
        v.iter()
            .any(|v| v.construct == construct && v.message.contains(needle))
    }

    const HEADER: &str = "formula = \"f\"\n";

    #[test]
    fn name_must_match_the_file_stem() {
        let v = violations_for(
            "formula = \"other\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n",
            "f",
        );
        assert!(has(&v, "formula", "file stem"), "{v:?}");
        assert!(
            violations_for(&format!("{HEADER}[[steps]]\nid = \"a\"\ntitle = \"t\"\n"), "f")
                .is_empty()
        );
    }

    #[test]
    fn missing_name_missing_steps_missing_title_all_reported_together() {
        let v = violations_for("[[steps]]\nid = \"a\"\n", "f");
        assert!(has(&v, "formula", "required"), "{v:?}");
        assert!(has(&v, "steps.a.title", "required"), "{v:?}");
        let v = violations_for("formula = \"f\"\n", "f");
        assert!(has(&v, "steps", "at least one step"), "{v:?}");
    }

    #[test]
    fn duplicate_ids_unknown_needs_self_needs_and_dup_needs_are_reported() {
        let text = format!(
            "{HEADER}\
             [[steps]]\nid = \"a\"\ntitle = \"t\"\nneeds = [\"a\", \"ghost\"]\n\
             [[steps]]\nid = \"a\"\ntitle = \"t\"\n\
             [[steps]]\nid = \"b\"\ntitle = \"t\"\nneeds = [\"a\", \"a\"]\n"
        );
        let v = violations_for(&text, "f");
        assert!(has(&v, "steps.a.id", "duplicate"), "{v:?}");
        assert!(has(&v, "steps.a.needs", "ghost"), "{v:?}");
        assert!(has(&v, "steps.a.needs", "itself"), "{v:?}");
        assert!(has(&v, "steps.b.needs", "duplicate"), "{v:?}");
    }

    #[test]
    fn cycles_are_reported_with_their_path() {
        let text = format!(
            "{HEADER}\
             [[steps]]\nid = \"a\"\ntitle = \"t\"\nneeds = [\"c\"]\n\
             [[steps]]\nid = \"b\"\ntitle = \"t\"\nneeds = [\"a\"]\n\
             [[steps]]\nid = \"c\"\ntitle = \"t\"\nneeds = [\"b\"]\n"
        );
        let v = violations_for(&text, "f");
        assert!(has(&v, "steps", "cycle"), "{v:?}");
        assert!(
            v.iter().any(|v| v.message.contains("a")
                && v.message.contains("b")
                && v.message.contains("c")),
            "{v:?}"
        );
    }

    #[test]
    fn combination_rules_mirror_gc() {
        let check =
            "[steps.check]\nmax_attempts = 1\n[steps.check.check]\nmode = \"exec\"\npath = \"v.sh\"\n";
        let requires = "[requires]\nformula_compiler = \">=2.0.0\"\n";
        // check + retry
        let v = violations_for(
            &format!(
                "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n{check}[steps.retry]\nmax_attempts = 2\n"
            ),
            "f",
        );
        assert!(has(&v, "steps.a.check", "retry"), "{v:?}");
        // check + assignee
        let v = violations_for(
            &format!(
                "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\nassignee = \"dev\"\n{check}"
            ),
            "f",
        );
        assert!(has(&v, "steps.a.check", "assignee"), "{v:?}");
        // retry + on_complete
        let v = violations_for(
            &format!(
                "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.retry]\nmax_attempts = 2\n\
                 [steps.on_complete]\nfor_each = \"output.i\"\nbond = \"b\"\n"
            ),
            "f",
        );
        assert!(has(&v, "steps.a.retry", "on_complete"), "{v:?}");
    }

    #[test]
    fn timeout_requires_check() {
        let v = violations_for(
            &format!(
                "{HEADER}[requires]\nformula_compiler = \">=2.0.0\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntimeout = \"5m\"\n"
            ),
            "f",
        );
        assert!(has(&v, "steps.a.timeout", "requires `check`"), "{v:?}");
    }

    #[test]
    fn graph_only_constructs_require_the_explicit_declaration() {
        let check =
            "[steps.check]\nmax_attempts = 1\n[steps.check.check]\nmode = \"exec\"\npath = \"v.sh\"\n";
        let v = violations_for(
            &format!("{HEADER}[[steps]]\nid = \"a\"\ntitle = \"t\"\n{check}"),
            "f",
        );
        assert!(
            has(
                &v,
                "requires",
                "graph-only constructs must declare [requires] formula_compiler"
            ),
            "{v:?}"
        );
        // with the declaration the same formula is clean
        let v = violations_for(
            &format!(
                "{HEADER}[requires]\nformula_compiler = \">=2.0.0\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n{check}"
            ),
            "f",
        );
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn semver_comparator_is_validated_and_checked_against_capability() {
        let v = violations_for(
            &format!(
                "{HEADER}[requires]\nformula_compiler = \"not-a-version\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n"
            ),
            "f",
        );
        assert!(has(&v, "requires.formula_compiler", "semver comparator"), "{v:?}");
        let v = violations_for(
            &format!(
                "{HEADER}[requires]\nformula_compiler = \">=3.0.0\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n"
            ),
            "f",
        );
        assert!(has(&v, "requires.formula_compiler", "capability"), "{v:?}");
    }

    #[test]
    fn retry_defaults_and_on_complete_rules() {
        let requires = "[requires]\nformula_compiler = \">=2.0.0\"\n";
        // default on_exhausted = hard_fail
        let (raw, mut v) = walk(&format!(
            "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.retry]\nmax_attempts = 2\n"
        ));
        super::check(&raw, Some("f"), &mut v);
        assert!(v.is_empty(), "{v:?}");
        let formula = super::assemble(raw, String::new());
        assert_eq!(
            formula.steps[0].retry.as_ref().unwrap().on_exhausted,
            crate::formula::ast::Disposition::HardFail
        );
        // bad on_exhausted value
        let v = violations_for(
            &format!(
                "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.retry]\nmax_attempts = 2\non_exhausted = \"explode\"\n"
            ),
            "f",
        );
        assert!(has(&v, "steps.a.retry.on_exhausted", "hard_fail"), "{v:?}");
        // for_each must start with output.
        let v = violations_for(
            &format!(
                "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.on_complete]\nfor_each = \"items\"\nbond = \"b\"\n"
            ),
            "f",
        );
        assert!(has(&v, "steps.a.on_complete.for_each", "output."), "{v:?}");
    }
}

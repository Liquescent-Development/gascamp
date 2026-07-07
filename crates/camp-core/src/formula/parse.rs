//! Raw TOML walk for the camp formula subset (acceptance/rejection key
//! tables) and the duration grammar shared by `timeout` fields.

use std::path::PathBuf;
use std::time::Duration;

use toml::Value;

use crate::formula::ast::{Check, CheckMode, OnComplete, Violation};

pub(crate) struct RawFormula {
    pub name: Option<String>,
    pub description: Option<String>,
    pub formula_compiler: Option<String>,
    pub steps: Vec<RawStep>,
}

pub(crate) struct RawStep {
    pub index: usize,
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub needs: Vec<String>,
    pub assignee: Option<String>,
    pub timeout: Option<Duration>,
    pub check: Option<Check>,
    pub retry: Option<RawRetry>,
    pub on_complete: Option<OnComplete>,
    /// The keys were PRESENT in the TOML, even if their tables failed to
    /// parse — presence (not parse success) drives the S8/S9/S11 rules so
    /// a malformed table never mutes them (review finding 5).
    pub has_check: bool,
    pub has_retry: bool,
    pub has_on_complete: bool,
}

pub(crate) struct RawRetry {
    pub max_attempts: u32,
    pub on_exhausted: Option<String>,
}

/// Keys that exist in Gas City formula v2 but are outside camp's subset
/// (spec §8.2 "City-only in v1"; gc formula-spec-v2 §1.2/§1.3).
const CITY_ONLY_TOP: &[&str] = &[
    "advice",
    "catalog",
    "compose",
    "contract",
    "extends",
    "phase",
    "pointcuts",
    "pour",
    "template",
    "type",
    "vars",
];
const CITY_ONLY_STEP: &[&str] = &[
    "children",
    "condition",
    "depends_on",
    "description_file",
    "drain",
    "expand",
    "expand_vars",
    "gate",
    "loop",
    "metadata",
    "notes",
    "priority",
    "tags",
    "tally",
    "type",
    "waits_for",
];

const ACCEPTED_TOP: &[&str] = &["description", "formula", "requires", "steps"];
const ACCEPTED_STEP: &[&str] = &[
    "assignee",
    "check",
    "description",
    "id",
    "needs",
    "on_complete",
    "retry",
    "timeout",
    "title",
];

fn city_only(key: &str) -> Violation {
    Violation {
        construct: key.to_owned(),
        message: format!(
            "`{key}` is a Gas City-only construct; camp does not accept it — \
             run this formula in a Gas City (spec §8.2)"
        ),
    }
}

fn unknown(key: &str) -> Violation {
    Violation {
        construct: key.to_owned(),
        message: format!("unknown key `{key}`: camp formulas accept no unknown keys (spec §8.2)"),
    }
}

fn wrong_type(construct: &str, expected: &str) -> Violation {
    Violation {
        construct: construct.to_owned(),
        message: format!("`{construct}` must be {expected}"),
    }
}

/// Sorted keys of a table — deterministic violation order for tests/users.
fn sorted_keys(table: &toml::Table) -> Vec<&str> {
    let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

fn get_string(
    table: &toml::Table,
    key: &str,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Option<String> {
    match table.get(key) {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            out.push(wrong_type(construct, "a string"));
            None
        }
    }
}

fn get_string_array(
    table: &toml::Table,
    key: &str,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Vec<String> {
    match table.get(key) {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut result = Vec::new();
            for item in items {
                match item {
                    Value::String(s) => result.push(s.clone()),
                    _ => {
                        out.push(wrong_type(construct, "an array of strings"));
                        return Vec::new();
                    }
                }
            }
            result
        }
        Some(_) => {
            out.push(wrong_type(construct, "an array of strings"));
            Vec::new()
        }
    }
}

fn get_duration(
    table: &toml::Table,
    key: &str,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Option<Duration> {
    let text = get_string(table, key, construct, out)?;
    match parse_duration(&text) {
        Ok(d) => Some(d),
        Err(message) => {
            out.push(Violation {
                construct: construct.to_owned(),
                message,
            });
            None
        }
    }
}

fn get_max_attempts(table: &toml::Table, construct: &str, out: &mut Vec<Violation>) -> u32 {
    match table.get("max_attempts") {
        Some(Value::Integer(n)) if *n >= 1 => u32::try_from(*n).unwrap_or_else(|_| {
            out.push(wrong_type(construct, "an integer >= 1"));
            1
        }),
        Some(_) => {
            out.push(wrong_type(construct, "an integer >= 1"));
            1
        }
        None => {
            out.push(Violation {
                construct: construct.to_owned(),
                message: "`max_attempts` is required and must be >= 1".to_owned(),
            });
            1
        }
    }
}

/// Walk raw TOML text against camp's acceptance/rejection tables, collecting
/// every violation. Returns whatever structure could be extracted so the
/// semantic checks can still run and report *their* violations too. Every
/// salvage records its violation first — nothing is silenced.
pub(crate) fn walk(text: &str) -> (RawFormula, Vec<Violation>) {
    let mut out = Vec::new();
    let empty = RawFormula {
        name: None,
        description: None,
        formula_compiler: None,
        steps: Vec::new(),
    };
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            out.push(Violation {
                construct: "toml".to_owned(),
                message: e.to_string(),
            });
            return (empty, out);
        }
    };

    for key in sorted_keys(&table) {
        if ACCEPTED_TOP.contains(&key) {
            continue;
        } else if CITY_ONLY_TOP.contains(&key) {
            out.push(city_only(key));
        } else {
            out.push(unknown(key));
        }
    }

    let name = get_string(&table, "formula", "formula", &mut out);
    let description = get_string(&table, "description", "description", &mut out);
    let formula_compiler = walk_requires(&table, &mut out);
    let steps = walk_steps(&table, &mut out);

    (
        RawFormula {
            name,
            description,
            formula_compiler,
            steps,
        },
        out,
    )
}

fn walk_requires(table: &toml::Table, out: &mut Vec<Violation>) -> Option<String> {
    let requires = match table.get("requires") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type("requires", "a table"));
            return None;
        }
    };
    for key in sorted_keys(requires) {
        if key != "formula_compiler" {
            // Mirrors gc's one hard-key exception: unknown [requires] axes
            // fail even in gc (formula.requirement_unknown).
            out.push(Violation {
                construct: format!("requires.{key}"),
                message: format!(
                    "unknown formula requirement `{key}`; supported requirements: formula_compiler"
                ),
            });
        }
    }
    get_string(
        requires,
        "formula_compiler",
        "requires.formula_compiler",
        out,
    )
}

fn walk_steps(table: &toml::Table, out: &mut Vec<Violation>) -> Vec<RawStep> {
    let raw_steps = match table.get("steps") {
        None => return Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => {
            out.push(wrong_type("steps", "an array of tables"));
            return Vec::new();
        }
    };
    let mut steps = Vec::new();
    for (index, item) in raw_steps.iter().enumerate() {
        let Value::Table(step) = item else {
            out.push(wrong_type(&format!("steps[{index}]"), "a table"));
            continue;
        };
        steps.push(walk_step(index, step, out));
    }
    steps
}

fn walk_step(index: usize, step: &toml::Table, out: &mut Vec<Violation>) -> RawStep {
    let id = match step.get("id") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            out.push(wrong_type(&format!("steps[{index}].id"), "a string"));
            None
        }
        None => None,
    };
    // Location prefix: the id when we have one, else the index.
    let at = |field: &str| match &id {
        Some(id) => format!("steps.{id}.{field}"),
        None => format!("steps[{index}].{field}"),
    };

    for key in sorted_keys(step) {
        if ACCEPTED_STEP.contains(&key) {
            continue;
        } else if CITY_ONLY_STEP.contains(&key) {
            out.push(city_only(key));
        } else {
            out.push(unknown(key));
        }
    }

    let title = get_string(step, "title", &at("title"), out);
    let description = get_string(step, "description", &at("description"), out);
    let needs = get_string_array(step, "needs", &at("needs"), out);
    let assignee = get_string(step, "assignee", &at("assignee"), out);
    let timeout = get_duration(step, "timeout", &at("timeout"), out);
    let check = walk_check(step, &at("check"), out);
    let retry = walk_retry(step, &at("retry"), out);
    let on_complete = walk_on_complete(step, &at("on_complete"), out);

    RawStep {
        index,
        id,
        title,
        description,
        needs,
        assignee,
        timeout,
        check,
        retry,
        on_complete,
        has_check: step.contains_key("check"),
        has_retry: step.contains_key("retry"),
        has_on_complete: step.contains_key("on_complete"),
    }
}

fn walk_check(step: &toml::Table, construct: &str, out: &mut Vec<Violation>) -> Option<Check> {
    let check = match step.get("check") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    for key in sorted_keys(check) {
        if !["check", "max_attempts"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let max_attempts = get_max_attempts(check, &format!("{construct}.max_attempts"), out);
    let inner = match check.get("check") {
        Some(Value::Table(t)) => t,
        Some(_) | None => {
            out.push(Violation {
                construct: format!("{construct}.check"),
                message: "check requires an inner [steps.check.check] table with \
                          mode = \"exec\" and a path"
                    .to_owned(),
            });
            return None;
        }
    };
    for key in sorted_keys(inner) {
        if !["mode", "path", "timeout"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let mode_construct = format!("{construct}.check.mode");
    let mode = match get_string(inner, "mode", &mode_construct, out) {
        Some(m) if m == "exec" => CheckMode::Exec,
        Some(m) => {
            out.push(Violation {
                construct: mode_construct,
                message: format!("check mode {m:?} is not supported; only \"exec\" is (spec §8.2)"),
            });
            return None;
        }
        None => {
            out.push(Violation {
                construct: mode_construct,
                message: "check mode is required; only \"exec\" is supported".to_owned(),
            });
            return None;
        }
    };
    let path_construct = format!("{construct}.check.path");
    let path = match get_string(inner, "path", &path_construct, out) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        Some(_) | None => {
            out.push(Violation {
                construct: path_construct,
                message: "check path is required and must be non-empty".to_owned(),
            });
            return None;
        }
    };
    let timeout = get_duration(inner, "timeout", &format!("{construct}.check.timeout"), out);
    Some(Check {
        max_attempts,
        mode,
        path,
        timeout,
    })
}

fn walk_retry(step: &toml::Table, construct: &str, out: &mut Vec<Violation>) -> Option<RawRetry> {
    let retry = match step.get("retry") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    for key in sorted_keys(retry) {
        if !["max_attempts", "on_exhausted"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let max_attempts = get_max_attempts(retry, &format!("{construct}.max_attempts"), out);
    let on_exhausted = get_string(
        retry,
        "on_exhausted",
        &format!("{construct}.on_exhausted"),
        out,
    );
    Some(RawRetry {
        max_attempts,
        on_exhausted,
    })
}

fn walk_on_complete(
    step: &toml::Table,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Option<OnComplete> {
    let oc = match step.get("on_complete") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    for key in sorted_keys(oc) {
        if !["bond", "for_each", "parallel", "sequential", "vars"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let for_each = get_string(oc, "for_each", &format!("{construct}.for_each"), out);
    let bond = get_string(oc, "bond", &format!("{construct}.bond"), out);
    let parallel_key = match oc.get("parallel") {
        None => None,
        Some(Value::Boolean(b)) => Some(*b),
        Some(_) => {
            out.push(wrong_type(&format!("{construct}.parallel"), "a boolean"));
            None
        }
    };
    let sequential_key = match oc.get("sequential") {
        None => None,
        Some(Value::Boolean(b)) => Some(*b),
        Some(_) => {
            out.push(wrong_type(&format!("{construct}.sequential"), "a boolean"));
            None
        }
    };
    if parallel_key.is_some() && sequential_key.is_some() {
        out.push(Violation {
            construct: construct.to_owned(),
            message: "`parallel` and `sequential` are mutually exclusive (gc formula-spec-v2 §3.4)"
                .to_owned(),
        });
    }
    let parallel = match (parallel_key, sequential_key) {
        (Some(p), None) => p,
        (None, Some(s)) => !s,
        _ => true, // gc default: parallel
    };
    let mut vars = std::collections::BTreeMap::new();
    match oc.get("vars") {
        None => {}
        Some(Value::Table(t)) => {
            for (k, v) in t {
                match v {
                    Value::String(s) => {
                        vars.insert(k.clone(), s.clone());
                    }
                    _ => out.push(wrong_type(&format!("{construct}.vars.{k}"), "a string")),
                }
            }
        }
        Some(_) => out.push(wrong_type(
            &format!("{construct}.vars"),
            "a table of strings",
        )),
    }
    // for_each and bond must be set together (gc formula-spec-v2 §3.4);
    // reported here because it is a shape rule, not a semantic one.
    match (&for_each, &bond) {
        (Some(f), Some(b)) => Some(OnComplete {
            for_each: f.clone(),
            bond: b.clone(),
            vars,
            parallel,
        }),
        (None, None) => {
            out.push(Violation {
                construct: construct.to_owned(),
                message: "`for_each` and `bond` are required and must be set together \
                          (gc formula-spec-v2 §3.4)"
                    .to_owned(),
            });
            None
        }
        _ => {
            out.push(Violation {
                construct: construct.to_owned(),
                message: "`for_each` and `bond` must be set together (gc formula-spec-v2 §3.4)"
                    .to_owned(),
            });
            None
        }
    }
}

/// Parse a duration in camp's strict subset of Go `time.ParseDuration`
/// (repo invariant 6: everything camp accepts must parse in gc): one or
/// more `<positive integer><unit>` segments with units `ms`|`s`|`m`|`h`,
/// summing to > 0. E.g. "5m", "300s", "1h30m".
pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    const UNITS: &[(&str, u64)] = &[("ms", 1), ("s", 1000), ("m", 60_000), ("h", 3_600_000)];
    let mut rest = s;
    let mut total_ms: u64 = 0;
    if rest.is_empty() {
        return Err("empty duration".to_owned());
    }
    while !rest.is_empty() {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            return Err(format!(
                "invalid duration {s:?}: expected digits, found {rest:?}"
            ));
        }
        let (digits, tail) = rest.split_at(digits_end);
        let value: u64 = digits
            .parse()
            .map_err(|e| format!("invalid duration {s:?}: {e}"))?;
        // Longest-match the unit ("ms" before "m").
        let Some((unit, factor)) = UNITS
            .iter()
            .filter(|(u, _)| tail.starts_with(u))
            .max_by_key(|(u, _)| u.len())
        else {
            return Err(format!(
                "invalid duration {s:?}: expected a unit (ms|s|m|h) after {digits:?}"
            ));
        };
        total_ms = total_ms
            .checked_add(
                value
                    .checked_mul(*factor)
                    .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?,
            )
            .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
        rest = &tail[unit.len()..];
    }
    if total_ms == 0 {
        return Err(format!("invalid duration {s:?}: must be greater than zero"));
    }
    Ok(Duration::from_millis(total_ms))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn duration_grammar_is_a_strict_go_subset() {
        for (input, secs) in [
            ("5m", 300),
            ("2m", 120),
            ("300s", 300),
            ("1h30m", 5400),
            ("1h", 3600),
        ] {
            let d = parse_duration(input).unwrap();
            assert_eq!(d.as_secs(), secs, "{input}");
        }
        assert_eq!(
            parse_duration("1500ms").unwrap(),
            Duration::from_millis(1500)
        );
        for bad in [
            "", "5", "m", "-3s", "1.5h", "5d", "1h 30m", "0s", "0m", "s5", "5S",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    fn violations(text: &str) -> Vec<crate::formula::ast::Violation> {
        walk(text).1
    }

    fn constructs(text: &str) -> Vec<String> {
        violations(text).into_iter().map(|v| v.construct).collect()
    }

    const MINIMAL: &str =
        "formula = \"minimal\"\n\n[[steps]]\nid = \"only\"\ntitle = \"Do the thing\"\n";

    #[test]
    fn minimal_formula_walks_clean() {
        let (raw, v) = walk(MINIMAL);
        assert!(v.is_empty(), "{v:?}");
        assert_eq!(raw.name.as_deref(), Some("minimal"));
        assert_eq!(raw.steps.len(), 1);
        assert_eq!(raw.steps[0].id.as_deref(), Some("only"));
        assert_eq!(raw.steps[0].title.as_deref(), Some("Do the thing"));
    }

    #[test]
    fn every_city_only_key_is_rejected_by_name_with_a_city_pointer() {
        for key in [
            "extends",
            "vars",
            "type",
            "phase",
            "pour",
            "contract",
            "catalog",
            "template",
            "compose",
            "advice",
            "pointcuts",
        ] {
            let text =
                format!("{key} = 1\nformula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n");
            let v = violations(&text);
            assert!(
                v.iter()
                    .any(|v| v.construct == key && v.message.contains("Gas City")),
                "{key}: {v:?}"
            );
        }
        for key in [
            "drain",
            "gate",
            "loop",
            "expand",
            "expand_vars",
            "children",
            "waits_for",
            "condition",
            "tally",
            "metadata",
            "depends_on",
            "type",
            "priority",
            "tags",
            "description_file",
            "notes",
        ] {
            let text =
                format!("formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n{key} = 1\n");
            let v = violations(&text);
            assert!(
                v.iter()
                    .any(|v| v.construct == key && v.message.contains("Gas City")),
                "step {key}: {v:?}"
            );
        }
    }

    #[test]
    fn unknown_keys_are_rejected_everywhere_gc_would_silently_ignore_them() {
        // gc silently drops a `dependson` typo (formula-spec-v2 §1.3 note);
        // camp names it.
        let text = "formula = \"x\"\nbogus = 1\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ndependson = [\"b\"]\n";
        let c = constructs(text);
        assert!(c.contains(&"bogus".to_owned()), "{c:?}");
        assert!(c.contains(&"dependson".to_owned()), "{c:?}");
    }

    #[test]
    fn walk_collects_all_violations_not_just_the_first() {
        let text = "formula = \"x\"\nvars = {}\npour = true\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntags = [\"x\"]\n";
        let c = constructs(text);
        assert_eq!(
            c,
            vec!["pour", "vars", "tags"],
            "sorted top keys then steps: {c:?}"
        );
    }

    #[test]
    fn check_retry_and_on_complete_tables_parse_with_gc_shapes() {
        let text = r#"
formula = "shapes"
[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"
timeout = "5m"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "scripts/verify.sh"
timeout = "2m"

[[steps]]
id = "b"
title = "t"

[steps.retry]
max_attempts = 2
on_exhausted = "soft_fail"

[[steps]]
id = "c"
title = "t"

[steps.on_complete]
for_each = "output.items"
bond = "minimal"
sequential = true

[steps.on_complete.vars]
name = "{item.name}"
"#;
        let (raw, v) = walk(text);
        assert!(v.is_empty(), "{v:?}");
        let check = raw.steps[0].check.as_ref().unwrap();
        assert_eq!(check.max_attempts, 3);
        assert_eq!(check.path, std::path::PathBuf::from("scripts/verify.sh"));
        assert_eq!(check.timeout, Some(std::time::Duration::from_secs(120)));
        assert_eq!(
            raw.steps[0].timeout,
            Some(std::time::Duration::from_secs(300))
        );
        let retry = raw.steps[1].retry.as_ref().unwrap();
        assert_eq!(
            (retry.max_attempts, retry.on_exhausted.as_deref()),
            (2, Some("soft_fail"))
        );
        let oc = raw.steps[2].on_complete.as_ref().unwrap();
        assert_eq!(oc.for_each, "output.items");
        assert_eq!(oc.bond, "minimal");
        assert!(!oc.parallel, "sequential = true must flip the default");
        assert_eq!(oc.vars.get("name").map(String::as_str), Some("{item.name}"));
    }

    #[test]
    fn bad_types_and_bad_values_are_violations_with_locations() {
        let text = "formula = 3\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntimeout = \"eleven\"\ncheck = { max_attempts = 1, check = { mode = \"inference\", path = \"x\" } }\n";
        let v = violations(text);
        assert!(
            v.iter()
                .any(|v| v.construct == "formula" && v.message.contains("string")),
            "{v:?}"
        );
        assert!(v.iter().any(|v| v.construct == "steps.a.timeout"), "{v:?}");
        assert!(
            v.iter()
                .any(|v| v.construct == "steps.a.check.check.mode" && v.message.contains("exec")),
            "{v:?}"
        );
    }

    #[test]
    fn toml_syntax_error_is_a_single_violation() {
        let (_, v) = walk("formula = \"x\"\n[[steps\n");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].construct, "toml");
    }

    #[test]
    fn parallel_and_sequential_both_present_is_a_violation() {
        let text = "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\non_complete = { for_each = \"output.i\", bond = \"b\", parallel = true, sequential = true }\n";
        let v = violations(text);
        assert!(
            v.iter().any(|v| v.construct == "steps.a.on_complete"
                && v.message.contains("mutually exclusive")),
            "{v:?}"
        );
    }
}

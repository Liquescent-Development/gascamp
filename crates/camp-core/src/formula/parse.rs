//! Raw TOML walk for camp formulas: the key table (compat §4, [`crate::formula::keys`])
//! applied at every nesting site, plus the duration grammar shared by
//! `timeout` fields.
//!
//! The walk is ORIGIN-SCOPED (D2′). It reports three kinds of finding and it
//! never conflates them: **violations** (malformed), **refusals** (well-formed
//! Gas City that camp declines — §4 rule 1), and **ignored keys** (gc's dead
//! config, and unrecognised keys in an imported layer).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use toml::Value;

use crate::formula::ast::{Check, CheckMode, OnComplete, Refusal, Violation};
use crate::formula::keys::{self, Class, Origin, Site};

pub(crate) struct RawFormula {
    pub name: Option<String>,
    pub description: Option<String>,
    pub formula_compiler: Option<String>,
    /// `contract = "graph.v2"` — rung 2a. Satisfies S11's compiler
    /// declaration (master spec line 449) and gates RUNNABLE (D1).
    pub contract: Option<String>,
    /// The top-level `type` key — rung 2d. `Some("expansion")` means the
    /// formula supplies `template` steps for an `expand` rule and has no
    /// `steps` of its own (S3), and is never directly runnable (D1).
    pub kind: Option<String>,
    pub steps: Vec<RawStep>,
}

pub(crate) struct RawStep {
    pub index: usize,
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// Rung 2a. CONSUMED at compile (stage 4): the file's contents REPLACE
    /// `description`. 328 corpus uses, and the steps that carry one typically
    /// have no inline description at all — ignore it and the worker gets zero
    /// instructions.
    pub description_file: Option<String>,
    /// Rung 2a. gc's step metadata, carried VERBATIM onto the bead. This is
    /// where routing lives (`gc.run_target`, 327 uses) — it is not annotation.
    pub metadata: BTreeMap<String, String>,
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

/// Everything one walk found. Refusals and ignored keys are NOT violations and
/// are carried separately: a refusal is well-formed gc camp declines, and an
/// ignored key is a warning the operator sees but the formula survives.
pub(crate) struct Walked {
    pub raw: RawFormula,
    pub violations: Vec<Violation>,
    pub refusals: Vec<Refusal>,
    pub ignored: Vec<String>,
}

/// The walk's mutable accumulator. Bundled so the per-site key loop does not
/// take eight parameters.
struct Ctx {
    origin: Origin,
    violations: Vec<Violation>,
    refusals: Vec<Refusal>,
    ignored: Vec<String>,
}

impl Ctx {
    /// Classify every key of one table at one SITE (§4 trap 1 — the site, never
    /// the name, decides). `prefix` is the dotted location of the table itself
    /// (`""` at top level); `step_id` stamps step-scoped refusals so BD2 can
    /// discard them with their step.
    fn keys(&mut self, site: Site, table: &toml::Table, prefix: &str, step_id: Option<&str>) {
        for key in sorted_keys(table) {
            // The dotted location. Refusals always carry it, so a step-scoped
            // refusal can be read back to its step.
            let at = if prefix.is_empty() {
                key.to_owned()
            } else {
                format!("{prefix}.{key}")
            };
            // Violations keep review finding 3's convention: the BARE key at top
            // and step level, the dotted location inside a nested table.
            let violation_at = match site {
                Site::Top | Site::Step => key.to_owned(),
                _ => at.clone(),
            };
            // The value-aware layer runs FIRST: it can refuse a key `classify`
            // calls Accepted (a `gc.kind = "scope"` inside a step's metadata).
            let value = &table[key];
            if let Some(mut refusal) = keys::refuse(site, key, value, &at) {
                refusal.step = step_id.map(str::to_owned);
                self.refusals.push(refusal);
                continue;
            }
            match keys::classify(site, key) {
                // §4 trap 3: accepted by the table, not yet honoured by the
                // pipeline. Without this the key would compile to NOTHING,
                // silently, and every rung count would be a lie. The check is
                // SITE-AWARE (trap 1) — see `keys::is_unimplemented`.
                Class::Accepted if keys::is_unimplemented(site, key) => {
                    self.violations.push(Violation {
                        construct: violation_at,
                        message: format!(
                            "`{key}` is on camp's key table but the compiler does not honour \
                             it yet (compat phase 2 lands it at a later rung); camp will not \
                             load a formula whose semantics it would silently drop"
                        ),
                    });
                }
                Class::Accepted => {}
                // `refuse` handles every Refused key; reaching here means the
                // two tables disagree, which is a bug in `keys`, not in the
                // formula.
                Class::Refused => self.violations.push(Violation {
                    construct: violation_at,
                    message: format!(
                        "internal: `{key}` classifies as Refused but produced no refusal"
                    ),
                }),
                // gc's own dead config: ignored + warned in BOTH tiers. Refusing
                // a formula over a key that does nothing even in gc would cost
                // real coverage for nothing.
                Class::DeadInGc => self.ignored.push(format!(
                    "{at}: `{key}` is dead in Gas City (no runtime behavior there either) — ignored"
                )),
                Class::Annotation => {}
                // D2′ — the whole permissiveness rule, in one match arm.
                Class::Unknown => match self.origin {
                    Origin::Imported => self.ignored.push(format!(
                        "{at}: unknown key `{key}` in an imported pack — ignored"
                    )),
                    Origin::CampLocal => self.violations.push(unknown(&violation_at, key)),
                },
            }
        }
    }
}

/// `construct` is the full location (equal to `key` at top/step level,
/// dotted inside nested tables — review finding 3).
fn unknown(construct: &str, key: &str) -> Violation {
    Violation {
        construct: construct.to_owned(),
        message: format!(
            "unknown key `{key}`: camp's own formulas accept no unknown keys (compat §4, D2′ — \
             an imported pack would only be warned)"
        ),
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

/// A flat map of string values — `steps.<id>.metadata`. gc's metadata values
/// are strings; a non-string is a violation, never a silent drop.
fn get_string_map(
    table: &toml::Table,
    key: &str,
    construct: &str,
    out: &mut Vec<Violation>,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    match table.get(key) {
        None => {}
        Some(Value::Table(t)) => {
            for (k, v) in t {
                match v {
                    Value::String(s) => {
                        map.insert(k.clone(), s.clone());
                    }
                    _ => out.push(wrong_type(&format!("{construct}.{k}"), "a string")),
                }
            }
        }
        Some(_) => out.push(wrong_type(construct, "a table of strings")),
    }
    map
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

/// Walk raw TOML text against camp's key table at `origin`'s strictness (D2′),
/// collecting every violation, refusal and ignored key. Returns whatever
/// structure could be extracted so the semantic checks can still run and report
/// *their* findings too. Every salvage records its violation first — nothing is
/// silenced.
pub(crate) fn walk(text: &str, origin: Origin) -> Walked {
    let mut ctx = Ctx {
        origin,
        violations: Vec::new(),
        refusals: Vec::new(),
        ignored: Vec::new(),
    };
    let empty = RawFormula {
        name: None,
        description: None,
        formula_compiler: None,
        contract: None,
        kind: None,
        steps: Vec::new(),
    };
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            ctx.violations.push(Violation {
                construct: "toml".to_owned(),
                message: e.to_string(),
            });
            return Walked {
                raw: empty,
                violations: ctx.violations,
                refusals: ctx.refusals,
                ignored: ctx.ignored,
            };
        }
    };

    ctx.keys(Site::Top, &table, "", None);

    let name = get_string(&table, "formula", "formula", &mut ctx.violations);
    let description = get_string(&table, "description", "description", &mut ctx.violations);
    let contract = get_string(&table, "contract", "contract", &mut ctx.violations);
    let kind = get_string(&table, "type", "type", &mut ctx.violations);
    let formula_compiler = walk_requires(&table, &mut ctx.violations);
    let steps = walk_steps(&table, &mut ctx);

    Walked {
        raw: RawFormula {
            name,
            description,
            formula_compiler,
            contract,
            kind,
            steps,
        },
        violations: ctx.violations,
        refusals: ctx.refusals,
        ignored: ctx.ignored,
    }
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

fn walk_steps(table: &toml::Table, ctx: &mut Ctx) -> Vec<RawStep> {
    let raw_steps = match table.get("steps") {
        None => return Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => {
            ctx.violations
                .push(wrong_type("steps", "an array of tables"));
            return Vec::new();
        }
    };
    let mut steps = Vec::new();
    for (index, item) in raw_steps.iter().enumerate() {
        let Value::Table(step) = item else {
            ctx.violations
                .push(wrong_type(&format!("steps[{index}]"), "a table"));
            continue;
        };
        steps.push(walk_step(index, step, ctx));
    }
    steps
}

fn walk_step(index: usize, step: &toml::Table, ctx: &mut Ctx) -> RawStep {
    let id = match step.get("id") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            ctx.violations
                .push(wrong_type(&format!("steps[{index}].id"), "a string"));
            None
        }
        None => None,
    };
    // Location prefix: the id when we have one, else the index.
    let loc = match &id {
        Some(id) => format!("steps.{id}"),
        None => format!("steps[{index}]"),
    };
    let at = |field: &str| format!("{loc}.{field}");

    ctx.keys(Site::Step, step, &loc, id.as_deref());

    let out = &mut ctx.violations;
    let title = get_string(step, "title", &at("title"), out);
    let description = get_string(step, "description", &at("description"), out);
    let description_file = get_string(step, "description_file", &at("description_file"), out);
    let metadata = get_string_map(step, "metadata", &at("metadata"), out);
    let needs = get_string_array(step, "needs", &at("needs"), out);
    let assignee = get_string(step, "assignee", &at("assignee"), out);
    let timeout = get_duration(step, "timeout", &at("timeout"), out);
    let check = walk_check(step, &at("check"), ctx);
    let retry = walk_retry(step, &at("retry"), ctx);
    let on_complete = walk_on_complete(step, &at("on_complete"), ctx);

    RawStep {
        index,
        id,
        title,
        description,
        description_file,
        metadata,
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

fn walk_check(step: &toml::Table, construct: &str, ctx: &mut Ctx) -> Option<Check> {
    let check = match step.get("check") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            ctx.violations.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    ctx.keys(Site::Check, check, construct, None);
    let out = &mut ctx.violations;
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
    // §4 trap 1: `mode` here is the load-bearing exec mode, not gc's dead
    // top-level `mode`.
    ctx.keys(Site::CheckInner, inner, &format!("{construct}.check"), None);
    let out = &mut ctx.violations;
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

fn walk_retry(step: &toml::Table, construct: &str, ctx: &mut Ctx) -> Option<RawRetry> {
    let retry = match step.get("retry") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            ctx.violations.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    ctx.keys(Site::Retry, retry, construct, None);
    let out = &mut ctx.violations;
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

fn walk_on_complete(step: &toml::Table, construct: &str, ctx: &mut Ctx) -> Option<OnComplete> {
    let oc = match step.get("on_complete") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            ctx.violations.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    ctx.keys(Site::OnComplete, oc, construct, None);
    let out = &mut ctx.violations;
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
// The plan's test names shout the load-bearing word (IMPORTED vs CAMP_LOCAL,
// BOTH tiers). Keeping them is worth one lint allow.
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn tally_refusal_notes_that_gc_also_rejects_it() {
        // Review finding 8: pointing tally authors at a Gas City would be wrong
        // advice — gc formula-v2 hard-rejects [steps.tally] too. It is a
        // REFUSAL now, not a violation, and it carries its step (BD2).
        let text = "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntally = true\n";
        let tally = refusals(text)
            .into_iter()
            .find(|r| r.key == "tally")
            .expect("tally refusal");
        assert_eq!(tally.construct, "steps.a.tally");
        assert_eq!(tally.step.as_deref(), Some("a"));
        assert!(tally.reason.contains("removed"), "{}", tally.reason);
        assert!(tally.reason.contains("Gas City"), "{}", tally.reason);
    }

    #[test]
    fn nested_unknown_keys_carry_their_location() {
        // Review finding 3: an unknown key inside check/retry/on_complete
        // must report its full location, like every other violation.
        let text = r#"
formula = "x"

[[steps]]
id = "a"
title = "t"

[steps.check]
max_attempts = 1
retries = 2

[steps.check.check]
mode = "exec"
path = "v.sh"
shell = "bash"

[[steps]]
id = "b"
title = "t"

[steps.retry]
max_attempts = 1
backoff = "1s"

[[steps]]
id = "c"
title = "t"

[steps.on_complete]
for_each = "output.i"
bond = "m"
mode = "fanout"
"#;
        let c = constructs(text);
        for expected in [
            "steps.a.check.retries",
            "steps.a.check.check.shell",
            "steps.b.retry.backoff",
            "steps.c.on_complete.mode",
        ] {
            assert!(
                c.contains(&expected.to_owned()),
                "missing {expected}: {c:?}"
            );
        }
    }

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
        walk(text, Origin::CampLocal).violations
    }

    fn refusals(text: &str) -> Vec<Refusal> {
        walk(text, Origin::CampLocal).refusals
    }

    fn constructs(text: &str) -> Vec<String> {
        violations(text).into_iter().map(|v| v.construct).collect()
    }

    /// A step carrying one key, at the camp-local (strict) tier.
    fn step_key(key: &str) -> String {
        format!("formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n{key} = 1\n")
    }

    const MINIMAL: &str =
        "formula = \"minimal\"\n\n[[steps]]\nid = \"only\"\ntitle = \"Do the thing\"\n";

    #[test]
    fn minimal_formula_walks_clean() {
        let w = walk(MINIMAL, Origin::CampLocal);
        assert!(w.violations.is_empty(), "{:?}", w.violations);
        assert!(w.refusals.is_empty(), "{:?}", w.refusals);
        assert_eq!(w.raw.name.as_deref(), Some("minimal"));
        assert_eq!(w.raw.steps.len(), 1);
        assert_eq!(w.raw.steps[0].id.as_deref(), Some("only"));
        assert_eq!(w.raw.steps[0].title.as_deref(), Some("Do the thing"));
    }

    #[test]
    fn every_section_4_rule_1_key_is_refused_by_name() {
        // What camp REFUSES — real gc semantics camp does not implement. This
        // list used to hold `extends`, `vars`, `contract`, `drain`, `condition`,
        // `metadata` … — the constructs the corpus is BUILT from. Refusing them
        // is what cost 95 of 100 formulas; the ones left here are refused on
        // purpose (§4 rule 1) and carry 0 corpus uses each.
        for key in ["advice", "compose", "phase", "pointcuts", "pour"] {
            let text =
                format!("{key} = 1\nformula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n");
            let r = refusals(&text);
            assert!(
                r.iter().any(|r| r.key == key && r.step.is_none()),
                "top {key} must refuse, formula-scoped: {r:?}"
            );
        }
        for key in ["depends_on", "gate", "loop", "tally", "waits_for"] {
            let r = refusals(&step_key(key));
            assert!(
                r.iter()
                    .any(|r| r.key == key && r.step.as_deref() == Some("a")),
                "step {key} must refuse, STEP-scoped: {r:?}"
            );
        }
    }

    #[test]
    fn the_keys_the_corpus_is_built_from_are_no_longer_refused() {
        // The inverse of the rule-1 list, and the whole point of the phase: these
        // now compile (or, until their rung lands, fail as UNIMPLEMENTED — never
        // as a refusal, which would be permanent).
        for key in ["contract", "vars", "extends", "type", "template"] {
            let text =
                format!("{key} = 1\nformula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n");
            assert!(refusals(&text).is_empty(), "top {key} must not refuse");
        }
        for key in [
            "description_file",
            "metadata",
            "condition",
            "expand",
            "expand_vars",
            "children",
            "drain",
        ] {
            assert!(
                refusals(&step_key(key)).is_empty(),
                "step {key} must not refuse"
            );
        }
    }

    #[test]
    fn an_unknown_key_is_ignored_in_an_IMPORTED_layer_and_fatal_in_the_CAMP_LOCAL_one() {
        // D2′, the whole rule. gc silently drops a `dependson` typo
        // (formula-spec-v2 §1.3 note). Camp names it in the operator's OWN
        // formulas — where a typo is a bug — and merely warns about it in a
        // third-party pack it happens to import, where refusing would fail the
        // load over a key camp has never heard of.
        let text = "formula = \"x\"\nbogus = 1\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ndependson = [\"b\"]\n";

        let local = walk(text, Origin::CampLocal);
        let c: Vec<String> = local
            .violations
            .iter()
            .map(|v| v.construct.clone())
            .collect();
        assert!(c.contains(&"bogus".to_owned()), "{c:?}");
        assert!(c.contains(&"dependson".to_owned()), "{c:?}");

        let imported = walk(text, Origin::Imported);
        assert!(
            imported.violations.is_empty(),
            "an imported pack must not fail over an unknown key: {:?}",
            imported.violations
        );
        assert_eq!(imported.ignored.len(), 2, "{:?}", imported.ignored);
        assert!(
            imported.ignored.iter().any(|w| w.contains("bogus"))
                && imported.ignored.iter().any(|w| w.contains("dependson")),
            "the warning must NAME the key: {:?}",
            imported.ignored
        );
    }

    #[test]
    fn a_key_dead_in_gc_is_ignored_in_BOTH_tiers() {
        // gc's own dead config. Refusing a formula over a key that does nothing
        // even in gc would cost real corpus coverage for nothing. Note `mode`
        // and `single_lane` — DEAD here, LOAD-BEARING nested (§4 trap 1).
        for key in [
            "version",
            "target_required",
            "internal",
            "mode",
            "single_lane",
            "sling_container_mode",
        ] {
            let text =
                format!("{key} = 1\nformula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n");
            for origin in [Origin::CampLocal, Origin::Imported] {
                let w = walk(&text, origin);
                assert!(w.violations.is_empty(), "{key} @ {origin:?}: not fatal");
                assert!(w.refusals.is_empty(), "{key} @ {origin:?}: not a refusal");
                assert!(
                    w.ignored.iter().any(|s| s.contains(key)),
                    "{key} @ {origin:?} must WARN: {:?}",
                    w.ignored
                );
            }
        }
    }

    #[test]
    fn annotations_are_silent_in_both_tiers() {
        for (text, what) in [
            (
                "notes = \"x\"\ncatalog = \"c\"\nmetadata = { a = \"b\" }\nformula = \"x\"\n\
                 [[steps]]\nid = \"a\"\ntitle = \"t\"\n",
                "top",
            ),
            (
                "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\nnotes = \"n\"\n\
                 tags = [\"x\"]\npriority = 2\n",
                "step",
            ),
        ] {
            for origin in [Origin::CampLocal, Origin::Imported] {
                let w = walk(text, origin);
                assert!(
                    w.violations.is_empty(),
                    "{what} @ {origin:?}: {:?}",
                    w.violations
                );
                assert!(
                    w.refusals.is_empty(),
                    "{what} @ {origin:?}: {:?}",
                    w.refusals
                );
                assert!(
                    w.ignored.is_empty(),
                    "{what} @ {origin:?}: annotations are SILENT, not warned: {:?}",
                    w.ignored
                );
            }
        }
    }

    #[test]
    fn walk_collects_every_finding_and_sorts_them_into_the_right_bucket() {
        // One file, three verdicts — and the buckets are the point. `pour` is a
        // REFUSAL (§4 rule 1, permanent). `vars` is a VIOLATION only because its
        // rung has not landed (temporary — UNIMPLEMENTED). `tags` is an
        // ANNOTATION and is SILENT. Conflating any two of these is how the old
        // table refused 95 of the 100 corpus formulas.
        let text = "formula = \"x\"\nvars = {}\npour = true\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntags = [\"x\"]\n";
        let w = walk(text, Origin::CampLocal);

        let c: Vec<&str> = w.violations.iter().map(|v| v.construct.as_str()).collect();
        assert_eq!(c, vec!["vars"], "only the unimplemented rung key: {c:?}");

        let r: Vec<&str> = w.refusals.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(r, vec!["pour"], "{r:?}");

        assert!(w.ignored.is_empty(), "tags is silent: {:?}", w.ignored);
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
        let w = walk(text, Origin::CampLocal);
        assert!(w.violations.is_empty(), "{:?}", w.violations);
        let raw = w.raw;
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
        let v = walk("formula = \"x\"\n[[steps\n", Origin::CampLocal).violations;
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

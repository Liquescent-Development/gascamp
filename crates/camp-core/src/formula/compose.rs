//! The COMPILE stage of camp's formula pipeline — gc's real staging, in gc's
//! real order (compat §9, D5).
//!
//! ```text
//!   1. parse::walk(text, origin)                        the key table (§4)
//!   2. extends: merge the chain                         rung 2c
//!   3. expansion: template/expand/children,
//!      + the {target} family, + single-brace {name}     rung 2d
//!   4. description_file: inline, or gc's pointer prompt rung 2a
//!   5. condition: prune the step, its children AND
//!      ITS REFUSALS; drop dangling `needs`              rung 2b
//!   6. validate (S1..S18) + collect SURVIVING refusals
//!      + decide runnability
//! ```
//!
//! **`{{var}}` is NOT substituted here.** It survives compile verbatim — 561
//! corpus steps still carry one in their description afterwards, and 55 routes
//! are still `{{implementation_target}}`, exactly as in gc's own compiled
//! Recipe. Substitution happens at INSTANTIATION, in `cook` (F1).
//!
//! **Two grammars, two stages, and they are not interchangeable.** Single-brace
//! `{name}` resolves HERE, and ONLY inside expansion (`resolve_single_brace`).
//! Double-brace `{{name}}` resolves at cook (`cook::substitute_vars`). Applying
//! the single-brace pass globally silently corrupts 55 routes and 121
//! `{target}.*.md` asset paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::config::CampConfig;
use crate::error::CoreError;
use crate::formula::ast::{Formula, FormulaError, Refusal, Violation};
use crate::formula::layers::FormulaLayers;
use crate::formula::{parse, validate};

/// gc's `descriptionFileInlineMaxBytes` (`parser.go:27`).
const DESCRIPTION_FILE_INLINE_MAX_BYTES: usize = 4 * 1024;

/// One compiled formula, plus everything the operator needs to know about how
/// it got that way.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub formula: Formula,
    /// Keys camp ignored and WARNED about: gc's dead config, and (D2′)
    /// unrecognised keys in an imported pack.
    pub ignored_keys: Vec<String>,
    /// SURVIVING refusals only — a refusal on a step that condition-pruning
    /// dropped died with it (BD2).
    pub refusals: Vec<Refusal>,
    /// D1 — the formula COMPILES but cannot be run. `Some(why)` for the 19
    /// contractless and 14 expansion corpus formulas.
    pub not_runnable: Option<Refusal>,
}

impl Compiled {
    pub fn is_runnable(&self) -> bool {
        self.not_runnable.is_none()
    }
}

/// Compile one formula file through the layer stack.
///
/// `vars_override` exists because **gc's `Compile` takes vars**, and conditions
/// (and `{name}`) resolve at COMPILE: a sling-time `--var drain_policy=same-session`
/// must change what is pruned. `camp sling` has no `--var` flag today; the
/// parameter is threaded now and passed empty, so compat-3/4 can add the flag
/// without re-plumbing the compiler.
pub fn compile(
    layers: &FormulaLayers,
    cfg: &CampConfig,
    path: &Path,
    vars_override: &BTreeMap<String, String>,
) -> Result<Compiled, FormulaError> {
    let fail = |violations: Vec<Violation>| FormulaError {
        path: path.to_path_buf(),
        violations,
        refusals: Vec::new(),
    };
    let source = std::fs::read_to_string(path).map_err(|e| {
        fail(vec![Violation {
            construct: "file".to_owned(),
            message: format!("cannot read: {e}"),
        }])
    })?;

    // ---- stages 1 + 2: the key table (at this file's ORIGIN, D2′) and the
    // extends chain, merged deepest-ancestor-first.
    let mut visiting = Vec::new();
    let mut walked = match chain(layers, path, &source, &mut visiting) {
        Ok(w) => w,
        Err(e) => {
            return Err(FormulaError {
                path: path.to_path_buf(),
                violations: vec![Violation {
                    construct: "extends".to_owned(),
                    message: e.to_string(),
                }],
                refusals: Vec::new(),
            });
        }
    };

    let _ = cfg;

    // The merged var VALUES: the chain's declared defaults, with the caller's
    // overrides on top. Conditions are evaluated over these — never by text
    // substitution (that is `{{var}}`, and it happens at COOK).
    let vars = merge_vars(&walked.raw.vars, vars_override);

    // ---- stage 3: expansion, and the single-brace `{name}` grammar (rung 2d).
    let defined: BTreeMap<String, String> = vars
        .iter()
        .filter_map(|(k, v)| v.clone().map(|v| (k.clone(), v)))
        .collect();
    match expand_steps(
        layers,
        std::mem::take(&mut walked.raw.steps),
        &defined,
        1,
        &mut walked.violations,
        &mut walked.refusals,
        &mut walked.ignored,
    ) {
        Ok(steps) => walked.raw.steps = steps,
        Err(e) => walked.violations.push(Violation {
            construct: "expand".to_owned(),
            message: e.to_string(),
        }),
    }

    // ---- stage 4: description_file (rung 2a).
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    if let Err(e) = inline_description_files(layers, &mut walked.raw, base_dir, &vars) {
        walked.violations.push(Violation {
            construct: "description_file".to_owned(),
            message: e.to_string(),
        });
    }

    // ---- stage 5: condition pruning (rung 2b).
    prune_conditions(
        &mut walked.raw,
        &vars,
        &mut walked.violations,
        &mut walked.refusals,
    );

    // ---- stage 6: validate, collect surviving refusals, decide runnability.
    let stem = validate::formula_stem(path);
    validate::check(&walked.raw, stem, &mut walked.violations);

    if !walked.violations.is_empty() || !walked.refusals.is_empty() {
        return Err(FormulaError {
            path: path.to_path_buf(),
            violations: walked.violations,
            refusals: walked.refusals,
        });
    }

    // D1 (ruling E) — origin-scoped, exactly as D2′ scopes permissiveness.
    let origin = layers.origin_of(path);
    let not_runnable = validate::not_runnable(&walked.raw, origin).map(|reason| Refusal {
        construct: "formula".to_owned(),
        key: "contract".to_owned(),
        reason,
        step: None,
    });
    Ok(Compiled {
        formula: validate::assemble(walked.raw, source, vars),
        ignored_keys: walked.ignored,
        refusals: Vec::new(),
        not_runnable,
    })
}

/// Compile a formula by BARE NAME, through the layers.
pub fn compile_named(
    layers: &FormulaLayers,
    cfg: &CampConfig,
    name: &str,
    vars_override: &BTreeMap<String, String>,
) -> Result<Compiled, FormulaError> {
    let path = layers.formula_path(name).map_err(|e| FormulaError {
        path: std::path::PathBuf::from(name),
        violations: vec![Violation {
            construct: "formula".to_owned(),
            message: e.to_string(),
        }],
        refusals: Vec::new(),
    })?;
    compile(layers, cfg, &path, vars_override)
}

/// Stage 2 — `extends` (rung 2c), merged DEEPEST ANCESTOR FIRST.
///
/// §9: *a child seeds scalars; parents' steps **APPEND**; a child step whose `id`
/// matches a parent's **REPLACES IT WHOLE, IN PLACE, preserving position**. No
/// field-level merge.* Parents resolve by BARE NAME through the layers — §7.2 is
/// what puts `build-base` in them.
///
/// **This is why 2c is 49 and not 57.** Camp resolves `extends` here, at stage 2,
/// and validates the MERGED step list at stage 6 — so eight formulas that inherit
/// a late-rung key ONLY from a parent (7 inherit `drain`, 1 inherits
/// `expand`/`expand_vars`) are blocked until that parent's rung lands. gc
/// corroborates: the corpus AUTHORS 12 separate drain steps and gc COMPILES 19.
fn chain(
    layers: &FormulaLayers,
    path: &Path,
    source: &str,
    visiting: &mut Vec<String>,
) -> Result<parse::Walked, CoreError> {
    let origin = layers.origin_of(path);
    let mut me = parse::walk(source, origin);
    // Stamp each step with ITS OWN formula's directory, before any merge can move
    // it into a child. A non-asset `description_file` on an inherited step
    // resolves against the parent's dir, not the child's.
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    for step in &mut me.raw.steps {
        step.base_dir = Some(base.to_path_buf());
    }
    if me.raw.extends.is_empty() {
        return Ok(me);
    }
    let parents = std::mem::take(&mut me.raw.extends);

    // Deepest ancestor first: fold the parents together, then lay the child on
    // top of them.
    let mut acc: Option<parse::Walked> = None;
    for parent in &parents {
        if visiting.iter().any(|v| v == parent) {
            return Err(CoreError::Formula(format!(
                "extends cycle: {} -> {parent}",
                visiting.join(" -> ")
            )));
        }
        let parent_path = layers.formula_path(parent).map_err(|e| {
            CoreError::Formula(format!(
                "extends {parent:?}: {e} (a parent resolves by bare name through the formula \
                 layers)"
            ))
        })?;
        let parent_source = std::fs::read_to_string(&parent_path).map_err(|e| {
            CoreError::Formula(format!(
                "extends {parent:?}: cannot read {}: {e}",
                parent_path.display()
            ))
        })?;
        visiting.push(parent.clone());
        let walked = chain(layers, &parent_path, &parent_source, visiting)?;
        visiting.pop();
        acc = Some(match acc {
            None => walked,
            Some(lower) => overlay(lower, walked),
        });
    }
    Ok(match acc {
        None => me,
        Some(ancestors) => overlay(ancestors, me),
    })
}

/// Lay `child` over `parent`. Scalars: the child SEEDS them, so a child value
/// wins and an absent one INHERITS (gc `parser.go:305-312` — `contract` and
/// `requires` really do inherit).
fn overlay(parent: parse::Walked, child: parse::Walked) -> parse::Walked {
    let mut steps = parent.raw.steps;
    let mut replaced: BTreeSet<String> = BTreeSet::new();
    for step in child.raw.steps {
        let existing = step
            .id
            .as_deref()
            .and_then(|id| steps.iter().position(|s| s.id.as_deref() == Some(id)));
        match existing {
            // REPLACED WHOLE, IN PLACE, position preserved. No field-level merge:
            // a child step that omits `description` does NOT inherit the parent's.
            Some(at) => {
                if let Some(id) = &step.id {
                    replaced.insert(id.clone());
                }
                steps[at] = step;
            }
            // APPENDED.
            None => steps.push(step),
        }
    }

    // BD2's NEW failure mode: a refusal carried from a PARENT step that the child
    // REPLACES IN PLACE must die with the step it belonged to.
    let mut refusals: Vec<Refusal> = parent
        .refusals
        .into_iter()
        .filter(|r| match &r.step {
            Some(step) => !replaced.contains(step),
            None => true,
        })
        .collect();
    refusals.extend(child.refusals);

    // Vars: PARENT DEFAULTS FIRST, CHILD OVERRIDES WIN. Load-bearing —
    // `drain_policy = "separate"` is declared in gascity's `build-base`, not in
    // the children that depend on it.
    let mut vars = parent.raw.vars;
    vars.extend(child.raw.vars);

    // `template` inherits like `steps` do — an expansion formula can extend
    // another one.
    let template = if child.raw.template.is_empty() {
        parent.raw.template
    } else {
        child.raw.template
    };

    let mut violations = parent.violations;
    violations.extend(child.violations);
    let mut ignored = parent.ignored;
    ignored.extend(child.ignored);

    parse::Walked {
        raw: parse::RawFormula {
            // The CHILD's identity, always — a formula is named by its own file.
            name: child.raw.name,
            description: child.raw.description.or(parent.raw.description),
            formula_compiler: child.raw.formula_compiler.or(parent.raw.formula_compiler),
            contract: child.raw.contract.or(parent.raw.contract),
            kind: child.raw.kind.or(parent.raw.kind),
            vars,
            extends: Vec::new(),
            template,
            steps,
        },
        violations,
        refusals,
        ignored,
    }
}

/// gc's `DefaultMaxExpansionDepth`. Exceeding it is a HARD ERROR, never a
/// truncation.
const MAX_EXPANSION_DEPTH: usize = 5;

/// gc's COMPILE-STAGE grammar, and the second of camp's three substitution
/// functions. **Applied ONLY inside expansion — never as a global pass.**
///
/// Two passes, in gc's order (`expandStep`, `expand.go:255`):
/// 1. the `{target}` FAMILY — a fixed 4-token vocabulary (`{target}`,
///    `{target.id}`, `{target.title}`, `{target.description}`), a plain
///    `ReplaceAll` (`substituteTargetPlaceholders`, `expand.go:446-464`). 362 of
///    the corpus's 435 single-brace occurrences are this family. It is NOT the
///    var grammar: `{target.title}` resolves with no such var, and
///    `{target.bogus}` is left verbatim.
/// 2. the general single-brace var grammar (`\{(\w+)\}`, `range.go:32`). An
///    unknown token is LEFT VERBATIM (`range.go:103`).
///
/// # ⚠️ THE DOUBLE-BRACE GUARD — a DELIBERATE DIVERGENCE FROM gc (D7)
///
/// `\{(\w+)\}` matches the INNER `{x}` of an authored `{{x}}` at offset 1. gc's
/// `substituteVars` is a bare `ReplaceAllStringFunc` with no guard, so inside
/// `expandStep` it really does corrupt them: **52 measured sites across 49 steps
/// in 20 formulas** — `{{implementation_target}}` becomes
/// `{superpowers.implementer}`, an outer brace pair wrapped around a substituted
/// VALUE. There is no var named `superpowers.implementer`.
///
/// **gc's own residual CHECKER carries this guard** (`parser.go:664-672`:
/// `if start > 0 && s[start-1] == '{' { continue }`). Its authors knew about the
/// ambiguity, guarded the checker, and did not guard the mutator. It is a bug,
/// not a semantic, and **camp does not reproduce it.**
///
/// **What protects the other 55 routes is SCOPE, not binding.** `bmad-build`'s
/// `implementation_target` HAS a default, and its `{{implementation_target}}`
/// route survives compile anyway — because that step is not inside an expansion
/// template, so `expandStep` never reaches it. An implementer who believed
/// "binding protects" could reasonably apply this function GLOBALLY (the guard
/// makes it feel safe) and would then resolve `{ISSUE_NUM}` and
/// `{artifact_path_keys}` outside expansion, where gc leaves them verbatim.
/// **This function is called ONLY from the expansion stage.**
pub(crate) fn resolve_single_brace(
    text: &str,
    target: Option<&parse::RawStep>,
    vars: &BTreeMap<String, String>,
) -> String {
    let staged = match target {
        Some(t) => substitute_target_placeholders(text, t),
        None => text.to_owned(),
    };
    substitute_single_brace_vars(&staged, vars)
}

/// gc's `substituteTargetPlaceholders` — a FIXED vocabulary, a plain replace.
fn substitute_target_placeholders(text: &str, target: &parse::RawStep) -> String {
    let id = target.id.as_deref().unwrap_or_default();
    text.replace("{target.id}", id)
        .replace(
            "{target.title}",
            target.title.as_deref().unwrap_or_default(),
        )
        .replace(
            "{target.description}",
            target.description.as_deref().unwrap_or_default(),
        )
        .replace("{target}", id)
}

/// gc's `substituteVars` (`range.go:94`) — PLUS the double-brace guard its
/// mutator lacks. See [`resolve_single_brace`].
fn substitute_single_brace_vars(text: &str, vars: &BTreeMap<String, String>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            let ch = text[i..].chars().next().unwrap_or('{');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // ⚠️ THE GUARD (D7). `{{` opens a DOUBLE-brace token: it belongs to the
        // COOK grammar and must survive compile byte-for-byte. Copy it through
        // whole — never let the var regex see the inner `{x}`.
        if bytes.get(i + 1) == Some(&b'{') {
            match text[i + 2..].find("}}") {
                Some(end) => {
                    out.push_str(&text[i..i + 2 + end + 2]);
                    i += 2 + end + 2;
                }
                None => {
                    out.push_str(&text[i..]);
                    return out;
                }
            }
            continue;
        }
        // A single-brace `{word}`: `\w+` only, exactly gc's class.
        let rest = &text[i + 1..];
        let end = rest.find('}');
        match end {
            Some(end) => {
                let token = &rest[..end];
                let is_word =
                    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_');
                match vars.get(token).filter(|_| is_word) {
                    Some(value) => out.push_str(value),
                    // Unknown, or not `\w+` (e.g. `{target.bogus}`): VERBATIM.
                    None if is_word => {
                        out.push('{');
                        out.push_str(token);
                        out.push('}');
                    }
                    None => {
                        out.push('{');
                        i += 1;
                        continue;
                    }
                }
                i += 1 + end + 1;
            }
            None => {
                out.push_str(&text[i..]);
                return out;
            }
        }
    }
    out
}

/// Stage 3 — EXPANSION (rung 2d).
///
/// An `expand` rule names an EXPANSION FORMULA; the step carrying it is the
/// TARGET, and it is REPLACED by that formula's `template` steps, with the
/// expansion's own `[vars]` merged UNDER the rule's `expand_vars` overrides
/// (gc `ApplyExpansionsWithVars` / `mergeVars` / `resolveOverrideVars`).
fn expand_steps(
    layers: &FormulaLayers,
    steps: Vec<parse::RawStep>,
    parent_vars: &BTreeMap<String, String>,
    depth: usize,
    violations: &mut Vec<Violation>,
    refusals: &mut Vec<Refusal>,
    ignored: &mut Vec<String>,
) -> Result<Vec<parse::RawStep>, CoreError> {
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        let Some(expansion) = step.expand.clone() else {
            // Not a target. FLATTEN its children in place — camp's step list has
            // no nesting, where gc keeps children on the Step. The step comes
            // first, then its children, in order.
            flatten_into(step, &mut out);
            continue;
        };
        if depth > MAX_EXPANSION_DEPTH {
            return Err(CoreError::Formula(format!(
                "expansion depth limit exceeded: max {MAX_EXPANSION_DEPTH} levels (step {:?})",
                step.id.as_deref().unwrap_or("<unnamed>")
            )));
        }

        // Load the expansion formula THROUGH THE LAYERS (it merges its own
        // `extends` on the way).
        let path = layers
            .formula_path(&expansion)
            .map_err(|e| CoreError::Formula(format!("expand {expansion:?}: {e}")))?;
        let source = std::fs::read_to_string(&path).map_err(|e| {
            CoreError::Formula(format!(
                "expand {expansion:?}: cannot read {}: {e}",
                path.display()
            ))
        })?;
        let mut visiting = vec![expansion.clone()];
        let mut walked = chain(layers, &path, &source, &mut visiting)?;
        if walked.raw.template.is_empty() {
            return Err(CoreError::Formula(format!(
                "expand {expansion:?}: that formula declares no `[[template]]` steps — an \
                 expansion formula supplies template steps for an `expand` rule"
            )));
        }
        violations.append(&mut walked.violations);
        refusals.append(&mut walked.refusals);
        ignored.append(&mut walked.ignored);

        // vars = the expansion's OWN defaults, with the rule's overrides on top.
        // The override VALUES are themselves resolved against the PARENT's vars
        // first (gc `resolveOverrideVars`, expand.go:210-223).
        let mut vars: BTreeMap<String, String> = walked
            .raw
            .vars
            .iter()
            .filter_map(|(k, v)| v.clone().map(|v| (k.clone(), v)))
            .collect();
        for (k, v) in &step.expand_vars {
            let resolved = crate::formula::cook::substitute_vars(v, parent_vars);
            vars.insert(
                k.clone(),
                substitute_single_brace_vars(&resolved, parent_vars),
            );
        }

        // Substitute the template against the TARGET (this step) and those vars.
        let mut expanded = Vec::new();
        for tmpl in walked.raw.template {
            expanded.push(substitute_template_step(tmpl, &step, &vars));
        }
        // A template step may itself carry an `expand` — recurse, bounded.
        let expanded = expand_steps(
            layers,
            expanded,
            &vars,
            depth + 1,
            violations,
            refusals,
            ignored,
        )?;
        // The TARGET is REPLACED, in position, by the expansion.
        out.extend(expanded);
    }
    Ok(out)
}

/// Flatten a step's `children` into the flat list, PRESERVING POSITION: the step
/// first, then its children in order, recursively. camp's step list has no
/// nesting, where gc keeps children on the Step.
fn flatten_into(mut step: parse::RawStep, out: &mut Vec<parse::RawStep>) {
    let children = std::mem::take(&mut step.children);
    out.push(step);
    for child in children {
        flatten_into(child, out);
    }
}

/// Apply gc's `expandStep` field list to one template step.
///
/// **`description_file` is NOT in it, and neither is `condition`** — the two
/// exemptions, both load-bearing (D5):
/// * 121 corpus asset files are named, on disk, literally `{target}.*.md`, and
///   130 `description_file` values carry the braces. Substituting there breaks
///   every one of them.
/// * gc exempts `Condition` explicitly (`expand.go:272`) with a comment naming
///   this exact bug. All four `{{review_mode}} != report` conditions live on the
///   `template/children` tree — inside `expandStep`'s reach. Substitute them and
///   `{{review_mode}} != report` becomes `{report} != report`, which
///   `eval_condition` REJECTS, and the four code-review formulas stop loading:
///   the ceiling is no longer 95.
fn substitute_template_step(
    mut tmpl: parse::RawStep,
    target: &parse::RawStep,
    vars: &BTreeMap<String, String>,
) -> parse::RawStep {
    let sub = |s: &str| resolve_single_brace(s, Some(target), vars);
    tmpl.id = tmpl.id.as_deref().map(&sub);
    tmpl.title = tmpl.title.as_deref().map(&sub);
    tmpl.description = tmpl.description.as_deref().map(&sub);
    tmpl.assignee = tmpl
        .assignee
        .as_deref()
        // gc: Assignee gets the VAR pass only, no target family.
        .map(|s| substitute_single_brace_vars(s, vars));
    tmpl.needs = tmpl.needs.iter().map(|n| sub(n)).collect();
    for value in tmpl.metadata.values_mut() {
        *value = sub(value);
    }
    for value in tmpl.expand_vars.values_mut() {
        *value = sub(value);
    }
    tmpl.expand = tmpl.expand.as_deref().map(&sub);
    if let Some(check) = &mut tmpl.check {
        check.path = std::path::PathBuf::from(sub(&check.path.to_string_lossy()));
    }
    // EXEMPT: `condition` gets the target family and NOTHING else.
    tmpl.condition = tmpl
        .condition
        .as_deref()
        .map(|c| substitute_target_placeholders(c, target));
    // EXEMPT ENTIRELY: `description_file` is never substituted.
    tmpl.children = tmpl
        .children
        .into_iter()
        .map(|c| substitute_template_step(c, target, vars))
        .collect();
    tmpl
}

/// The merged var VALUES. Declared defaults first, the caller's overrides on
/// top. A var declared with no default is DECLARED BUT UNDEFINED: it keeps its
/// name (gc's oversize prompt lists every declared name) and contributes no
/// value, so its `{{placeholder}}` survives to the worker verbatim.
pub(crate) fn merge_vars(
    declared: &BTreeMap<String, Option<String>>,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, Option<String>> {
    let mut out: BTreeMap<String, Option<String>> = declared.clone();
    for (k, v) in overrides {
        out.insert(k.clone(), Some(v.clone()));
    }
    out
}

/// §9's condition grammar: `==` and `!=` ONLY, LHS a single `{{var}}`.
///
/// **Evaluated over the merged var VALUES, never by text substitution.** That
/// distinction is the whole of D5: `{{var}}` is not substituted at compile, so a
/// condition can only be decided by LOOKING UP the var — and an undefined var
/// equals nothing.
///
/// The RHS is an unquoted bare word in every corpus use; a quoted one is accepted
/// too. Measured: 4 distinct conditions, 29 uses.
pub(crate) fn eval_condition(
    expr: &str,
    vars: &BTreeMap<String, Option<String>>,
) -> Result<bool, String> {
    let (lhs, op, rhs) = match expr.split_once("==") {
        Some((l, r)) => (l, Op::Eq, r),
        None => match expr.split_once("!=") {
            Some((l, r)) => (l, Op::Ne, r),
            None => {
                return Err(format!(
                    "condition {expr:?} is outside camp's subset: only `{{{{var}}}} == value` \
                     and `{{{{var}}}} != value` are supported (compat §9)"
                ));
            }
        },
    };
    let lhs = lhs.trim();
    let name = lhs
        .strip_prefix("{{")
        .and_then(|s| s.strip_suffix("}}"))
        .map(str::trim)
        .ok_or_else(|| {
            format!(
                "condition {expr:?}: the left-hand side must be a single `{{{{var}}}}`, not {lhs:?}"
            )
        })?;
    let rhs = rhs.trim().trim_matches('"').trim_matches('\'');

    // An UNDEFINED var equals nothing. `==` is therefore false (the step prunes)
    // and `!=` is true (it stays).
    let actual = vars.get(name).and_then(Option::as_deref);
    let equal = actual == Some(rhs);
    Ok(match op {
        Op::Eq => equal,
        Op::Ne => !equal,
    })
}

enum Op {
    Eq,
    Ne,
}

/// Stage 5 — condition pruning.
///
/// **A pruned step takes its REFUSALS with it (BD2), and that is what separates a
/// ceiling of 95 from one of 76.** 19 corpus formulas carry a CONDITIONAL
/// shared-drain arm — `context = "shared"`, which camp refuses. gc prunes those
/// arms under the default `drain_policy = "separate"` (13 authored shared drains
/// compile to 1). If camp collected the refusal at parse and never re-filtered
/// it, all 19 would refuse — taking `bmad-build`, `gstack-build` and
/// `compound-build` with them.
fn prune_conditions(
    raw: &mut parse::RawFormula,
    vars: &BTreeMap<String, Option<String>>,
    violations: &mut Vec<Violation>,
    refusals: &mut Vec<Refusal>,
) {
    let mut kept: Vec<parse::RawStep> = Vec::new();
    let mut pruned: BTreeSet<String> = BTreeSet::new();
    for step in std::mem::take(&mut raw.steps) {
        let Some(expr) = step.condition.clone() else {
            kept.push(step);
            continue;
        };
        match eval_condition(&expr, vars) {
            Ok(true) => kept.push(step),
            Ok(false) => {
                if let Some(id) = &step.id {
                    pruned.insert(id.clone());
                }
            }
            Err(message) => {
                violations.push(Violation {
                    construct: match &step.id {
                        Some(id) => format!("steps.{id}.condition"),
                        None => format!("steps[{}].condition", step.index),
                    },
                    message,
                });
                kept.push(step);
            }
        }
    }
    raw.steps = kept;

    // The refusals of a pruned step die WITH it.
    refusals.retain(|r| match &r.step {
        Some(step) => !pruned.contains(step),
        None => true,
    });

    // Dangling `needs` are DROPPED, silently — a surviving step that still needed
    // a pruned one would never dispatch and the run would dead-end (§9).
    for step in &mut raw.steps {
        step.needs.retain(|n| !pruned.contains(n));
    }
}

/// Stage 4 — `description_file`: the file's CONTENTS REPLACE the step's
/// description, and the key is consumed (gc `parser.go:808-828`).
///
/// The steps that carry one typically have NO inline description. Ignore the key
/// and the worker gets zero instructions — which is why 328 corpus uses make this
/// the highest-value key on rung 2a.
fn inline_description_files(
    layers: &FormulaLayers,
    raw: &mut parse::RawFormula,
    base_dir: &Path,
    vars: &BTreeMap<String, Option<String>>,
) -> Result<(), CoreError> {
    for step in &mut raw.steps {
        let Some(rel) = step.description_file.clone() else {
            continue;
        };
        // The step's OWN formula's directory — see `RawStep::base_dir`.
        let base = step
            .base_dir
            .clone()
            .unwrap_or_else(|| base_dir.to_path_buf());
        let resolved = layers.asset_path(&rel, &base)?;
        let bytes = std::fs::read(&resolved).map_err(|e| {
            CoreError::Formula(format!(
                "description_file {rel:?}: cannot read {}: {e}",
                resolved.display()
            ))
        })?;
        step.description = Some(if bytes.len() > DESCRIPTION_FILE_INLINE_MAX_BYTES {
            pointer_prompt(&rel, &resolved, bytes.len(), vars)
        } else {
            String::from_utf8(bytes).map_err(|e| {
                CoreError::Formula(format!(
                    "description_file {rel:?}: {} is not UTF-8: {e}",
                    resolved.display()
                ))
            })?
        });
        // Consumed: it must not survive into the compiled formula.
        step.description_file = None;
    }
    Ok(())
}

/// gc's `descriptionFileReferenceDescription` (`parser.go:977-1005`), BYTE FOR
/// BYTE. A file over 4 KiB is not inlined — the bead gets a pointer to it.
///
/// This is transcribed, not paraphrased, and `ci/gc-compat/differential.py`
/// diffs its sha256 against gc's own output: a mis-typed paragraph here is a
/// divergence no camp-only test could ever see.
///
/// Its `## Formula Variables` block deliberately emits `name="{{name}}"` lines.
/// They are NOT a bug — they resolve at COOK, which is exactly what D5 does.
fn pointer_prompt(
    raw_path: &str,
    resolved: &Path,
    size: usize,
    vars: &BTreeMap<String, Option<String>>,
) -> String {
    let mut b = String::new();
    b.push_str("# External Prompt Required\n\n");
    b.push_str("This bead still follows the normal runtime and lifecycle protocol from your startup prompt and current agent prompt, including claiming work, honoring result contracts, checking for follow-on work, and draining only when appropriate.\n\n");
    b.push_str("In addition to that protocol, this bead's task-specific instructions come from a formula `description_file` that is too large to inline safely into bead storage.\n\n");
    b.push_str("Before you start the task-specific work, you MUST read the file below and treat it as the task prompt for this bead. Do not proceed from memory, ambient skills, or prior workflow knowledge until you have read it.\n\n");
    b.push_str(&format!(
        "- Resolved prompt file: `{}`\n",
        resolved.display()
    ));
    b.push_str(&format!(
        "- Original formula description_file: `{raw_path}`\n"
    ));
    b.push_str(&format!("- Prompt file size: {size} bytes\n\n"));
    b.push_str("Treat the file contents as the authoritative task prompt for this bead. It augments the startup/runtime protocol; it does not replace the startup prompt, the current agent prompt, or any bead lifecycle/result-contract instructions already given to you.\n");
    b.push_str("Follow the section matching this bead's `gc.step_id` metadata and title, plus any result, closure, lifecycle, or post-close contract sections in that file.\n");

    // gc sorts the var names (`slices.Sort`); a BTreeMap already is. It lists
    // EVERY DECLARED name — including the ones with no default, whose
    // `{{placeholder}}` will still be unresolved when the worker reads it.
    let keys: BTreeSet<&String> = vars.keys().collect();
    if !keys.is_empty() {
        b.push_str("\n## Formula Variables\n\n");
        b.push_str("Use these resolved formula values when interpreting `{{...}}` placeholders in the prompt file:\n\n");
        b.push_str("```bash\n");
        for name in keys {
            b.push_str(&format!("{name}=\"{{{{{name}}}}}\"\n"));
        }
        b.push_str("```\n");
    }
    b
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_prompt_is_gcs_text_byte_for_byte() {
        let vars = BTreeMap::from([
            ("kind".to_owned(), Some("build".to_owned())),
            // Declared with NO default: it still appears in the block.
            ("agent".to_owned(), None),
        ]);
        let text = pointer_prompt("../assets/x.md", Path::new("/p/assets/x.md"), 5000, &vars);
        assert!(text.starts_with("# External Prompt Required\n\n"), "{text}");
        assert!(text.contains("- Resolved prompt file: `/p/assets/x.md`\n"));
        assert!(text.contains("- Original formula description_file: `../assets/x.md`\n"));
        assert!(text.contains("- Prompt file size: 5000 bytes\n\n"));
        // The var block, SORTED, and its `{{name}}` lines survive compile on
        // purpose: they resolve at COOK (D5).
        assert!(text.contains("## Formula Variables"));
        let bash = text.split("```bash\n").nth(1).unwrap();
        assert!(
            bash.starts_with("agent=\"{{agent}}\"\nkind=\"{{kind}}\"\n"),
            "sorted, and still templated: {bash}"
        );
    }

    #[test]
    fn no_vars_means_no_variables_block() {
        let text = pointer_prompt(
            "../assets/x.md",
            Path::new("/p/x.md"),
            5000,
            &BTreeMap::new(),
        );
        assert!(!text.contains("## Formula Variables"), "{text}");
        // gc's builder ends on the "Follow the section" line.
        assert!(text.ends_with("in that file.\n"), "{text}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[allow(non_snake_case)]
mod single_brace_tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn resolving_single_brace_leaves_double_brace_untouched() {
        // ⭐ D7 — camp is CORRECT where gc is BUGGY. This is the guard gc's own
        // residual CHECKER carries (parser.go:664-672) and its MUTATOR lacks.
        //
        // DO NOT DELETE. gc corrupts 52 sites across 20 formulas exactly here, and
        // `differential.py` excludes those sites so they cannot be "fixed" back
        // into a bug.
        let v = vars(&[("x", "RESOLVED")]);
        // x IS BOUND — and the double-brace token still survives BYTE-IDENTICAL.
        // Binding is not what protects it; the GUARD is.
        assert_eq!(resolve_single_brace("{{x}}", None, &v), "{{x}}");
        assert_eq!(resolve_single_brace("{x}", None, &v), "RESOLVED");
        // gc would produce "{RESOLVED}" for the first. Camp does not.
        assert_eq!(
            resolve_single_brace("route: {{x}} and {x}", None, &v),
            "route: {{x}} and RESOLVED"
        );
    }

    #[test]
    fn the_target_family_is_a_fixed_vocabulary_not_the_var_grammar() {
        let target = parse::RawStep {
            index: 0,
            id: Some("review".into()),
            title: Some("Review it".into()),
            description: Some("body".into()),
            description_file: None,
            metadata: BTreeMap::new(),
            condition: None,
            base_dir: None,
            expand: None,
            expand_vars: BTreeMap::new(),
            children: Vec::new(),
            drain: None,
            needs: Vec::new(),
            assignee: None,
            timeout: None,
            check: None,
            retry: None,
            on_complete: None,
            has_check: false,
            has_retry: false,
            has_on_complete: false,
            has_drain: false,
        };
        let empty = BTreeMap::new();
        // Resolves with NO such var — it is a plain ReplaceAll over a fixed
        // 4-token vocabulary, not the var grammar. 362 of the corpus's 435
        // single-brace occurrences are this family.
        assert_eq!(
            resolve_single_brace("{target}.blind-hunter", Some(&target), &empty),
            "review.blind-hunter"
        );
        assert_eq!(
            resolve_single_brace("{target.title}", Some(&target), &empty),
            "Review it"
        );
        // Not in the vocabulary ⇒ LEFT VERBATIM (and `target.bogus` is not `\w+`
        // either, so the var pass cannot touch it).
        assert_eq!(
            resolve_single_brace("{target.bogus}", Some(&target), &empty),
            "{target.bogus}"
        );
    }

    #[test]
    fn an_unknown_single_brace_token_is_left_verbatim() {
        // gc range.go:103. `{GC_PACK_DIR}` in prose survives.
        let v = vars(&[("x", "X")]);
        assert_eq!(
            resolve_single_brace("see {GC_PACK_DIR}/x", None, &v),
            "see {GC_PACK_DIR}/x"
        );
        assert_eq!(resolve_single_brace("{ISSUE_NUM}", None, &v), "{ISSUE_NUM}");
    }

    #[test]
    fn a_lone_or_unterminated_brace_is_carried_through() {
        let v = vars(&[("x", "X")]);
        assert_eq!(resolve_single_brace("a { b", None, &v), "a { b");
        assert_eq!(resolve_single_brace("{{x", None, &v), "{{x");
        assert_eq!(resolve_single_brace("100% {", None, &v), "100% {");
    }
}

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

    // ---- stage 1: the key table, at this file's ORIGIN (D2′).
    let origin = layers.origin_of(path);
    let mut walked = parse::walk(&source, origin);

    // ---- stage 2: extends (rung 2c). Identity until Task 6.
    // ---- stage 3: expansion + {name} (rung 2d). Identity until Task 7.
    // ---- stage 5: condition pruning (rung 2b). Identity until Task 5.
    //
    // Until those rungs land, `keys::UNIMPLEMENTED` makes any formula that USES
    // them a hard violation, so an identity stub here can never silently drop a
    // construct (§4 trap 3). That is the whole reason UNIMPLEMENTED exists, and
    // it is deleted the moment the last rung lands.
    let _ = vars_override;
    let _ = cfg;

    // ---- stage 4: description_file (rung 2a).
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let vars: BTreeMap<String, String> = BTreeMap::new();
    if let Err(e) = inline_description_files(layers, &mut walked.raw, base_dir, &vars) {
        walked.violations.push(Violation {
            construct: "description_file".to_owned(),
            message: e.to_string(),
        });
    }

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

    let not_runnable = validate::not_runnable(&walked.raw).map(|reason| Refusal {
        construct: "formula".to_owned(),
        key: "contract".to_owned(),
        reason,
        step: None,
    });
    Ok(Compiled {
        formula: validate::assemble(walked.raw, source),
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
    vars: &BTreeMap<String, String>,
) -> Result<(), CoreError> {
    for step in &mut raw.steps {
        let Some(rel) = step.description_file.clone() else {
            continue;
        };
        let resolved = layers.asset_path(&rel, base_dir)?;
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
    vars: &BTreeMap<String, String>,
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

    // gc sorts the var names (`slices.Sort`); a BTreeMap already is.
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
            ("kind".to_owned(), "build".to_owned()),
            ("agent".to_owned(), "dev".to_owned()),
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

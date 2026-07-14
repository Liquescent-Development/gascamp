//! The camp formula compiler (spec §8.2, compat spec §4/§9).
//!
//! Camp is a PERMISSIVE, LAYERED reader of Gas City formula-v2 files, not a
//! strict subset validator. Repo invariant 6 still binds — every formula camp
//! *accepts* is a valid gc file — but the converse was never true and pretending
//! it was cost 95 of the 100 real corpus formulas.
//!
//! Three verdicts, and which one a key gets depends on WHERE it sits, WHAT its
//! value is, and WHERE THE FILE CAME FROM (compat §4, [`keys`]):
//!
//! * **Refused** — real gc semantics camp does not implement. Named, loudly,
//!   never approximated (§4 rule 1).
//! * **Ignored + warned** — gc's own dead config, and (D2′) any unrecognised key
//!   in an IMPORTED pack layer. A third-party pack must not fail to load over a
//!   key camp has never heard of.
//! * **Hard error** — an unrecognised key in the operator's OWN
//!   `<root>/formulas/`, where a typo is a bug camp must name.
//!
//! D2′ INVERTS this module's original sentence ("camp accepts no unknown keys,
//! where gc silently ignores them"): camp's strictness is scoped by
//! [`keys::Origin`], because the two tiers are asking different questions.

pub mod ast;
pub mod compose;
pub(crate) mod cook;
pub mod drain;
pub mod keys;
pub mod layers;
mod parse;
pub mod runtime;
mod validate;

pub use ast::{
    Check, CheckMode, Disposition, Formula, FormulaError, OnComplete, Refusal, Requires, Step,
    Violation,
};
pub use compose::{Compiled, compile, compile_named};
pub use cook::{CookOptions, CookedRun, RECIPE_VERSION, RUN_TARGET, cook, cook_with, instantiate};
pub use drain::{Drain, DrainContext, DrainItem, MemberAccess, OnItemFailure};
pub use keys::{Class, Origin, Site};
pub use layers::FormulaLayers;
pub use validate::{FORMULA_COMPILER_CAPABILITY, formula_stem};

use std::path::Path;

/// Parse and validate one formula file as a CAMP-LOCAL formula (D2′: the strict
/// tier — an unrecognised key here is a hard error). On failure the error lists
/// ALL violations and ALL refusals, never just the first.
pub fn parse_and_validate(path: &Path) -> Result<Formula, FormulaError> {
    let source = std::fs::read_to_string(path).map_err(|e| FormulaError {
        path: path.to_path_buf(),
        violations: vec![Violation {
            construct: "file".to_owned(),
            message: format!("cannot read: {e}"),
        }],
        refusals: Vec::new(),
    })?;
    let stem = formula_stem(path);
    let mut walked = parse::walk(&source, Origin::CampLocal);
    validate::check(&walked.raw, stem, &mut walked.violations);
    // In the CAMP-LOCAL tier an unrecognised key is already a violation, so
    // `ignored` here holds only gc's dead config — real, warnable, and not
    // fatal. `parse_and_validate` has no channel to surface it; the layered
    // entry point (`compose::compile`) carries it out as `Compiled::ignored_keys`
    // and `camp doctor --formula` prints it.
    let _dead_gc_keys = walked.ignored;
    if walked.violations.is_empty() && walked.refusals.is_empty() {
        let vars = walked.raw.vars.clone();
        Ok(validate::assemble(walked.raw, source, vars))
    } else {
        Err(FormulaError {
            path: path.to_path_buf(),
            violations: walked.violations,
            refusals: walked.refusals,
        })
    }
}

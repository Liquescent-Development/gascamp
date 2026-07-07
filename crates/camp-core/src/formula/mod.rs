//! The camp formula subset compiler (spec §8.2). Every valid camp formula
//! is a valid Gas City formula-v2 file (repo invariant 6): camp adopts
//! constructs with gc's exact syntax and semantics or not at all, and camp
//! is strictly *tighter* — it rejects every city-only construct by name and
//! accepts no unknown keys, where gc silently ignores them.

pub mod ast;
mod cook;
mod parse;
mod validate;

pub use ast::{
    Check, CheckMode, Disposition, Formula, FormulaError, OnComplete, Requires, Step, Violation,
};
pub use cook::{CookedRun, cook};
pub use validate::FORMULA_COMPILER_CAPABILITY;

use std::path::Path;

/// Parse and validate one formula file against the camp subset (spec §8.2).
/// On failure the error lists ALL violations, not just the first. The file
/// stem is the enforced formula name.
pub fn parse_and_validate(path: &Path) -> Result<Formula, FormulaError> {
    let source = std::fs::read_to_string(path).map_err(|e| FormulaError {
        path: path.to_path_buf(),
        violations: vec![Violation {
            construct: "file".to_owned(),
            message: format!("cannot read: {e}"),
        }],
    })?;
    let stem = path.file_stem().and_then(|s| s.to_str());
    let (raw, mut violations) = parse::walk(&source);
    validate::check(&raw, stem, &mut violations);
    if violations.is_empty() {
        Ok(validate::assemble(raw, source))
    } else {
        Err(FormulaError {
            path: path.to_path_buf(),
            violations,
        })
    }
}

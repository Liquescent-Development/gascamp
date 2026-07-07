//! The camp formula subset compiler (spec §8.2). Every valid camp formula
//! is a valid Gas City formula-v2 file (repo invariant 6): camp adopts
//! constructs with gc's exact syntax and semantics or not at all, and camp
//! is strictly *tighter* — it rejects every city-only construct by name and
//! accepts no unknown keys, where gc silently ignores them.

pub mod ast;

pub use ast::{
    Check, CheckMode, Disposition, Formula, FormulaError, OnComplete, Requires, Step, Violation,
};

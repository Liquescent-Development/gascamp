//! The camp formula AST — the master-plan Phase 5 pinned interfaces, plus
//! `Formula::source` (the verbatim authored bytes, pinned into the run dir
//! by cook; re-serializing would lose the authored form).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Formula {
    pub name: String,
    pub description: Option<String>,
    pub requires: Option<Requires>,
    pub steps: Vec<Step>,
    /// Verbatim bytes of the authored file (plan contract deviation 1).
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Requires {
    /// A semver comparator, e.g. ">=2.0.0" (gc: the only [requires] axis).
    pub formula_compiler: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub needs: Vec<String>,
    pub assignee: Option<String>,
    /// General bound on the step's check script (gc: requires `check`).
    pub timeout: Option<Duration>,
    pub check: Option<Check>,
    pub retry: Option<Retry>,
    pub on_complete: Option<OnComplete>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    Exec,
}

impl CheckMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckMode::Exec => "exec",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub max_attempts: u32,
    pub mode: CheckMode,
    pub path: PathBuf,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    HardFail,
    SoftFail,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::HardFail => "hard_fail",
            Disposition::SoftFail => "soft_fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    pub max_attempts: u32,
    /// Default hard_fail (gc formula-spec-v2 §3.2).
    pub on_exhausted: Disposition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OnComplete {
    /// Path into structured step output; must start with "output.".
    pub for_each: String,
    /// Formula instantiated per item; set together with `for_each`.
    pub bond: String,
    pub vars: BTreeMap<String, String>,
    /// true = parallel (gc default); `sequential = true` sets false.
    pub parallel: bool,
}

/// One rule violation. `construct` names what the message is about (a
/// rejected key like "drain", or a location like "steps.review.needs") so
/// tests and users can see exactly which construct failed.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub construct: String,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.construct, self.message)
    }
}

/// The complete verdict on one formula file: ALL violations, never just the
/// first (master-plan Phase 5 contract).
#[derive(Debug)]
pub struct FormulaError {
    pub path: PathBuf,
    pub violations: Vec<Violation>,
}

impl std::fmt::Display for FormulaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{}: {} violation(s):",
            self.path.display(),
            self.violations.len()
        )?;
        for v in &self.violations {
            writeln!(f, "  {v}")?;
        }
        Ok(())
    }
}

impl std::error::Error for FormulaError {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn formula_error_display_lists_every_violation_with_its_construct() {
        let err = FormulaError {
            path: std::path::PathBuf::from("bad.toml"),
            violations: vec![
                Violation {
                    construct: "drain".into(),
                    message: "x".into(),
                },
                Violation {
                    construct: "steps.review.needs".into(),
                    message: "y".into(),
                },
            ],
        };
        let text = err.to_string();
        assert!(text.contains("bad.toml"), "{text}");
        assert!(text.contains("2 violation"), "{text}");
        assert!(text.contains("drain: x"), "{text}");
        assert!(text.contains("steps.review.needs: y"), "{text}");
    }

    #[test]
    fn disposition_and_check_mode_spell_gc_vocabulary() {
        assert_eq!(Disposition::HardFail.as_str(), "hard_fail");
        assert_eq!(Disposition::SoftFail.as_str(), "soft_fail");
        assert_eq!(CheckMode::Exec.as_str(), "exec");
    }
}

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

/// One §4 rule-1 refusal: a Gas City construct camp declines to implement,
/// named rather than approximated.
///
/// A refusal is NOT a violation. A violation says the formula is malformed; a
/// refusal says it is well-formed Gas City that camp will not run. They are
/// reported together and they both fail a load, but only refusals are
/// STEP-SCOPED — see `step`.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    /// The full construct location (e.g. `steps.impl.drain.context`).
    pub construct: String,
    /// The key the refusal is ABOUT — which is not always the key that
    /// carried it: a `gc.kind = "scope"` inside an accepted `metadata` map
    /// refuses as `gc.kind` (§4 trap 2).
    pub key: String,
    pub reason: String,
    /// `Some(step_id)` ⇒ the refusal belongs to a STEP, and is DISCARDED with
    /// it when the step is pruned by a false `condition` (stage 5) or replaced
    /// in place by an `extends` child (stage 2). This is BD2, and it is what
    /// separates a ceiling of 95 from one of 76: 19 corpus formulas carry a
    /// CONDITIONAL shared-drain arm whose refusal must die with the pruned
    /// step, exactly as gc prunes it.
    ///
    /// `None` ⇒ formula-level (e.g. `phase`) — never discarded.
    pub step: Option<String>,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.construct, self.reason)
    }
}

/// The complete verdict on one formula file: ALL violations and ALL refusals,
/// never just the first (master-plan Phase 5 contract).
#[derive(Debug)]
pub struct FormulaError {
    pub path: PathBuf,
    pub violations: Vec<Violation>,
    pub refusals: Vec<Refusal>,
}

impl FormulaError {
    /// True when any violation or refusal names `what` — either as a construct
    /// LOCATION (`steps.impl.drain.context`) or, for a refusal, as the KEY it
    /// is about (`context`, `gc.kind`, `phase`).
    ///
    /// Both, deliberately. A caller should never have to know whether `phase`
    /// failed as a violation or a refusal, nor whether to spell a refusal by
    /// its key or its location: the corpus gate asks "does the refusal name
    /// `gc.kind`?" (a key, which is not even the key that carried it — §4 trap
    /// 2), while a fixture test asks "does anything name `steps.a.gate`?" (a
    /// location).
    pub fn names(&self, what: &str) -> bool {
        self.violations.iter().any(|v| v.construct == what)
            || self
                .refusals
                .iter()
                .any(|r| r.construct == what || r.key == what)
    }
}

impl std::fmt::Display for FormulaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Both counts, always. A refusal-only error (`phase`) must not print
        // "0 violation(s):" and then list nothing — `camp doctor --formula`'s
        // human mode and several `to_string().contains(..)` assertions read
        // this string.
        writeln!(
            f,
            "{}: {} violation(s), {} refusal(s):",
            self.path.display(),
            self.violations.len(),
            self.refusals.len()
        )?;
        for v in &self.violations {
            writeln!(f, "  {v}")?;
        }
        for r in &self.refusals {
            writeln!(f, "  {r}")?;
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
            refusals: Vec::new(),
        };
        let text = err.to_string();
        assert!(text.contains("bad.toml"), "{text}");
        assert!(text.contains("2 violation"), "{text}");
        assert!(text.contains("drain: x"), "{text}");
        assert!(text.contains("steps.review.needs: y"), "{text}");
    }

    #[test]
    fn a_refusal_only_error_still_renders_its_refusal() {
        // The regression this exists for: Display used to print only
        // violations, so a `phase`-refused formula rendered as
        // "0 violation(s):" and then listed NOTHING — the operator was told
        // the load failed and never told why.
        let err = FormulaError {
            path: std::path::PathBuf::from("mol.toml"),
            violations: Vec::new(),
            refusals: vec![Refusal {
                construct: "phase".into(),
                key: "phase".into(),
                reason: "`phase` (= \"vapor\") is a Gas City molecule-phase key".into(),
                step: None,
            }],
        };
        let text = err.to_string();
        assert!(text.contains("1 refusal"), "{text}");
        assert!(text.contains("phase: `phase` (= \"vapor\")"), "{text}");
        assert!(err.names("phase"), "names() must see refusals too");
    }

    #[test]
    fn names_sees_violations_and_refusals_alike() {
        let err = FormulaError {
            path: std::path::PathBuf::from("bad.toml"),
            violations: vec![Violation {
                construct: "steps.a.needs".into(),
                message: "y".into(),
            }],
            refusals: vec![Refusal {
                construct: "steps.a.gate".into(),
                key: "gate".into(),
                reason: "r".into(),
                step: Some("a".into()),
            }],
        };
        assert!(err.names("steps.a.needs"));
        assert!(err.names("steps.a.gate"));
        assert!(!err.names("steps.a.drain"));
    }

    #[test]
    fn disposition_and_check_mode_spell_gc_vocabulary() {
        assert_eq!(Disposition::HardFail.as_str(), "hard_fail");
        assert_eq!(Disposition::SoftFail.as_str(), "soft_fail");
        assert_eq!(CheckMode::Exec.as_str(), "exec");
    }
}

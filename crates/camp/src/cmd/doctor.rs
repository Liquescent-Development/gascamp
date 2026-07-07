use anyhow::{Result, bail};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp doctor --refold [--repair]`: verify (or rebuild) the fold property —
/// state tables ≡ fold of the event log (spec §13.5).
pub fn run(camp: &CampDir, repair: bool) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let report = if repair {
        ledger.refold_repair()?
    } else {
        ledger.refold_check()?
    };
    if report.drift.is_empty() {
        println!(
            "refold: replayed {} events; 0 drift rows",
            report.events_replayed
        );
        Ok(())
    } else {
        for entry in &report.drift {
            println!("drift in {}: {}", entry.table, entry.detail);
        }
        bail!(
            "refold drift detected: {} rows (camp doctor --refold --repair rebuilds state from the event log)",
            report.drift.len()
        );
    }
}

/// `camp doctor --formula <path>`: validate one formula file against the
/// camp subset (spec §8.2). Exit 0 = valid camp formula (and therefore a
/// valid Gas City formula-v2 file, repo invariant 6); exit 1 = every
/// violation printed, not just the first.
pub fn run_formula(path: &std::path::Path) -> Result<()> {
    match camp_core::formula::parse_and_validate(path) {
        Ok(formula) => {
            println!(
                "formula ok: {} ({} step(s))",
                formula.name,
                formula.steps.len()
            );
            Ok(())
        }
        Err(err) => {
            for violation in &err.violations {
                println!("{violation}");
            }
            bail!(
                "{}: {} violation(s) — camp accepts a strict subset of Gas City formula v2 (spec §8.2)",
                err.path.display(),
                err.violations.len()
            );
        }
    }
}

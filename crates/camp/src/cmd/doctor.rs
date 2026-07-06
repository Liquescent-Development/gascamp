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

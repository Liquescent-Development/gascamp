//! `camp export --city <dir>` (spec §15.3): graduation is an export, not a
//! backend. All logic lives in camp-core; this shim resolves paths, runs
//! the export, and renders the report (notes and skips on stderr —
//! visible degradation, never silence).

use std::path::Path;

use camp_core::config::CampConfig;
use camp_core::export::{ExportOptions, export_city};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

pub fn run(camp: &CampDir, city: &Path, skip_untranslatable: bool) -> anyhow::Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let config = CampConfig::load(&camp.config_path())?;
    let report = export_city(
        &ledger,
        &config,
        &camp.root,
        city,
        &ExportOptions {
            skip_untranslatable,
        },
    )?;
    for note in &report.notes {
        eprintln!("camp export: {note}");
    }
    for skipped in &report.skipped_orders {
        eprintln!(
            "camp export: skipped untranslatable order {}: {}",
            skipped.name, skipped.reason
        );
    }
    println!(
        "exported to {}: {} issues, {} memories, {} archive formulas, {} pack formulas, \
         {} agents, {} orders ({} skipped)",
        city.display(),
        report.issues,
        report.memories,
        report.archive_formulas,
        report.pack_formulas,
        report.agents,
        report.orders,
        report.skipped_orders.len()
    );
    Ok(())
}

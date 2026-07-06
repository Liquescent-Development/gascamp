use anyhow::Result;
use camp_core::ledger::Ledger;
use camp_core::readiness::ListFilter;

use crate::campdir::CampDir;

/// `camp ls [--ready | --mine <session>] [--rig <r>] [--json]`.
pub fn run(
    camp: &CampDir,
    ready: bool,
    mine: Option<String>,
    rig: Option<String>,
    json: bool,
) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let beads = if ready {
        ledger.ready_beads(rig.as_deref())?
    } else {
        ledger.list_beads(&ListFilter {
            rig: rig.as_deref(),
            mine: mine.as_deref(),
        })?
    };
    if json {
        println!("{}", serde_json::to_string(&beads)?);
    } else {
        for b in &beads {
            println!("{}\t{}\t{}\t{}", b.id, b.status, b.rig, b.title);
        }
    }
    Ok(())
}

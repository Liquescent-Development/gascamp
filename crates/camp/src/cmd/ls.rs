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
            // Phase 3 (#48 finding 2): the work axis on closed beads
            // (`closed:blocked`) and the fail-fast dispatch marker on open
            // ones (`open:dispatch-failed`) are list-level facts.
            let status = match (&b.work_outcome, &b.dispatch_failure) {
                (Some(wo), _) => format!("{}:{}", b.status, wo),
                (None, Some(_)) if b.status != "closed" => {
                    format!("{}:dispatch-failed", b.status)
                }
                _ => b.status.clone(),
            };
            println!("{}\t{}\t{}\t{}", b.id, status, b.rig, b.title);
        }
    }
    Ok(())
}

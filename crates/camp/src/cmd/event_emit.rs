use anyhow::{Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp event emit <text> [--bead b] [--session s]` (master plan Phase 8):
/// append a `worker.milestone` breadcrumb. The actor is the emitting
/// session's name when given — Phase 11's stall patrol resets a worker's
/// timer on any ledger event from that session, so attribution matters.
pub fn run(
    camp: &CampDir,
    text: String,
    bead: Option<String>,
    session: Option<String>,
) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let rig = match bead.as_deref() {
        Some(id) => match ledger.get_bead(id)? {
            Some(row) => Some(row.rig),
            None => bail!("unknown bead {id}"),
        },
        None => None,
    };
    let seq = ledger.append(EventInput {
        kind: EventType::WorkerMilestone,
        rig,
        actor: session.unwrap_or_else(|| "cli".to_owned()),
        bead,
        data: serde_json::json!({ "text": text }),
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    Ok(())
}

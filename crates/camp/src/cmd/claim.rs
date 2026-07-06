use anyhow::Result;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp claim <bead> --session <name>`: open → in_progress (worker contract).
pub fn run(camp: &CampDir, bead: String, session: String) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    ledger.append(EventInput {
        kind: EventType::BeadClaimed,
        rig: None,
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data: serde_json::json!({ "session": session }),
    })?;
    println!("claimed {bead}");
    Ok(())
}

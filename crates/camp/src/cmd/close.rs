use anyhow::Result;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp close <bead> --outcome pass|fail [--reason r]`: close with outcome.
pub fn run(camp: &CampDir, bead: String, outcome: String, reason: Option<String>) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let mut data = serde_json::json!({ "outcome": outcome });
    if let Some(r) = reason {
        data["reason"] = serde_json::json!(r);
    }
    let seq = ledger.append(EventInput {
        kind: EventType::BeadClosed,
        rig: None,
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data,
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    println!("closed {bead} ({outcome})");
    Ok(())
}

use anyhow::{Result, anyhow};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp show <bead>`: current state plus full event history — the one
/// sanctioned history read (spec §7.4).
pub fn run(camp: &CampDir, bead: String) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let row = ledger
        .get_bead(&bead)?
        .ok_or_else(|| anyhow!("no such bead: {bead}"))?;
    let ready = ledger.is_ready(&bead)?;
    let history = ledger.events_for_bead(&bead)?;

    println!("bead     {}", row.id);
    println!("rig      {}", row.rig);
    println!("type     {}", row.kind);
    println!("title    {}", row.title);
    println!(
        "status   {}{}",
        row.status,
        if ready { "  (ready)" } else { "" }
    );
    if let Some(a) = &row.assignee {
        println!("assignee {a}");
    }
    if let Some(c) = &row.claimed_by {
        println!("claimed  {c}");
    }
    if let Some(o) = &row.outcome {
        println!("outcome  {o}");
    }
    if let Some(wo) = &row.work_outcome {
        println!("work     {wo}");
    }
    if let Some(df) = &row.dispatch_failure {
        println!("dispatch-failed  {df}");
    }
    if !row.labels.is_empty() {
        println!("labels   {}", row.labels.join(", "));
    }
    println!("created  {}", row.created_ts);
    println!("updated  {}", row.updated_ts);
    println!();
    println!("history:");
    for e in &history {
        println!(
            "  {:>4}  {}  {:<14}  {}",
            e.seq,
            e.ts,
            e.kind.as_str(),
            e.data
        );
    }
    Ok(())
}

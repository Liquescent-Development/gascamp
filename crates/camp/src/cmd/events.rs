use std::io::Write;

use anyhow::Result;
use camp_core::Seq;
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp events [--json] [--from N] [--to N]`: the event log for any range.
/// `--json` emits the canonical JSONL (spec §7.2).
pub fn run(camp: &CampDir, json: bool, from: Option<Seq>, to: Option<Seq>) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let events = ledger.events_range(from.unwrap_or(1), to)?;
    let mut stdout = std::io::stdout().lock();
    for event in events {
        if json {
            writeln!(stdout, "{}", serde_json::to_string(&event)?)?;
        } else {
            writeln!(
                stdout,
                "{}\t{}\t{}\t{}\t{}",
                event.seq,
                event.ts,
                event.kind.as_str(),
                event.bead.as_deref().unwrap_or("-"),
                event.actor
            )?;
        }
    }
    Ok(())
}

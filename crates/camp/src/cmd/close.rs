use anyhow::{Context, Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp close <bead> --outcome pass|fail [--reason r] [--transient]
/// [--output-json <file|->]`: close with an outcome. `--transient` is the
/// worker's retry classification (spec §8.2, `failure_class:"transient"`);
/// `--output-json` records structured step output in the close event's
/// `data.output` for `on_complete` fan-out.
pub fn run(
    camp: &CampDir,
    bead: String,
    outcome: String,
    reason: Option<String>,
    transient: bool,
    output_json: Option<String>,
) -> Result<()> {
    if transient && outcome != "fail" {
        // same rule the fold enforces; fail at the user's prompt
        bail!("--transient requires --outcome fail (it classifies a failure)");
    }
    let output = match output_json {
        None => None,
        Some(source) => {
            let raw = if source == "-" {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                    .context("reading --output-json from stdin")?;
                buf
            } else {
                std::fs::read_to_string(&source)
                    .with_context(|| format!("reading --output-json file {source}"))?
            };
            let value: serde_json::Value = serde_json::from_str(&raw)
                .with_context(|| format!("--output-json {source} is not valid JSON"))?;
            Some(value)
        }
    };
    let mut ledger = Ledger::open(&camp.db_path())?;
    let mut data = serde_json::json!({ "outcome": outcome });
    if let Some(r) = reason {
        data["reason"] = serde_json::json!(r);
    }
    if transient {
        data["failure_class"] = serde_json::json!("transient");
    }
    if let Some(output) = output {
        data["output"] = output;
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

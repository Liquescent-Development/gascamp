use anyhow::{Result, bail};
use camp_core::config::{CampConfig, RigConfig};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp create <title> [--rig r] [--needs id]… [--label l]… [--type t]
/// [--description d] [--assignee a]`: append `bead.created` with a freshly
/// allocated per-rig id, and print the id. Named for parity with `bd create`;
/// the plumbing `sling` (Phase 8) will wrap it.
#[allow(clippy::too_many_arguments)]
pub fn run(
    camp: &CampDir,
    title: String,
    rig: Option<String>,
    description: Option<String>,
    needs: Vec<String>,
    labels: Vec<String>,
    bead_type: Option<String>,
    assignee: Option<String>,
) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let rig_cfg = resolve_rig(&config, rig.as_deref())?;

    let mut ledger = Ledger::open(&camp.db_path())?;
    let id = ledger.next_bead_id(&rig_cfg.prefix)?;

    let mut data = serde_json::json!({ "title": title });
    if let Some(d) = description {
        data["description"] = serde_json::json!(d);
    }
    if !needs.is_empty() {
        data["needs"] = serde_json::json!(needs);
    }
    if !labels.is_empty() {
        data["labels"] = serde_json::json!(labels);
    }
    if let Some(t) = bead_type {
        data["type"] = serde_json::json!(t);
    }
    if let Some(a) = assignee {
        data["assignee"] = serde_json::json!(a);
    }

    ledger.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig_cfg.name.clone()),
        actor: "cli".into(),
        bead: Some(id.clone()),
        data,
    })?;
    println!("{id}");
    Ok(())
}

fn resolve_rig<'a>(config: &'a CampConfig, rig: Option<&str>) -> Result<&'a RigConfig> {
    match rig {
        Some(name) => Ok(config.rig(name)?),
        None => match config.rigs.as_slice() {
            [only] => Ok(only),
            [] => bail!("no rigs configured; run camp rig add <path> first"),
            _ => bail!("multiple rigs configured; pass --rig <name>"),
        },
    }
}

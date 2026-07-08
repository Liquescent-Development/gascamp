use anyhow::{Result, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::pack;

use crate::campdir::CampDir;
use crate::cmd::create::resolve_rig;
use crate::daemon::autostart;
use crate::daemon::socket::Request;

/// `camp sling "<title>" [--agent a] [--rig r]` (spec §8.1, Tier 0): one
/// `bead.created` with the routed agent stamped as assignee, then a poke
/// that auto-starts campd if needed — sling promises dispatch, so a
/// fire-and-forget poke is not enough (Phase 8 plan decision P). campd
/// does the spawning; the attended-teammate surface is Phase 12.
pub fn run(
    camp: &CampDir,
    title: String,
    agent: Option<String>,
    rig: Option<String>,
) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let rig_cfg = resolve_rig(&config, rig.as_deref())?;

    // Routing (plan decision D), resolved and validated NOW — a routing
    // hole should fail at the user's prompt, not inside the daemon.
    let agent_name = match agent
        .or_else(|| rig_cfg.default_agent.clone())
        .or_else(|| config.dispatch.default_agent.clone())
    {
        Some(name) => name,
        None => bail!(
            "no agent to route to: pass --agent <name>, set default_agent on [[rigs]] {:?}, \
             or set default_agent under [dispatch] in {}",
            rig_cfg.name,
            camp.config_path().display()
        ),
    };
    // The routed agent must actually resolve in the pack layers.
    pack::resolve_agent(&config, &agent_name)?;

    let rig_name = rig_cfg.name.clone();
    let prefix = rig_cfg.prefix.clone();
    let mut ledger = Ledger::open(&camp.db_path())?;
    let id = ledger.next_bead_id(&prefix)?;
    let seq = ledger.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig_name),
        actor: "cli".into(),
        bead: Some(id.clone()),
        data: serde_json::json!({ "title": title, "assignee": agent_name }),
    })?;
    drop(ledger); // campd may need the write lock immediately

    autostart::request_with_autostart(camp, &Request::Poke { seq }, "sling")?;
    println!("{id}");
    Ok(())
}

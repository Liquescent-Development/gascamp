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
    run: Option<String>,
) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let rig_cfg = resolve_rig(&config, rig.as_deref())?;

    let mut ledger = Ledger::open(&camp.db_path())?;

    // Fail fast on an unknown run: a member bead silently attached to a run
    // that does not exist would simply never be scattered, and nothing would
    // say why.
    if let Some(run_id) = &run {
        if !ledger.run_exists(run_id)? {
            bail!("unknown run {run_id:?} — `camp sling` cooks a run before it can have members");
        }
        // …and the run's formula must actually HAVE a drain.
        //
        // INVARIANT 3 (nothing hidden). A member is deliberately excluded from
        // `dispatchable_beads` AND from `ready_task_count` — campd never dispatches
        // one; a DRAIN scatters over it. So a member on a DRAINLESS run is SILENT
        // DEAD WORK: it never runs, it never appears in `camp top`'s ready count, and
        // nothing anywhere says why. Refuse it at creation, where the operator can
        // still see the mistake.
        let ctx = camp_core::formula::runtime::load_run(&camp.runs_path(), run_id)
            .map_err(|e| anyhow::anyhow!("run {run_id:?}: {e}"))?;
        if !ctx.formula.steps.iter().any(|s| s.drain.is_some()) {
            bail!(
                "run {run_id:?} was cooked from formula {:?}, which has NO drain step — \
                 a run member is only ever consumed by a drain (campd never dispatches \
                 one), so this bead would never run and would never appear in \
                 `camp top`. Sling a formula with a `[steps.<id>.drain]`, or create \
                 the bead without --run.",
                ctx.formula.name
            );
        }
    }

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
    // D3 — a run MEMBER: run_id set, step_id NULL, type task. That triple is
    // exactly what `run_members` selects and what a drain scatters over.
    if let Some(r) = run {
        data["run_id"] = serde_json::json!(r);
    }

    let seq = ledger.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig_cfg.name.clone()),
        actor: "cli".into(),
        bead: Some(id.clone()),
        data,
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    println!("{id}");
    Ok(())
}

pub(crate) fn resolve_rig<'a>(config: &'a CampConfig, rig: Option<&str>) -> Result<&'a RigConfig> {
    match rig {
        Some(name) => Ok(config.rig(name)?),
        None => match config.rigs.as_slice() {
            [only] => Ok(only),
            [] => bail!("no rigs configured; run camp rig add <path> first"),
            _ => bail!("multiple rigs configured; pass --rig <name>"),
        },
    }
}

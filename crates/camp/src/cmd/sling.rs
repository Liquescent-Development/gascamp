use anyhow::{Result, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::pack;

use crate::campdir::CampDir;
use crate::cmd::create::resolve_rig;
use crate::daemon::socket::{self, Request};

/// `camp sling "<title>" [--agent a] [--rig r]` (spec §8.1, Tier 0): one
/// `bead.created` with the routed agent stamped as assignee, then a poke to a
/// RUNNING campd — sling promises dispatch, so a fire-and-forget poke is not
/// enough (Phase 8 plan decision P). A PURE CLIENT (design §4.3): it never
/// starts campd. A campd that cannot serve the poke fails the verb loudly —
/// but the bead is already durable, so its id is printed FIRST and the error
/// says the bead exists and will dispatch once campd is up and serving (spec
/// §7.2: campd catches up from its cursor on start). campd does the spawning;
/// there is no second dispatch path (dispatch-lifecycle Phase 1, #29).
///
/// `camp sling --formula <name> [--rig r]` (spec §8.2, Phase 9 plan
/// Decision 7): cook `<camp>/formulas/<name>.toml` into `<camp>/runs/`
/// and poke — from that moment campd advances the run (spec §8.3).
pub fn run(
    camp: &CampDir,
    title: Option<String>,
    agent: Option<String>,
    rig: Option<String>,
    formula: Option<String>,
) -> Result<()> {
    match (title, formula) {
        (Some(title), None) => sling_bead(camp, title, agent, rig),
        (None, Some(formula)) => {
            if agent.is_some() {
                bail!("--agent applies to Tier-0 slings; formula steps route via `assignee`");
            }
            sling_formula(camp, &formula, rig)
        }
        (Some(_), Some(_)) => bail!("pass a title OR --formula <name>, not both"),
        (None, None) => bail!("pass a title to sling, or --formula <name> to cook a run"),
    }
}

/// Cook a formula run (spec §8.2): pin into runs/, materialize beads, poke a
/// running campd. Prints "<run_id> root <root-bead>".
fn sling_formula(camp: &CampDir, name: &str, rig: Option<String>) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let rig_cfg = resolve_rig(&config, rig.as_deref())?.clone();
    // compat §9: compile through the LAYER STACK. `parse_and_validate` (the
    // no-layer, camp-local entry) cannot resolve an imported formula's `extends`,
    // its `description_file`, or its routes — and every corpus formula needs all
    // three.
    let layers = camp_core::formula::FormulaLayers::from_config(&config, &camp.root)?;
    let compiled = camp_core::formula::compile_named(
        &layers,
        &config,
        name,
        &std::collections::BTreeMap::new(),
    )
    .map_err(|e| anyhow::anyhow!("formula {name:?} did not compile:\n{e}"))?;

    // D1 (ruling E) — LOADABLE ≠ RUNNABLE. Of the 95 corpus formulas camp
    // compiles, 16 declare no graph compiler at all and 14 are expansions: 30
    // that camp refuses at RUN time rather than run under semantics they never
    // claimed. Camp-LOCAL formulas are exempt from the gate (the operator's own
    // formula is not a gc pack making a contract claim), so this refuses only
    // imported ones.
    if let Some(why) = &compiled.not_runnable {
        let mut ledger = Ledger::open(&camp.db_path())?;
        refuse_formula(&mut ledger, name, why)?;
        bail!("formula {name:?} cannot be run: {}", why.reason);
    }

    let opts = camp_core::formula::CookOptions {
        config: Some(config.clone()),
        ..Default::default()
    };
    let mut ledger = Ledger::open(&camp.db_path())?;
    let cooked = camp_core::formula::cook_with(
        &mut ledger,
        &compiled.formula,
        &camp.runs_path(),
        &rig_cfg,
        "cli",
        &opts,
    )?;
    // the root's run.cooked is the batch's last event — the poke seq
    // (advisory; the settle reads past the cursor regardless)
    let head = ledger
        .events_for_bead(&cooked.root_bead)?
        .last()
        .map(|e| e.seq)
        .unwrap_or(0);
    drop(ledger); // campd may need the write lock immediately
    // The run is cooked and PINNED — durable before the poke, so print it
    // before we can fail on a campd that cannot serve us (spec §7.2).
    println!("{} root {}", cooked.run_id, cooked.root_bead);
    socket::require(camp, &Request::Poke { seq: head }).map_err(|e| {
        e.context(format!(
            "run {} is cooked and pinned, but NOT started — campd advances runs, and only \
             a healthy, running campd can; it starts as soon as one is (campd catches up \
             from its cursor)",
            cooked.run_id
        ))
    })?;
    Ok(())
}

fn sling_bead(
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

    // The bead is DURABLE now: print it before the poke, so a campd that
    // cannot serve us costs the operator the dispatch and never the id.
    println!("{id}");
    socket::require(camp, &Request::Poke { seq }).map_err(|e| {
        e.context(format!(
            "{id} is created and durable, but NOT dispatched — sling promises dispatch, and \
             only a healthy, running campd dispatches; it runs as soon as one is (campd \
             catches up from its cursor on start)"
        ))
    })?;
    Ok(())
}

/// A refusal LANDS IN THE LEDGER (compat §4 rule 1; exit criterion: "refusals
/// name their key and land as ledger events").
///
/// "The formula did not run" is not an answer an operator can act on. The event
/// NAMES THE KEY — which, for a scope-check hiding in step metadata, is not even
/// the key that carried it (§4 trap 2).
pub(crate) fn refuse_formula(
    ledger: &mut Ledger,
    name: &str,
    refusal: &camp_core::formula::Refusal,
) -> Result<()> {
    let mut data = serde_json::json!({
        "formula": name,
        "key": refusal.key,
        "reason": refusal.reason,
    });
    if let Some(step) = &refusal.step {
        data["step"] = serde_json::json!(step);
    }
    ledger.append(EventInput {
        kind: EventType::FormulaRefused,
        rig: None,
        actor: "cli".into(),
        bead: None,
        data,
    })?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use camp_core::config::CampConfig;
    use camp_core::orders::resolve_formula;

    #[test]
    fn sling_formula_resolves_an_imported_formula_path() {
        // compat §6: `camp sling --formula` goes through `resolve_formula`,
        // so an imported formula is reachable. (Phase 1 fixes RESOLUTION only;
        // running an imported formula is phase 2 — no cook here.)
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname=\"t\"\n[imports.bmad]\nsource=\"file:///x\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        let f = root.join("imports/bmad/formulas");
        std::fs::create_dir_all(&f).unwrap();
        std::fs::write(f.join("build.toml"), "formula = \"build\"\n").unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
        let path = resolve_formula(&cfg, "build").unwrap();
        assert!(
            path.to_string_lossy().contains("imports/bmad/formulas"),
            "sling --formula must resolve through the import layer: {}",
            path.display()
        );
    }
}

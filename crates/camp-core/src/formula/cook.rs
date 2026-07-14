//! Cook: materialize a validated formula into the ledger (spec §8.2).
//! Files first (runs/<run-id>/ with the pinned copy + manifest), then ONE
//! append_batch transaction for root + steps + run.cooked. Gas City's
//! materialization property, kept: after cook the run is independent of
//! the formula file.
//!
//! Crash window (deliberate, reviewed): files land BEFORE the ledger
//! transaction, so a hard kill between the run-dir write and the
//! append_batch commit leaves an orphan runs/<run-id>/ directory with no
//! ledger record. That is the safe direction — a DB-first ordering could
//! commit a run whose pinned formula never hit disk, breaking the
//! file-independence property. An orphan dir references nothing, nothing
//! references it, and the missing run.cooked event makes it
//! self-explaining. A future `camp doctor` check may sweep run dirs that
//! have no run.cooked event; v1 does not build it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::RigConfig;
use crate::error::CoreError;
use crate::event::{EventInput, EventType};
use crate::formula::ast::Formula;
use crate::ledger::Ledger;

/// The schema of `runs/<id>/recipe.json`.
///
/// **STRICT EQUALITY** (BD-C). `recipe.json` is the reload path for every live
/// run. compat-3 touches the worker contract; compat-4 adds `type = "mail"`. If
/// either adds a field to `Formula`/`Step`, this MUST be bumped — and bumping it
/// kills in-flight runs LOUDLY, with a named remedy ("re-sling it"), which is
/// invariant 5. The alternative — deserializing a recipe that means something
/// else — is the failure mode BD8 already shipped once, and no compat-2 gate can
/// see it, because every fixture cooks and loads with the SAME binary.
pub const RECIPE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct CookedRun {
    pub run_id: String,
    pub root_bead: String,
    pub step_beads: BTreeMap<String, String>,
}

/// Cook-time options for bond fan-out (Phase 9, spec §8.2 on_complete).
/// The default cooks exactly as before.
#[derive(Debug, Clone, Default)]
pub struct CookOptions {
    /// Substituted into step titles/descriptions (`{key}` -> value) on the
    /// cooked BEADS only — the pinned file stays byte-verbatim. Recorded
    /// in manifest.json under "vars".
    pub vars: BTreeMap<String, String>,
    /// Extra `needs` on the root bead (sequential bond chaining edge).
    pub extra_root_needs: Vec<String>,
    /// Labels on the root bead (`bond:<anchor>:<index>` linkage).
    pub extra_root_labels: Vec<String>,
    /// Metadata stamped on the run's ROOT bead. A drain uses it to carry gc's
    /// `gc.drain_member_id` / `_index` / `_count` / `_control_id` onto each item
    /// run — the link that tells the item worker WHICH MEMBER it is working.
    /// Without it a drain scatters byte-identical clones.
    pub root_metadata: BTreeMap<String, String>,
    /// The camp config, for RESOLVING ROUTES through the binding namespace
    /// (compat §7.1). `None` cooks a formula with no `gc.run_target` — every
    /// camp-local fixture — and fails loudly on one that has a route, rather
    /// than silently dispatching an unrouted worker.
    pub config: Option<crate::config::CampConfig>,
}

/// gc's `Substitute` (`parser.go:617`); `varPattern` is
/// `\{\{([a-zA-Z_][a-zA-Z0-9_]*)\}\}` (`parser.go:557`).
///
/// **This is the INSTANTIATION grammar, and it is the SECOND of camp's three.**
/// It runs at cook, over EVERY field and EVERY metadata value, with **no
/// exemption list** (gc `molecule.go:1035-1037`) — **including `check.path`**
/// (→ `gc.check_path`, `ralph.go:76`) and **`drain.formula`** (→
/// `gc.drain_formula`, `compile.go:590`). §9's "substitution asymmetry" list is
/// wrong and is deleted; a templated `drain.formula` is blocked separately, at
/// VALIDATION, exactly as gc blocks it.
///
/// An unknown token is LEFT VERBATIM — authored text, not a template language.
/// A single left-to-right pass: a substituted value is never re-scanned.
///
/// **Do NOT merge this with [`substitute`] below.** That one is `{name}` over
/// `CookOptions.vars` for bond children — a different grammar, a different
/// scope, a different stage. Three substitution functions, three grammars, three
/// stages; the day two of them meet, 55 routes corrupt.
pub(crate) fn substitute_vars(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &after[..close];
        let legal = !token.is_empty()
            && token.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        match vars.get(token).filter(|_| legal) {
            Some(value) => out.push_str(value),
            // Unknown (or not a legal var name): verbatim, braces and all.
            None => {
                out.push_str("{{");
                out.push_str(token);
                out.push_str("}}");
            }
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

/// Replace every `{key}` from `vars` in `text` — a SINGLE left-to-right
/// pass (review MEDIUM 2): inserted values are worker output, never
/// re-scanned as template syntax. Unknown tokens stay verbatim (authored
/// text, not a template language).
///
/// This is the BOND FAN-OUT grammar (`{item}`, `{item.field}`, `{index}`), not
/// the formula var grammar. See [`substitute_vars`].
fn substitute(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &rest[open + 1..open + close];
        match vars.get(token) {
            Some(value) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(token);
                out.push('}');
            }
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// gc's routing key. 327 corpus occurrences; ZERO step `assignee`. Routing is
/// ENTIRELY step metadata.
pub const RUN_TARGET: &str = "gc.run_target";

/// INSTANTIATE the compiled formula — gc's `stepToBead`, and the stage that
/// makes the pinned recipe executable.
///
/// 1. `{{var}}` substituted over EVERY field and EVERY metadata value, with no
///    exemption list — including `check.path` and `drain.formula`.
/// 2. The route (`gc.run_target`) resolved through the binding namespace into
///    the step's `assignee`.
/// 3. The residual check, which §9 scopes to `title` ONLY.
///
/// **Both of those outputs are what campd actually uses at runtime** (BD-A):
/// `spawn_check` EXECs `step.check.path` straight out of the reloaded recipe,
/// and `create_attempt` reads `step.assignee` to route the ATTEMPT bead — a
/// DIFFERENT bead from the anchor cook wrote. Pin the pre-substitution formula
/// and campd execs a literal `{{kind}}.sh` and dispatches unrouted workers.
pub fn instantiate(
    formula: &Formula,
    cfg: Option<&crate::config::CampConfig>,
) -> Result<Formula, CoreError> {
    let vars: BTreeMap<String, String> = formula
        .vars
        .iter()
        .filter_map(|(k, v)| v.clone().map(|v| (k.clone(), v)))
        .collect();
    let sub = |s: &str| substitute_vars(s, &vars);

    let mut out = formula.clone();
    out.description = formula.description.as_deref().map(sub);
    for step in &mut out.steps {
        step.title = sub(&step.title);
        step.description = step.description.as_deref().map(sub);
        for value in step.metadata.values_mut() {
            *value = sub(value);
        }
        if let Some(check) = &mut step.check {
            // NO exemption (F8). gc substitutes here; §9 said it did not.
            check.path = std::path::PathBuf::from(sub(&check.path.to_string_lossy()));
        }
        if let Some(oc) = &mut step.on_complete {
            oc.bond = sub(&oc.bond);
            for value in oc.vars.values_mut() {
                *value = sub(value);
            }
        }
        step.assignee = step.assignee.as_deref().map(sub);

        // §9's residual check is TITLE-ONLY, and it runs HERE — after
        // substitution. A residual `{{var}}` in a description is normal (561
        // corpus steps carry one); in a title it is a bug the operator must see.
        if step.title.contains("{{") {
            return Err(CoreError::Cook(format!(
                "formula {:?} step {:?}: title still has an unresolved variable after \
                 substitution: {:?}",
                formula.name, step.id, step.title
            )));
        }

        // The ROUTE. Resolved through compat-1's binding namespace — the one
        // resolver, never a second one.
        if let Some(target) = step.metadata.get(RUN_TARGET) {
            let target = target.clone();
            if target.contains("{{") {
                return Err(CoreError::Cook(format!(
                    "formula {:?} step {:?}: route {target:?} still has an unresolved variable — \
                     no binding can be found for it",
                    formula.name, step.id
                )));
            }
            let cfg = cfg.ok_or_else(|| {
                CoreError::Cook(format!(
                    "formula {:?} step {:?} routes to {target:?}, but this cook has no camp \
                     config to resolve the binding against",
                    formula.name, step.id
                ))
            })?;
            let agent = crate::pack::resolve_agent(cfg, &target)?;
            step.assignee = Some(agent.name);
        }
    }
    Ok(out)
}

pub fn cook(
    ledger: &mut Ledger,
    formula: &Formula,
    run_dir: &Path,
    rig: &RigConfig,
    actor: &str,
) -> Result<CookedRun, CoreError> {
    cook_with(
        ledger,
        formula,
        run_dir,
        rig,
        actor,
        &CookOptions::default(),
    )
}

pub fn cook_with(
    ledger: &mut Ledger,
    formula: &Formula,
    run_dir: &Path,
    rig: &RigConfig,
    actor: &str,
    opts: &CookOptions,
) -> Result<CookedRun, CoreError> {
    // INSTANTIATE FIRST (BD-A). Everything below — the beads AND the pinned
    // recipe — is written from the instantiated formula, because that is what
    // campd reloads and executes.
    let instantiated = instantiate(formula, opts.config.as_ref())?;
    let formula = &instantiated;

    if formula.steps.is_empty() {
        // parse_and_validate guarantees this (rule S3); cook re-checks its
        // own precondition rather than cooking an empty run.
        return Err(CoreError::Cook(format!(
            "formula {:?} has no steps — cook requires parse_and_validate output",
            formula.name
        )));
    }

    let ts = ledger.now_utc();
    let compact_ts = ts.replace(['-', ':'], "");
    let new_run_id = || format!("{compact_ts}-{:06x}", fastrand::u32(..) & 0xFF_FFFF);
    let mut run_id = new_run_id();

    // ---- id block allocation (Phase 3 counter). A concurrent writer racing
    // the same block makes the batch fail on the duplicate id and roll back
    // everything — the same fail-fast race window `camp create` has.
    let first = ledger.next_bead_id(&rig.prefix)?;
    let (prefix, n) = crate::id::parse_bead_id(&first).ok_or_else(|| {
        CoreError::Corrupt(format!("next_bead_id returned unparseable id {first:?}"))
    })?;
    let root_bead = format!("{prefix}-{n}");
    let mut step_beads: BTreeMap<String, String> = BTreeMap::new();
    for (offset, step) in formula.steps.iter().enumerate() {
        step_beads.insert(
            step.id.clone(),
            format!("{prefix}-{}", n + 1 + offset as i64),
        );
    }

    // Resolve every needs edge BEFORE touching disk or the ledger: a
    // hand-built Formula (bypassing parse_and_validate) naming an unknown
    // step must fail loudly, never cook a bead with a silently missing
    // edge (review finding 1).
    let mut step_needs: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for step in &formula.steps {
        let mut needs = Vec::with_capacity(step.needs.len());
        for id in &step.needs {
            let bead = step_beads.get(id).ok_or_else(|| {
                CoreError::Cook(format!(
                    "formula {:?} step {:?} needs unknown step id {id:?} — \
                     cook requires parse_and_validate output",
                    formula.name, step.id
                ))
            })?;
            needs.push(bead.clone());
        }
        step_needs.insert(step.id.as_str(), needs);
    }

    // ---- files first: runs/<run-id>/ with pinned copy + manifest
    std::fs::create_dir_all(run_dir)
        .map_err(|e| CoreError::Cook(format!("cannot create {}: {e}", run_dir.display())))?;
    // Same-second suffix collision (24 random bits): regenerate once
    // (review finding 6); a second collision fails loudly.
    let mut dir = run_dir.join(&run_id);
    if let Err(e) = std::fs::create_dir(&dir) {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(CoreError::Cook(format!(
                "cannot create {}: {e}",
                dir.display()
            )));
        }
        run_id = new_run_id();
        dir = run_dir.join(&run_id);
        std::fs::create_dir(&dir).map_err(|e| {
            CoreError::Cook(format!(
                "run id collision persisted after retry at {}: {e}",
                dir.display()
            ))
        })?;
    }
    let write = |name: &str, bytes: &[u8]| -> Result<(), CoreError> {
        std::fs::write(dir.join(name), bytes)
            .map_err(|e| CoreError::Cook(format!("cannot write {}/{name}: {e}", dir.display())))
    };
    // The authored bytes, verbatim — invariant 3 ("human-readable run files").
    // AUDIT ONLY. Nothing re-parses this.
    write(&format!("{}.toml", formula.name), formula.source.as_bytes())?;
    // BD8 — the run's REAL reload path. `load_run` used to re-parse the authored
    // `.toml` above with no layers and a default config; for every imported
    // corpus formula (they carry `extends`, `description_file`, and routes that
    // need `cfg.imports`) that re-parse CANNOT succeed, so every cooked corpus
    // run dead-ended on campd's first event.
    //
    // This is the INSTANTIATED recipe (BD-A): written AFTER `{{var}}`
    // substitution and AFTER route resolution, because merged campd EXECs
    // `step.check.path` and DISPATCHES on `step.assignee` straight out of it.
    let recipe = serde_json::json!({
        "recipe_version": RECIPE_VERSION,
        "formula": formula,
    });
    write("recipe.json", format!("{recipe:#}").as_bytes())?;
    let mut manifest = serde_json::json!({
        "run_id": run_id,
        "formula": formula.name,
        "rig": rig.name,
        "actor": actor,
        "cooked_ts": ts,
        "root": root_bead,
        "steps": step_beads,
    });
    if !opts.vars.is_empty() {
        manifest["vars"] = serde_json::json!(opts.vars);
    }
    write("manifest.json", format!("{manifest:#}").as_bytes())?;

    // ---- one transaction: root, steps, run.cooked
    let mut inputs = Vec::with_capacity(formula.steps.len() + 2);
    let mut root_needs: Vec<&str> = step_beads.values().map(String::as_str).collect();
    root_needs.extend(opts.extra_root_needs.iter().map(String::as_str));
    let mut root_data = serde_json::json!({
        "title": formula.name,
        "needs": root_needs,
        "run_id": run_id,
    });
    if let Some(d) = &formula.description {
        root_data["description"] = serde_json::json!(d);
    }
    if !opts.extra_root_labels.is_empty() {
        root_data["labels"] = serde_json::json!(opts.extra_root_labels);
    }
    if !opts.root_metadata.is_empty() {
        root_data["metadata"] = serde_json::json!(opts.root_metadata);
    }
    inputs.push(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig.name.clone()),
        actor: actor.to_owned(),
        bead: Some(root_bead.clone()),
        data: root_data,
    });
    for step in &formula.steps {
        let needs = &step_needs[step.id.as_str()];
        let mut data = serde_json::json!({
            "title": substitute(&step.title, &opts.vars),
            "run_id": run_id,
            "step_id": step.id,
        });
        if let Some(d) = &step.description {
            data["description"] = serde_json::json!(substitute(d, &opts.vars));
        }
        if !needs.is_empty() {
            data["needs"] = serde_json::json!(needs);
        }
        if let Some(a) = &step.assignee {
            data["assignee"] = serde_json::json!(a);
        }
        // gc's step metadata, onto the bead (compat §6.1). This is where routing
        // lives (`gc.run_target`) — it is not annotation.
        if !step.metadata.is_empty() {
            data["metadata"] = serde_json::json!(step.metadata);
        }
        inputs.push(EventInput {
            kind: EventType::BeadCreated,
            rig: Some(rig.name.clone()),
            actor: actor.to_owned(),
            bead: Some(step_beads[&step.id].clone()),
            data,
        });
    }
    inputs.push(EventInput {
        kind: EventType::RunCooked,
        rig: Some(rig.name.clone()),
        actor: actor.to_owned(),
        bead: Some(root_bead.clone()),
        data: serde_json::json!({
            "run_id": run_id,
            "formula": formula.name,
            "root": root_bead,
            "steps": step_beads,
        }),
    });

    if let Err(batch_err) = ledger.append_batch(inputs) {
        // Roll the files back too; a cleanup failure is reported WITH the
        // original error, never instead of it and never silently.
        return Err(match std::fs::remove_dir_all(&dir) {
            Ok(()) => batch_err,
            Err(cleanup) => CoreError::Cook(format!(
                "cook failed ({batch_err}) and the run dir {} could not be removed: {cleanup}",
                dir.display()
            )),
        });
    }

    Ok(CookedRun {
        run_id,
        root_bead,
        step_beads,
    })
}

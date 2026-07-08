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
}

/// Replace every `{key}` from `vars` in `text` — a SINGLE left-to-right
/// pass (review MEDIUM 2): inserted values are worker output, never
/// re-scanned as template syntax. Unknown tokens stay verbatim (authored
/// text, not a template language).
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
    write(&format!("{}.toml", formula.name), formula.source.as_bytes())?;
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

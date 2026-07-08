//! The graph-execution runtime (spec §8.3, Phase 9): attempt/iteration
//! bookkeeping as PURE functions over ledger state. Nothing here writes —
//! the daemon appends; these functions read `&Connection` state (plus the
//! pinned run dir) and answer "what is mechanically due". Every judgment
//! stays with agents and user-supplied check scripts (Zero-Framework-
//! Cognition); this module only counts, walks edges, and applies declared
//! budgets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Deserialize;

use crate::error::CoreError;
use crate::formula::ast::{Formula, Step};
use crate::readiness::BeadRow;

/// Where cooked runs live under the camp root — the single definition
/// (`<camp>/runs/`), shared by cook callers and `CampDir::runs_path()`.
pub const RUNS_SUBDIR: &str = "runs";

/// One cooked run, loaded back from its pinned dir: the manifest's bead
/// mapping plus the re-parsed pinned formula. Everything campd knows about
/// a run's STRUCTURE comes from here; everything about its STATE comes
/// from the ledger.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub run_id: String,
    pub rig: String,
    pub root: String,
    pub formula: Formula,
    /// step_id -> anchor bead id (the bead cook materialized for the step).
    pub anchors: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Manifest {
    run_id: String,
    formula: String,
    rig: String,
    root: String,
    steps: BTreeMap<String, String>,
    // actor / cooked_ts / vars: audit content, not needed here
    #[serde(flatten)]
    _rest: BTreeMap<String, serde_json::Value>,
}

/// Load a run's context from `<runs_dir>/<run_id>/`. A missing dir,
/// unreadable manifest, invalid pinned formula, or manifest/formula step
/// drift is `CoreError::Corrupt` naming the run dir — fail fast, the
/// caller decides how to surface it.
pub fn load_run(runs_dir: &Path, run_id: &str) -> Result<RunContext, CoreError> {
    let dir = runs_dir.join(run_id);
    let manifest_path = dir.join("manifest.json");
    let corrupt = |what: String| CoreError::Corrupt(format!("run {run_id}: {what}"));
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| corrupt(format!("cannot read {}: {e}", manifest_path.display())))?;
    let manifest: Manifest =
        serde_json::from_str(&raw).map_err(|e| corrupt(format!("bad manifest: {e}")))?;
    if manifest.run_id != run_id {
        return Err(corrupt(format!(
            "manifest run_id {:?} does not match the dir",
            manifest.run_id
        )));
    }
    let formula_path = dir.join(format!("{}.toml", manifest.formula));
    let formula = crate::formula::parse_and_validate(&formula_path)
        .map_err(|e| corrupt(format!("pinned formula invalid: {e}")))?;
    let formula_steps: Vec<&str> = formula.steps.iter().map(|s| s.id.as_str()).collect();
    let manifest_steps: Vec<&str> = manifest.steps.keys().map(String::as_str).collect();
    if formula_steps
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        != manifest_steps
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(corrupt(format!(
            "manifest steps {manifest_steps:?} do not match the pinned formula {formula_steps:?}"
        )));
    }
    Ok(RunContext {
        run_id: manifest.run_id,
        rig: manifest.rig,
        root: manifest.root,
        formula,
        anchors: manifest.steps,
    })
}

/// A looping step is one campd owns the anchor for: `check` or `retry`
/// (gc rule S9 forbids combining them).
pub fn is_looping(step: &Step) -> bool {
    step.check.is_some() || step.retry.is_some()
}

/// A step reference resolved from a step id: the formula step and its
/// anchor bead.
pub struct StepRef<'a> {
    pub step: &'a Step,
    pub anchor: &'a str,
}

impl RunContext {
    /// Resolve a step id to its formula step + anchor (`None` for unknown
    /// ids — e.g. a bead from another run).
    pub fn step_ref(&self, step_id: &str) -> Option<StepRef<'_>> {
        let anchor = self.anchors.get(step_id)?;
        let step = self.formula.steps.iter().find(|s| s.id == step_id)?;
        Some(StepRef { step, anchor })
    }

    pub fn is_anchor(&self, bead_id: &str) -> bool {
        self.anchors.values().any(|a| a == bead_id)
    }
}

/// A bead's run membership, straight from the fold's beads row: `None`
/// for plain beads; `step_id: None` marks a run ROOT.
#[derive(Debug, Clone, PartialEq)]
pub struct RunMembership {
    pub run_id: String,
    pub step_id: Option<String>,
}

pub fn run_membership(conn: &Connection, bead: &str) -> Result<Option<RunMembership>, CoreError> {
    use rusqlite::OptionalExtension;
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT run_id, step_id FROM beads WHERE id = ?1",
            [bead],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        None | Some((None, _)) => None,
        Some((Some(run_id), step_id)) => Some(RunMembership { run_id, step_id }),
    })
}

/// All beads of one run step in creation order (the bead.created event
/// seq — robust where same-second `created_ts` values tie).
pub fn run_step_beads(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
) -> Result<Vec<BeadRow>, CoreError> {
    let mut stmt = conn.prepare(
        "SELECT b.id FROM beads b
         JOIN events e ON e.bead = b.id AND e.type = 'bead.created'
         WHERE b.run_id = ?1 AND b.step_id = ?2
         ORDER BY e.seq",
    )?;
    let ids: Vec<String> = stmt
        .query_map([run_id, step_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut rows = Vec::with_capacity(ids.len());
    for id in ids {
        rows.push(
            crate::readiness::get_bead(conn, &id)?
                .ok_or_else(|| CoreError::Corrupt(format!("bead {id} vanished mid-query")))?,
        );
    }
    Ok(rows)
}

/// The attempts of a looping step: its beads minus the anchor, creation
/// order. Attempt N is index N-1.
pub fn attempts(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
    anchor: &str,
) -> Result<Vec<BeadRow>, CoreError> {
    Ok(run_step_beads(conn, run_id, step_id)?
        .into_iter()
        .filter(|b| b.id != anchor)
        .collect())
}

/// The data of a bead's close event, if it has closed. A bead closes at
/// most once (the fold forbids a second close).
pub fn close_event_data(
    conn: &Connection,
    bead: &str,
) -> Result<Option<serde_json::Value>, CoreError> {
    use rusqlite::OptionalExtension;
    let raw: Option<String> = conn
        .query_row(
            "SELECT data FROM events WHERE bead = ?1 AND type = 'bead.closed'
             ORDER BY seq DESC LIMIT 1",
            [bead],
            |r| r.get(0),
        )
        .optional()?;
    match raw {
        None => Ok(None),
        Some(text) => Ok(Some(serde_json::from_str(&text).map_err(|e| {
            CoreError::Corrupt(format!("bead {bead} close data is not JSON: {e}"))
        })?)),
    }
}

/// Check iterations used = attempts closed pass (each passing attempt
/// triggers exactly one check run).
pub fn check_runs_used(attempts: &[BeadRow]) -> u32 {
    attempts
        .iter()
        .filter(|b| b.status == "closed" && b.outcome.as_deref() == Some("pass"))
        .count() as u32
}

/// Retry budget used = attempts closed fail with `failure_class:"transient"`.
pub fn transient_fails_used(conn: &Connection, attempts: &[BeadRow]) -> Result<u32, CoreError> {
    let mut used = 0u32;
    for b in attempts {
        if b.status == "closed" && b.outcome.as_deref() == Some("fail") {
            let data = close_event_data(conn, &b.id)?;
            if data
                .as_ref()
                .and_then(|d| d.get("failure_class"))
                .and_then(|c| c.as_str())
                == Some("transient")
            {
                used += 1;
            }
        }
    }
    Ok(used)
}

/// True when `bead`'s needs can NEVER all pass: some need is missing,
/// closed non-pass, or (recursively) itself unsatisfiable. Open needs
/// whose own needs can still pass keep the bead satisfiable. Cycles are
/// impossible (validate rejects same-run cycles; cross-run edges point at
/// closed roots).
pub fn unsatisfiable(conn: &Connection, bead: &str) -> Result<bool, CoreError> {
    let mut stmt = conn.prepare("SELECT needs_id FROM deps WHERE bead_id = ?1")?;
    let needs: Vec<String> = stmt
        .query_map([bead], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    for need in needs {
        match crate::readiness::get_bead(conn, &need)? {
            None => return Ok(true),
            Some(row) => {
                if row.status == "closed" {
                    if row.outcome.as_deref() != Some("pass") {
                        return Ok(true);
                    }
                } else if unsatisfiable(conn, &need)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// The finalization verdict for a run (plan Decision 3, as approved):
/// quiescence + the mechanical aggregation table. Dispositions of closed
/// anchors are read from their close events; the run-level disposition
/// (including "pass") belongs to `run.finalized` only.
#[derive(Debug, PartialEq)]
pub enum RunVerdict {
    NotQuiescent,
    Finalize {
        outcome: &'static str,
        disposition: &'static str,
        /// step ids of soft-failed anchors (audit content)
        soft_failed: Vec<String>,
        /// step ids whose OPEN anchors must be closed `skipped` by the
        /// caller, plus those already closed `skipped` (restart idempotency)
        skipped: Vec<String>,
        /// bead ids of the open unsatisfiable anchors to close `skipped`
        to_skip: Vec<String>,
    },
}

pub fn finalization(conn: &Connection, ctx: &RunContext) -> Result<RunVerdict, CoreError> {
    let root = crate::readiness::get_bead(conn, &ctx.root)?
        .ok_or_else(|| CoreError::UnknownBead(ctx.root.clone()))?;
    if root.status == "closed" {
        return Ok(RunVerdict::NotQuiescent); // already finalized
    }
    let mut hard = false;
    let mut soft_failed: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut to_skip: Vec<String> = Vec::new();
    for step in &ctx.formula.steps {
        let anchor_id = &ctx.anchors[&step.id];
        let anchor = crate::readiness::get_bead(conn, anchor_id)?
            .ok_or_else(|| CoreError::UnknownBead(anchor_id.clone()))?;
        match (anchor.status.as_str(), anchor.outcome.as_deref()) {
            ("closed", Some("pass")) => {}
            ("closed", Some("skipped")) => skipped.push(step.id.clone()),
            ("closed", _) => {
                let disposition = close_event_data(conn, anchor_id)?
                    .as_ref()
                    .and_then(|d| d.get("final_disposition"))
                    .and_then(|d| d.as_str())
                    .map(str::to_owned);
                if disposition.as_deref() == Some("soft_fail") {
                    soft_failed.push(step.id.clone());
                } else {
                    hard = true;
                }
            }
            ("in_progress", _) => return Ok(RunVerdict::NotQuiescent),
            _ => {
                // open: quiescent only if it can never run
                if unsatisfiable(conn, anchor_id)? {
                    skipped.push(step.id.clone());
                    to_skip.push(anchor_id.clone());
                } else {
                    return Ok(RunVerdict::NotQuiescent);
                }
            }
        }
    }
    let (outcome, disposition) = if hard {
        ("fail", "hard_fail")
    } else if !skipped.is_empty() {
        ("fail", "soft_fail")
    } else if !soft_failed.is_empty() {
        ("pass", "soft_fail")
    } else {
        ("pass", "pass")
    };
    Ok(RunVerdict::Finalize {
        outcome,
        disposition,
        soft_failed,
        skipped,
        to_skip,
    })
}

/// Resolve an `on_complete.for_each` path (validated by Phase 5 to start
/// with `output.`) against a close event's data. The error strings are
/// human-actionable — they land in a ledger event.
pub fn resolve_for_each<'v>(
    close_data: &'v serde_json::Value,
    path: &str,
) -> Result<&'v Vec<serde_json::Value>, String> {
    let rest = path
        .strip_prefix("output.")
        .ok_or_else(|| format!("for_each {path:?} must start with \"output.\""))?;
    let mut node = close_data.get("output").ok_or_else(|| {
        format!("for_each {path:?}: the close carries no output (use camp close --output-json)")
    })?;
    for segment in rest.split('.') {
        node = node.get(segment).ok_or_else(|| {
            format!("for_each {path:?}: output has no field {segment:?} (found: {node})")
        })?;
    }
    node.as_array()
        .ok_or_else(|| format!("for_each {path:?} must name an array, found: {node}"))
}

fn item_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Substitute `{item}` / `{item.<path>}` / `{index}` in each var value
/// (spec §8.2). `{index}` is 0-based. Unknown `{...}` tokens stay verbatim.
/// A missing item path or non-scalar terminal is an error naming the var.
pub fn substitute_vars(
    vars: &BTreeMap<String, String>,
    item: &serde_json::Value,
    index: usize,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (key, template) in vars {
        let mut result = String::with_capacity(template.len());
        let mut rest = template.as_str();
        while let Some(open) = rest.find('{') {
            result.push_str(&rest[..open]);
            let Some(close) = rest[open..].find('}') else {
                // no closing brace: the remainder is literal
                result.push_str(&rest[open..]);
                rest = "";
                break;
            };
            let token = &rest[open + 1..open + close];
            if token == "index" {
                result.push_str(&index.to_string());
            } else if token == "item" {
                result.push_str(&item_string(item));
            } else if let Some(path) = token.strip_prefix("item.") {
                let mut node = item;
                for segment in path.split('.') {
                    node = node.get(segment).ok_or_else(|| {
                        format!("var {key:?}: item has no field {segment:?} (item: {item})")
                    })?;
                }
                if node.is_object() || node.is_array() {
                    return Err(format!(
                        "var {key:?}: {{item.{path}}} is not a scalar (found: {node})"
                    ));
                }
                result.push_str(&item_string(node));
            } else {
                // unknown token: verbatim (authored text, not a template language)
                result.push('{');
                result.push_str(token);
                result.push('}');
            }
            rest = &rest[open + close + 1..];
        }
        result.push_str(rest);
        out.insert(key.clone(), result);
    }
    Ok(out)
}

/// The label linking a bond child's root to the anchor that fanned it out.
pub fn bond_label(anchor: &str, index: usize) -> String {
    format!("bond:{anchor}:{index}")
}

pub fn parse_bond_label(label: &str) -> Option<(&str, usize)> {
    let rest = label.strip_prefix("bond:")?;
    let (anchor, index) = rest.rsplit_once(':')?;
    if anchor.is_empty() {
        return None;
    }
    Some((anchor, index.parse().ok()?))
}

/// PathBuf helper: the runs dir under a camp root.
pub fn runs_dir(camp_root: &Path) -> PathBuf {
    camp_root.join(RUNS_SUBDIR)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::config::RigConfig;
    use crate::event::{EventInput, EventType};
    use crate::formula::CookedRun;
    use crate::ledger::Ledger;

    fn ledger(dir: &tempfile::TempDir) -> Ledger {
        Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-07T12:00:00Z")),
        )
        .unwrap()
    }

    fn rig() -> RigConfig {
        RigConfig {
            name: "gc".into(),
            path: "/tmp".into(),
            prefix: "gc".into(),
            default_agent: None,
        }
    }

    /// Write, parse, and cook a formula into `<tempdir>/runs/`.
    fn cook_formula(
        dir: &tempfile::TempDir,
        ledger: &mut Ledger,
        name: &str,
        toml: &str,
    ) -> CookedRun {
        let path = dir.path().join(format!("{name}.toml"));
        std::fs::write(&path, toml).unwrap();
        let formula = crate::formula::parse_and_validate(&path).unwrap();
        crate::formula::cook(ledger, &formula, &runs_dir(dir.path()), &rig(), "test").unwrap()
    }

    fn create_attempt(l: &mut Ledger, id: &str, run_id: &str, step_id: &str) {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some(id.into()),
            data: serde_json::json!({
                "title": format!("attempt {id}"),
                "run_id": run_id, "step_id": step_id,
            }),
        })
        .unwrap();
    }

    fn close(l: &mut Ledger, id: &str, data: serde_json::Value) {
        l.append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data,
        })
        .unwrap();
    }

    const RETRY_SOFT: &str = "formula = \"retry-soft\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n\
        [[steps]]\nid = \"fetch\"\ntitle = \"Fetch\"\n\n[steps.retry]\nmax_attempts = 2\non_exhausted = \"soft_fail\"\n";

    #[test]
    fn attempts_order_and_budget_counters() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "retry-soft", RETRY_SOFT);
        let anchor = cooked.step_beads["fetch"].clone();
        let run = cooked.run_id.clone();
        for id in ["gc-90", "gc-9", "gc-100"] {
            create_attempt(&mut l, id, &run, "fetch");
        }
        // creation order, NOT lexicographic id order (gc-9 sorts before
        // gc-90 lexicographically; FixedClock ties every created_ts)
        let got: Vec<String> = l
            .step_attempts(&run, "fetch", &anchor)
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(got, vec!["gc-90", "gc-9", "gc-100"]);

        close(&mut l, "gc-90", serde_json::json!({"outcome":"pass"}));
        close(
            &mut l,
            "gc-9",
            serde_json::json!({"outcome":"fail","failure_class":"transient"}),
        );
        close(&mut l, "gc-100", serde_json::json!({"outcome":"fail"}));
        let attempts = l.step_attempts(&run, "fetch", &anchor).unwrap();
        assert_eq!(check_runs_used(&attempts), 1, "one passing attempt");
        assert_eq!(
            l.transient_fails_used(&attempts).unwrap(),
            1,
            "one transient fail; the hard fail does not count"
        );
    }

    #[test]
    fn unsatisfiable_walks_transitively() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let create = |l: &mut Ledger, id: &str, needs: &[&str]| {
            l.append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(id.into()),
                data: serde_json::json!({"title": id, "needs": needs}),
            })
            .unwrap();
        };
        create(&mut l, "gc-1", &[]); // will fail
        create(&mut l, "gc-2", &["gc-1"]); // blocked on the failure
        create(&mut l, "gc-3", &["gc-2"]); // transitively blocked
        create(&mut l, "gc-4", &[]); // open and healthy
        create(&mut l, "gc-5", &["gc-4"]); // blocked but satisfiable
        create(&mut l, "gc-6", &["gc-404"]); // missing dep
        close(&mut l, "gc-1", serde_json::json!({"outcome":"fail"}));
        assert!(l.unsatisfiable("gc-2").unwrap());
        assert!(l.unsatisfiable("gc-3").unwrap(), "walks through open beads");
        assert!(!l.unsatisfiable("gc-5").unwrap(), "open dep can still pass");
        assert!(l.unsatisfiable("gc-6").unwrap(), "missing dep never passes");
    }

    const TWO_STEP: &str = "formula = \"two-step\"\n\n[[steps]]\nid = \"a\"\ntitle = \"A\"\n\n\
        [[steps]]\nid = \"b\"\ntitle = \"B\"\nneeds = [\"a\"]\n";
    const TWO_INDEP: &str = "formula = \"two-indep\"\n\n[[steps]]\nid = \"a\"\ntitle = \"A\"\n\n\
        [[steps]]\nid = \"b\"\ntitle = \"B\"\n";

    fn load(dir: &tempfile::TempDir, run_id: &str) -> RunContext {
        load_run(&runs_dir(dir.path()), run_id).unwrap()
    }

    #[test]
    fn finalization_table() {
        // all pass -> (pass, pass)
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "two-indep", TWO_INDEP);
        let ctx = load(&dir, &cooked.run_id);
        assert_eq!(l.finalization(&ctx).unwrap(), RunVerdict::NotQuiescent);
        close(
            &mut l,
            &cooked.step_beads["a"],
            serde_json::json!({"outcome":"pass"}),
        );
        assert_eq!(l.finalization(&ctx).unwrap(), RunVerdict::NotQuiescent);
        close(
            &mut l,
            &cooked.step_beads["b"],
            serde_json::json!({"outcome":"pass"}),
        );
        match l.finalization(&ctx).unwrap() {
            RunVerdict::Finalize {
                outcome,
                disposition,
                soft_failed,
                skipped,
                to_skip,
            } => {
                assert_eq!((outcome, disposition), ("pass", "pass"));
                assert!(soft_failed.is_empty() && skipped.is_empty() && to_skip.is_empty());
            }
            v => panic!("expected Finalize, got {v:?}"),
        }

        // hard fail + unreachable dependent -> (fail, hard_fail), b to skip
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "two-step", TWO_STEP);
        let ctx = load(&dir, &cooked.run_id);
        close(
            &mut l,
            &cooked.step_beads["a"],
            serde_json::json!({"outcome":"fail"}),
        );
        match l.finalization(&ctx).unwrap() {
            RunVerdict::Finalize {
                outcome,
                disposition,
                skipped,
                to_skip,
                ..
            } => {
                assert_eq!((outcome, disposition), ("fail", "hard_fail"));
                assert_eq!(skipped, vec!["b".to_owned()]);
                assert_eq!(to_skip, vec![cooked.step_beads["b"].clone()]);
            }
            v => panic!("expected Finalize, got {v:?}"),
        }

        // soft fail whose dependent is skipped -> (fail, soft_fail)
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "two-step", TWO_STEP);
        let ctx = load(&dir, &cooked.run_id);
        close(
            &mut l,
            &cooked.step_beads["a"],
            serde_json::json!({"outcome":"fail","final_disposition":"soft_fail"}),
        );
        match l.finalization(&ctx).unwrap() {
            RunVerdict::Finalize {
                outcome,
                disposition,
                soft_failed,
                skipped,
                ..
            } => {
                assert_eq!((outcome, disposition), ("fail", "soft_fail"));
                assert_eq!(soft_failed, vec!["a".to_owned()]);
                assert_eq!(skipped, vec!["b".to_owned()]);
            }
            v => panic!("expected Finalize, got {v:?}"),
        }

        // soft fail with no dependents, rest pass -> (pass, soft_fail)
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "two-indep", TWO_INDEP);
        let ctx = load(&dir, &cooked.run_id);
        close(
            &mut l,
            &cooked.step_beads["a"],
            serde_json::json!({"outcome":"fail","final_disposition":"soft_fail"}),
        );
        close(
            &mut l,
            &cooked.step_beads["b"],
            serde_json::json!({"outcome":"pass"}),
        );
        match l.finalization(&ctx).unwrap() {
            RunVerdict::Finalize {
                outcome,
                disposition,
                soft_failed,
                ..
            } => {
                assert_eq!((outcome, disposition), ("pass", "soft_fail"));
                assert_eq!(soft_failed, vec!["a".to_owned()]);
            }
            v => panic!("expected Finalize, got {v:?}"),
        }

        // in_progress anchor -> NotQuiescent; closed root -> NotQuiescent
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "two-indep", TWO_INDEP);
        let ctx = load(&dir, &cooked.run_id);
        close(
            &mut l,
            &cooked.step_beads["a"],
            serde_json::json!({"outcome":"pass"}),
        );
        l.append(EventInput {
            kind: EventType::BeadClaimed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(cooked.step_beads["b"].clone()),
            data: serde_json::json!({"session":"t/dev/1"}),
        })
        .unwrap();
        assert_eq!(l.finalization(&ctx).unwrap(), RunVerdict::NotQuiescent);
        close(
            &mut l,
            &cooked.step_beads["b"],
            serde_json::json!({"outcome":"pass"}),
        );
        close(
            &mut l,
            &cooked.root_bead,
            serde_json::json!({"outcome":"pass"}),
        );
        assert_eq!(
            l.finalization(&ctx).unwrap(),
            RunVerdict::NotQuiescent,
            "an already-finalized run demands nothing"
        );
    }

    #[test]
    fn for_each_resolution_and_errors() {
        let data = serde_json::json!({"outcome":"pass",
            "output":{"items":[1,2,3],"count":3,"nested":{"deep":[{"x":1}]}}});
        assert_eq!(resolve_for_each(&data, "output.items").unwrap().len(), 3);
        assert_eq!(
            resolve_for_each(&data, "output.nested.deep").unwrap().len(),
            1
        );
        let err = resolve_for_each(&data, "output.missing").unwrap_err();
        assert!(err.contains("missing"), "{err}");
        let err = resolve_for_each(&data, "output.count").unwrap_err();
        assert!(err.contains("must name an array"), "{err}");
        let no_output = serde_json::json!({"outcome":"pass"});
        let err = resolve_for_each(&no_output, "output.items").unwrap_err();
        assert!(err.contains("--output-json"), "{err}");
    }

    #[test]
    fn var_substitution_matrix() {
        let mut vars = BTreeMap::new();
        vars.insert("name".to_owned(), "{item.name}".to_owned());
        vars.insert("pos".to_owned(), "{index}".to_owned());
        vars.insert("whole".to_owned(), "{item}".to_owned());
        vars.insert(
            "mixed".to_owned(),
            "{item.name}-{index} {unknown}".to_owned(),
        );
        let item = serde_json::json!({"name":"alpha","n":7});
        let out = substitute_vars(&vars, &item, 2).unwrap();
        assert_eq!(out["name"], "alpha");
        assert_eq!(out["pos"], "2");
        // serde_json (without preserve_order) sorts object keys: deterministic
        assert_eq!(out["whole"], r#"{"n":7,"name":"alpha"}"#);
        assert_eq!(out["mixed"], "alpha-2 {unknown}");

        // string items substitute raw
        let mut vars = BTreeMap::new();
        vars.insert("v".to_owned(), "{item}".to_owned());
        let out = substitute_vars(&vars, &serde_json::json!("plain"), 0).unwrap();
        assert_eq!(out["v"], "plain");

        // a missing item path names the var
        let mut vars = BTreeMap::new();
        vars.insert("bad".to_owned(), "{item.nope}".to_owned());
        let err = substitute_vars(&vars, &item, 0).unwrap_err();
        assert!(err.contains("bad") && err.contains("nope"), "{err}");

        // a non-scalar terminal is an error
        let mut vars = BTreeMap::new();
        vars.insert("obj".to_owned(), "{item.inner}".to_owned());
        let err = substitute_vars(&vars, &serde_json::json!({"inner":{"a":1}}), 0).unwrap_err();
        assert!(err.contains("not a scalar"), "{err}");
    }

    #[test]
    fn bond_label_round_trips() {
        assert_eq!(
            parse_bond_label(&bond_label("gc-7", 2)),
            Some(("gc-7", 2usize))
        );
        assert_eq!(parse_bond_label("not-a-bond"), None);
        assert_eq!(parse_bond_label("bond:gc-7:x"), None);
        assert_eq!(parse_bond_label("bond::1"), None);
    }

    #[test]
    fn load_run_round_trips_a_cooked_run_and_errors_on_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "two-step", TWO_STEP);
        let ctx = load(&dir, &cooked.run_id);
        assert_eq!(ctx.run_id, cooked.run_id);
        assert_eq!(ctx.root, cooked.root_bead);
        assert_eq!(ctx.rig, "gc");
        assert_eq!(
            ctx.anchors, cooked.step_beads,
            "manifest anchors round-trip"
        );
        assert!(ctx.step_ref("a").is_some());
        assert!(ctx.step_ref("zz").is_none());
        assert!(ctx.is_anchor(&cooked.step_beads["b"]));
        assert!(!ctx.is_anchor("gc-999"));
        assert!(!is_looping(ctx.step_ref("a").unwrap().step));
        // run membership: root vs step vs plain
        assert_eq!(
            l.run_membership(&cooked.root_bead).unwrap(),
            Some(RunMembership {
                run_id: cooked.run_id.clone(),
                step_id: None
            })
        );
        assert_eq!(
            l.run_membership(&cooked.step_beads["a"]).unwrap(),
            Some(RunMembership {
                run_id: cooked.run_id.clone(),
                step_id: Some("a".into())
            })
        );
        assert!(load_run(&runs_dir(dir.path()), "nope").is_err());
    }
}

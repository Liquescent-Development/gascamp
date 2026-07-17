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
    // actor / cooked_ts: audit content, not needed here.
    //
    // `vars` too, and DELIBERATELY: the recipe is already INSTANTIATED (BD-A),
    // so there is nothing left to substitute at load. The vars are pinned in the
    // manifest for audit — so an operator can see what the run was cooked with —
    // not for re-derivation.
    #[serde(flatten)]
    _rest: BTreeMap<String, serde_json::Value>,
}

/// `runs/<id>/recipe.json` — the INSTANTIATED formula (D6/BD-A).
#[derive(Deserialize)]
struct PinnedRecipe {
    recipe_version: u32,
    formula: Formula,
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
    // BD8 — deserialize the PINNED RECIPE. This used to re-parse the authored
    // `<formula>.toml` with `parse_and_validate` (no layers, no config), which
    // for any imported formula could not possibly succeed: `ctx()` turned the
    // error into `None` and every caller then DEAD-ENDED the run. Every one of
    // the 65 runnable corpus formulas would have hit it.
    //
    // Nothing here re-parses, resolves layers, or reads config. The recipe is
    // already instantiated: `{{var}}` substituted, routes resolved.
    let recipe_path = dir.join("recipe.json");
    let raw = std::fs::read_to_string(&recipe_path).map_err(|_| {
        corrupt(format!(
            "no recipe.json at {} — this run was cooked by an older camp; re-sling it",
            recipe_path.display()
        ))
    })?;
    let pinned: PinnedRecipe =
        serde_json::from_str(&raw).map_err(|e| corrupt(format!("bad recipe.json: {e}")))?;
    // STRICT equality, and it kills in-flight runs LOUDLY rather than
    // deserializing a recipe that means something else (BD-C).
    if pinned.recipe_version != crate::formula::cook::RECIPE_VERSION {
        return Err(corrupt(format!(
            "was cooked by a different camp (recipe v{}, this camp reads v{}) — re-sling it",
            pinned.recipe_version,
            crate::formula::cook::RECIPE_VERSION
        )));
    }
    let formula = pinned.formula;
    // `Formula::source` is `#[serde(skip)]` (BD-C: the authored bytes are already
    // pinned verbatim next to the recipe, and duplicating them here would double the
    // run dir). So a formula reconstituted from `recipe.json` carries an EMPTY
    // source — by design, and NOTHING reads it today.
    //
    // It is asserted rather than merely commented, because a future caller reaching
    // for `.source` off a reloaded formula would silently get `""` and behave as if
    // the file were empty. Make that structural: if the invariant ever changes, this
    // fires in dev before it ships.
    debug_assert!(
        formula.source.is_empty(),
        "a formula reloaded from recipe.json has no authored source (serde skip); \
         read <run>/<formula>.toml if you need the bytes"
    );
    // The two pinned artifacts must agree about what was cooked. They are written
    // in the same transaction, so a mismatch means the run dir was edited or
    // corrupted — never a thing to shrug at.
    if formula.name != manifest.formula {
        return Err(corrupt(format!(
            "manifest names formula {:?} but recipe.json holds {:?}",
            manifest.formula, formula.name
        )));
    }
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

/// A CAMPD-HELD step: campd claims its anchor and drives it, and NO WORKER is
/// ever dispatched for the anchor itself.
///
/// gc calls a drain's anchor a *"controller-owned control bead"* (`types.go:318`),
/// and that is exactly what this is. Two shapes hold an anchor, and they hold it
/// for DIFFERENT reasons — which is why `is_campd_held` is not a rename of
/// [`is_looping`]:
///
/// * **looping** (`check`/`retry`) — campd claims the anchor and dispatches
///   worker ATTEMPTS against it. The attempts are the mechanism.
/// * **drain** — campd claims the anchor and SCATTERS one item run per run
///   member. There are no attempts, and a worker for the anchor would be a
///   worker for a step that has no work (§13's money invariant).
pub fn is_campd_held(step: &Step) -> bool {
    is_looping(step) || step.drain.is_some()
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

/// Every bead of a run: (id, step_id) — step_id None is the root. Used by
/// the daemon's dead-end path when a run dir is unreadable and the pinned
/// structure is gone (the ledger still knows the beads).
pub fn run_bead_ids(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<(String, Option<String>)>, CoreError> {
    let mut stmt = conn.prepare("SELECT id, step_id FROM beads WHERE run_id = ?1 ORDER BY id")?;
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([run_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The data of a bead's creation event (title/description as authored,
/// with any bond vars already substituted).
pub fn created_event_data(
    conn: &Connection,
    bead: &str,
) -> Result<Option<serde_json::Value>, CoreError> {
    use rusqlite::OptionalExtension;
    let raw: Option<String> = conn
        .query_row(
            "SELECT data FROM events WHERE bead = ?1 AND type = 'bead.created'
             ORDER BY seq LIMIT 1",
            [bead],
            |r| r.get(0),
        )
        .optional()?;
    match raw {
        None => Ok(None),
        Some(text) => Ok(Some(serde_json::from_str(&text).map_err(|e| {
            CoreError::Corrupt(format!("bead {bead} created data is not JSON: {e}"))
        })?)),
    }
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
/// whose own needs can still pass keep the bead satisfiable. Formula
/// cooking cannot produce cycles (validate rejects them; cross-run edges
/// point at closed roots), but the general append path CAN (a forward
/// reference closed into a loop) — a revisit is a Corrupt error naming
/// the cycle, never unbounded recursion (review LOW 5).
pub fn unsatisfiable(conn: &Connection, bead: &str) -> Result<bool, CoreError> {
    let mut gray = std::collections::HashSet::new();
    let mut black = std::collections::HashMap::new();
    unsatisfiable_walk(conn, bead, &mut gray, &mut black)
}

/// DFS with proper three-color bookkeeping (fix-pass HIGH). `gray` is the
/// CURRENT recursion path only — a need already gray is a genuine cycle;
/// it is removed before every return, so a diamond's shared ancestor,
/// reached again on a second path, is not mistaken for one. `black`
/// memoizes fully-computed beads so a dense DAG stays O(V+E) rather than
/// exponential (only completed results are memoized — never a partial
/// answer for a bead still on the path).
fn unsatisfiable_walk(
    conn: &Connection,
    bead: &str,
    gray: &mut std::collections::HashSet<String>,
    black: &mut std::collections::HashMap<String, bool>,
) -> Result<bool, CoreError> {
    if let Some(&known) = black.get(bead) {
        return Ok(known);
    }
    if !gray.insert(bead.to_owned()) {
        return Err(CoreError::Corrupt(format!(
            "needs cycle involving bead {bead:?} — the dependency graph must be acyclic"
        )));
    }
    // compute with the bead gray, then ALWAYS ungray before propagating
    let result = unsatisfiable_needs(conn, bead, gray, black);
    gray.remove(bead);
    let result = result?;
    black.insert(bead.to_owned(), result);
    Ok(result)
}

/// The needs iteration for `unsatisfiable_walk` (a separate fn so its
/// several early returns all funnel back through the caller's
/// `gray.remove`). A missing or closed-non-pass need is terminal; an open
/// need recurses.
fn unsatisfiable_needs(
    conn: &Connection,
    bead: &str,
    gray: &mut std::collections::HashSet<String>,
    black: &mut std::collections::HashMap<String, bool>,
) -> Result<bool, CoreError> {
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
                } else if unsatisfiable_walk(conn, &need, gray, black)? {
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

/// gc's `defaultDrainMaxUnits` (`drain.go:24`). The authored KEY `drain.max_units`
/// is refused by name (0 corpus uses); this is gc's RUNTIME cap, and camp honours
/// it: a drain whose member set exceeds it CLOSES `fail` and scatters NOTHING
/// (`drain.go:244-255`, reason `limit_exceeded`).
pub const DRAIN_MAX_UNITS: usize = crate::formula::drain::DEFAULT_MAX_UNITS;

/// A run MEMBER (D3): a bead with this run's `run_id`, **no `step_id`**,
/// `type = 'task'`, and **not closed**.
///
/// gc: `convoycore.Members(store, id, includeClosed=false, …)`
/// (`membership.go:96-144` — *"if !includeClosed && IsTerminalStatus(b.Status)
/// { return }"*). A CLOSED member is not a member: it is finished work, and
/// scattering an item run over it would redo it.
///
/// Members are added by `camp create --run <run_id>`. The run ROOT is excluded
/// (it also has `step_id IS NULL`), and so is anything wearing a `bond:` or
/// `drain:` label — those are cooked run roots, not work the operator handed the
/// run.
pub fn run_members(conn: &Connection, ctx: &RunContext) -> Result<Vec<BeadRow>, CoreError> {
    let sql = format!(
        "SELECT {cols} FROM beads b
         WHERE b.run_id = ?1 AND b.step_id IS NULL AND b.type = 'task' AND b.status <> 'closed'
           AND b.id <> ?2
           AND b.labels NOT LIKE '%\"bond:%' AND b.labels NOT LIKE '%\"drain:%'
         ORDER BY (SELECT MIN(e.seq) FROM events e WHERE e.bead = b.id AND e.type = 'bead.created'), b.id",
        cols = crate::readiness::BEAD_COLS,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<BeadRow> = stmt
        .query_map(rusqlite::params![ctx.run_id, ctx.root], |r| {
            crate::readiness::row_to_bead(r)
        })?
        .collect::<rusqlite::Result<_>>()?;
    // The `NOT LIKE`s above are NOT a prefilter — they are the OPERATIVE exclusion, and
    // they are BROADER than this re-parse. That is the opposite of `bond_children` /
    // `drain_children`, where a POSITIVE `LIKE` selects candidates and the re-parse
    // narrows them; inverting the LIKE inverts the relationship, and this comment used
    // to claim the `bond_children` shape here. It was wrong, and expensively so — a test
    // harness read it, reproduced the label rule as "call the two parsers", and shipped
    // a hole (see `close_member_REFUSES_a_MALFORMED_LABELLED_nonroot` in
    // `camp/tests/daemon_drain.rs`).
    //
    // Concretely: a MALFORMED label like `drain:gc-999` (no index) is EXCLUDED by the
    // SQL — labels serialize as a JSON array, so it appears as `"drain:` — while
    // `parse_drain_label` returns `None` for it and would ADMIT it. The SQL drops beads
    // the parsers accept as members.
    //
    // So this filter cannot drop a row the SQL kept: every label a parser accepts starts
    // with `bond:`/`drain:`, hence appears as `"bond:`/`"drain:` in the column, hence was
    // already excluded. Verified by deleting it — the workspace suite stays green. It is
    // kept as belt-and-braces: if the SQL exclusion is ever narrowed, correctness must
    // not silently depend on the LIKE alone.
    Ok(rows
        .into_iter()
        .filter(|row| {
            !row.labels
                .iter()
                .any(|l| parse_bond_label(l).is_some() || parse_drain_label(l).is_some())
        })
        .collect())
}

/// The label linking a drain item run's ROOT to the anchor that scattered it.
pub fn drain_label(anchor: &str, index: usize) -> String {
    format!("drain:{anchor}:{index}")
}

pub fn parse_drain_label(label: &str) -> Option<(&str, usize)> {
    let rest = label.strip_prefix("drain:")?;
    let (anchor, index) = rest.rsplit_once(':')?;
    if anchor.is_empty() {
        return None;
    }
    Some((anchor, index.parse().ok()?))
}

/// The item runs already scattered for a drain anchor, by index. The
/// `bond_children` mold exactly — a LIKE prefilter, then a real label re-parse.
pub fn drain_children(
    conn: &Connection,
    anchor: &str,
) -> Result<std::collections::BTreeMap<usize, BeadRow>, CoreError> {
    let escaped = anchor
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%\"drain:{escaped}:%");
    let mut stmt = conn.prepare(
        "SELECT id, labels FROM beads
         WHERE run_id IS NOT NULL AND step_id IS NULL
           AND labels LIKE ?1 ESCAPE '\\'",
    )?;
    let candidates: Vec<(String, String)> = stmt
        .query_map([pattern], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut children = std::collections::BTreeMap::new();
    for (id, labels_json) in candidates {
        let labels: Vec<String> = serde_json::from_str(&labels_json)
            .map_err(|e| CoreError::Corrupt(format!("bead {id} labels are not JSON: {e}")))?;
        for label in &labels {
            if let Some((parent, index)) = parse_drain_label(label)
                && parent == anchor
            {
                let row = crate::readiness::get_bead(conn, &id)?
                    .ok_or_else(|| CoreError::Corrupt(format!("bead {id} vanished mid-query")))?;
                children.insert(index, row);
                break;
            }
        }
    }
    Ok(children)
}

/// Every member THIS anchor currently holds a reservation on.
///
/// **Status-agnostic, and that is the point (V-4).** `run_members` filters
/// `status <> 'closed'`, so a member that CLOSED while its item run was in flight
/// is invisible to it — and a release loop built on `run_members` would skip that
/// member and leave its reservation held FOREVER. A reservation is a fact about
/// `bead_meta`, so it is released by asking `bead_meta`.
pub fn reservations_held_by(conn: &Connection, anchor: &str) -> Result<Vec<BeadRow>, CoreError> {
    let sql = format!(
        "SELECT {cols} FROM beads b
           JOIN bead_meta m ON m.bead_id = b.id
          WHERE m.key = ?1 AND m.value = ?2
          ORDER BY b.id",
        cols = crate::readiness::BEAD_COLS,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![crate::readiness::EXCLUSIVE_DRAIN_RESERVATION, anchor],
        crate::readiness::row_to_bead,
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Reservations whose holding anchor is CLOSED or GONE — orphans.
///
/// A `kill -9` between the reserve batch and the cook leaves members held by an
/// anchor that will never gather them. Without a sweep they are held FOREVER and
/// no other drain can ever take them. Returns `(member_bead, holding_anchor)`.
pub fn orphaned_reservations(conn: &Connection) -> Result<Vec<(String, String)>, CoreError> {
    let mut stmt = conn.prepare(
        "SELECT m.bead_id, m.value
           FROM bead_meta m
           LEFT JOIN beads a ON a.id = m.value
          WHERE m.key = ?1
            AND (a.id IS NULL OR a.status = 'closed')
          ORDER BY m.bead_id",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([crate::readiness::EXCLUSIVE_DRAIN_RESERVATION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The label linking a bond child's root to the anchor that fanned it out.
pub fn bond_label(anchor: &str, index: usize) -> String {
    format!("bond:{anchor}:{index}")
}

/// The bond children already cooked for an anchor, by index: run ROOT
/// beads whose labels parse as `bond:<anchor>:<i>`. One beads-table query
/// narrowed by a LIKE prefilter on the folded labels column (review
/// LOW 4 — the events-scan predecessor cost O(total runs) per chain
/// link); the Rust-side `parse_bond_label` re-check keeps correctness
/// independent of the prefilter (decoy substrings cannot slip through).
pub fn bond_children(
    conn: &Connection,
    anchor: &str,
) -> Result<std::collections::BTreeMap<usize, BeadRow>, CoreError> {
    // labels serialize as a JSON array of strings, so a real bond label
    // appears as `"bond:<anchor>:` — escape LIKE metacharacters in the id
    let escaped = anchor
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%\"bond:{escaped}:%");
    let mut stmt = conn.prepare(
        "SELECT id, labels FROM beads
         WHERE run_id IS NOT NULL AND step_id IS NULL
           AND labels LIKE ?1 ESCAPE '\\'",
    )?;
    let candidates: Vec<(String, String)> = stmt
        .query_map([pattern], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut children = std::collections::BTreeMap::new();
    for (id, labels_json) in candidates {
        let labels: Vec<String> = serde_json::from_str(&labels_json)
            .map_err(|e| CoreError::Corrupt(format!("bead {id} labels are not JSON: {e}")))?;
        for label in &labels {
            if let Some((parent, index)) = parse_bond_label(label)
                && parent == anchor
            {
                let row = crate::readiness::get_bead(conn, &id)?
                    .ok_or_else(|| CoreError::Corrupt(format!("bead {id} vanished mid-query")))?;
                children.insert(index, row);
                break;
            }
        }
    }
    Ok(children)
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

/// How long a `runs/<id>/` dir must have sat UNTOUCHED before the sweep will
/// remove it (#124).
///
/// THE RACE this exists for: a run dir that exists with no `run.cooked` is
/// EXACTLY the state a healthy in-flight cook is in for the moment between its
/// run-dir write and its `append_batch` commit (see `cook.rs`'s header — files
/// land first, deliberately). "No `run.cooked` → delete" is therefore, on its
/// own, a rule that deletes LIVE run state.
///
/// cook writes three small files and commits in the same breath — milliseconds.
/// Ten minutes is ~5 orders of magnitude of headroom, and the cost of erring
/// long is zero: an orphan the sweep declines today is swept tomorrow. The cost
/// of erring short is a destroyed run. That asymmetry sets this number.
pub const ORPHAN_RUN_SWEEP_GRACE: std::time::Duration = std::time::Duration::from_secs(600);

/// A `runs/<id>/` directory that no `run.cooked` event names (#124) — the
/// leftover of a `kill -9` inside cook's files-before-ledger window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanRunDir {
    pub run_id: String,
    pub path: PathBuf,
    /// How long the dir has sat untouched. `None` when the mtime is
    /// unreadable OR in the future — an age we cannot compute is an age that
    /// cannot clear the grace window, so it reads as NOT sweepable.
    pub idle: Option<std::time::Duration>,
}

impl OrphanRunDir {
    /// May the sweep remove this? Only when we can prove the dir has been
    /// untouched longer than any cook could plausibly still be writing to it.
    /// Unknown age → false, always: when in doubt, do not delete.
    pub fn sweepable(&self) -> bool {
        self.idle.is_some_and(|idle| idle >= ORPHAN_RUN_SWEEP_GRACE)
    }
}

/// Every run id the LEDGER names in a `run.cooked` event.
///
/// The id set is derived from the log, never from filesystem heuristics: the
/// log is the durable truth (invariant 3), and "this dir looks old" is not
/// evidence about whether a run exists. Bounded by the `events_type` index.
fn cooked_run_ids(conn: &Connection) -> Result<std::collections::BTreeSet<String>, CoreError> {
    let mut stmt = conn.prepare(
        "SELECT json_extract(data, '$.run_id') FROM events
          WHERE type = 'run.cooked' AND json_extract(data, '$.run_id') IS NOT NULL",
    )?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The `runs/<id>/` dirs no `run.cooked` event names (#124). READ-ONLY.
///
/// ORDERING IS THE SAFETY PROPERTY: the directories are enumerated FIRST and
/// the ledger read SECOND. A cook that commits during the scan then lands in
/// the cooked set and its dir is correctly excluded; a dir created after the
/// enumeration is not considered at all. The reverse order (ledger, then dirs)
/// would flag every dir born in between as an orphan, which is the bug this
/// whole feature could most easily have shipped.
///
/// A missing `runs/` is a camp that has never cooked, not an error.
pub fn orphaned_run_dirs(
    conn: &Connection,
    camp_root: &Path,
) -> Result<Vec<OrphanRunDir>, CoreError> {
    let root = runs_dir(camp_root);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(CoreError::Cook(format!(
                "cannot read {}: {e}",
                root.display()
            )));
        }
    };
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| CoreError::Cook(format!("cannot read {}: {e}", root.display())))?;
        // Directories only. A stray FILE under runs/ is not a run dir, and
        // this sweep's job is not to have opinions about it.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(run_id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        dirs.push((run_id, entry.path()));
    }
    // …dirs enumerated. NOW the ledger.
    let cooked = cooked_run_ids(conn)?;
    let mut orphans: Vec<OrphanRunDir> = dirs
        .into_iter()
        .filter(|(run_id, _)| !cooked.contains(run_id))
        .map(|(run_id, path)| {
            let idle = dir_idle(&path);
            OrphanRunDir { run_id, path, idle }
        })
        .collect();
    orphans.sort_by(|a, b| a.run_id.cmp(&b.run_id));
    Ok(orphans)
}

/// How long since this dir was last written. `None` on any unreadable mtime or
/// an mtime in the FUTURE (a clock skew we cannot reason about) — both mean
/// "cannot prove it is idle", which the caller reads as "do not delete".
fn dir_idle(path: &Path) -> Option<std::time::Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    std::time::SystemTime::now().duration_since(modified).ok()
}

/// Remove the orphaned run dirs that clear the grace window; return exactly
/// what was removed. NEVER called automatically — not by `reconcile`, not on
/// campd start. The operator asks, or it does not happen (#124).
///
/// The caller is responsible for the OTHER half of the race defense (campd
/// must be down); this half is the grace window, re-checked against a FRESH
/// stat immediately before each removal so a dir a cook touched since the scan
/// is spared.
pub fn sweep_orphan_run_dirs(
    conn: &Connection,
    camp_root: &Path,
) -> Result<Vec<OrphanRunDir>, CoreError> {
    let mut swept = Vec::new();
    for orphan in orphaned_run_dirs(conn, camp_root)? {
        let fresh = OrphanRunDir {
            idle: dir_idle(&orphan.path),
            ..orphan
        };
        if !fresh.sweepable() {
            continue;
        }
        std::fs::remove_dir_all(&fresh.path)
            .map_err(|e| CoreError::Cook(format!("cannot remove {}: {e}", fresh.path.display())))?;
        swept.push(fresh);
    }
    Ok(swept)
}

/// The dead-end batch for a run that can never advance (its pinned dir is
/// unreadable, so campd has no structure to execute): close every open
/// run bead `skipped`, the root `fail`, and finalize hard — evented,
/// never silent. Empty when the root is already closed (idempotent) or
/// the run has no beads. Pure input construction; the caller appends
/// (cursor-atomically in the processor, or as one batch from reconcile).
pub fn dead_end_inputs(
    conn: &Connection,
    run_id: &str,
    cause_seq: i64,
    reason: &str,
) -> Result<Vec<crate::event::EventInput>, CoreError> {
    use crate::event::{EventInput, EventType};
    let beads = run_bead_ids(conn, run_id)?;
    let Some((root_id, _)) = beads.iter().find(|(_, step)| step.is_none()) else {
        return Ok(Vec::new());
    };
    let Some(root_row) = crate::readiness::get_bead(conn, root_id)? else {
        return Ok(Vec::new());
    };
    if root_row.status == "closed" {
        return Ok(Vec::new());
    }
    let mut inputs = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for (id, step_id) in &beads {
        let Some(step_id) = step_id else { continue };
        let Some(row) = crate::readiness::get_bead(conn, id)? else {
            continue;
        };
        if row.status != "closed" {
            inputs.push(EventInput {
                kind: EventType::BeadClosed,
                rig: Some(row.rig.clone()),
                actor: "campd".into(),
                bead: Some(id.clone()),
                data: serde_json::json!({ "outcome": "skipped", "reason": reason }),
            });
            if !skipped.contains(step_id) {
                skipped.push(step_id.clone());
            }
        }
    }
    inputs.push(EventInput {
        kind: EventType::BeadClosed,
        rig: Some(root_row.rig.clone()),
        actor: "campd".into(),
        bead: Some(root_id.clone()),
        data: serde_json::json!({ "outcome": "fail", "reason": reason }),
    });
    inputs.push(EventInput {
        kind: EventType::RunFinalized,
        rig: Some(root_row.rig.clone()),
        actor: "campd".into(),
        bead: Some(root_id.clone()),
        data: serde_json::json!({
            "run_id": run_id,
            "root": root_id,
            "outcome": "fail",
            "final_disposition": "hard_fail",
            "cause_seq": cause_seq,
            "soft_failed": [],
            "skipped": skipped,
        }),
    });
    Ok(inputs)
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

    /// Fix-pass HIGH: a diamond DAG revisits its shared ancestor on a
    /// SECOND path — that is not a cycle. "Ever seen" tracking
    /// misclassified it; only the current recursion path (gray set) may
    /// trigger the cycle error.
    #[test]
    fn a_diamond_dag_with_a_shared_open_ancestor_is_not_a_cycle() {
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
        create(&mut l, "gc-1", &[]); // A: open, healthy
        create(&mut l, "gc-2", &["gc-1"]); // B
        create(&mut l, "gc-3", &["gc-1"]); // C
        create(&mut l, "gc-4", &["gc-2", "gc-3"]); // D: both paths reach A
        assert!(
            !l.unsatisfiable("gc-4").unwrap(),
            "an acyclic diamond with an open ancestor is satisfiable"
        );
        // and the memo returns the RESULT, not a false negative: fail A
        // and both paths must report unsatisfiable
        close(&mut l, "gc-1", serde_json::json!({"outcome":"fail"}));
        assert!(l.unsatisfiable("gc-4").unwrap());
    }

    /// Fix-pass HIGH, end to end: a sink-first diamond formula (valid per
    /// S6/S7 — forward references are legal) cooks fine, and finalization
    /// of the fresh all-open run is NotQuiescent — never Corrupt. On the
    /// broken head this errored on EVERY settle of the run, wedging it.
    #[test]
    fn finalization_of_a_sink_first_diamond_is_not_a_false_cycle() {
        const SINK_FIRST: &str = "formula = \"sink-first\"\n\n\
            [[steps]]\nid = \"release\"\ntitle = \"R\"\nneeds = [\"implement\", \"document\"]\n\n\
            [[steps]]\nid = \"implement\"\ntitle = \"I\"\nneeds = [\"design\"]\n\n\
            [[steps]]\nid = \"document\"\ntitle = \"D\"\nneeds = [\"design\"]\n\n\
            [[steps]]\nid = \"design\"\ntitle = \"Des\"\n";
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let cooked = cook_formula(&dir, &mut l, "sink-first", SINK_FIRST);
        let ctx = load(&dir, &cooked.run_id);
        assert_eq!(
            l.finalization(&ctx).unwrap(),
            RunVerdict::NotQuiescent,
            "a fresh valid diamond run is simply not quiescent"
        );
    }

    /// Review LOW 5: a needs cycle IS constructible through the normal
    /// append path (A needs B before B exists, then B needs A) — the walk
    /// must fail fast naming the cycle, never recurse unboundedly inside
    /// the cursor transaction.
    #[test]
    fn a_needs_cycle_is_a_corrupt_error_not_a_stack_overflow() {
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
        create(&mut l, "gc-1", &["gc-2"]); // forward reference
        create(&mut l, "gc-2", &["gc-1"]); // closes the cycle
        create(&mut l, "gc-3", &["gc-1"]); // hangs off the cycle
        for bead in ["gc-1", "gc-2", "gc-3"] {
            match l.unsatisfiable(bead) {
                Err(crate::error::CoreError::Corrupt(message)) => {
                    assert!(message.contains("cycle"), "{message}");
                }
                other => panic!("{bead}: expected Corrupt(cycle), got {other:?}"),
            }
        }
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

    /// Review LOW 4: bond-children lookup is a beads-table label query,
    /// and decoys cannot slip through — a similar label on a STEP bead, a
    /// label whose anchor merely shares a prefix, and a non-bond label
    /// containing the substring are all excluded by the Rust re-parse.
    #[test]
    fn bond_children_finds_roots_by_label_and_rejects_decoys() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger(&dir);
        let create_labeled = |l: &mut Ledger,
                              id: &str,
                              labels: serde_json::Value,
                              run_id: Option<&str>,
                              step_id: Option<&str>| {
            let mut data = serde_json::json!({"title": id, "labels": labels});
            if let Some(r) = run_id {
                data["run_id"] = serde_json::json!(r);
            }
            if let Some(st) = step_id {
                data["step_id"] = serde_json::json!(st);
            }
            l.append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some(id.into()),
                data,
            })
            .unwrap();
        };
        // real children of gc-7 (run roots)
        create_labeled(
            &mut l,
            "gc-10",
            serde_json::json!(["bond:gc-7:0"]),
            Some("r1"),
            None,
        );
        create_labeled(
            &mut l,
            "gc-11",
            serde_json::json!(["bond:gc-7:1"]),
            Some("r2"),
            None,
        );
        // decoy: same label on a STEP bead (not a root)
        create_labeled(
            &mut l,
            "gc-12",
            serde_json::json!(["bond:gc-7:2"]),
            Some("r3"),
            Some("s"),
        );
        // decoy: a DIFFERENT anchor sharing the prefix
        create_labeled(
            &mut l,
            "gc-13",
            serde_json::json!(["bond:gc-70:0"]),
            Some("r4"),
            None,
        );
        // decoy: a non-bond label containing the substring mid-text
        create_labeled(
            &mut l,
            "gc-14",
            serde_json::json!(["notes about \"bond:gc-7:9\" elsewhere"]),
            Some("r5"),
            None,
        );
        let children = l.bond_children("gc-7").unwrap();
        let got: Vec<(usize, String)> = children.into_iter().map(|(i, row)| (i, row.id)).collect();
        assert_eq!(
            got,
            vec![(0, "gc-10".to_owned()), (1, "gc-11".to_owned())],
            "exactly the real root children, in index order"
        );
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

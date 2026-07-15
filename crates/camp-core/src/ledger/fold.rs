//! The fold: how each event type mutates the state tables. `apply` runs
//! inside the same transaction that inserts the event row (spec §7.2), and
//! `refold` replays it against a shadow database — it must stay a pure
//! function of (state, event).

use std::collections::BTreeMap;

use rusqlite::{Connection, params};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::CoreError;
use crate::event::{Event, EventType};

const BEAD_TYPES: &[&str] = &["task", "mail", "memory"];

pub(crate) fn apply(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    match event.kind {
        EventType::BeadCreated => bead_created(conn, event),
        EventType::BeadClaimed => bead_claimed(conn, event),
        EventType::BeadUpdated => bead_updated(conn, event),
        EventType::BeadClosed => bead_closed(conn, event),
        EventType::SessionWoke => session_woke(conn, event),
        EventType::SessionStopped => session_ended(conn, event, "stopped"),
        EventType::SessionCrashed => session_ended(conn, event, "crashed"),
        EventType::SessionNudged => session_nudged(conn, event),
        EventType::RigAdded => rig_added(event),
        EventType::CampdAutostarted => campd_autostarted(event),
        EventType::RunCooked => run_cooked(event),
        EventType::OrderFired => order_fired(event),
        EventType::OrderCompleted => order_completed(event),
        EventType::OrderFailed => order_failed(event),
        EventType::ConfigChanged => config_changed(event),
        EventType::WorkerMilestone => worker_milestone(conn, event),
        EventType::WorktreeKept => worktree_kept(conn, event),
        EventType::BeadWorktreeReaped => bead_worktree_reaped(conn, event),
        EventType::DispatchFailed => dispatch_failed(conn, event),
        EventType::DispatchRearmed => dispatch_rearmed(conn, event),
        EventType::DispatchLiveTree => dispatch_live_tree(conn, event),
        EventType::CheckPassed => check_passed(conn, event),
        EventType::CheckFailed => check_failed(conn, event),
        EventType::RunFinalized => run_finalized(conn, event),
        EventType::AgentStalled => agent_stalled(conn, event),
        EventType::PatrolDegraded => patrol_degraded(event),
        // cp-0: declarative — the cause event; the reap's session.crashed
        // carries the session-end state. No fold state changes here.
        EventType::SessionStreamCapped => Ok(()),
        // Log-only events: no state effect.
        EventType::CampdStarted | EventType::CampdStopped => Ok(()),
        // cp-1 (§2.1/§4.4): the control plane's four events are AUDIT-ONLY —
        // durable truth with no state fold. Each is parsed against a
        // `deny_unknown_fields` struct and DISCARDED: the parse is the
        // validation. A typo'd key is refused AT APPEND, so a malformed event
        // can never reach the ledger and be replayed forever.
        EventType::SessionInterrupted => audit::<SessionInterrupted>(event),
        EventType::ControlResponded => audit::<ControlResponded>(event),
        EventType::ControlFailed => control_failed(event),
        EventType::SubscriberDropped => audit::<SubscriberDropped>(event),
        // compat §7/§5.4: import audit events — durable in the ledger, no
        // state fold (the materialized tree under <root>/imports/ is the
        // state, owned by `camp import`).
        EventType::ImportAdded | EventType::ImportRefused => Ok(()),
        EventType::FormulaRefused => formula_refused(event),
        // compat §6 (worker contract): AUDIT-ONLY — durable truth, no state
        // fold. The `deny_unknown_fields` parse IS the append-time validation
        // (never a bare `=> Ok(())` that would drop it, B7). A malformed shim
        // event is refused at append, not stored and replayed forever.
        EventType::ShimRefused => audit::<ShimRefused>(event),
        EventType::WorkerDrainAcked => audit::<WorkerDrainAcked>(event),
    }
}

/// cp-1: parse-and-discard. The payload struct exists to VALIDATE the shape
/// (`deny_unknown_fields`) at append time, never to be read back — the
/// fold.rs:541 precedent. An event whose shape is wrong is REFUSED, not
/// stored and hoped over.
fn audit<T: DeserializeOwned>(event: &Event) -> Result<(), CoreError> {
    let _validated: T = payload(event)?;
    Ok(())
}

fn payload<T: DeserializeOwned>(event: &Event) -> Result<T, CoreError> {
    serde_json::from_value(event.data.clone()).map_err(|e| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason: e.to_string(),
    })
}

fn required_bead(event: &Event) -> Result<&str, CoreError> {
    event
        .bead
        .as_deref()
        .ok_or_else(|| CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "missing bead id".to_owned(),
        })
}

fn bead_status(conn: &Connection, id: &str) -> Result<Option<String>, CoreError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT status FROM beads WHERE id = ?1", [id], |r| r.get(0))
        .optional()?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadCreated {
    title: String,
    #[serde(rename = "type", default = "default_bead_type")]
    bead_type: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    needs: Vec<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    step_id: Option<String>,
    /// compat §6.1 — gc's step metadata, carried onto the bead verbatim.
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

fn default_bead_type() -> String {
    "task".to_owned()
}

fn bead_created(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let rig = event
        .rig
        .as_deref()
        .ok_or_else(|| CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "missing rig".to_owned(),
        })?;
    let p: BeadCreated = payload(event)?;
    if !BEAD_TYPES.contains(&p.bead_type.as_str()) {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("unknown bead type {:?}", p.bead_type),
        });
    }
    // An empty title is unusable everywhere downstream (bd import rejects
    // empty-title issues and silently drops empty-value memories — spec
    // §15.3 export bridge, PR #18 review finding 1). Fail fast at the
    // creation boundary so no consumer ever sees one.
    if p.title.trim().is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "title must be non-empty".to_owned(),
        });
    }
    if bead_status(conn, id)?.is_some() {
        return Err(CoreError::InvalidTransition {
            bead: id.to_owned(),
            reason: "bead already exists".to_owned(),
        });
    }
    conn.execute(
        "INSERT INTO beads (id, rig, type, title, description, status, assignee, labels,
                            run_id, step_id, created_ts, updated_ts)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            id,
            rig,
            p.bead_type,
            p.title,
            p.description,
            p.assignee,
            serde_json::to_string(&p.labels)?,
            p.run_id,
            p.step_id,
            event.ts,
        ],
    )?;
    for needs_id in &p.needs {
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO deps (bead_id, needs_id) VALUES (?1, ?2)",
            params![id, needs_id],
        )?;
        if inserted == 0 {
            return Err(CoreError::InvalidEventData {
                event_type: event.kind.as_str().to_owned(),
                reason: format!("duplicate needs entry {needs_id:?}"),
            });
        }
    }
    for (key, value) in &p.metadata {
        write_meta(conn, event, id, key, value)?;
    }
    conn.execute(
        "INSERT INTO search (bead_id, kind, content) VALUES (?1, 'body', ?2)",
        params![id, format!("{}\n{}", p.title, p.description)],
    )?;
    crate::id::bump_counter(conn, id)?;
    Ok(())
}

/// Set one metadata key, applying the two rules that make `bead_meta` a single
/// source of truth.
///
/// **1. A key with a dedicated column is REFUSED**, naming the column
/// ([`PROJECTED_METADATA`]). Otherwise a bead could carry a routing target in
/// its `assignee` column AND a different one in its metadata, and nothing would
/// be wrong enough to fail.
///
/// **2. The reservation is a COMPARE-AND-SET, and it lives HERE, in the fold.**
/// A read-then-append in the caller would be a real TOCTOU race: two drains
/// could both read "unheld" and both append. The fold is the only place where
/// the check and the write are the same transaction — `append` rolls back
/// entirely on `Err` ("rejections appended nothing"), and `build_shadow`
/// replays the ACCEPTED log through this same function, so the CAS is a pure
/// function of the accepted prefix and the refold reproduces it exactly.
fn write_meta(
    conn: &Connection,
    event: &Event,
    bead: &str,
    key: &str,
    value: &str,
) -> Result<(), CoreError> {
    if let Some((_, column)) = crate::readiness::PROJECTED_METADATA
        .iter()
        .find(|(k, _)| *k == key)
    {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!(
                "metadata key {key:?} has a dedicated column (`beads.{column}`) and is projected \
                 from it at read; write the column, not the metadata — two sources of truth for \
                 one fact is not a thing camp keeps"
            ),
        });
    }
    if key == crate::readiness::EXCLUSIVE_DRAIN_RESERVATION {
        use rusqlite::OptionalExtension;
        let held: Option<String> = conn
            .query_row(
                "SELECT value FROM bead_meta WHERE bead_id = ?1 AND key = ?2",
                params![bead, key],
                |r| r.get(0),
            )
            .optional()?;
        // Same holder re-reserving is idempotent; a DIFFERENT holder is the
        // conflict the reservation exists to prevent, and it NAMES the holder.
        if let Some(holder) = held
            && holder != value
        {
            return Err(CoreError::InvalidEventData {
                event_type: event.kind.as_str().to_owned(),
                reason: format!(
                    "bead {bead} is already reserved by drain {holder}; drain {value} cannot take \
                     it — two drains must never mutate one bead"
                ),
            });
        }
    }
    conn.execute(
        "INSERT INTO bead_meta (bead_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (bead_id, key) DO UPDATE SET value = excluded.value",
        params![bead, key, value],
    )?;
    Ok(())
}

use crate::vocab::{CAMP_FINAL_DISPOSITIONS, CAMP_OUTCOMES, CAMP_RUN_DISPOSITIONS};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadClaimed {
    session: String,
    /// compat §6.1 — the dispatch branch, projected as `gc.work_branch`
    /// (`beads.work_branch`). The route is NOT here: cook owns `beads.assignee`
    /// (= `gc.routed_to`) and the claim must not re-derive it from `GC_AGENT`
    /// env (round-1 B1). `None` (camp's own `camp claim`) leaves the column
    /// untouched.
    #[serde(default)]
    work_branch: Option<String>,
}

fn bead_claimed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let p: BeadClaimed = payload(event)?;
    match bead_status(conn, id)?.as_deref() {
        None => Err(CoreError::UnknownBead(id.to_owned())),
        Some("open") => {
            // `assignee` (the cooked route → `gc.routed_to`) is deliberately
            // NOT in this UPDATE — cook owns it. `COALESCE` leaves work_branch
            // untouched when the claim carries none.
            conn.execute(
                "UPDATE beads SET status = 'in_progress', claimed_by = ?1,
                                  work_branch = COALESCE(?2, work_branch),
                                  updated_ts = ?3
                 WHERE id = ?4",
                params![p.session, p.work_branch, event.ts, id],
            )?;
            // Phase 3 (#48 finding 2): a claim means the work is under way
            // — the fail-fast dispatch marker no longer describes reality.
            conn.execute(
                "UPDATE beads SET dispatch_failure = NULL WHERE id = ?1 AND dispatch_failure IS NOT NULL",
                [id],
            )?;
            Ok(())
        }
        Some(status) => Err(CoreError::InvalidTransition {
            bead: id.to_owned(),
            reason: format!("cannot claim a bead with status {status:?}"),
        }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadUpdated {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// compat §6.1/§9: `Some(v)` sets, `None` UNSETS. The drain reservation
    /// rides this event — there is no `drain.reserved` event and there never
    /// will be (`vocab::no_reservation_vocabulary_exists`).
    #[serde(default)]
    metadata: BTreeMap<String, Option<String>>,
}

fn bead_updated(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let p: BeadUpdated = payload(event)?;
    if p.title.is_none() && p.description.is_none() && p.metadata.is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "update must set title and/or description and/or metadata".to_owned(),
        });
    }
    // Same rule as creation (PR #18 review finding 1): a patch must not
    // blank a title.
    if let Some(title) = &p.title
        && title.trim().is_empty()
    {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "title, when set, must be non-empty".to_owned(),
        });
    }
    if bead_status(conn, id)?.is_none() {
        return Err(CoreError::UnknownBead(id.to_owned()));
    }
    for (key, value) in &p.metadata {
        match value {
            Some(value) => write_meta(conn, event, id, key, value)?,
            // Unset. The release path — and it must be able to release a key
            // that is not held (an orphan sweep runs against beads it has not
            // checked), so a no-op DELETE is success, not an error.
            None => {
                conn.execute(
                    "DELETE FROM bead_meta WHERE bead_id = ?1 AND key = ?2",
                    params![id, key],
                )?;
            }
        }
    }
    conn.execute(
        "UPDATE beads SET title = COALESCE(?1, title),
                          description = COALESCE(?2, description),
                          updated_ts = ?3
         WHERE id = ?4",
        params![p.title, p.description, event.ts, id],
    )?;
    rewrite_body_search_row(conn, id)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadClosed {
    outcome: String,
    #[serde(default)]
    reason: Option<String>,
    /// Phase 9 (spec §8.2 retry classification): only "transient", only on
    /// a fail close.
    #[serde(default)]
    failure_class: Option<String>,
    /// Phase 9: retry-exhaustion disposition on a looping-step anchor's
    /// fail close. NEVER "pass" — the run-level pass disposition lives in
    /// `run.finalized` only (plan Decision 3).
    #[serde(default)]
    final_disposition: Option<String>,
    /// Phase 9: structured step output (`camp close --output-json`), read
    /// back by `on_complete` fan-out. Audit content — any JSON.
    #[serde(default)]
    #[allow(dead_code)]
    output: Option<serde_json::Value>,
    /// Phase 3 (#34, Q3): Gas City's WorkOutcome axis, mirrored verbatim —
    /// a SEPARATE additive axis from the control `outcome`.
    #[serde(default)]
    work_outcome: Option<String>,
    #[serde(default)]
    work_commit: Option<String>,
    #[serde(default)]
    work_branch: Option<String>,
}

fn bead_closed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let p: BeadClosed = payload(event)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    if !CAMP_OUTCOMES.contains(&p.outcome.as_str()) {
        return Err(bad(format!(
            "outcome {:?} is not in camp's vocabulary {CAMP_OUTCOMES:?}",
            p.outcome
        )));
    }
    if let Some(class) = p.failure_class.as_deref() {
        if !crate::vocab::CAMP_FAILURE_CLASSES.contains(&class) {
            return Err(bad(format!(
                "failure_class {class:?} is not in camp's vocabulary \
                 {:?}",
                crate::vocab::CAMP_FAILURE_CLASSES
            )));
        }
        if p.outcome != "fail" {
            return Err(bad(format!(
                "failure_class requires outcome \"fail\", got {:?}",
                p.outcome
            )));
        }
    }
    if let Some(disposition) = p.final_disposition.as_deref() {
        if !CAMP_FINAL_DISPOSITIONS.contains(&disposition) {
            return Err(bad(format!(
                "final_disposition {disposition:?} is not in camp's close vocabulary \
                 {CAMP_FINAL_DISPOSITIONS:?} (the run-level \"pass\" lives in run.finalized only)"
            )));
        }
        if p.outcome != "fail" {
            return Err(bad(format!(
                "final_disposition requires outcome \"fail\", got {:?}",
                p.outcome
            )));
        }
    }
    match p.work_outcome.as_deref() {
        None => {
            if p.work_commit.is_some() || p.work_branch.is_some() {
                return Err(bad(
                    "work_commit/work_branch require work_outcome \"shipped\"".to_owned(),
                ));
            }
        }
        Some(wo) => {
            if !crate::vocab::CAMP_WORK_OUTCOMES.contains(&wo) {
                return Err(bad(format!(
                    "work_outcome {wo:?} is not in camp's vocabulary {:?}",
                    crate::vocab::CAMP_WORK_OUTCOMES
                )));
            }
            // Coherence (the #34 gate): shipped/no-op assert success,
            // blocked/abandoned assert the work did NOT land — `pass` over
            // un-integrable work is exactly the lie this rejects.
            // Deliberately STRICTER than gc, which keeps WorkOutcome and
            // the control Outcome disjoint and uncoupled (values.go); camp
            // couples them. Mirror-safe: names/values stay verbatim, and
            // export emits only gc-valid pairings.
            let wants_pass = matches!(wo, "shipped" | "no-op");
            if wants_pass && p.outcome != "pass" {
                return Err(bad(format!(
                    "work_outcome {wo:?} requires outcome \"pass\", got {:?}",
                    p.outcome
                )));
            }
            if !wants_pass && p.outcome != "fail" {
                return Err(bad(format!(
                    "work_outcome {wo:?} requires outcome \"fail\", got {:?}",
                    p.outcome
                )));
            }
            // Only shipped carries an artifact (gc values.go, verbatim).
            let has_artifact = p.work_commit.is_some() && p.work_branch.is_some();
            if wo == "shipped" && !has_artifact {
                return Err(bad(
                    "work_outcome \"shipped\" requires work_commit and work_branch".to_owned(),
                ));
            }
            if wo != "shipped" && (p.work_commit.is_some() || p.work_branch.is_some()) {
                return Err(bad(format!(
                    "work_outcome {wo:?} must not carry work_commit/work_branch (only shipped has an artifact)"
                )));
            }
        }
    }
    match bead_status(conn, id)?.as_deref() {
        None => Err(CoreError::UnknownBead(id.to_owned())),
        Some("closed") => Err(CoreError::InvalidTransition {
            bead: id.to_owned(),
            reason: "bead is already closed".to_owned(),
        }),
        Some(_) => {
            conn.execute(
                "UPDATE beads SET status = 'closed', outcome = ?1, close_reason = ?2,
                                  work_outcome = ?3, work_commit = ?4, work_branch = ?5,
                                  closed_ts = ?6, updated_ts = ?6
                 WHERE id = ?7",
                params![
                    p.outcome,
                    p.reason,
                    p.work_outcome,
                    p.work_commit,
                    p.work_branch,
                    event.ts,
                    id
                ],
            )?;
            if let Some(reason) = p.reason.as_deref()
                && !reason.is_empty()
            {
                conn.execute(
                    "INSERT INTO search (bead_id, kind, content) VALUES (?1, 'close', ?2)",
                    params![id, reason],
                )?;
            }
            Ok(())
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RigAdded {
    path: String,
    prefix: String,
}

/// `rig.added` is log-only: rigs live in camp.toml (decision D). The fold
/// validates the audit payload — the rig name, a non-empty path, and a
/// well-formed prefix — so a malformed config event fails fast.
fn rig_added(event: &Event) -> Result<(), CoreError> {
    if event.rig.is_none() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "missing rig name".to_owned(),
        });
    }
    let p: RigAdded = payload(event)?;
    if p.path.is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "empty rig path".to_owned(),
        });
    }
    crate::id::validate_prefix(&p.prefix)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CampdAutostarted {
    verb: String,
}

/// `campd.autostarted` is log-only, and HISTORICAL: nothing emits it since the
/// CLI became a pure socket client (design §4.3). The arm stays so that ledgers
/// written before that still fold — and it still validates the audit payload,
/// so a malformed event fails fast, then and now.
fn campd_autostarted(event: &Event) -> Result<(), CoreError> {
    let p: CampdAutostarted = payload(event)?;
    if p.verb.is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "empty verb".to_owned(),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCooked {
    run_id: String,
    formula: String,
    root: String,
    #[allow(dead_code)] // audit content: validated for shape, not read back
    steps: std::collections::BTreeMap<String, String>,
}

/// `run.cooked` is log-only: the run's durable truth is its beads (created
/// in the same transaction) and the pinned run dir. The fold validates the
/// audit payload so a malformed cook event fails fast.
fn known_bead(conn: &Connection, id: &str) -> Result<(), CoreError> {
    if bead_status(conn, id)?.is_none() {
        return Err(CoreError::UnknownBead(id.to_owned()));
    }
    Ok(())
}

fn non_empty(event: &Event, field: &str, value: &str) -> Result<(), CoreError> {
    if value.is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("empty {field}"),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormulaRefused {
    formula: String,
    /// The KEY camp refused — not always the key that carried it: a
    /// `gc.kind = "scope"` inside an accepted `metadata` map refuses as
    /// `gc.kind` (compat §4 trap 2).
    key: String,
    reason: String,
    /// The step the refusal belongs to, when it belongs to one.
    #[serde(default)]
    step: Option<String>,
}

/// `formula.refused` is log-only (compat §4 rule 1): a formula named a Gas City
/// construct camp does not implement, and camp declined to load it rather than
/// approximate its semantics. No state fold — the formula never became a run.
///
/// It has no bead: the refusal happens BEFORE anything is cooked. That is the
/// whole point — §13's money invariant says a formula camp cannot honour must
/// never reach a worker.
fn formula_refused(event: &Event) -> Result<(), CoreError> {
    let p: FormulaRefused = payload(event)?;
    non_empty(event, "formula", &p.formula)?;
    non_empty(event, "key", &p.key)?;
    non_empty(event, "reason", &p.reason)?;
    // Present-but-empty is a bug in the producer, not a step-less refusal:
    // a formula-scoped refusal omits the field entirely.
    if let Some(step) = &p.step {
        non_empty(event, "step", step)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerMilestone {
    text: String,
}

/// `worker.milestone` is log-only: worker breadcrumbs (spec §8.1). The
/// bead is optional; when named it must exist (fail fast on typos).
fn worker_milestone(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: WorkerMilestone = payload(event)?;
    non_empty(event, "text", &p.text)?;
    if let Some(bead) = event.bead.as_deref() {
        known_bead(conn, bead)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeKept {
    path: String,
    reason: String,
}

/// `worktree.kept` is log-only: a failed bead's worktree stays for
/// forensics (spec §12), and the ledger records where and why.
fn worktree_kept(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: WorktreeKept = payload(event)?;
    non_empty(event, "path", &p.path)?;
    non_empty(event, "reason", &p.reason)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadWorktreeReaped {
    path: String,
}

/// `bead.worktree.reaped` (gc-mirrored name) is log-only: a clean close's
/// worktree was removed (spec §12).
fn bead_worktree_reaped(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: BeadWorktreeReaped = payload(event)?;
    non_empty(event, "path", &p.path)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStalled {
    session: String,
    agent: String,
    action: String,
    threshold: String,
    #[allow(dead_code)] // audit content: validated for shape, not read back
    restarts: u32,
    #[serde(default)]
    via: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

const STALL_ACTIONS: &[&str] = &["nudge", "nudge_failed", "restart", "exhausted", "annotate"];

/// `agent.stalled` is log-only (spec §10.2): the patrol fire declaration —
/// which worker, which bead, which ladder action, at what effective
/// threshold. Escalation to judgment matches this event (pack content).
/// The bead is required and must exist; sessions.status deliberately does
/// NOT change (observation over state — subsequent activity in the ledger
/// is the recovery signal).
fn agent_stalled(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: AgentStalled = payload(event)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    for (field, value) in [
        ("session", &p.session),
        ("agent", &p.agent),
        ("threshold", &p.threshold),
    ] {
        if value.is_empty() {
            return Err(bad(format!("empty {field}")));
        }
    }
    if !STALL_ACTIONS.contains(&p.action.as_str()) {
        return Err(bad(format!(
            "action {:?} is not in {STALL_ACTIONS:?}",
            p.action
        )));
    }
    if p.action == "nudge_failed" && p.error.as_deref().is_none_or(str::is_empty) {
        return Err(bad("a nudge_failed record requires the error".into()));
    }
    if p.via.is_some() && !matches!(p.action.as_str(), "nudge" | "nudge_failed") {
        return Err(bad("via is a nudge-only field".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatrolDegraded {
    error: String,
    #[serde(default)]
    #[allow(dead_code)] // audit content: which session's patrol degraded
    session: Option<String>,
}

/// `patrol.degraded` is log-only: a patrol subsystem impairment (a dead
/// transcript watcher, a failed nudge-resume child) — durable in the
/// ledger, never just a stderr line on a detached daemon (invariant 5;
/// the Phase 10 LOW-8 mold).
fn patrol_degraded(event: &Event) -> Result<(), CoreError> {
    let p: PatrolDegraded = payload(event)?;
    non_empty(event, "error", &p.error)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchFailed {
    reason: String,
}

/// `dispatch.failed`: campd could not dispatch a ready bead (unresolvable
/// agent, missing rig, worktree failure). campd has no caller, so the
/// error lands here (invariant 5); Phase 8 plan decision F. Phase 3 (#48
/// finding 2): no longer log-only — the fail-fast reason folds onto the
/// bead (`beads.dispatch_failure`) so `camp ls` can mark work that looks
/// ready but will not dispatch. Cleared by a later session.woke/claim, or by
/// `dispatch.rearmed` (`camp retry`).
fn dispatch_failed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: DispatchFailed = payload(event)?;
    non_empty(event, "reason", &p.reason)?;
    conn.execute(
        "UPDATE beads SET dispatch_failure = ?1, updated_ts = ?2 WHERE id = ?3",
        params![p.reason, event.ts, bead],
    )?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchRearmed {
    /// The dispatch.failed reason being cleared — carried so the ledger
    /// history is self-describing ("re-armed after: <reason>").
    previous_reason: String,
}

/// `dispatch.rearmed`: an operator re-armed a bead whose dispatch failed
/// (`camp retry`, issue #83). Clears `beads.dispatch_failure` so the bead
/// re-enters the dispatchable set on the next converge — the explicit
/// re-arm path (invariant 1: no automatic retry). Idempotent (like the
/// session.woke/claim clears): a bead whose marker is already clear is a
/// harmless no-op, which keeps refold deterministic.
fn dispatch_rearmed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: DispatchRearmed = payload(event)?;
    non_empty(event, "previous_reason", &p.previous_reason)?;
    conn.execute(
        "UPDATE beads SET dispatch_failure = NULL, updated_ts = ?2
         WHERE id = ?1 AND dispatch_failure IS NOT NULL",
        params![bead, event.ts],
    )?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchLiveTree {
    path: String,
    agent: String,
}

/// `dispatch.live_tree` is log-only (spec §12, dispatch-lifecycle Q1):
/// campd dispatched an autonomous worker onto the rig's live tree because
/// the agent explicitly declared `isolation = "none"`. The opt-out is
/// LOUD — running on the live tree is a ledger fact, never silent.
fn dispatch_live_tree(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: DispatchLiveTree = payload(event)?;
    non_empty(event, "path", &p.path)?;
    non_empty(event, "agent", &p.agent)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckPassed {
    run_id: String,
    step_id: String,
    attempt: u32,
}

/// `check.passed` is log-only (Phase 9, spec §8.3): the check script for a
/// looping step's Nth attempt exited 0. The anchor's pass close (same
/// batch) is the state effect; event.bead is the verified attempt bead.
fn check_passed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: CheckPassed = payload(event)?;
    non_empty(event, "run_id", &p.run_id)?;
    non_empty(event, "step_id", &p.step_id)?;
    if p.attempt == 0 {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "attempt numbers start at 1".to_owned(),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckFailed {
    run_id: String,
    step_id: String,
    attempt: u32,
    #[serde(default)]
    exit_code: Option<i64>,
    #[serde(default)]
    signal: Option<i64>,
    #[serde(default)]
    timed_out: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    log: Option<String>,
}

/// `check.failed` is log-only (Phase 9): a check iteration failed — by
/// exit code, signal, timeout, or a spawn error. At least one piece of
/// evidence is required so the ledger always says WHY (invariant 5).
fn check_failed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: CheckFailed = payload(event)?;
    non_empty(event, "run_id", &p.run_id)?;
    non_empty(event, "step_id", &p.step_id)?;
    let bad = |reason: &str| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason: reason.to_owned(),
    };
    if p.attempt == 0 {
        return Err(bad("attempt numbers start at 1"));
    }
    let timed_out = p.timed_out == Some(true);
    if p.exit_code.is_none() && p.signal.is_none() && !timed_out && p.error.is_none() {
        return Err(bad(
            "check.failed requires evidence: exit_code, signal, timed_out, or error",
        ));
    }
    if let Some(error) = p.error.as_deref()
        && error.is_empty()
    {
        return Err(bad("empty error"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunFinalized {
    run_id: String,
    root: String,
    outcome: String,
    final_disposition: String,
    /// The close event that made the run quiescent — the spec §13.3 cause
    /// chain.
    cause_seq: i64,
    #[serde(default)]
    #[allow(dead_code)]
    soft_failed: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    skipped: Vec<String>,
}

/// `run.finalized` is log-only (Phase 9, spec §8.3): the run's aggregated
/// verdict with its cause. The root's close (same batch) is the state
/// effect; the run-level disposition (including "pass") lives HERE, never
/// on a bead.closed (plan Decision 3).
fn run_finalized(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: RunFinalized = payload(event)?;
    non_empty(event, "run_id", &p.run_id)?;
    non_empty(event, "root", &p.root)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    if bead != p.root {
        return Err(bad(format!(
            "run.finalized bead {bead:?} must be the root {:?}",
            p.root
        )));
    }
    if !CAMP_OUTCOMES.contains(&p.outcome.as_str()) {
        return Err(bad(format!(
            "outcome {:?} is not in camp's vocabulary {CAMP_OUTCOMES:?}",
            p.outcome
        )));
    }
    if !CAMP_RUN_DISPOSITIONS.contains(&p.final_disposition.as_str()) {
        return Err(bad(format!(
            "final_disposition {:?} is not in camp's run vocabulary {CAMP_RUN_DISPOSITIONS:?}",
            p.final_disposition
        )));
    }
    if p.cause_seq < 1 {
        return Err(bad(format!("cause_seq {} is not a seq", p.cause_seq)));
    }
    Ok(())
}

fn run_cooked(event: &Event) -> Result<(), CoreError> {
    let p: RunCooked = payload(event)?;
    for (field, value) in [
        ("run_id", &p.run_id),
        ("formula", &p.formula),
        ("root", &p.root),
    ] {
        if value.is_empty() {
            return Err(CoreError::InvalidEventData {
                event_type: event.kind.as_str().to_owned(),
                reason: format!("empty {field}"),
            });
        }
    }
    Ok(())
}

fn rewrite_body_search_row(conn: &Connection, id: &str) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM search WHERE bead_id = ?1 AND kind = 'body'",
        [id],
    )?;
    conn.execute(
        "INSERT INTO search (bead_id, kind, content)
         SELECT id, 'body', title || char(10) || description FROM beads WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionWoke {
    name: String,
    agent: String,
    #[serde(default)]
    rig: Option<String>,
    #[serde(default)]
    claude_session_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    pid: Option<i64>,
    #[serde(default)]
    bead: Option<String>,
    /// Audit-only (Phase 8): the worktree the worker runs in, when
    /// isolated. No sessions column exists — schema v1 is frozen; campd
    /// keeps the live copy in memory and the ledger keeps the record.
    #[serde(default)]
    #[allow(dead_code)]
    worktree: Option<String>,
    /// Phase 3: dispatch-time facts, audit-only in the fold — read back via
    /// the woke-JSON join in session_rows (sessions DDL unchanged): the
    /// rig's base commit at dispatch (the shipped gate's reference) and the
    /// F7 pins (re-applied on resume turns).
    #[serde(default)]
    #[allow(dead_code)]
    base: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    model: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    permission_mode: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    allowed_tools: Option<String>,
}

fn session_woke(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    use rusqlite::OptionalExtension;
    let p: SessionWoke = payload(event)?;
    let exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sessions WHERE name = ?1",
            [&p.name],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("session {:?} is already registered", p.name),
        });
    }
    conn.execute(
        "INSERT INTO sessions (name, agent, rig, claude_session_id, transcript_path, pid,
                               status, bead, spawned_ts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'live', ?7, ?8)",
        params![
            p.name,
            p.agent,
            p.rig,
            p.claude_session_id,
            p.transcript_path,
            p.pid,
            p.bead,
            event.ts,
        ],
    )?;
    // Phase 3 (#48 finding 2): a woke naming the bead means a dispatch
    // succeeded — the fail-fast marker no longer describes reality.
    if let Some(bead) = &p.bead {
        conn.execute(
            "UPDATE beads SET dispatch_failure = NULL WHERE id = ?1 AND dispatch_failure IS NOT NULL",
            [bead],
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionNudged {
    session: String,
    via: String,
    text: String,
}

const NUDGE_VIAS: &[&str] = &["stdin", "resume"];

/// `session.nudged` is log-only (dispatch-lifecycle Phase 1, #29): a turn
/// was delivered into a session's conversation — live over the campd-held
/// stdin pipe, or via `claude --resume` after the turn (A4). The named
/// session must exist (fail fast on typos, like worker.milestone's bead);
/// the text must be non-blank (it is the delivered turn — the audit record
/// is worthless without it, same rule as bead.created's title).
fn session_nudged(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: SessionNudged = payload(event)?;
    if p.text.trim().is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "text must be non-empty".to_owned(),
        });
    }
    if !NUDGE_VIAS.contains(&p.via.as_str()) {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("unknown via {:?} (stdin|resume)", p.via),
        });
    }
    let known: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE name = ?1)",
        [&p.session],
        |r| r.get(0),
    )?;
    if !known {
        return Err(CoreError::UnknownSession(p.session));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionEnd {
    name: String,
    /// Audit-only (Phase 8, F4 evidence): how the process ended.
    #[serde(default)]
    #[allow(dead_code)]
    exit_code: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    signal: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
    /// Audit-only (Phase 11): a patrol-initiated kill names the
    /// agent.stalled event that caused it (every action carries its cause).
    #[serde(default)]
    #[allow(dead_code)]
    cause_seq: Option<i64>,
}

fn session_ended(conn: &Connection, event: &Event, new_status: &str) -> Result<(), CoreError> {
    use rusqlite::OptionalExtension;
    let p: SessionEnd = payload(event)?;
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM sessions WHERE name = ?1",
            [&p.name],
            |r| r.get(0),
        )
        .optional()?;
    match status.as_deref() {
        None => Err(CoreError::UnknownSession(p.name)),
        Some("live") => {
            conn.execute(
                "UPDATE sessions SET status = ?1, ended_ts = ?2 WHERE name = ?3",
                params![new_status, event.ts, p.name],
            )?;
            if new_status == "crashed" {
                // The bead is the work; the session is disposable (spec §10).
                conn.execute(
                    "UPDATE beads SET status = 'open', claimed_by = NULL, updated_ts = ?1
                     WHERE claimed_by = ?2 AND status = 'in_progress'",
                    params![event.ts, p.name],
                )?;
            }
            Ok(())
        }
        Some(s) => Err(CoreError::InvalidTransition {
            bead: p.name,
            reason: format!("session already ended with status {s:?}"),
        }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderFired {
    order: String,
    trigger: String,
    #[serde(default)]
    scheduled_ts: Option<String>,
    #[serde(default)]
    catch_up: Option<bool>,
    #[serde(default)]
    cause_seq: Option<i64>,
}

/// `order.fired` is log-only: the durable declaration that a trigger
/// tripped (spec §9). campd cooks the formula in response to *processing*
/// this event, so a fire is never lost to a crash (Phase 10 plan
/// Decision D). The fold validates the cause shape per trigger.
fn order_fired(event: &Event) -> Result<(), CoreError> {
    let p: OrderFired = payload(event)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    if p.order.is_empty() {
        return Err(bad("empty order".into()));
    }
    match p.trigger.as_str() {
        "cron" => {
            if p.scheduled_ts.is_none() {
                return Err(bad("cron trigger requires scheduled_ts".into()));
            }
            if p.cause_seq.is_some() {
                return Err(bad("cron trigger does not carry cause_seq".into()));
            }
        }
        "event" => {
            if p.cause_seq.is_none() {
                return Err(bad("event trigger requires cause_seq".into()));
            }
            if p.scheduled_ts.is_some() || p.catch_up.is_some() {
                return Err(bad("event trigger carries only cause_seq".into()));
            }
        }
        "manual" => {
            if p.scheduled_ts.is_some() || p.catch_up.is_some() || p.cause_seq.is_some() {
                return Err(bad("manual trigger carries no schedule data".into()));
            }
        }
        other => return Err(bad(format!("unknown trigger {other:?}"))),
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderCompleted {
    order: String,
    #[allow(dead_code)] // audit content: validated for shape, not read back
    fired_seq: i64,
    root_bead: String,
    run_id: String,
    outcome: String,
}

/// `order.completed` is log-only: the run's truth lives in its beads; this
/// event closes the order's cause chain (fired → cooked → root closed).
fn order_completed(event: &Event) -> Result<(), CoreError> {
    let p: OrderCompleted = payload(event)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    for (field, value) in [
        ("order", &p.order),
        ("root_bead", &p.root_bead),
        ("run_id", &p.run_id),
    ] {
        if value.is_empty() {
            return Err(bad(format!("empty {field}")));
        }
    }
    if p.outcome != "pass" {
        return Err(bad(format!(
            "order.completed outcome must be \"pass\", got {:?} (failures are order.failed)",
            p.outcome
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderFailed {
    order: String,
    #[allow(dead_code)] // audit content: validated for shape, not read back
    fired_seq: i64,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    root_bead: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
}

/// `order.failed` is log-only and has exactly two legal shapes: a
/// fire-stage failure `{order, fired_seq, error}` or a run failure
/// `{order, fired_seq, root_bead, run_id, outcome:"fail"}`.
fn order_failed(event: &Event) -> Result<(), CoreError> {
    let p: OrderFailed = payload(event)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    if p.order.is_empty() {
        return Err(bad("empty order".into()));
    }
    match (&p.error, &p.root_bead) {
        (Some(error), None) => {
            if error.is_empty() {
                return Err(bad("empty error".into()));
            }
            if p.run_id.is_some() || p.outcome.is_some() {
                return Err(bad("the error shape carries no run fields".into()));
            }
            Ok(())
        }
        (None, Some(root_bead)) => {
            if root_bead.is_empty() {
                return Err(bad("empty root_bead".into()));
            }
            if p.run_id.as_deref().is_none_or(str::is_empty) {
                return Err(bad("the run shape requires run_id".into()));
            }
            if p.outcome.as_deref() != Some("fail") {
                return Err(bad("the run shape requires outcome \"fail\"".into()));
            }
            Ok(())
        }
        _ => Err(bad(
            "exactly one of error / root_bead must be present".into()
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigChanged {
    path: String,
    applied: bool,
    #[serde(default)]
    orders: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

/// `config.changed` is log-only (spec §13.4: config changes are themselves
/// events; camp.toml stays the source of truth). `applied` decides the
/// legal shape: applied changes report the order count, rejected ones the
/// error.
fn config_changed(event: &Event) -> Result<(), CoreError> {
    let p: ConfigChanged = payload(event)?;
    let bad = |reason: String| CoreError::InvalidEventData {
        event_type: event.kind.as_str().to_owned(),
        reason,
    };
    if p.path.is_empty() {
        return Err(bad("empty path".into()));
    }
    if p.applied {
        if p.error.is_some() {
            return Err(bad("an applied change carries no error".into()));
        }
        if p.orders.is_none() {
            return Err(bad("an applied change reports its order count".into()));
        }
    } else if p.error.as_deref().is_none_or(str::is_empty) {
        return Err(bad("a rejected change requires the error".into()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cp-1 (control-plane spec §2.1, §4.4): the control plane's four events.
//
// All four are AUDIT-ONLY: they carry durable truth and fold no state. The
// structs below exist so `deny_unknown_fields` REFUSES a malformed payload at
// append time (the fold.rs:541 precedent) — the fields are never read back,
// hence the PERMANENT allows.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // PERMANENT: audit-only — the fields exist to VALIDATE the
// shape (deny_unknown_fields), never to be read (the fold.rs:541 precedent).
struct SessionInterrupted {
    session: String,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // PERMANENT: audit-only — see SessionInterrupted.
struct ControlResponded {
    session: String,
    request_id: String,
    verb: String,
    ok: bool,
    #[serde(default)]
    detail: String,
    /// C11: the answer arrived AFTER campd declared the request unanswered.
    /// That `control.failed` was premature, and this event is the correction.
    #[serde(default)]
    late: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // PERMANENT: audit-only — see SessionInterrupted.
struct ControlFailed {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    verb: Option<String>,
    /// G5: REQUIRED, and it is the root fix, not a decoration. Rehydration
    /// ROUTES on this: without it, campd cannot tell "timed out — an answer
    /// may still come" from "the pipe write failed — no answer can ever come",
    /// and it collapses both, SILENTLY SWALLOWING a late answer across a
    /// restart. Prose is not a cause (invariant 3).
    cause: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // PERMANENT: audit-only — see SessionInterrupted.
struct SubscriberDropped {
    session: String,
    subscription: String,
    buffered_bytes: u64,
    cap_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // PERMANENT: audit-only — see SessionInterrupted.
struct ShimRefused {
    /// The pack binding + qualified agent the refusing worker belongs to, when
    /// campd exported them (`$GC_TEMPLATE`/`$GC_AGENT`). Optional: an
    /// unattributable refusal is still worth recording.
    #[serde(default)]
    binding: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    /// The refused verb (e.g. `bd mol`) — NAMED so the ledger tells the whole
    /// story (invariant 3, §6).
    verb: String,
    detail: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // PERMANENT: audit-only — see SessionInterrupted.
struct WorkerDrainAcked {
    session: String,
}

/// `control.failed` (§2.1): a control request campd could not complete.
/// Audit-only, but `cause` is validated against the closed set — an event
/// carrying a cause nothing can route is worse than no event at all.
fn control_failed(event: &Event) -> Result<(), CoreError> {
    let p: ControlFailed = payload(event)?;
    non_empty(event, "reason", &p.reason)?;
    // The cause is validated through the SHARED enum (`vocab::ControlFailureCause`),
    // which is also what the daemon routes on — so the fold and the routing cannot
    // drift apart. An event carrying a cause nothing can route is worse than no
    // event at all.
    if crate::vocab::ControlFailureCause::parse(&p.cause).is_none() {
        let known: Vec<&str> = crate::vocab::ControlFailureCause::ALL
            .iter()
            .map(|c| c.as_str())
            .collect();
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!(
                "unknown cause {:?} (one of {known:?}). Rehydration ROUTES on this value: \
                 an unroutable cause is a swallowed fault waiting to happen",
                p.cause
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        (dir, ledger)
    }

    /// The audit arm VALIDATES: an unknown field is refused AT APPEND
    /// (`deny_unknown_fields`) and nothing is stored.
    ///
    /// Mutation caught: routing `EventType::ShimRefused` to a bare `=> Ok(())`
    /// (which would accept the malformed event and replay it forever).
    #[test]
    fn shim_refused_with_an_unknown_field_is_refused_at_append() {
        let (_dir, mut ledger) = temp_ledger();
        let err = ledger
            .append(EventInput {
                kind: EventType::ShimRefused,
                rig: None,
                actor: "gc-shim".into(),
                bead: None,
                data: serde_json::json!({"verb": "x", "detail": "y", "bogus": 1}),
            })
            .unwrap_err();
        assert!(
            matches!(err, crate::error::CoreError::InvalidEventData { .. }),
            "unexpected error: {err:?}"
        );
        // Rejections appended nothing: the transaction rolled back.
        assert!(
            ledger
                .events_of_type(EventType::ShimRefused)
                .unwrap()
                .is_empty(),
            "a refused event must not be stored"
        );
    }

    /// A well-formed `shim.refused` (with the optional binding/agent absent)
    /// appends; the arm is not rejecting valid events.
    #[test]
    fn shim_refused_well_formed_appends() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(EventInput {
                kind: EventType::ShimRefused,
                rig: None,
                actor: "gc-shim".into(),
                bead: None,
                data: serde_json::json!({"verb": "bd mol", "detail": "unknown verb"}),
            })
            .unwrap();
        assert_eq!(
            ledger.events_of_type(EventType::ShimRefused).unwrap().len(),
            1
        );
    }

    /// Same validation guard on the other new audit arm.
    #[test]
    fn worker_drain_acked_with_an_unknown_field_is_refused_at_append() {
        let (_dir, mut ledger) = temp_ledger();
        let err = ledger
            .append(EventInput {
                kind: EventType::WorkerDrainAcked,
                rig: None,
                actor: "gc-shim".into(),
                bead: None,
                data: serde_json::json!({"session": "t/gc.publisher/1", "bogus": true}),
            })
            .unwrap_err();
        assert!(
            matches!(err, crate::error::CoreError::InvalidEventData { .. }),
            "unexpected error: {err:?}"
        );
        assert!(
            ledger
                .events_of_type(EventType::WorkerDrainAcked)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn worker_drain_acked_well_formed_appends() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(EventInput {
                kind: EventType::WorkerDrainAcked,
                rig: None,
                actor: "gc-shim".into(),
                bead: None,
                data: serde_json::json!({"session": "t/gc.publisher/1"}),
            })
            .unwrap();
        assert_eq!(
            ledger
                .events_of_type(EventType::WorkerDrainAcked)
                .unwrap()
                .len(),
            1
        );
    }

    /// The `COALESCE(?2, work_branch)` in `bead_claimed` PROTECTS a pre-existing
    /// `work_branch` when a claim carries none (camp's own `camp claim`). This
    /// is unreachable through `Ledger::append` alone — an OPEN bead never has a
    /// `work_branch` (the only writers are the claim itself and a shipped close,
    /// both of which leave the bead non-open), so the guard's job only shows on
    /// a directly-seeded column. We seed it here (in-crate `conn` access) to
    /// exercise the branch the integration test structurally cannot.
    ///
    /// Mutation caught: `work_branch = ?2` (drop the COALESCE) — it would write
    /// NULL over the seeded `camp/pre`, so `gc.work_branch` disappears → RED.
    #[test]
    fn claim_with_no_branch_preserves_a_pre_existing_work_branch_coalesce() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-2".into()),
                data: serde_json::json!({ "title": "work", "assignee": "gc.publisher" }),
            })
            .unwrap();
        // Seed a work_branch on the still-OPEN bead (artificial — the product
        // cannot reach this state, which is exactly why the COALESCE guard is
        // untestable via the public API).
        ledger
            .conn
            .execute(
                "UPDATE beads SET work_branch = 'camp/pre' WHERE id = 'gc-2'",
                [],
            )
            .unwrap();

        // Claim it WITHOUT a work_branch (the `camp claim` path).
        ledger
            .append(EventInput {
                kind: EventType::BeadClaimed,
                rig: None,
                actor: "cli".into(),
                bead: Some("gc-2".into()),
                data: serde_json::json!({ "session": "t/gc.publisher/1" }),
            })
            .unwrap();

        // The claim flipped the bead but LEFT the pre-existing branch intact.
        let meta = ledger.bead_metadata("gc-2").unwrap();
        assert_eq!(
            meta.get("gc.work_branch").map(String::as_str),
            Some("camp/pre"),
            "COALESCE must not clobber a pre-existing work_branch with NULL"
        );
    }
}

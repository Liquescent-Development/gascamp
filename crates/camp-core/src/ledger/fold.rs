//! The fold: how each event type mutates the state tables. `apply` runs
//! inside the same transaction that inserts the event row (spec §7.2), and
//! `refold` replays it against a shadow database — it must stay a pure
//! function of (state, event).

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
        EventType::DispatchLiveTree => dispatch_live_tree(conn, event),
        EventType::CheckPassed => check_passed(conn, event),
        EventType::CheckFailed => check_failed(conn, event),
        EventType::RunFinalized => run_finalized(conn, event),
        EventType::AgentStalled => agent_stalled(conn, event),
        EventType::PatrolDegraded => patrol_degraded(event),
        // Log-only events: no state effect.
        EventType::CampdStarted | EventType::CampdStopped => Ok(()),
    }
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
    conn.execute(
        "INSERT INTO search (bead_id, kind, content) VALUES (?1, 'body', ?2)",
        params![id, format!("{}\n{}", p.title, p.description)],
    )?;
    crate::id::bump_counter(conn, id)?;
    Ok(())
}

use crate::vocab::{CAMP_FINAL_DISPOSITIONS, CAMP_OUTCOMES, CAMP_RUN_DISPOSITIONS};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadClaimed {
    session: String,
}

fn bead_claimed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let p: BeadClaimed = payload(event)?;
    match bead_status(conn, id)?.as_deref() {
        None => Err(CoreError::UnknownBead(id.to_owned())),
        Some("open") => {
            conn.execute(
                "UPDATE beads SET status = 'in_progress', claimed_by = ?1, updated_ts = ?2
                 WHERE id = ?3",
                params![p.session, event.ts, id],
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
}

fn bead_updated(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let p: BeadUpdated = payload(event)?;
    if p.title.is_none() && p.description.is_none() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "update must set title and/or description".to_owned(),
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

/// `campd.autostarted` is log-only: the CLI records which verb caused the
/// spawn (spec §13.3 — every action carries its cause). The fold validates
/// the audit payload so a malformed event fails fast.
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

/// `dispatch.failed` is log-only: campd could not dispatch a ready bead
/// (unresolvable agent, missing rig, worktree failure). campd has no
/// caller, so the error lands here (invariant 5); Phase 8 plan decision F.
fn dispatch_failed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, bead)?;
    let p: DispatchFailed = payload(event)?;
    non_empty(event, "reason", &p.reason)?;
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

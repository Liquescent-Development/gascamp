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
        EventType::RigAdded => rig_added(event),
        EventType::CampdAutostarted => campd_autostarted(event),
        EventType::RunCooked => run_cooked(event),
        EventType::OrderFired => order_fired(event),
        EventType::OrderCompleted => order_completed(event),
        EventType::OrderFailed => order_failed(event),
        EventType::ConfigChanged => config_changed(event),
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

use crate::vocab::CAMP_OUTCOMES;

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
}

fn bead_closed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let p: BeadClosed = payload(event)?;
    if !CAMP_OUTCOMES.contains(&p.outcome.as_str()) {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!(
                "outcome {:?} is not in camp's vocabulary {CAMP_OUTCOMES:?}",
                p.outcome
            ),
        });
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
                                  closed_ts = ?3, updated_ts = ?3
                 WHERE id = ?4",
                params![p.outcome, p.reason, event.ts, id],
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
struct SessionEnd {
    name: String,
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

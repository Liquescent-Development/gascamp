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
                            created_ts, updated_ts)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8, ?8)",
        params![
            id,
            rig,
            p.bead_type,
            p.title,
            p.description,
            p.assignee,
            serde_json::to_string(&p.labels)?,
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
/// validates the audit payload shape and the rig name so a malformed config
/// event fails fast.
fn rig_added(event: &Event) -> Result<(), CoreError> {
    if event.rig.is_none() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "missing rig name".to_owned(),
        });
    }
    let _p: RigAdded = payload(event)?;
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

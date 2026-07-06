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
        EventType::BeadUpdated | EventType::BeadClosed => Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "fold not implemented yet (Task 1.4)".to_owned(),
        }),
        EventType::SessionWoke | EventType::SessionStopped | EventType::SessionCrashed => {
            Err(CoreError::InvalidEventData {
                event_type: event.kind.as_str().to_owned(),
                reason: "fold not implemented yet (Task 1.4)".to_owned(),
            })
        }
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
    Ok(())
}

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

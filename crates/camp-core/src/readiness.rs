//! Readiness (spec §7.3, plan decision 6): a bead is ready when it is open
//! and every `needs` target exists, is closed, and passed. A failed or
//! missing dependency never unblocks its dependents. Also the read surface
//! `camp ls` uses. Pure queries over the state tables — no writes.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::error::CoreError;

/// One bead as `camp ls`/`camp show` present it. Optional fields serialize as
/// explicit `null` (stable machine-readable JSON, decision G).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BeadRow {
    pub id: String,
    pub rig: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: String,
    pub assignee: Option<String>,
    pub claimed_by: Option<String>,
    pub outcome: Option<String>,
    pub labels: Vec<String>,
    pub created_ts: String,
    pub updated_ts: String,
}

/// Filter for `list_beads`. `None` fields impose no constraint.
#[derive(Debug, Default)]
pub struct ListFilter<'a> {
    pub rig: Option<&'a str>,
    pub mine: Option<&'a str>,
}

const BEAD_COLS: &str = "id, rig, type, title, status, assignee, claimed_by, outcome, \
                         labels, created_ts, updated_ts";

fn row_to_bead(row: &rusqlite::Row<'_>) -> rusqlite::Result<BeadRow> {
    let labels_json: String = row.get(8)?;
    let labels: Vec<String> = serde_json::from_str(&labels_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(BeadRow {
        id: row.get(0)?,
        rig: row.get(1)?,
        kind: row.get(2)?,
        title: row.get(3)?,
        status: row.get(4)?,
        assignee: row.get(5)?,
        claimed_by: row.get(6)?,
        outcome: row.get(7)?,
        labels,
        created_ts: row.get(9)?,
        updated_ts: row.get(10)?,
    })
}

fn collect(
    rows: impl Iterator<Item = rusqlite::Result<BeadRow>>,
) -> Result<Vec<BeadRow>, CoreError> {
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// A `needs` target counts as unmet unless it exists, is closed, and passed.
const UNMET_DEP: &str = "(t.id IS NULL OR t.status <> 'closed' OR t.outcome IS NOT 'pass')";

pub fn is_ready(conn: &Connection, bead: &str) -> Result<bool, CoreError> {
    let status: Option<String> = conn
        .query_row("SELECT status FROM beads WHERE id = ?1", [bead], |r| {
            r.get(0)
        })
        .optional()?;
    let status = status.ok_or_else(|| CoreError::UnknownBead(bead.to_owned()))?;
    if status != "open" {
        return Ok(false);
    }
    let unmet: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM deps d LEFT JOIN beads t ON t.id = d.needs_id
             WHERE d.bead_id = ?1 AND {UNMET_DEP}"
        ),
        [bead],
        |r| r.get(0),
    )?;
    Ok(unmet == 0)
}

pub fn ready_beads(conn: &Connection, rig: Option<&str>) -> Result<Vec<BeadRow>, CoreError> {
    let sql = format!(
        "SELECT {BEAD_COLS} FROM beads b
         WHERE b.status = 'open' AND (?1 IS NULL OR b.rig = ?1)
           AND NOT EXISTS (
             SELECT 1 FROM deps d LEFT JOIN beads t ON t.id = d.needs_id
             WHERE d.bead_id = b.id AND {UNMET_DEP})
         ORDER BY b.created_ts, b.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![rig], row_to_bead)?;
    collect(rows)
}

/// The dependents of `closed_bead` that its close just made ready — campd's
/// affected-subgraph recompute (spec §7.3). A fail close unblocks nothing.
pub fn newly_ready(conn: &Connection, closed_bead: &str) -> Result<Vec<String>, CoreError> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT bead_id FROM deps WHERE needs_id = ?1 ORDER BY bead_id")?;
    let dependents: Vec<String> = stmt
        .query_map([closed_bead], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut ready = Vec::new();
    for dep in dependents {
        if is_ready(conn, &dep)? {
            ready.push(dep);
        }
    }
    Ok(ready)
}

pub fn list_beads(conn: &Connection, filter: &ListFilter) -> Result<Vec<BeadRow>, CoreError> {
    let sql = format!(
        "SELECT {BEAD_COLS} FROM beads
         WHERE (?1 IS NULL OR rig = ?1) AND (?2 IS NULL OR claimed_by = ?2)
         ORDER BY created_ts, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![filter.rig, filter.mine], row_to_bead)?;
    collect(rows)
}

pub fn get_bead(conn: &Connection, id: &str) -> Result<Option<BeadRow>, CoreError> {
    let row = conn
        .query_row(
            &format!("SELECT {BEAD_COLS} FROM beads WHERE id = ?1"),
            [id],
            row_to_bead,
        )
        .optional()?;
    Ok(row)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        (dir, l)
    }

    fn create(l: &mut Ledger, id: &str, needs: &[&str]) {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"title": id, "needs": needs}),
        })
        .unwrap();
    }

    fn close(l: &mut Ledger, id: &str, outcome: &str) {
        l.append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"outcome": outcome}),
        })
        .unwrap();
    }

    #[test]
    fn open_bead_with_no_deps_is_ready() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        assert!(l.is_ready("gc-1").unwrap());
    }

    #[test]
    fn open_dependency_blocks() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        assert!(!l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn closed_fail_dependency_stays_blocked() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "fail");
        assert!(!l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn missing_dependency_stays_blocked() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-2", &["gc-404"]);
        assert!(!l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn closed_pass_dependency_unblocks() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "pass");
        assert!(l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn claimed_bead_is_not_ready() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        l.append(EventInput {
            kind: EventType::BeadClaimed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"session": "camp/dev/1"}),
        })
        .unwrap();
        assert!(!l.is_ready("gc-1").unwrap());
    }

    #[test]
    fn is_ready_on_unknown_bead_errors() {
        let (_d, l) = ledger();
        assert!(matches!(
            l.is_ready("gc-nope"),
            Err(crate::error::CoreError::UnknownBead(_))
        ));
    }

    #[test]
    fn ready_beads_lists_only_the_unblocked() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]); // ready
        create(&mut l, "gc-2", &["gc-1"]); // blocked
        let ready: Vec<String> = l
            .ready_beads(None)
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ready, vec!["gc-1"]);
    }

    #[test]
    fn diamond_graph_readiness() {
        // A <- B, A <- C, {B,C} <- D
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]); // A
        create(&mut l, "gc-2", &["gc-1"]); // B
        create(&mut l, "gc-3", &["gc-1"]); // C
        create(&mut l, "gc-4", &["gc-2", "gc-3"]); // D

        // close A -> B and C become ready, D still blocked
        close(&mut l, "gc-1", "pass");
        assert_eq!(l.newly_ready("gc-1").unwrap(), vec!["gc-2", "gc-3"]);
        assert!(!l.is_ready("gc-4").unwrap());

        // close B -> D not yet ready (C still open)
        close(&mut l, "gc-2", "pass");
        assert!(l.newly_ready("gc-2").unwrap().is_empty());

        // close C -> D becomes ready
        close(&mut l, "gc-3", "pass");
        assert_eq!(l.newly_ready("gc-3").unwrap(), vec!["gc-4"]);
        assert!(l.is_ready("gc-4").unwrap());
    }

    #[test]
    fn newly_ready_is_empty_for_a_fail_close() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "fail");
        assert!(l.newly_ready("gc-1").unwrap().is_empty());
    }

    #[test]
    fn list_and_get_beads() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        assert_eq!(l.list_beads(&Default::default()).unwrap().len(), 1);
        assert_eq!(l.get_bead("gc-1").unwrap().unwrap().status, "open");
        assert!(l.get_bead("gc-404").unwrap().is_none());
    }
}

//! `camp export --city <dir>` (spec §15.3): graduation is an export, not a
//! backend. Everything here is read-only — over the ledger and the camp
//! directory. Camp never writes into a live city's store, and export
//! appends nothing to camp's own ledger. Field-level mapping tables:
//! docs/reference/export.md.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::error::CoreError;

/// One bead with every column `beads.jsonl` needs — the full-fidelity
/// superset of [`crate::readiness::BeadRow`] plus the `needs` edges from
/// `deps`. Creation order (`ORDER BY created_ts, id`), read-only.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportBead {
    pub id: String,
    pub rig: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub assignee: Option<String>,
    pub claimed_by: Option<String>,
    pub outcome: Option<String>,
    pub close_reason: Option<String>,
    pub labels: Vec<String>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub needs: Vec<String>,
    pub created_ts: String,
    pub updated_ts: String,
    pub closed_ts: Option<String>,
}

pub(crate) fn export_beads(conn: &Connection) -> Result<Vec<ExportBead>, CoreError> {
    let mut needs_by_bead: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dep_stmt =
        conn.prepare("SELECT bead_id, needs_id FROM deps ORDER BY bead_id, needs_id")?;
    let dep_rows =
        dep_stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in dep_rows {
        let (bead_id, needs_id) = row?;
        needs_by_bead.entry(bead_id).or_default().push(needs_id);
    }

    let mut stmt = conn.prepare(
        "SELECT id, rig, type, title, description, status, assignee, claimed_by,
                outcome, close_reason, labels, run_id, step_id, created_ts,
                updated_ts, closed_ts
         FROM beads ORDER BY created_ts, id",
    )?;
    let rows = stmt.query_map([], |row| {
        let labels_json: String = row.get(10)?;
        let labels: Vec<String> = serde_json::from_str(&labels_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(ExportBead {
            id: row.get(0)?,
            rig: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
            status: row.get(5)?,
            assignee: row.get(6)?,
            claimed_by: row.get(7)?,
            outcome: row.get(8)?,
            close_reason: row.get(9)?,
            labels,
            run_id: row.get(11)?,
            step_id: row.get(12)?,
            needs: Vec::new(),
            created_ts: row.get(13)?,
            updated_ts: row.get(14)?,
            closed_ts: row.get(15)?,
        })
    })?;
    let mut beads = Vec::new();
    for row in rows {
        let mut bead = row?;
        if let Some(needs) = needs_by_bead.remove(&bead.id) {
            bead.needs = needs;
        }
        beads.push(bead);
    }
    Ok(beads)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    pub(crate) const TS: &str = "2026-07-05T21:14:03Z";

    pub(crate) fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger =
            Ledger::open_with_clock(&dir.path().join("camp.db"), Box::new(FixedClock::new(TS)))
                .unwrap();
        (dir, ledger)
    }

    pub(crate) fn append(
        ledger: &mut Ledger,
        kind: EventType,
        bead: &str,
        data: serde_json::Value,
    ) {
        ledger
            .append(EventInput {
                kind,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(bead.into()),
                data,
            })
            .unwrap();
    }

    /// gc-1 closed with outcome+reason after a claim; gc-2 open with
    /// description/needs/labels/assignee; gc-3 mail; gc-4 memory; gc-5
    /// with run/step provenance.
    pub(crate) fn seed(ledger: &mut Ledger) {
        append(
            ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "implement widget", "labels": ["cli"]}),
        );
        append(
            ledger,
            EventType::BeadClaimed,
            "gc-1",
            serde_json::json!({"session": "camp/dev/1"}),
        );
        append(
            ledger,
            EventType::BeadClosed,
            "gc-1",
            serde_json::json!({"outcome": "pass", "reason": "shipped the widget"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-2",
            serde_json::json!({
                "title": "review widget",
                "description": "the change",
                "needs": ["gc-1"],
                "labels": ["cli", "review"],
                "assignee": "reviewer"
            }),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-3",
            serde_json::json!({"title": "ping from ci", "type": "mail"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-4",
            serde_json::json!({"title": "deploy needs the VPN profile", "type": "memory"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-5",
            serde_json::json!({
                "title": "step one",
                "run_id": "20260705T211403Z-abc123",
                "step_id": "s1"
            }),
        );
    }

    #[test]
    fn export_beads_returns_full_fidelity_rows_in_creation_order() {
        let (_dir, mut ledger) = temp_ledger();
        seed(&mut ledger);

        let beads = ledger.export_beads().unwrap();
        assert_eq!(
            beads.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec!["gc-1", "gc-2", "gc-3", "gc-4", "gc-5"]
        );

        let b1 = &beads[0];
        assert_eq!(b1.status, "closed");
        assert_eq!(b1.kind, "task");
        assert_eq!(b1.rig, "gc");
        assert_eq!(b1.claimed_by.as_deref(), Some("camp/dev/1"));
        assert_eq!(b1.outcome.as_deref(), Some("pass"));
        assert_eq!(b1.close_reason.as_deref(), Some("shipped the widget"));
        assert_eq!(b1.closed_ts.as_deref(), Some(TS));
        assert_eq!(b1.labels, vec!["cli".to_owned()]);
        assert_eq!(b1.created_ts, TS);
        assert_eq!(b1.updated_ts, TS);

        let b2 = &beads[1];
        assert_eq!(b2.description, "the change");
        assert_eq!(b2.needs, vec!["gc-1".to_owned()]);
        assert_eq!(b2.assignee.as_deref(), Some("reviewer"));
        assert_eq!(b2.status, "open");
        assert_eq!(b2.outcome, None);
        assert_eq!(b2.closed_ts, None);

        assert_eq!(beads[2].kind, "mail");
        assert_eq!(beads[3].kind, "memory");

        let b5 = &beads[4];
        assert_eq!(b5.run_id.as_deref(), Some("20260705T211403Z-abc123"));
        assert_eq!(b5.step_id.as_deref(), Some("s1"));
    }
}

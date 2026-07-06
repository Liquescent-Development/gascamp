//! The Gas Camp ledger: one WAL-mode SQLite file holding the append-only
//! event log and the state tables folded from it (spec §7).

mod fold;
mod schema;

use std::path::Path;

use rusqlite::{Connection, TransactionBehavior, params};

use crate::Seq;
use crate::clock::{Clock, SystemClock};
use crate::error::CoreError;
use crate::event::{Event, EventInput};

pub struct Ledger {
    conn: Connection,
    clock: Box<dyn Clock>,
}

impl Ledger {
    pub fn open(db_path: &Path) -> Result<Self, CoreError> {
        Self::open_with_clock(db_path, Box::new(SystemClock))
    }

    pub fn open_with_clock(db_path: &Path, clock: Box<dyn Clock>) -> Result<Self, CoreError> {
        let conn = schema::open_db(db_path)?;
        Ok(Self { conn, clock })
    }

    /// The single write path (spec §7.2): one WAL transaction inserts the
    /// event row and applies its state effect. Any fold error rolls back the
    /// event row — current state can never lag or outrun the history.
    pub fn append(&mut self, input: EventInput) -> Result<Seq, CoreError> {
        let seqs = self.append_batch(vec![input])?;
        match seqs.as_slice() {
            [seq] => Ok(*seq),
            _ => Err(CoreError::Corrupt(
                "append_batch(1 input) did not return exactly one seq".to_owned(),
            )),
        }
    }

    /// Append several events in ONE transaction (used by formula cook, which
    /// must materialize a whole run atomically).
    pub fn append_batch(&mut self, inputs: Vec<EventInput>) -> Result<Vec<Seq>, CoreError> {
        let ts = self.clock.now_utc();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut seqs = Vec::with_capacity(inputs.len());
        for input in inputs {
            let EventInput {
                kind,
                rig,
                actor,
                bead,
                data,
            } = input;
            tx.execute(
                "INSERT INTO events (ts, type, rig, actor, bead, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![ts, kind.as_str(), rig, actor, bead, data.to_string()],
            )?;
            let seq = tx.last_insert_rowid();
            let event = Event {
                seq,
                ts: ts.clone(),
                kind,
                rig,
                actor,
                bead,
                data,
            };
            fold::apply(&tx, &event)?;
            seqs.push(seq);
        }
        tx.commit()?;
        Ok(seqs)
    }

    /// Events with `from <= seq <= to` (unbounded above when `to` is None),
    /// in seq order.
    pub fn events_range(&self, from: Seq, to: Option<Seq>) -> Result<Vec<Event>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, type, rig, actor, bead, data FROM events
             WHERE seq >= ?1 AND (?2 IS NULL OR seq <= ?2) ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![from, to], row_to_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    use crate::event::EventType;
    let type_str: String = row.get(2)?;
    let data_str: String = row.get(6)?;
    let kind = EventType::parse(&type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let data = serde_json::from_str(&data_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Event {
        seq: row.get(0)?,
        ts: row.get(1)?,
        kind,
        rig: row.get(3)?,
        actor: row.get(4)?,
        bead: row.get(5)?,
        data,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};

    pub(crate) fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        (dir, ledger)
    }

    #[test]
    fn open_applies_pragmas_and_creates_schema_v1() {
        let (_dir, ledger) = temp_ledger();
        let conn = &ledger.conn;

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "synchronous must be NORMAL (decided 2026-07-05)");
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);

        for table in [
            "meta", "events", "beads", "deps", "sessions", "cursors", "search",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {table}");
        }

        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "1");
    }

    fn input(
        kind: EventType,
        rig: Option<&str>,
        bead: Option<&str>,
        data: serde_json::Value,
    ) -> EventInput {
        EventInput {
            kind,
            rig: rig.map(Into::into),
            actor: "test".into(),
            bead: bead.map(Into::into),
            data,
        }
    }

    fn created(bead: &str, data: serde_json::Value) -> EventInput {
        input(EventType::BeadCreated, Some("gc"), Some(bead), data)
    }

    fn count(ledger: &Ledger, sql: &str) -> i64 {
        ledger.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn append_assigns_monotonic_seqs() {
        let (_dir, mut ledger) = temp_ledger();
        let s1 = ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        let s2 = ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        assert_eq!((s1, s2), (1, 2));
    }

    #[test]
    fn bead_created_folds_into_state() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created(
                "gc-1",
                serde_json::json!({
                    "title": "add flag",
                    "description": "a --json flag for ls",
                    "needs": ["gc-0"],
                    "labels": ["cli"],
                    "assignee": "dev"
                }),
            ))
            .unwrap();

        let row = ledger
            .conn
            .query_row(
                "SELECT rig, type, title, description, status, assignee, labels, created_ts
                 FROM beads WHERE id = 'gc-1'",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "gc".into(),
                "task".into(),
                "add flag".into(),
                "a --json flag for ls".into(),
                "open".into(),
                Some("dev".into()),
                r#"["cli"]"#.into(),
                "2026-07-05T21:14:03Z".into()
            )
        );
        assert_eq!(
            count(
                &ledger,
                "SELECT count(*) FROM deps WHERE bead_id = 'gc-1' AND needs_id = 'gc-0'"
            ),
            1
        );
        let hit: String = ledger
            .conn
            .query_row(
                "SELECT bead_id FROM search WHERE search MATCH 'flag'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, "gc-1");
    }

    #[test]
    fn events_round_trip_through_events_range() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();

        let all = ledger.events_range(1, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 1);
        assert_eq!(all[0].kind, EventType::BeadCreated);
        assert_eq!(all[0].bead.as_deref(), Some("gc-1"));
        assert_eq!(all[0].ts, "2026-07-05T21:14:03Z");
        assert_eq!(all[0].data, serde_json::json!({"title": "one"}));

        let tail = ledger.events_range(2, None).unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].bead.as_deref(), Some("gc-2"));

        let bounded = ledger.events_range(1, Some(1)).unwrap();
        assert_eq!(bounded.len(), 1);
    }

    #[test]
    fn duplicate_bead_id_rolls_back_the_event_row() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        assert!(
            ledger
                .append(created("gc-1", serde_json::json!({"title": "again"})))
                .is_err()
        );
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 1);
    }

    #[test]
    fn claim_of_missing_bead_appends_nothing() {
        let (_dir, mut ledger) = temp_ledger();
        match ledger.append(input(
            EventType::BeadClaimed,
            Some("gc"),
            Some("gc-9"),
            serde_json::json!({"session": "camp/dev/1"}),
        )) {
            Err(CoreError::UnknownBead(id)) => assert_eq!(id, "gc-9"),
            other => panic!("expected UnknownBead, got {other:?}"),
        }
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 0);
    }

    #[test]
    fn append_batch_is_all_or_nothing() {
        let (_dir, mut ledger) = temp_ledger();
        let result = ledger.append_batch(vec![
            created("gc-1", serde_json::json!({"title": "one"})),
            created("gc-2", serde_json::json!({"title": "two"})),
            created("gc-1", serde_json::json!({"title": "dup"})),
        ]);
        assert!(result.is_err());
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 0);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 0);

        let seqs = ledger
            .append_batch(vec![
                created("gc-1", serde_json::json!({"title": "one"})),
                created("gc-2", serde_json::json!({"title": "two"})),
            ])
            .unwrap();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn fts5_is_available_and_searchable() {
        let (_dir, ledger) = temp_ledger();
        ledger
            .conn
            .execute(
                "INSERT INTO search (bead_id, kind, content) VALUES ('gc-1', 'body', 'refactor the auth layer')",
                [],
            )
            .unwrap();
        let hit: String = ledger
            .conn
            .query_row(
                "SELECT bead_id FROM search WHERE search MATCH 'auth'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, "gc-1");
    }

    #[test]
    fn reopen_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.db");
        drop(Ledger::open(&path).unwrap());
        // second open must not re-run migration or error
        drop(Ledger::open(&path).unwrap());
    }

    #[test]
    fn unsupported_schema_version_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.db");
        {
            let ledger = Ledger::open(&path).unwrap();
            ledger
                .conn
                .execute(
                    "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        match Ledger::open(&path) {
            Err(CoreError::UnsupportedSchema { found, supported }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, 1);
            }
            Err(other) => panic!("expected UnsupportedSchema, got {other:?}"),
            Ok(_) => panic!("open must fail on schema_version 999"),
        }
    }
}

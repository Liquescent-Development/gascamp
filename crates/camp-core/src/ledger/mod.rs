//! The Gas Camp ledger: one WAL-mode SQLite file holding the append-only
//! event log and the state tables folded from it (spec §7).

mod fold;
mod refold;
mod schema;

pub use refold::{DriftEntry, RefoldReport};

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

    /// The next unused bead id for `prefix` (spec §12). See `camp_core::id`.
    pub fn next_bead_id(&self, prefix: &str) -> Result<String, CoreError> {
        crate::id::next_bead_id(&self.conn, prefix)
    }

    /// True when `bead` is open and every `needs` target passed (decision 6).
    pub fn is_ready(&self, bead: &str) -> Result<bool, CoreError> {
        crate::readiness::is_ready(&self.conn, bead)
    }

    /// Open, unblocked beads, optionally scoped to a rig.
    pub fn ready_beads(
        &self,
        rig: Option<&str>,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::ready_beads(&self.conn, rig)
    }

    /// Dependents of `closed_bead` its close just made ready (spec §7.3).
    pub fn newly_ready(&self, closed_bead: &str) -> Result<Vec<String>, CoreError> {
        crate::readiness::newly_ready(&self.conn, closed_bead)
    }

    /// Beads matching `filter`, in creation order.
    pub fn list_beads(
        &self,
        filter: &crate::readiness::ListFilter,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::list_beads(&self.conn, filter)
    }

    /// One bead's current state, or `None`.
    pub fn get_bead(&self, id: &str) -> Result<Option<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::get_bead(&self.conn, id)
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

    /// The named consumer cursor's position; 0 when the consumer has never
    /// processed anything (spec §7.2: campd "catches up from its
    /// processed-cursor on start"). `cursors` is consumer bookkeeping —
    /// deliberately outside refold.
    pub fn cursor(&self, name: &str) -> Result<Seq, CoreError> {
        use rusqlite::OptionalExtension;
        let seq: Option<Seq> = self
            .conn
            .query_row("SELECT seq FROM cursors WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(seq.unwrap_or(0))
    }

    /// Process every event past the named cursor, exactly once (spec §7.3).
    ///
    /// Each event runs in its own `BEGIN IMMEDIATE` transaction that executes
    /// `process` and advances the cursor together: a crash or a `process`
    /// error never loses an event and never replays one. `process` receives
    /// the transaction's connection, so any writes it makes commit atomically
    /// with the cursor advance. On error the cursor stays on the last
    /// successfully processed event and the error surfaces to the caller.
    /// Returns the cursor position after the run.
    pub fn process_past_cursor(
        &mut self,
        name: &str,
        process: &mut dyn FnMut(&Connection, &Event) -> Result<(), CoreError>,
    ) -> Result<Seq, CoreError> {
        let mut cursor = self.cursor(name)?;
        let pending = self.events_range(cursor + 1, None)?;
        for event in pending {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            process(&tx, &event)?;
            tx.execute(
                "INSERT INTO cursors (name, seq) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET seq = excluded.seq",
                params![name, event.seq],
            )?;
            tx.commit()?;
            cursor = event.seq;
        }
        Ok(cursor)
    }

    /// Full event history for one bead, in seq order (spec §7.4 — the one
    /// sanctioned history read, used by `camp show`). Indexed via `events_bead`.
    pub fn events_for_bead(&self, bead: &str) -> Result<Vec<Event>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, type, rig, actor, bead, data FROM events
             WHERE bead = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([bead], row_to_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Ranked full-text search over titles, descriptions, close notes, and
    /// memory (spec §7.4), best match first. See [`crate::search::search`].
    pub fn search(
        &self,
        query: &str,
        type_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::search::SearchHit>, CoreError> {
        crate::search::search(&self.conn, query, type_filter, limit)
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
            "meta", "events", "beads", "deps", "sessions", "cursors", "search", "counters",
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
    fn next_bead_id_starts_at_one_and_follows_creates() {
        let (_dir, mut ledger) = temp_ledger();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-1");
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-2");
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-3");
        // per-prefix, independent
        assert_eq!(ledger.next_bead_id("t3").unwrap(), "t3-1");
        // the counter is folded state
        let high: i64 = ledger
            .conn
            .query_row("SELECT high FROM counters WHERE prefix = 'gc'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(high, 2);
    }

    #[test]
    fn rolled_back_create_does_not_bump_the_counter() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        // duplicate id: whole txn rolls back, counter must stay at 1
        assert!(
            ledger
                .append(created("gc-1", serde_json::json!({"title": "dup"})))
                .is_err()
        );
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-2");
    }

    #[test]
    fn counters_are_refold_exact() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        assert!(ledger.refold_check().unwrap().drift.is_empty());
        // tamper the counter, refold must catch it, repair must fix it
        ledger
            .conn
            .execute("UPDATE counters SET high = 99 WHERE prefix = 'gc'", [])
            .unwrap();
        assert!(
            ledger
                .refold_check()
                .unwrap()
                .drift
                .iter()
                .any(|d| d.table == "counters")
        );
        ledger.refold_repair().unwrap();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-3");
        assert_eq!(count(&ledger, "SELECT count(*) FROM counters"), 1);
    }

    fn woke(name: &str) -> EventInput {
        input(
            EventType::SessionWoke,
            Some("gc"),
            None,
            serde_json::json!({
                "name": name,
                "agent": "dev",
                "rig": "gc",
                "claude_session_id": "8f3c2e01",
                "transcript_path": "/tmp/t.jsonl",
                "pid": 4242,
                "bead": "gc-1"
            }),
        )
    }

    #[test]
    fn claim_moves_open_to_in_progress() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(input(
                EventType::BeadClaimed,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"session": "camp/dev/1"}),
            ))
            .unwrap();
        let (status, claimed_by): (String, String) = ledger
            .conn
            .query_row(
                "SELECT status, claimed_by FROM beads WHERE id = 'gc-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (status.as_str(), claimed_by.as_str()),
            ("in_progress", "camp/dev/1")
        );

        // claiming again is an invalid transition
        match ledger.append(input(
            EventType::BeadClaimed,
            Some("gc"),
            Some("gc-1"),
            serde_json::json!({"session": "camp/dev/2"}),
        )) {
            Err(CoreError::InvalidTransition { bead, .. }) => assert_eq!(bead, "gc-1"),
            other => panic!("expected InvalidTransition, got {other:?}"),
        }
    }

    #[test]
    fn close_records_outcome_reason_and_search_row() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(input(
                EventType::BeadClosed,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"outcome": "pass", "reason": "shipped the flamboyant widget"}),
            ))
            .unwrap();
        let (status, outcome, reason, closed_ts): (String, String, String, String) = ledger
            .conn
            .query_row(
                "SELECT status, outcome, close_reason, closed_ts FROM beads WHERE id = 'gc-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, "closed");
        assert_eq!(outcome, "pass");
        assert_eq!(reason, "shipped the flamboyant widget");
        assert_eq!(closed_ts, "2026-07-05T21:14:03Z");
        let hit: String = ledger
            .conn
            .query_row(
                "SELECT bead_id FROM search WHERE search MATCH 'flamboyant' AND kind = 'close'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, "gc-1");

        // closing a closed bead is an error
        assert!(
            ledger
                .append(input(
                    EventType::BeadClosed,
                    Some("gc"),
                    Some("gc-1"),
                    serde_json::json!({"outcome": "fail"}),
                ))
                .is_err()
        );
    }

    #[test]
    fn close_outcome_vocabulary_is_pass_or_fail_only() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        match ledger.append(input(
            EventType::BeadClosed,
            Some("gc"),
            Some("gc-1"),
            serde_json::json!({"outcome": "skipped"}),
        )) {
            Err(CoreError::InvalidEventData { reason, .. }) => {
                assert!(reason.contains("skipped"), "reason was: {reason}");
            }
            other => panic!("expected InvalidEventData, got {other:?}"),
        }
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
    }

    #[test]
    fn update_patches_fields_and_rewrites_search() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created(
                "gc-1",
                serde_json::json!({"title": "aardvark", "description": "old body"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::BeadUpdated,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"title": "zebra"}),
            ))
            .unwrap();
        let (title, description): (String, String) = ledger
            .conn
            .query_row(
                "SELECT title, description FROM beads WHERE id = 'gc-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (title.as_str(), description.as_str()),
            ("zebra", "old body")
        );
        let zebra_hits = count(
            &ledger,
            "SELECT count(*) FROM search WHERE search MATCH 'zebra'",
        );
        let aardvark_hits = count(
            &ledger,
            "SELECT count(*) FROM search WHERE search MATCH 'aardvark'",
        );
        assert_eq!((zebra_hits, aardvark_hits), (1, 0));

        // an empty patch is invalid
        assert!(
            ledger
                .append(input(
                    EventType::BeadUpdated,
                    Some("gc"),
                    Some("gc-1"),
                    serde_json::json!({}),
                ))
                .is_err()
        );
    }

    #[test]
    fn session_woke_registers_and_end_events_update() {
        let (_dir, mut ledger) = temp_ledger();
        ledger.append(woke("camp/dev/1")).unwrap();
        let (agent, status, sid, transcript, pid): (String, String, String, String, i64) = ledger
            .conn
            .query_row(
                "SELECT agent, status, claude_session_id, transcript_path, pid
                 FROM sessions WHERE name = 'camp/dev/1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(agent, "dev");
        assert_eq!(status, "live");
        assert_eq!(sid, "8f3c2e01");
        assert_eq!(transcript, "/tmp/t.jsonl");
        assert_eq!(pid, 4242);

        // duplicate registration is an error
        assert!(ledger.append(woke("camp/dev/1")).is_err());

        ledger
            .append(input(
                EventType::SessionStopped,
                Some("gc"),
                None,
                serde_json::json!({"name": "camp/dev/1"}),
            ))
            .unwrap();
        let (status, ended): (String, String) = ledger
            .conn
            .query_row(
                "SELECT status, ended_ts FROM sessions WHERE name = 'camp/dev/1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "stopped");
        assert_eq!(ended, "2026-07-05T21:14:03Z");
    }

    #[test]
    fn session_crash_releases_the_claimed_bead() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger.append(woke("camp/dev/1")).unwrap();
        ledger
            .append(input(
                EventType::BeadClaimed,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"session": "camp/dev/1"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::SessionCrashed,
                Some("gc"),
                None,
                serde_json::json!({"name": "camp/dev/1"}),
            ))
            .unwrap();
        let (bead_status, claimed_by): (String, Option<String>) = ledger
            .conn
            .query_row(
                "SELECT status, claimed_by FROM beads WHERE id = 'gc-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(bead_status, "open");
        assert_eq!(claimed_by, None);
        let session_status: String = ledger
            .conn
            .query_row(
                "SELECT status FROM sessions WHERE name = 'camp/dev/1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(session_status, "crashed");
    }

    #[test]
    fn ending_an_unknown_session_is_an_error() {
        let (_dir, mut ledger) = temp_ledger();
        match ledger.append(input(
            EventType::SessionStopped,
            None,
            None,
            serde_json::json!({"name": "camp/ghost/1"}),
        )) {
            Err(CoreError::UnknownSession(name)) => assert_eq!(name, "camp/ghost/1"),
            other => panic!("expected UnknownSession, got {other:?}"),
        }
    }

    #[test]
    fn events_for_bead_returns_only_that_beads_history_in_order() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        ledger
            .append(input(
                EventType::BeadClosed,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"outcome": "pass"}),
            ))
            .unwrap();
        let hist = ledger.events_for_bead("gc-1").unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].kind, EventType::BeadCreated);
        assert_eq!(hist[1].kind, EventType::BeadClosed);
        assert!(hist.iter().all(|e| e.bead.as_deref() == Some("gc-1")));
    }

    #[test]
    fn rig_added_is_validated_and_log_only() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(EventInput {
                kind: EventType::RigAdded,
                rig: Some("gascity".into()),
                actor: "cli".into(),
                bead: None,
                data: serde_json::json!({"path": "/code/gascity", "prefix": "gc"}),
            })
            .unwrap();
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 0);
        // malformed payload fails fast, appends nothing
        assert!(
            ledger
                .append(EventInput {
                    kind: EventType::RigAdded,
                    rig: Some("x".into()),
                    actor: "cli".into(),
                    bead: None,
                    data: serde_json::json!({"path": "/p", "prefix": "x", "extra": 1}),
                })
                .is_err()
        );
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
    }

    #[test]
    fn cursor_defaults_to_zero_and_tracks_processing() {
        let (_dir, mut ledger) = temp_ledger();
        assert_eq!(ledger.cursor("campd").unwrap(), 0);
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();

        let mut seen = Vec::new();
        let end = ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                seen.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert_eq!(end, 2);
        assert_eq!(seen, vec![1, 2]);
        assert_eq!(ledger.cursor("campd").unwrap(), 2);

        // nothing pending: nothing is reprocessed (exactly once)
        let mut again = Vec::new();
        ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                again.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn a_processing_error_halts_the_cursor_and_resume_repeats_nothing() {
        let (_dir, mut ledger) = temp_ledger();
        for i in 1..=3 {
            ledger
                .append(created(
                    &format!("gc-{i}"),
                    serde_json::json!({"title": "t"}),
                ))
                .unwrap();
        }
        let result = ledger.process_past_cursor("campd", &mut |_conn, event| {
            if event.seq == 2 {
                return Err(CoreError::Corrupt("injected".to_owned()));
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            ledger.cursor("campd").unwrap(),
            1,
            "cursor halts before the failure"
        );

        // resume with a healthy processor: exactly the unprocessed tail
        let mut tail = Vec::new();
        ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                tail.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert_eq!(tail, vec![2, 3]);
    }

    #[test]
    fn processor_effects_commit_atomically_with_the_cursor() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        // The processor writes a marker row through the transaction's
        // connection, then fails on seq 2: seq 1's effect+cursor committed,
        // seq 2's effect rolled back with its cursor advance.
        let result = ledger.process_past_cursor("campd", &mut |conn, event| {
            conn.execute(
                "INSERT INTO cursors (name, seq) VALUES ('marker', ?1)
                 ON CONFLICT(name) DO UPDATE SET seq = excluded.seq",
                [event.seq],
            )?;
            if event.seq == 2 {
                return Err(CoreError::Corrupt("injected".to_owned()));
            }
            Ok(())
        });
        assert!(result.is_err());
        let marker: i64 = ledger
            .conn
            .query_row("SELECT seq FROM cursors WHERE name = 'marker'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(marker, 1, "seq 2's effect must roll back with the cursor");
        assert_eq!(ledger.cursor("campd").unwrap(), 1);
    }

    #[test]
    fn campd_lifecycle_events_are_log_only() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(input(
                EventType::CampdStarted,
                None,
                None,
                serde_json::json!({}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::CampdStopped,
                None,
                None,
                serde_json::json!({}),
            ))
            .unwrap();
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 2);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 0);
        assert_eq!(count(&ledger, "SELECT count(*) FROM sessions"), 0);
    }

    #[test]
    fn campd_autostarted_is_validated_and_log_only() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(input(
                EventType::CampdAutostarted,
                None,
                None,
                serde_json::json!({"verb": "top"}),
            ))
            .unwrap();
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 0);
        assert_eq!(count(&ledger, "SELECT count(*) FROM sessions"), 0);

        // missing verb fails fast, appends nothing
        assert!(
            ledger
                .append(input(
                    EventType::CampdAutostarted,
                    None,
                    None,
                    serde_json::json!({})
                ))
                .is_err()
        );
        // unknown fields fail fast
        assert!(
            ledger
                .append(input(
                    EventType::CampdAutostarted,
                    None,
                    None,
                    serde_json::json!({"verb": "top", "extra": 1}),
                ))
                .is_err()
        );
        // empty verb fails fast
        assert!(
            ledger
                .append(input(
                    EventType::CampdAutostarted,
                    None,
                    None,
                    serde_json::json!({"verb": ""}),
                ))
                .is_err()
        );
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
    }

    #[test]
    fn unknown_payload_fields_fail_fast() {
        let (_dir, mut ledger) = temp_ledger();
        match ledger.append(created(
            "gc-1",
            serde_json::json!({"title": "one", "dependson": ["gc-0"]}),
        )) {
            Err(CoreError::InvalidEventData { reason, .. }) => {
                assert!(reason.contains("dependson"), "reason was: {reason}");
            }
            other => panic!("expected InvalidEventData, got {other:?}"),
        }
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 0);
    }

    /// A representative slice of ledger life: creates, deps, claim, close,
    /// sessions, log-only events. 8 events total.
    fn seed_representative(ledger: &mut Ledger) {
        ledger
            .append(input(
                EventType::CampdStarted,
                None,
                None,
                serde_json::json!({}),
            ))
            .unwrap();
        ledger
            .append(created(
                "gc-1",
                serde_json::json!({"title": "implement", "description": "the change", "labels": ["cli"]}),
            ))
            .unwrap();
        ledger
            .append(created(
                "gc-2",
                serde_json::json!({"title": "review", "needs": ["gc-1"]}),
            ))
            .unwrap();
        ledger.append(woke("camp/dev/1")).unwrap();
        ledger
            .append(input(
                EventType::BeadClaimed,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"session": "camp/dev/1"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::BeadClosed,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"outcome": "pass", "reason": "done"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::SessionStopped,
                Some("gc"),
                None,
                serde_json::json!({"name": "camp/dev/1"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::CampdStopped,
                None,
                None,
                serde_json::json!({}),
            ))
            .unwrap();
    }

    #[test]
    fn refold_is_clean_after_a_representative_sequence() {
        let (_dir, mut ledger) = temp_ledger();
        seed_representative(&mut ledger);
        let report = ledger.refold_check().unwrap();
        assert_eq!(report.events_replayed, 8);
        assert!(
            report.drift.is_empty(),
            "unexpected drift: {:?}",
            report.drift
        );
    }

    #[test]
    fn refold_on_an_empty_log_is_clean() {
        let (_dir, mut ledger) = temp_ledger();
        let report = ledger.refold_check().unwrap();
        assert_eq!(report.events_replayed, 0);
        assert!(report.drift.is_empty());
    }

    #[test]
    fn refold_detects_tampering_in_every_state_table() {
        let (_dir, mut ledger) = temp_ledger();
        seed_representative(&mut ledger);
        ledger
            .conn
            .execute("UPDATE beads SET status = 'open' WHERE id = 'gc-1'", [])
            .unwrap();
        ledger
            .conn
            .execute(
                "INSERT INTO deps (bead_id, needs_id) VALUES ('gc-2', 'gc-99')",
                [],
            )
            .unwrap();
        ledger
            .conn
            .execute(
                "UPDATE sessions SET status = 'live', ended_ts = NULL WHERE name = 'camp/dev/1'",
                [],
            )
            .unwrap();
        ledger
            .conn
            .execute("DELETE FROM search WHERE kind = 'close'", [])
            .unwrap();

        let report = ledger.refold_check().unwrap();
        for table in ["beads", "deps", "sessions", "search"] {
            assert!(
                report.drift.iter().any(|d| d.table == table),
                "no drift reported for {table}: {:?}",
                report.drift
            );
        }
        assert!(
            report
                .drift
                .iter()
                .any(|d| d.table == "beads" && d.detail.contains("gc-1")),
            "beads drift should name gc-1: {:?}",
            report.drift
        );
    }

    #[test]
    fn refold_repair_rebuilds_state_from_the_log() {
        let (_dir, mut ledger) = temp_ledger();
        seed_representative(&mut ledger);
        ledger
            .conn
            .execute(
                "UPDATE beads SET status = 'open', outcome = NULL WHERE id = 'gc-1'",
                [],
            )
            .unwrap();
        assert!(!ledger.refold_check().unwrap().drift.is_empty());

        let repaired = ledger.refold_repair().unwrap();
        assert_eq!(repaired.events_replayed, 8);
        assert!(
            repaired.drift.is_empty(),
            "drift after repair: {:?}",
            repaired.drift
        );

        let (status, outcome): (String, String) = ledger
            .conn
            .query_row(
                "SELECT status, outcome FROM beads WHERE id = 'gc-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((status.as_str(), outcome.as_str()), ("closed", "pass"));
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

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

/// How many events `process_past_cursor` holds in memory at once (PR #8
/// review finding 4): large enough that a page is one indexed read, small
/// enough that a 1M-event first-start catch-up never balloons RSS.
const CATCH_UP_PAGE_SIZE: usize = 500;

/// One `{"op":"status"}` snapshot (master plan Phase 7 protocol): computed
/// from the state tables at request time — no cached copy to drift.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StatusSummary {
    pub live_sessions: Vec<String>,
    pub ready: u64,
    pub open: u64,
}

impl Ledger {
    pub fn open(db_path: &Path) -> Result<Self, CoreError> {
        Self::open_with_clock(db_path, Box::new(SystemClock))
    }

    pub fn open_with_clock(db_path: &Path, clock: Box<dyn Clock>) -> Result<Self, CoreError> {
        let conn = schema::open_db(db_path)?;
        Ok(Self { conn, clock })
    }

    /// Open an existing ledger read-only (`SQLITE_OPEN_READ_ONLY`) — the
    /// `camp export` path (spec §15.3): read-only by construction, not by
    /// discipline (PR #18 review finding 4). Appends fail; a missing
    /// database is a hard error, never created.
    pub fn open_read_only(db_path: &Path) -> Result<Self, CoreError> {
        let conn = schema::open_db_read_only(db_path)?;
        Ok(Self {
            conn,
            clock: Box::new(SystemClock),
        })
    }

    /// The clock's current timestamp (RFC3339 UTC, whole seconds) — the same
    /// source event timestamps use, so run ids are deterministic in tests.
    pub fn now_utc(&self) -> String {
        self.clock.now_utc()
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
            seqs.push(insert_and_fold(&tx, &ts, input)?);
        }
        tx.commit()?;
        Ok(seqs)
    }

    /// The single write path (insert the event row + fold its state effect)
    /// on a caller-provided connection. MUST run inside a transaction the
    /// caller commits — the sanctioned caller is a `process_past_cursor`
    /// processor (spec §7.3), whose appends then commit atomically with the
    /// cursor advance (exactly-once even across kill -9). Same fold, same
    /// validation, same refold story as `append`.
    pub fn append_on(conn: &Connection, ts: &str, input: EventInput) -> Result<Seq, CoreError> {
        insert_and_fold(conn, ts, input)
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
    pub fn dispatchable_beads(&self) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::dispatchable_beads(&self.conn)
    }

    /// Allocate the next session name `<camp>/<agent>/<n>` (spec §7.4,
    /// master plan Phase 8). n = 1 + the highest existing suffix among
    /// sessions with this exact prefix; suffix parsing happens in Rust so
    /// odd agent names cannot break a LIKE pattern. Only campd allocates
    /// in v1; the fold's duplicate-name rejection backstops any race.
    pub fn next_session_name(&self, camp: &str, agent: &str) -> Result<String, CoreError> {
        let prefix = format!("{camp}/{agent}/");
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sessions WHERE agent = ?1")?;
        let names = stmt.query_map([agent], |r| r.get::<_, String>(0))?;
        let mut max_n: i64 = 0;
        for name in names {
            let name = name?;
            if let Some(rest) = name.strip_prefix(&prefix)
                && let Ok(n) = rest.parse::<i64>()
            {
                max_n = max_n.max(n);
            }
        }
        Ok(format!("{prefix}{}", max_n + 1))
    }

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

    /// Full-fidelity bead rows for `camp export` (spec §15.3): every
    /// `beads` column plus the `needs` edges, in creation order.
    pub fn export_beads(&self) -> Result<Vec<crate::export::ExportBead>, CoreError> {
        crate::export::export_beads(&self.conn)
    }

    /// One bead's current state, or `None`.
    pub fn get_bead(&self, id: &str) -> Result<Option<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::get_bead(&self.conn, id)
    }

    /// Events with `from <= seq <= to` (unbounded above when `to` is None),
    /// in seq order.
    /// Whether any event exists past `seq` — the settle-fixpoint probe
    /// (PR #14 review finding 8: SELECT 1 LIMIT 1, never a materialized
    /// tail).
    pub fn has_events_past(&self, seq: Seq) -> Result<bool, CoreError> {
        use rusqlite::OptionalExtension;
        let hit: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM events WHERE seq > ?1 LIMIT 1", [seq], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(hit.is_some())
    }

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

    /// Live session names, ready-bead count, and open-bead count. `open`
    /// counts `status='open'` beads (blocked ones included; claimed and
    /// closed ones not).
    pub fn status_summary(&self) -> Result<StatusSummary, CoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sessions WHERE status = 'live' ORDER BY name")?;
        let live_sessions: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let ready = crate::readiness::ready_beads(&self.conn, None)?.len() as u64;
        let open: i64 = self.conn.query_row(
            "SELECT count(*) FROM beads WHERE status = 'open'",
            [],
            |r| r.get(0),
        )?;
        let open = u64::try_from(open)
            .map_err(|_| CoreError::Corrupt(format!("negative open-bead count {open}")))?;
        Ok(StatusSummary {
            live_sessions,
            ready,
            open,
        })
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
    ///
    /// The backlog drains one page at a time (PR #8 review finding 4): peak
    /// memory is bounded by `CATCH_UP_PAGE_SIZE` events even on a first
    /// start against a year-scale ledger, keeping the idle-RSS budget
    /// (invariant 1) intact after catch-up.
    pub fn process_past_cursor(
        &mut self,
        name: &str,
        process: &mut dyn FnMut(&Connection, &Event) -> Result<(), CoreError>,
    ) -> Result<Seq, CoreError> {
        let mut cursor = self.cursor(name)?;
        loop {
            let page = self.events_page(cursor + 1, CATCH_UP_PAGE_SIZE)?;
            if page.is_empty() {
                return Ok(cursor);
            }
            for event in page {
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
        }
    }

    /// At most `limit` events with `seq >= from`, in seq order — the
    /// pagination read behind `process_past_cursor`.
    fn events_page(&self, from: Seq, limit: usize) -> Result<Vec<Event>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, type, rig, actor, bead, data FROM events
             WHERE seq >= ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![from, limit as i64], row_to_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
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

    /// Every event of one type, in seq order (via the `events_type` index).
    /// Order counts are small; this backs fire reconciliation, not user
    /// queries (spec §7.2: state reads go to the state tables).
    pub fn events_of_type(&self, kind: crate::event::EventType) -> Result<Vec<Event>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, type, rig, actor, bead, data FROM events
             WHERE type = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([kind.as_str()], row_to_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Is there any event of `kind` with exactly this actor? A targeted
    /// existence probe bounded by the `events_type` index (PR #13 review
    /// LOW 5) — the fire-dedupe hot path must not scan the ledger.
    pub fn has_event_with_actor(
        &self,
        kind: crate::event::EventType,
        actor: &str,
    ) -> Result<bool, CoreError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM events WHERE type = ?1 AND actor = ?2 LIMIT 1",
                params![kind.as_str(), actor],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Is there any event of `kind` whose `data.<field>` equals this
    /// integer? `json_extract` over the type-indexed subset (PR #13 review
    /// LOW 5).
    pub fn has_event_with_data_i64(
        &self,
        kind: crate::event::EventType,
        field: &str,
        value: i64,
    ) -> Result<bool, CoreError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM events
                 WHERE type = ?1 AND json_extract(data, '$.' || ?2) = ?3 LIMIT 1",
                params![kind.as_str(), field, value],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Is there any event of `kind` whose `data.<f1>` and `data.<f2>`
    /// equal these strings? Targeted existence probe over the type-indexed
    /// subset (PR #13 fix-pass review: idempotent cron-fire declaration).
    pub fn has_event_with_data_strs(
        &self,
        kind: crate::event::EventType,
        (f1, v1): (&str, &str),
        (f2, v2): (&str, &str),
    ) -> Result<bool, CoreError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM events
                 WHERE type = ?1
                   AND json_extract(data, '$.' || ?2) = ?3
                   AND json_extract(data, '$.' || ?4) = ?5
                 LIMIT 1",
                params![kind.as_str(), f1, v1, f2, v2],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
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

/// The one write path shared by `append`/`append_batch`/`append_on`: insert
/// the event row (monotonic seq) and apply its fold in the caller's open
/// transaction (spec §7.2 — a write is one transaction).
fn insert_and_fold(conn: &Connection, ts: &str, input: EventInput) -> Result<Seq, CoreError> {
    let EventInput {
        kind,
        rig,
        actor,
        bead,
        data,
    } = input;
    conn.execute(
        "INSERT INTO events (ts, type, rig, actor, bead, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![ts, kind.as_str(), rig, actor, bead, data.to_string()],
    )?;
    let seq = conn.last_insert_rowid();
    let event = Event {
        seq,
        ts: ts.to_owned(),
        kind,
        rig,
        actor,
        bead,
        data,
    };
    fold::apply(conn, &event)?;
    Ok(seq)
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

    // ---- Phase 8 events (worker.milestone, worktree.kept,
    // bead.worktree.reaped, dispatch.failed) + extended session payloads --

    fn seeded_bead(l: &mut Ledger, id: &str) {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"title": "t"}),
        })
        .unwrap();
    }

    #[test]
    fn has_events_past_probes_without_materializing() {
        // PR #14 review finding 8: the settle fixpoint probe must not
        // materialize the tail just to test emptiness.
        let (_dir, mut l) = temp_ledger();
        assert!(!l.has_events_past(0).unwrap());
        seeded_bead(&mut l, "gc-1");
        assert!(l.has_events_past(0).unwrap());
        assert!(!l.has_events_past(1).unwrap());
    }

    #[test]
    fn next_session_name_allocates_per_camp_and_agent() {
        let (_dir, mut l) = temp_ledger();
        assert_eq!(l.next_session_name("t", "dev").unwrap(), "t/dev/1");
        for name in ["t/dev/1", "t/dev/7", "other/dev/40"] {
            l.append(EventInput {
                kind: EventType::SessionWoke,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"name": name, "agent": "dev"}),
            })
            .unwrap();
        }
        // other agents and other camps do not collide
        assert_eq!(l.next_session_name("t", "dev").unwrap(), "t/dev/8");
        assert_eq!(
            l.next_session_name("t", "reviewer").unwrap(),
            "t/reviewer/1"
        );
    }

    #[test]
    fn worker_milestone_is_log_only_and_validates_payload() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        let seq = l
            .append(EventInput {
                kind: EventType::WorkerMilestone,
                rig: Some("gc".into()),
                actor: "t/dev/1".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"text": "tests passing"}),
            })
            .unwrap();
        assert!(seq > 0);
        // no bead: still fine (a general breadcrumb)
        l.append(EventInput {
            kind: EventType::WorkerMilestone,
            rig: None,
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({"text": "note"}),
        })
        .unwrap();
        // empty text rejected, nothing appended
        let before = l.events_range(1, None).unwrap().len();
        let err = l.append(EventInput {
            kind: EventType::WorkerMilestone,
            rig: None,
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({"text": ""}),
        });
        assert!(err.is_err());
        // unknown bead rejected
        let err = l.append(EventInput {
            kind: EventType::WorkerMilestone,
            rig: None,
            actor: "cli".into(),
            bead: Some("gc-999".into()),
            data: serde_json::json!({"text": "x"}),
        });
        assert!(err.is_err());
        assert_eq!(l.events_range(1, None).unwrap().len(), before);
    }

    #[test]
    fn worktree_events_are_log_only_and_validate_payloads() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        l.append(EventInput {
            kind: EventType::WorktreeKept,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"path": "/camp/worktrees/gc-1", "reason": "outcome fail"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::BeadWorktreeReaped,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"path": "/camp/worktrees/gc-1"}),
        })
        .unwrap();
        // missing bead is an error for both
        for (kind, data) in [
            (
                EventType::WorktreeKept,
                serde_json::json!({"path": "/p", "reason": "r"}),
            ),
            (
                EventType::BeadWorktreeReaped,
                serde_json::json!({"path": "/p"}),
            ),
        ] {
            let err = l.append(EventInput {
                kind,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data,
            });
            assert!(err.is_err(), "{kind:?} without a bead must fail");
        }
    }

    #[test]
    fn dispatch_failed_requires_bead_and_reason() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        l.append(EventInput {
            kind: EventType::DispatchFailed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"reason": "no agent named \"dev\""}),
        })
        .unwrap();
        let err = l.append(EventInput {
            kind: EventType::DispatchFailed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"reason": ""}),
        });
        assert!(err.is_err(), "empty reason must fail");
    }

    #[test]
    fn session_woke_accepts_worktree_and_session_end_accepts_exit_details() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({
                "name": "t/dev/1", "agent": "dev", "rig": "gc",
                "claude_session_id": "7bd2befc-b018-4080-8738-429d541b3646",
                "transcript_path": "/home/u/.claude/projects/-x/7bd2befc.jsonl",
                "bead": "gc-1",
                "worktree": "/camp/worktrees/gc-1"
            }),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionCrashed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/1", "exit_code": 7}),
        })
        .unwrap();
        // signal + reason variants also parse (fresh session to end)
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/2", "agent": "dev"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionCrashed,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/2", "signal": 9, "reason": "spawn failed: ..."}),
        })
        .unwrap();
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
    fn close_outcome_vocabulary_is_enforced() {
        // Phase 9 (plan Decision 2, approved): "skipped" joined the close
        // vocabulary — campd's finalization close for unreachable steps.
        // The out-of-vocabulary counterexample is a value gc has but camp
        // deliberately does not accept ("missing_root").
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        match ledger.append(input(
            EventType::BeadClosed,
            Some("gc"),
            Some("gc-1"),
            serde_json::json!({"outcome": "missing_root"}),
        )) {
            Err(CoreError::InvalidEventData { reason, .. }) => {
                assert!(reason.contains("missing_root"), "reason was: {reason}");
            }
            other => panic!("expected InvalidEventData, got {other:?}"),
        }
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
    }

    /// PR #18 review finding 1: bd v1.0.4 silently SKIPS memory records
    /// with an empty value and REJECTS a whole import over an empty-title
    /// issue line — so an empty title must never enter the ledger at all
    /// (fail fast at the creation boundary, fixing every consumer).
    #[test]
    fn bead_titles_must_be_non_empty() {
        let (_dir, mut ledger) = temp_ledger();
        for bad in ["", "   "] {
            match ledger.append(created("gc-1", serde_json::json!({"title": bad}))) {
                Err(CoreError::InvalidEventData { reason, .. }) => {
                    assert!(reason.contains("title"), "reason was: {reason}");
                }
                other => panic!("expected InvalidEventData, got {other:?}"),
            }
        }
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 0);

        // an update cannot blank a title either
        ledger
            .append(created("gc-1", serde_json::json!({"title": "ok"})))
            .unwrap();
        match ledger.append(input(
            EventType::BeadUpdated,
            Some("gc"),
            Some("gc-1"),
            serde_json::json!({"title": "  "}),
        )) {
            Err(CoreError::InvalidEventData { reason, .. }) => {
                assert!(reason.contains("title"), "reason was: {reason}");
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
    fn status_summary_reports_live_sessions_ready_and_open() {
        let (_dir, mut ledger) = temp_ledger();
        // empty camp: all zeroes
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 0,
                open: 0
            }
        );

        // gc-1 ready; gc-2 open but blocked on gc-1
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created(
                "gc-2",
                serde_json::json!({"title": "two", "needs": ["gc-1"]}),
            ))
            .unwrap();
        // one live session, one stopped
        ledger.append(woke("camp/dev/1")).unwrap();
        ledger
            .append(input(
                EventType::SessionWoke,
                Some("gc"),
                None,
                serde_json::json!({"name": "camp/dev/2", "agent": "dev"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::SessionStopped,
                Some("gc"),
                None,
                serde_json::json!({"name": "camp/dev/2"}),
            ))
            .unwrap();

        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec!["camp/dev/1".to_owned()],
                ready: 1,
                open: 2
            }
        );
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

    /// PR #8 review finding 4: catch-up must not materialize the whole
    /// backlog at once — it drains in pages. These assertions pin the
    /// pagination's correctness (order preserved, nothing skipped or
    /// repeated across page boundaries); the memory bound itself is the
    /// page-size constant.
    #[test]
    fn process_past_cursor_pages_through_a_large_backlog() {
        let (_dir, mut ledger) = temp_ledger();
        // 2.4x the page size, plus a partial final page
        let total = CATCH_UP_PAGE_SIZE as i64 * 2 + 203;
        for i in 1..=total {
            ledger
                .append(created(
                    &format!("gc-{i}"),
                    serde_json::json!({"title": "t"}),
                ))
                .unwrap();
        }
        let mut seen = Vec::new();
        let end = ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                seen.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert_eq!(end, total);
        assert_eq!(seen.len() as i64, total, "every event exactly once");
        assert_eq!(seen, (1..=total).collect::<Vec<_>>(), "in seq order");
        assert_eq!(ledger.cursor("campd").unwrap(), total);

        // nothing left
        let mut again = Vec::new();
        ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                again.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert!(again.is_empty());
    }

    /// A processing error just past a page boundary halts the cursor on the
    /// boundary; resume covers exactly the tail (finding 4 must not weaken
    /// the exactly-once guarantee).
    #[test]
    fn a_mid_page_error_resumes_exactly_across_page_boundaries() {
        let (_dir, mut ledger) = temp_ledger();
        let page = CATCH_UP_PAGE_SIZE as i64;
        let total = page + 103;
        for i in 1..=total {
            ledger
                .append(created(
                    &format!("gc-{i}"),
                    serde_json::json!({"title": "t"}),
                ))
                .unwrap();
        }
        let poison = page + 1; // first event of the second page
        let result = ledger.process_past_cursor("campd", &mut |_conn, event| {
            if event.seq == poison {
                return Err(CoreError::Corrupt("injected".to_owned()));
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(ledger.cursor("campd").unwrap(), page);

        let mut tail = Vec::new();
        ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                tail.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert_eq!(tail, (poison..=total).collect::<Vec<_>>());
    }

    #[test]
    fn append_on_writes_through_a_processor_transaction_atomically() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(input(
                EventType::CampdStarted,
                None,
                None,
                serde_json::json!({}),
            ))
            .unwrap();
        // A processor that appends a config.changed for the event it sees:
        let end = ledger
            .process_past_cursor("t", &mut |conn, event| {
                if event.kind == EventType::CampdStarted {
                    Ledger::append_on(
                        conn,
                        "2026-07-06T07:00:00Z",
                        EventInput {
                            kind: EventType::ConfigChanged,
                            rig: None,
                            actor: "campd".into(),
                            bead: None,
                            data: serde_json::json!({"path":"camp.toml","applied":true,"orders":0}),
                        },
                    )?;
                }
                Ok(())
            })
            .unwrap();
        // process_past_cursor drains pages until empty WITHIN one call: the
        // config.changed appended while processing seq 1 lands at seq 2 and
        // is processed by the same call.
        assert_eq!(
            end, 2,
            "the same call drains events appended mid-processing"
        );
        let events = ledger.events_range(1, None).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, EventType::ConfigChanged);
        assert_eq!(ledger.cursor("t").unwrap(), 2);
    }

    #[test]
    fn append_on_rejects_invalid_payloads_like_append() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(input(
                EventType::CampdStarted,
                None,
                None,
                serde_json::json!({}),
            ))
            .unwrap();
        let err = ledger.process_past_cursor("t", &mut |conn, _event| {
            Ledger::append_on(
                conn,
                "2026-07-06T07:00:00Z",
                EventInput {
                    kind: EventType::ConfigChanged,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({"applied": true}), // missing path/orders
                },
            )?;
            Ok(())
        });
        assert!(err.is_err());
        // the failed processor transaction rolled back: no event, no cursor move
        assert_eq!(ledger.events_range(1, None).unwrap().len(), 1);
        assert_eq!(ledger.cursor("t").unwrap(), 0);
    }

    #[test]
    fn targeted_event_existence_queries() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(input(
                EventType::OrderFailed,
                None,
                None,
                serde_json::json!({"order":"t","fired_seq":41,"error":"e"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::ConfigChanged,
                None,
                None,
                serde_json::json!({"path":"p","applied":true,"orders":0}),
            ))
            .unwrap();
        assert!(
            ledger
                .has_event_with_data_i64(EventType::OrderFailed, "fired_seq", 41)
                .unwrap()
        );
        assert!(
            !ledger
                .has_event_with_data_i64(EventType::OrderFailed, "fired_seq", 42)
                .unwrap()
        );
        // actor equality, bounded by the type index
        assert!(
            ledger
                .has_event_with_actor(EventType::ConfigChanged, "test")
                .unwrap()
        );
        assert!(
            !ledger
                .has_event_with_actor(EventType::ConfigChanged, "order:t:41")
                .unwrap()
        );
        assert!(
            !ledger
                .has_event_with_actor(EventType::RunCooked, "test")
                .unwrap()
        );
        // two-string-field probe (idempotent cron-fire declaration)
        ledger
            .append(input(
                EventType::OrderFired,
                None,
                None,
                serde_json::json!({"order":"t","trigger":"cron","scheduled_ts":"2026-07-06T07:00:00Z"}),
            ))
            .unwrap();
        assert!(
            ledger
                .has_event_with_data_strs(
                    EventType::OrderFired,
                    ("order", "t"),
                    ("scheduled_ts", "2026-07-06T07:00:00Z"),
                )
                .unwrap()
        );
        assert!(
            !ledger
                .has_event_with_data_strs(
                    EventType::OrderFired,
                    ("order", "t"),
                    ("scheduled_ts", "2026-07-06T08:00:00Z"),
                )
                .unwrap()
        );
        assert!(
            !ledger
                .has_event_with_data_strs(
                    EventType::OrderFired,
                    ("order", "u"),
                    ("scheduled_ts", "2026-07-06T07:00:00Z"),
                )
                .unwrap()
        );
    }

    #[test]
    fn events_of_type_lists_exactly_that_kind() {
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
        assert_eq!(
            ledger
                .events_of_type(EventType::CampdStarted)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            ledger.events_of_type(EventType::OrderFired).unwrap().len(),
            0
        );
    }

    #[test]
    fn order_events_are_validated_and_log_only() {
        let (_dir, mut ledger) = temp_ledger();
        for data in [
            serde_json::json!({"order":"t","trigger":"cron","scheduled_ts":"2026-07-06T07:00:00Z"}),
            serde_json::json!({"order":"t","trigger":"cron","scheduled_ts":"2026-07-06T07:00:00Z","catch_up":true}),
            serde_json::json!({"order":"t","trigger":"event","cause_seq":4}),
            serde_json::json!({"order":"t","trigger":"manual"}),
        ] {
            ledger
                .append(input(EventType::OrderFired, None, None, data))
                .unwrap();
        }
        ledger
            .append(input(
                EventType::OrderCompleted,
                None,
                None,
                serde_json::json!({"order":"t","fired_seq":1,"root_bead":"gc-1","run_id":"r","outcome":"pass"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::OrderFailed,
                None,
                None,
                serde_json::json!({"order":"t","fired_seq":1,"error":"formula not found"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::OrderFailed,
                None,
                None,
                serde_json::json!({"order":"t","fired_seq":1,"root_bead":"gc-1","run_id":"r","outcome":"fail"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::ConfigChanged,
                None,
                None,
                serde_json::json!({"path":"camp.toml","applied":true,"orders":2}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::ConfigChanged,
                None,
                None,
                serde_json::json!({"path":"camp.toml","applied":false,"error":"unknown field"}),
            ))
            .unwrap();
        // all log-only: no state effect
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 9);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 0);
    }

    #[test]
    fn malformed_order_events_are_rejected() {
        let (_dir, mut ledger) = temp_ledger();
        for (kind, data) in [
            (
                EventType::OrderFired,
                serde_json::json!({"order":"t","trigger":"vibes"}),
            ),
            (
                EventType::OrderFired,
                serde_json::json!({"order":"t","trigger":"cron"}), // no scheduled_ts
            ),
            (
                EventType::OrderFired,
                serde_json::json!({"order":"t","trigger":"event"}), // no cause_seq
            ),
            (
                EventType::OrderFired,
                serde_json::json!({"order":"t","trigger":"manual","catch_up":true}),
            ),
            (
                EventType::OrderCompleted,
                serde_json::json!({"order":"t","fired_seq":1,"root_bead":"gc-1","run_id":"r","outcome":"fail"}),
            ),
            (
                EventType::OrderFailed,
                serde_json::json!({"order":"t","fired_seq":1}), // neither shape
            ),
            (
                EventType::OrderFailed,
                serde_json::json!({"order":"t","fired_seq":1,"error":"e","root_bead":"gc-1"}), // both
            ),
            (
                EventType::ConfigChanged,
                serde_json::json!({"path":"p","applied":true,"error":"e"}),
            ),
            (
                EventType::ConfigChanged,
                serde_json::json!({"path":"p","applied":false}),
            ),
            (
                EventType::OrderFired,
                serde_json::json!({"order":"t","trigger":"manual","bogus":1}),
            ),
        ] {
            assert!(
                ledger
                    .append(input(kind, None, None, data.clone()))
                    .is_err(),
                "{kind:?} {data}"
            );
        }
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

    /// PR #18 review finding 4: `camp export` opens the ledger read-only
    /// by construction — reads work, appends fail, and a missing database
    /// is never created.
    #[test]
    fn read_only_open_reads_but_never_writes_or_creates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.db");
        {
            let mut rw = Ledger::open(&path).unwrap();
            rw.append(created("gc-1", serde_json::json!({"title": "one"})))
                .unwrap();
        }
        let mut ro = Ledger::open_read_only(&path).unwrap();
        assert_eq!(ro.export_beads().unwrap().len(), 1);
        assert!(
            ro.append(created("gc-2", serde_json::json!({"title": "two"})))
                .is_err(),
            "appends must fail on a read-only ledger"
        );

        let missing = dir.path().join("nope.db");
        assert!(Ledger::open_read_only(&missing).is_err());
        assert!(!missing.exists(), "read-only open must never create a db");
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

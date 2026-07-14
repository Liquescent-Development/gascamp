//! The Gas Camp ledger: one WAL-mode SQLite file holding the append-only
//! event log and the state tables folded from it (spec §7).

mod fold;
mod refold;
mod schema;

pub use refold::{DriftEntry, RefoldReport};

use std::path::Path;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

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
    pub stuck: u64,
}

/// One live `sessions` registry row with its `session.woke` provenance
/// (Phase 11 adoption, spec §8.5). `woke_actor == "campd"` marks a
/// campd-spawned worker; anything else is hook-registered (annotate-only).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub name: String,
    pub agent: String,
    pub rig: Option<String>,
    pub claude_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub pid: Option<i64>,
    pub bead: Option<String>,
    pub spawned_ts: String,
    pub woke_actor: String,
    pub worktree: Option<String>,
    /// The rig's base commit at dispatch time (Phase 3, Q4) — the shipped
    /// gate's descent reference; None when the rig had none (non-repo /
    /// unborn HEAD) or the woke predates Phase 3.
    pub base: Option<String>,
    /// F7 pins as spawned (Phase 3, #48 finding 1) — re-applied on resume
    /// turns; None = registered without pins, resumes bare.
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<String>,
    /// `live` / `stopped` / `crashed` (the sessions table CHECK set).
    pub status: String,
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

    // ---- Phase 9 graph-runtime reads (thin wrappers over the pure
    // functions in formula::runtime, mirroring the readiness wrappers) ----

    /// A bead's run membership (`None` for plain beads; `step_id: None`
    /// marks a run root).
    pub fn run_membership(
        &self,
        bead: &str,
    ) -> Result<Option<crate::formula::runtime::RunMembership>, CoreError> {
        crate::formula::runtime::run_membership(&self.conn, bead)
    }

    /// All beads of one run step (anchor + attempts), creation order.
    pub fn run_step_beads(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::formula::runtime::run_step_beads(&self.conn, run_id, step_id)
    }

    /// Does any bead carry this `run_id`? (`camp create --run` fails fast on an
    /// unknown run: a member silently attached to a run that does not exist
    /// would simply never be scattered, and nothing would say why.)
    pub fn run_exists(&self, run_id: &str) -> Result<bool, CoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM beads WHERE run_id = ?1)",
            [run_id],
            |r| r.get(0),
        )?)
    }

    /// A bead's metadata, with the dedicated columns projected over it
    /// (compat §6.1; `readiness::PROJECTED_METADATA`).
    pub fn bead_metadata(
        &self,
        bead: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, CoreError> {
        crate::readiness::bead_metadata(&self.conn, bead)
    }

    /// The run MEMBERS a drain scatters over (D3).
    pub fn run_members(
        &self,
        ctx: &crate::formula::runtime::RunContext,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::formula::runtime::run_members(&self.conn, ctx)
    }

    /// The item runs already scattered for a drain anchor, by index.
    pub fn drain_children(
        &self,
        anchor: &str,
    ) -> Result<std::collections::BTreeMap<usize, crate::readiness::BeadRow>, CoreError> {
        crate::formula::runtime::drain_children(&self.conn, anchor)
    }

    /// Every member this anchor holds a reservation on (status-agnostic — V-4).
    pub fn reservations_held_by(
        &self,
        anchor: &str,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::formula::runtime::reservations_held_by(&self.conn, anchor)
    }

    /// Reservations whose holding anchor is closed or gone (the orphan sweep).
    pub fn orphaned_reservations(&self) -> Result<Vec<(String, String)>, CoreError> {
        crate::formula::runtime::orphaned_reservations(&self.conn)
    }

    /// A bead row by id.
    pub fn bead_row(&self, bead: &str) -> Result<Option<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::get_bead(&self.conn, bead)
    }

    /// The attempts of a looping step (its beads minus the anchor),
    /// creation order.
    pub fn step_attempts(
        &self,
        run_id: &str,
        step_id: &str,
        anchor: &str,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::formula::runtime::attempts(&self.conn, run_id, step_id, anchor)
    }

    /// The retry budget already spent on a step's attempts.
    pub fn transient_fails_used(
        &self,
        attempts: &[crate::readiness::BeadRow],
    ) -> Result<u32, CoreError> {
        crate::formula::runtime::transient_fails_used(&self.conn, attempts)
    }

    /// The data of a bead's close event, if closed.
    pub fn close_event_data(&self, bead: &str) -> Result<Option<serde_json::Value>, CoreError> {
        crate::formula::runtime::close_event_data(&self.conn, bead)
    }

    /// The data of a bead's creation event (authored title/description).
    pub fn created_event_data(&self, bead: &str) -> Result<Option<serde_json::Value>, CoreError> {
        crate::formula::runtime::created_event_data(&self.conn, bead)
    }

    /// The bond children already cooked for an anchor, by index (Phase 9).
    pub fn bond_children(
        &self,
        anchor: &str,
    ) -> Result<std::collections::BTreeMap<usize, crate::readiness::BeadRow>, CoreError> {
        crate::formula::runtime::bond_children(&self.conn, anchor)
    }

    /// The dead-end batch for a run that can never advance (Phase 9).
    pub fn dead_end_inputs(
        &self,
        run_id: &str,
        cause_seq: Seq,
        reason: &str,
    ) -> Result<Vec<EventInput>, CoreError> {
        crate::formula::runtime::dead_end_inputs(&self.conn, run_id, cause_seq, reason)
    }

    /// True when `bead`'s needs can never all pass.
    pub fn unsatisfiable(&self, bead: &str) -> Result<bool, CoreError> {
        crate::formula::runtime::unsatisfiable(&self.conn, bead)
    }

    /// The finalization verdict for a run (Phase 9 plan Decision 3).
    pub fn finalization(
        &self,
        ctx: &crate::formula::runtime::RunContext,
    ) -> Result<crate::formula::runtime::RunVerdict, CoreError> {
        crate::formula::runtime::finalization(&self.conn, ctx)
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

    /// Live session names, ready-task count, and open-task count. Both counts
    /// are scoped to `type='task'` — the only dispatchable work (issue #36) —
    /// so `camp top` reflects what campd will pick up, never memory or mail
    /// beads. `ready` is open and unblocked; `open` counts every open task
    /// (blocked ones included; claimed and closed ones not).
    pub fn status_summary(&self) -> Result<StatusSummary, CoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sessions WHERE status = 'live' ORDER BY name")?;
        let live_sessions: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let ready = crate::readiness::ready_task_count(&self.conn)?;
        let open = crate::readiness::open_task_count(&self.conn)?;
        let stuck = crate::readiness::stuck_task_count(&self.conn)?;
        Ok(StatusSummary {
            live_sessions,
            ready,
            open,
            stuck,
        })
    }

    /// The current status of a registered session (`live`/`stopped`/
    /// `crashed`), or `None` if the name was never registered. Session
    /// names are fold-unique forever, so this is the existence check the
    /// plugin's hook-facing `register` (idempotent — a repeat SessionStart
    /// or a resumed-after-end session must not attempt a duplicate
    /// `session.woke`) and `end --if-registered` rely on.
    pub fn session_status(&self, name: &str) -> Result<Option<String>, CoreError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT status FROM sessions WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// The live session registry rows, with each row's `session.woke`
    /// provenance joined in (Phase 11, spec §8.5): `woke_actor` tells
    /// adoption whether campd spawned the session (`"campd"`) or a hook
    /// registered it (annotate-only patrol); `worktree` is the woke
    /// payload's audit field (the sessions table deliberately has no
    /// column — schema v1 is frozen). A live row without its woke event is
    /// ledger corruption: the fold writes them in one transaction.
    ///
    /// Best-effort caveat for **hook-registered attended** rows (Phase 12):
    /// their liveness is keyed on the SessionEnd hook. If the operator's TUI
    /// dies without SessionEnd firing (kill -9, crash, power loss), the row
    /// stays `live` here indefinitely — campd cannot probe an unattributable
    /// interactive process, and adoption deliberately never crashes an
    /// attended session (spec §10). Such phantom-live rows are expected;
    /// bounded reaping is a deferred follow-up (see `patrol::adopt`).
    pub fn live_sessions(&self) -> Result<Vec<SessionRow>, CoreError> {
        self.session_rows("s.status = 'live'", [])
    }

    /// One registry row by name, any status (dispatch-lifecycle Phase 1:
    /// the converse verb must reach exited sessions for the resume path).
    /// Same woke-provenance join as `live_sessions`; a registered session
    /// without its `session.woke` event is ledger corruption.
    pub fn session_by_name(&self, name: &str) -> Result<Option<SessionRow>, CoreError> {
        Ok(self.session_rows("s.name = ?1", [name])?.into_iter().next())
    }

    /// Shared body of `live_sessions` / `session_by_name`: registry rows
    /// matching `where_clause` (a fixed, camp-authored predicate — values
    /// always arrive through `params`, never interpolated), each joined
    /// with its `session.woke` provenance (actor + worktree audit field).
    fn session_rows<P: rusqlite::Params>(
        &self,
        where_clause: &str,
        params: P,
    ) -> Result<Vec<SessionRow>, CoreError> {
        let sql = format!(
            "SELECT s.name, s.agent, s.rig, s.claude_session_id, s.transcript_path,
                    s.pid, s.bead, s.spawned_ts,
                    (SELECT e.actor FROM events e WHERE e.type = 'session.woke'
                      AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1),
                    (SELECT json_extract(e.data, '$.worktree') FROM events e
                      WHERE e.type = 'session.woke'
                      AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1),
                    (SELECT json_extract(e.data, '$.base') FROM events e
                      WHERE e.type = 'session.woke'
                      AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1),
                    (SELECT json_extract(e.data, '$.model') FROM events e
                      WHERE e.type = 'session.woke'
                      AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1),
                    (SELECT json_extract(e.data, '$.permission_mode') FROM events e
                      WHERE e.type = 'session.woke'
                      AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1),
                    (SELECT json_extract(e.data, '$.allowed_tools') FROM events e
                      WHERE e.type = 'session.woke'
                      AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1),
                    s.status
             FROM sessions s WHERE {where_clause} ORDER BY s.name"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<(SessionRow, Option<String>)> = stmt
            .query_map(params, |r| {
                let woke_actor: Option<String> = r.get(8)?;
                Ok((
                    SessionRow {
                        name: r.get(0)?,
                        agent: r.get(1)?,
                        rig: r.get(2)?,
                        claude_session_id: r.get(3)?,
                        transcript_path: r.get(4)?,
                        pid: r.get(5)?,
                        bead: r.get(6)?,
                        spawned_ts: r.get(7)?,
                        woke_actor: String::new(), // filled below after the NULL check
                        worktree: r.get(9)?,
                        base: r.get(10)?,
                        model: r.get(11)?,
                        permission_mode: r.get(12)?,
                        allowed_tools: r.get(13)?,
                        status: r.get(14)?,
                    },
                    woke_actor,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter()
            .map(|(mut row, woke_actor)| match woke_actor {
                Some(actor) => {
                    row.woke_actor = actor;
                    Ok(row)
                }
                None => Err(CoreError::Corrupt(format!(
                    "session {:?} has no session.woke event",
                    row.name
                ))),
            })
            .collect()
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

    /// cp-0 (control-plane spec §2.3): the byte offset campd has consumed
    /// for `session`'s stdout stream file. 0 when campd has never tailed
    /// it. Consumer bookkeeping (the `cursors` mold) — deliberately
    /// outside refold; durable so a campd restart resumes from the exact
    /// byte the last life consumed (§8 append-only-cursors test).
    pub fn stream_cursor(&self, session: &str) -> Result<u64, CoreError> {
        use rusqlite::OptionalExtension;
        let offset: Option<i64> = self
            .conn
            .query_row(
                "SELECT byte_offset FROM stream_cursors WHERE session_name = ?1",
                [session],
                |r| r.get(0),
            )
            .optional()?;
        Ok(offset.unwrap_or(0) as u64)
    }

    /// cp-0: persist the byte offset for `session` (UPSERT). Called only
    /// after the consumed line's ledger effect commits (§2.3), so a crash
    /// between read and persist re-reads — never loses, never silently
    /// duplicates (the ledger dedupes by request_id in phase 1+).
    ///
    /// Phase-1+ obligation: once consumed lines become `permission.pending`
    /// events with their own ledger effect, this persist must move to
    /// AFTER that effect's transaction commits (persist-after-event-
    /// commit), not after each read chunk. Phase 0 has no per-line ledger
    /// effect, so persisting after the drain is correct today.
    pub fn set_stream_cursor(&self, session: &str, offset: u64) -> Result<(), CoreError> {
        self.conn.execute(
            "INSERT INTO stream_cursors (session_name, byte_offset) VALUES (?1, ?2)
             ON CONFLICT(session_name) DO UPDATE SET byte_offset = excluded.byte_offset",
            params![session, offset as i64],
        )?;
        Ok(())
    }

    /// cp-0: drop the offset row when the session ends (the stream file is
    /// disposed at reap, §2.3). Idempotent. Keeps the table from
    /// accumulating rows for long-dead sessions.
    pub fn clear_stream_cursor(&self, session: &str) -> Result<(), CoreError> {
        self.conn.execute(
            "DELETE FROM stream_cursors WHERE session_name = ?1",
            [session],
        )?;
        Ok(())
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

    /// Write a consistent, defragmented copy of the ledger to `dest` via
    /// SQLite `VACUUM INTO`, then verify the copy with `PRAGMA
    /// integrity_check`. The copy is a single standalone db file with no
    /// WAL sidecar — safe to archive or move. `dest` must not already
    /// exist. Never modifies the source; safe on a read-only `Ledger`.
    pub fn backup_into(&self, dest: &Path) -> Result<(), CoreError> {
        if dest.exists() {
            return Err(CoreError::Backup(format!(
                "destination {} already exists",
                dest.display()
            )));
        }
        let dest_str = dest.to_str().ok_or_else(|| {
            CoreError::Backup(format!("destination {} is not valid UTF-8", dest.display()))
        })?;
        // VACUUM INTO does not accept a bound parameter for the filename;
        // inline it with single-quotes doubled to escape.
        let escaped = dest_str.replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{escaped}'"))?;

        let verify = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let report: String = verify.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if report != "ok" {
            return Err(CoreError::Backup(format!(
                "integrity_check on backup {} reported: {report}",
                dest.display()
            )));
        }
        Ok(())
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
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};
    use crate::ledger::schema::SCHEMA_VERSION;
    use crate::readiness::{EXCLUSIVE_DRAIN_RESERVATION, PROJECTED_METADATA};

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
    fn backup_into_copies_and_passes_integrity_check() {
        let (dir, mut l) = temp_ledger();
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "title": "backup me" }),
        })
        .unwrap();

        let dest = dir.path().join("backup.db");
        l.backup_into(&dest).unwrap();
        assert!(dest.exists());

        // The copy is a standalone, valid ledger carrying the same event.
        let copy = rusqlite::Connection::open(&dest).unwrap();
        let ok: String = copy
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        let n: i64 = copy
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        // Fail fast: refuse to overwrite an existing destination.
        let err = l.backup_into(&dest).unwrap_err();
        assert!(
            matches!(err, CoreError::Backup(msg) if msg.contains("already exists")),
            "expected an already-exists Backup error"
        );
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

    /// session.nudged (dispatch-lifecycle Phase 1, #29): log-only record of
    /// a turn delivered into a session — via the campd-held stdin pipe
    /// ("stdin") or claude --resume ("resume"). The session must exist
    /// (fail fast on typos); text must be non-empty; unknown fields and
    /// unknown vias are rejected (deny_unknown_fields).
    #[test]
    fn session_nudged_is_log_only_and_validated() {
        let (_dir, mut l) = temp_ledger();
        // a registered session to nudge
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "camp/dev/1", "agent": "dev", "rig": "gc"}),
        })
        .unwrap();

        // accepted: stdin and resume
        for via in ["stdin", "resume"] {
            l.append(EventInput {
                kind: EventType::SessionNudged,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"session": "camp/dev/1", "via": via, "text": "status?"}),
            })
            .unwrap();
        }
        let before = l.events_range(1, None).unwrap().len();
        // rejected: unknown session
        assert!(
            l.append(EventInput {
                kind: EventType::SessionNudged,
                rig: None,
                actor: "cli".into(),
                bead: None,
                data: serde_json::json!({"session": "camp/dev/99", "via": "stdin", "text": "x"}),
            })
            .is_err()
        );
        // rejected: bogus via
        assert!(
            l.append(EventInput {
                kind: EventType::SessionNudged,
                rig: None,
                actor: "cli".into(),
                bead: None,
                data: serde_json::json!({"session": "camp/dev/1", "via": "carrier-pigeon", "text": "x"}),
            })
            .is_err()
        );
        // rejected: blank text
        assert!(
            l.append(EventInput {
                kind: EventType::SessionNudged,
                rig: None,
                actor: "cli".into(),
                bead: None,
                data: serde_json::json!({"session": "camp/dev/1", "via": "stdin", "text": "  "}),
            })
            .is_err()
        );
        // rejected: unknown field (deny_unknown_fields)
        assert!(
            l.append(EventInput {
                kind: EventType::SessionNudged,
                rig: None,
                actor: "cli".into(),
                bead: None,
                data: serde_json::json!({
                    "session": "camp/dev/1", "via": "stdin", "text": "x", "mode": "attended",
                }),
            })
            .is_err()
        );
        // rejections appended nothing (one-transaction event+state property)
        assert_eq!(l.events_range(1, None).unwrap().len(), before);
    }

    // ---- Phase 11: the adoption registry query ---------------------------

    #[test]
    fn live_sessions_returns_registry_rows_with_their_woke_provenance() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        // w1: a fully populated campd-spawned worker
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({
                "name": "t/dev/1", "agent": "dev", "rig": "gc",
                "claude_session_id": "7bd2befc-b018-4080-8738-429d541b3646",
                "transcript_path": "/home/u/.claude/projects/-code-gc/x.jsonl",
                "bead": "gc-1", "worktree": "/camps/t/worktrees/gc-1",
                "base": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "model": "sonnet", "permission_mode": "acceptEdits",
                "allowed_tools": "Read,Edit,Bash",
            }),
        })
        .unwrap();
        // a1: a minimal hook-registered attended session
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: None,
            actor: "hook:session-start".into(),
            bead: None,
            data: serde_json::json!({"name": "a1", "agent": "operator"}),
        })
        .unwrap();
        // w2: woke then stopped — must not appear
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/2", "agent": "dev"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionStopped,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/2", "exit_code": 0}),
        })
        .unwrap();

        let rows = l.live_sessions().unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        // name-ordered: a1 then t/dev/1
        assert_eq!(rows[0].name, "a1");
        assert_eq!(rows[0].agent, "operator");
        assert_eq!(rows[0].woke_actor, "hook:session-start");
        assert!(rows[0].claude_session_id.is_none());
        assert!(rows[0].worktree.is_none());
        // Phase 3: no dispatch-time base / F7 pins on a minimal woke
        assert!(rows[0].base.is_none());
        assert!(rows[0].model.is_none());
        assert!(rows[0].permission_mode.is_none());
        assert!(rows[0].allowed_tools.is_none());
        let w1 = &rows[1];
        assert_eq!(w1.name, "t/dev/1");
        assert_eq!(w1.agent, "dev");
        assert_eq!(w1.rig.as_deref(), Some("gc"));
        assert_eq!(
            w1.claude_session_id.as_deref(),
            Some("7bd2befc-b018-4080-8738-429d541b3646")
        );
        assert_eq!(
            w1.transcript_path.as_deref(),
            Some("/home/u/.claude/projects/-code-gc/x.jsonl")
        );
        assert_eq!(w1.bead.as_deref(), Some("gc-1"));
        assert_eq!(w1.woke_actor, "campd");
        assert_eq!(w1.worktree.as_deref(), Some("/camps/t/worktrees/gc-1"));
        // Phase 3: the dispatch-time base and F7 pins round-trip through
        // the woke-JSON join (no sessions-table schema change)
        assert_eq!(
            w1.base.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(w1.model.as_deref(), Some("sonnet"));
        assert_eq!(w1.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(w1.allowed_tools.as_deref(), Some("Read,Edit,Bash"));
        assert!(w1.pid.is_none());
        assert_eq!(w1.spawned_ts, "2026-07-05T21:14:03Z");
    }

    /// The converse verb's registry lookup (dispatch-lifecycle Phase 1):
    /// any session by name, ANY status — an exited worker must be findable
    /// for the resume path — carrying claude_session_id, rig, bead,
    /// worktree, status.
    #[test]
    fn session_by_name_finds_live_and_ended_rows_with_provenance() {
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
                "transcript_path": "/home/u/.claude/projects/-code-gc/x.jsonl",
                "bead": "gc-1", "worktree": "/camps/t/worktrees/gc-1",
            }),
        })
        .unwrap();

        let live = l.session_by_name("t/dev/1").unwrap().expect("row exists");
        assert_eq!(live.status, "live");
        assert_eq!(live.agent, "dev");
        assert_eq!(live.woke_actor, "campd");

        l.append(EventInput {
            kind: EventType::SessionStopped,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/1", "exit_code": 0}),
        })
        .unwrap();

        let row = l.session_by_name("t/dev/1").unwrap().expect("row exists");
        assert_eq!(row.status, "stopped", "ended rows stay findable");
        assert_eq!(
            row.claude_session_id.as_deref(),
            Some("7bd2befc-b018-4080-8738-429d541b3646")
        );
        assert_eq!(row.rig.as_deref(), Some("gc"));
        assert_eq!(row.bead.as_deref(), Some("gc-1"));
        assert_eq!(row.worktree.as_deref(), Some("/camps/t/worktrees/gc-1"));
        assert!(l.session_by_name("nobody/here/9").unwrap().is_none());
    }

    // ---- Phase 11 events (agent.stalled, patrol.degraded) + crash cause --

    #[test]
    fn agent_stalled_validates_shape_and_is_log_only() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        let valid = serde_json::json!({
            "session": "t/dev/1", "agent": "dev", "action": "nudge",
            "threshold": "10m", "restarts": 0, "via": "stdin",
        });
        let seq = l
            .append(EventInput {
                kind: EventType::AgentStalled,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: valid.clone(),
            })
            .unwrap();
        assert!(seq > 0);
        // log-only: the bead is untouched
        let bead = l.get_bead("gc-1").unwrap().unwrap();
        assert_eq!(bead.status, "open");

        let before = l.events_range(1, None).unwrap().len();
        let reject = |l: &mut Ledger, data: serde_json::Value, bead: Option<&str>| {
            let err = l.append(EventInput {
                kind: EventType::AgentStalled,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: bead.map(str::to_owned),
                data,
            });
            assert!(err.is_err(), "must reject");
        };
        // missing session
        reject(
            &mut l,
            serde_json::json!({"agent": "dev", "action": "nudge", "threshold": "10m", "restarts": 0}),
            Some("gc-1"),
        );
        // unknown action
        let mut bad = valid.clone();
        bad["action"] = serde_json::json!("dance");
        reject(&mut l, bad, Some("gc-1"));
        // nudge_failed requires the error
        let mut bad = valid.clone();
        bad["action"] = serde_json::json!("nudge_failed");
        reject(&mut l, bad, Some("gc-1"));
        // via is a nudge-only field
        let mut bad = valid.clone();
        bad["action"] = serde_json::json!("restart");
        reject(&mut l, bad, Some("gc-1"));
        // unknown field
        let mut bad = valid.clone();
        bad["extra"] = serde_json::json!(1);
        reject(&mut l, bad, Some("gc-1"));
        // bead absent / unknown
        reject(&mut l, valid.clone(), None);
        reject(&mut l, valid.clone(), Some("gc-999"));
        assert_eq!(l.events_range(1, None).unwrap().len(), before);

        // the other legal shapes: nudge_failed with error+via, restart,
        // exhausted, annotate (no via)
        for (action, extra) in [
            ("nudge_failed", Some(("error", "broken pipe"))),
            ("restart", None),
            ("exhausted", None),
            ("annotate", None),
        ] {
            let mut data = serde_json::json!({
                "session": "t/dev/1", "agent": "dev", "action": action,
                "threshold": "10m", "restarts": 1,
            });
            if let Some((k, v)) = extra {
                data[k] = serde_json::json!(v);
                data["via"] = serde_json::json!("resume");
            }
            l.append(EventInput {
                kind: EventType::AgentStalled,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data,
            })
            .unwrap_or_else(|e| panic!("{action} must be a legal shape: {e}"));
        }
    }

    #[test]
    fn patrol_degraded_requires_the_error() {
        let (_dir, mut l) = temp_ledger();
        l.append(EventInput {
            kind: EventType::PatrolDegraded,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"error": "inotify watch limit reached"}),
        })
        .unwrap();
        // optional session context
        l.append(EventInput {
            kind: EventType::PatrolDegraded,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"error": "nudge resume exited 1", "session": "t/dev/1"}),
        })
        .unwrap();
        for bad in [serde_json::json!({}), serde_json::json!({"error": ""})] {
            assert!(
                l.append(EventInput {
                    kind: EventType::PatrolDegraded,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: bad,
                })
                .is_err()
            );
        }
    }

    /// cp-0 amendment fix 1: the read-channel fault events reuse
    /// `patrol.degraded`, so their payloads MUST satisfy the
    /// `PatrolDegraded` fold contract (deny_unknown_fields: `error` +
    /// optional `session` only). The read-channel source id, offset, and
    /// offending line ride the `error` STRING (not separate keys). This
    /// test pins the fold contract: each shape appends without
    /// `InvalidEventData` and the ledger refolds clean.
    #[test]
    fn read_channel_patrol_degraded_shapes_round_trip_through_the_fold() {
        let (_dir, mut l) = temp_ledger();
        // watcher error (no session context).
        l.append(EventInput {
            kind: EventType::PatrolDegraded,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "error": "read_channel: stream watcher error: inotify watch limit reached",
            }),
        })
        .unwrap();
        // drain (open/seek/read) error (with session context).
        l.append(EventInput {
            kind: EventType::PatrolDegraded,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "session": "t/dev/1",
                "error": "read_channel: stream drain: opening sessions/t-dev-1.json: Permission denied",
            }),
        })
        .unwrap();
        // non-JSON line (with session + offset + line in the error string).
        l.append(EventInput {
            kind: EventType::PatrolDegraded,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "session": "t/dev/1",
                "error": "read_channel: non-JSON line in stream at offset 4096: expected value at line 1 column 1: claimed gc-1",
            }),
        })
        .unwrap();
        // patrol.degraded is log-only — no state drift.
        assert_eq!(count(&l, "SELECT count(*) FROM events"), 3);
        assert_eq!(count(&l, "SELECT count(*) FROM beads"), 0);
        assert!(
            l.refold_check().unwrap().drift.is_empty(),
            "refold is clean"
        );
    }

    #[test]
    fn session_crashed_accepts_an_audit_cause_seq() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"name": "t/dev/1", "agent": "dev", "bead": "gc-1"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::BeadClaimed,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"session": "t/dev/1"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionCrashed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "name": "t/dev/1", "reason": "patrol restart", "cause_seq": 3, "signal": 9,
            }),
        })
        .unwrap();
        // the release semantics are unchanged: the claimed bead reopened
        let bead = l.get_bead("gc-1").unwrap().unwrap();
        assert_eq!(bead.status, "open");
        assert!(bead.claimed_by.is_none());
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

    /// Phase 2 (dispatch-lifecycle Q1): `dispatch.live_tree` is the LOUD
    /// marker that campd dispatched a worker onto the rig's live tree
    /// because the agent explicitly declared `isolation = "none"`.
    /// Log-only, but the payload is validated like every other event.
    #[test]
    fn dispatch_live_tree_is_log_only_and_validates_payload() {
        let (_dir, mut l) = temp_ledger();
        seeded_bead(&mut l, "gc-1");
        // the happy shape appends
        l.append(EventInput {
            kind: EventType::DispatchLiveTree,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"path": "/code/rig", "agent": "dev"}),
        })
        .unwrap();
        // missing bead is an error
        assert!(
            l.append(EventInput {
                kind: EventType::DispatchLiveTree,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"path": "/p", "agent": "dev"}),
            })
            .is_err(),
            "dispatch.live_tree without a bead must fail"
        );
        // empty path, empty agent, and unknown fields are all rejected
        for data in [
            serde_json::json!({"path": "", "agent": "dev"}),
            serde_json::json!({"path": "/p", "agent": ""}),
            serde_json::json!({"path": "/p", "agent": "dev", "extra": 1}),
        ] {
            assert!(
                l.append(EventInput {
                    kind: EventType::DispatchLiveTree,
                    rig: Some("gc".into()),
                    actor: "campd".into(),
                    bead: Some("gc-1".into()),
                    data: data.clone(),
                })
                .is_err(),
                "invalid payload must be rejected: {data}"
            );
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
    fn open_applies_pragmas_and_creates_the_current_schema() {
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
        // Bound to the CONST, never a literal (BD9): `init_schema` writes the
        // version as a LITERAL inside FULL_DDL_PREFIX and returns early without
        // verifying, while every later open compares `SCHEMA_VERSION`. Asserting
        // a literal here lets the two drift apart — which is a camp that inits
        // fine and then cannot open itself.
        assert_eq!(version, SCHEMA_VERSION.to_string());
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

    /// Dispatch-lifecycle Phase 3 (#34, Q3): the WorkOutcome axis on
    /// bead.closed — additive, separate from the control outcome, coherence
    /// fold-enforced. Pure shape rules only: the git facts (reachable,
    /// based) are gated in `camp close`, never here — refold replays events
    /// after worktrees are gone, so a fold that shelled to git would be
    /// nondeterministic.
    #[test]
    fn bead_closed_records_the_work_outcome_axis_with_coherence() {
        let (_dir, mut ledger) = temp_ledger();
        let close = |l: &mut Ledger, id: &str, data: serde_json::Value| {
            l.append(input(EventType::BeadClosed, Some("gc"), Some(id), data))
        };
        let cols = |l: &Ledger, id: &str| -> (Option<String>, Option<String>, Option<String>) {
            l.conn
                .query_row(
                    "SELECT work_outcome, work_commit, work_branch FROM beads WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
        };

        // ACCEPTED shapes, one bead each — and the columns fold through:
        seeded_bead(&mut ledger, "gc-1");
        close(
            &mut ledger,
            "gc-1",
            serde_json::json!({
                "outcome": "pass", "work_outcome": "shipped",
                "work_commit": "c0ffee", "work_branch": "camp/gc-1"}),
        )
        .unwrap();
        assert_eq!(
            cols(&ledger, "gc-1"),
            (
                Some("shipped".into()),
                Some("c0ffee".into()),
                Some("camp/gc-1".into())
            )
        );
        seeded_bead(&mut ledger, "gc-2");
        close(
            &mut ledger,
            "gc-2",
            serde_json::json!({"outcome": "pass", "work_outcome": "no-op"}),
        )
        .unwrap();
        assert_eq!(cols(&ledger, "gc-2"), (Some("no-op".into()), None, None));
        seeded_bead(&mut ledger, "gc-3");
        close(
            &mut ledger,
            "gc-3",
            serde_json::json!({"outcome": "fail", "work_outcome": "blocked", "reason": "no base"}),
        )
        .unwrap();
        assert_eq!(cols(&ledger, "gc-3"), (Some("blocked".into()), None, None));
        seeded_bead(&mut ledger, "gc-4");
        close(
            &mut ledger,
            "gc-4",
            serde_json::json!({"outcome": "fail", "work_outcome": "abandoned", "reason": "obsolete"}),
        )
        .unwrap();
        seeded_bead(&mut ledger, "gc-5");
        close(&mut ledger, "gc-5", serde_json::json!({"outcome": "pass"})).unwrap(); // the v1 shape
        assert_eq!(cols(&ledger, "gc-5"), (None, None, None));

        // REJECTED shapes — each on a fresh OPEN bead so the rejection is
        // the payload's, not a double-close:
        let rejected: &[serde_json::Value] = &[
            serde_json::json!({"outcome": "pass", "work_outcome": "blocked"}), // the #34 lie
            serde_json::json!({"outcome": "fail", "work_outcome": "shipped",
                               "work_commit": "c", "work_branch": "b"}),
            serde_json::json!({"outcome": "pass", "work_outcome": "shipped"}),
            serde_json::json!({"outcome": "pass", "work_outcome": "shipped", "work_commit": "c"}),
            serde_json::json!({"outcome": "pass", "work_outcome": "no-op",
                               "work_commit": "c", "work_branch": "b"}),
            serde_json::json!({"outcome": "fail", "work_outcome": "blocked",
                               "work_commit": "c", "work_branch": "b"}),
            serde_json::json!({"outcome": "pass", "work_outcome": "delivered"}), // not pinned
            serde_json::json!({"outcome": "pass", "work_commit": "c"}), // artifact without axis
        ];
        for (i, data) in rejected.iter().enumerate() {
            let id = format!("gc-9{i}");
            seeded_bead(&mut ledger, &id);
            assert!(
                close(&mut ledger, &id, data.clone()).is_err(),
                "must reject: {data}"
            );
        }
    }

    /// Issue #48 finding 2 (dispatch-lifecycle Phase 3): a fail-fast
    /// dispatch is a bead-level fact the list can show — dispatch.failed
    /// folds into beads.dispatch_failure (the reason), cleared by a later
    /// session.woke or claim for that bead. The dispatchable query is
    /// untouched: the marker informs, it never gates.
    #[test]
    fn dispatch_failed_marks_the_bead_and_dispatch_or_claim_clears_it() {
        let (_dir, mut l) = temp_ledger();
        let marker = |l: &Ledger, id: &str| -> Option<String> {
            l.conn
                .query_row(
                    "SELECT dispatch_failure FROM beads WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let failed = |l: &mut Ledger, id: &str, data: serde_json::Value| {
            l.append(EventInput {
                kind: EventType::DispatchFailed,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some(id.into()),
                data,
            })
        };

        // the marker folds on ...
        seeded_bead(&mut l, "gc-1");
        let reason = "rig gc cannot host a worktree (no base commit)";
        failed(&mut l, "gc-1", serde_json::json!({"reason": reason})).unwrap();
        assert_eq!(marker(&l, "gc-1").as_deref(), Some(reason));
        // ... and a later successful dispatch (session.woke naming the
        // bead) clears it
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"name": "t/dev/1", "agent": "dev", "bead": "gc-1"}),
        })
        .unwrap();
        assert_eq!(marker(&l, "gc-1"), None, "session.woke clears the marker");

        // a claim clears it too
        seeded_bead(&mut l, "gc-2");
        failed(&mut l, "gc-2", serde_json::json!({"reason": reason})).unwrap();
        assert_eq!(marker(&l, "gc-2").as_deref(), Some(reason));
        l.append(input(
            EventType::BeadClaimed,
            Some("gc"),
            Some("gc-2"),
            serde_json::json!({"session": "t/dev/2"}),
        ))
        .unwrap();
        assert_eq!(marker(&l, "gc-2"), None, "bead.claimed clears the marker");

        // fail fast (regression pins): an unknown bead and an unknown
        // payload key are both rejected
        assert!(
            failed(&mut l, "gc-404", serde_json::json!({"reason": reason})).is_err(),
            "dispatch.failed on an unknown bead must fail fast"
        );
        seeded_bead(&mut l, "gc-3");
        assert!(
            failed(
                &mut l,
                "gc-3",
                serde_json::json!({"reason": reason, "extra": 1})
            )
            .is_err(),
            "unknown payload keys must be rejected (deny_unknown_fields)"
        );
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
                open: 0,
                stuck: 0,
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
                open: 2,
                stuck: 0,
            }
        );
    }

    #[test]
    fn status_summary_counts_only_task_beads() {
        // Issue #36: the status surface must reflect dispatchable work.
        // campd only ever dispatches tasks (`dispatchable_beads` filters
        // `type='task'`), so memory/mail beads must count as neither ready
        // nor open — otherwise `camp top` implies pending work that will
        // never be picked up.
        let (_dir, mut ledger) = temp_ledger();

        // A camp whose only open bead is a memory bead: 0 ready, 0 open.
        ledger
            .append(created(
                "gc-1",
                serde_json::json!({"title": "a durable fact", "type": "memory"}),
            ))
            .unwrap();
        // A mail bead is likewise non-dispatchable.
        ledger
            .append(created(
                "gc-2",
                serde_json::json!({"title": "a note", "type": "mail"}),
            ))
            .unwrap();
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 0,
                open: 0,
                stuck: 0,
            }
        );

        // Positive case: an open task bead is counted, ready and open.
        ledger
            .append(created(
                "gc-3",
                serde_json::json!({"title": "do the thing"}),
            ))
            .unwrap();
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 1,
                open: 1,
                stuck: 0,
            }
        );

        // A blocked task counts as open but not ready — guards against a
        // predicate that just makes every count zero.
        ledger
            .append(created(
                "gc-4",
                serde_json::json!({"title": "later", "needs": ["gc-3"]}),
            ))
            .unwrap();
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 1,
                open: 2,
                stuck: 0,
            }
        );
    }

    #[test]
    fn status_summary_moves_a_dispatch_failed_bead_from_ready_to_stuck() {
        use crate::event::{EventInput, EventType};
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({ "title": "one" })))
            .unwrap();
        // ready before the failure
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 1,
                open: 1,
                stuck: 0,
            }
        );
        // a dispatch failure: no longer ready, now stuck (still open)
        ledger
            .append(EventInput {
                kind: EventType::DispatchFailed,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({ "reason": "no agent" }),
            })
            .unwrap();
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec![],
                ready: 0,
                open: 1,
                stuck: 1,
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

    /// cp-0: the per-session stream byte offset is consumer bookkeeping
    /// (the `cursors` mold) — defaults to 0, UPSERTs, and clears.
    #[test]
    fn stream_cursor_defaults_to_zero_upserts_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::open(&dir.path().join("camp.db")).unwrap();
        assert_eq!(l.stream_cursor("t/dev/1").unwrap(), 0, "absent => 0");
        l.set_stream_cursor("t/dev/1", 4096).unwrap();
        assert_eq!(l.stream_cursor("t/dev/1").unwrap(), 4096);
        // UPSERT, not insert-or-fail:
        l.set_stream_cursor("t/dev/1", 8192).unwrap();
        assert_eq!(l.stream_cursor("t/dev/1").unwrap(), 8192);
        l.clear_stream_cursor("t/dev/1").unwrap();
        assert_eq!(l.stream_cursor("t/dev/1").unwrap(), 0, "cleared => 0");
        // clearing an absent row is a no-op (idempotent)
        l.clear_stream_cursor("t/dev/1").unwrap();
    }

    /// cp-0: stream cursors are isolated per session.
    #[test]
    fn stream_cursors_are_isolated_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::open(&dir.path().join("camp.db")).unwrap();
        l.set_stream_cursor("t/dev/1", 100).unwrap();
        l.set_stream_cursor("t/dev/2", 200).unwrap();
        assert_eq!(l.stream_cursor("t/dev/1").unwrap(), 100);
        assert_eq!(l.stream_cursor("t/dev/2").unwrap(), 200);
        l.clear_stream_cursor("t/dev/1").unwrap();
        assert_eq!(l.stream_cursor("t/dev/1").unwrap(), 0);
        assert_eq!(l.stream_cursor("t/dev/2").unwrap(), 200, "unaffected");
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
                assert_eq!(supported, SCHEMA_VERSION);
            }
            Err(other) => panic!("expected UnsupportedSchema, got {other:?}"),
            Ok(_) => panic!("open must fail on schema_version 999"),
        }
    }

    // ---- compat §6.1: bead metadata, the reservation CAS, schema 3 ----------

    fn mk_bead(l: &mut Ledger, id: &str, data: serde_json::Value) {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data,
        })
        .unwrap();
    }

    fn update(l: &mut Ledger, id: &str, data: serde_json::Value) -> Result<Seq, CoreError> {
        l.append(EventInput {
            kind: EventType::BeadUpdated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data,
        })
    }

    #[test]
    fn a_fresh_camp_reopens() {
        // BD9. `SCHEMA_VERSION` lives in TWO places: the const, and a LITERAL
        // inside FULL_DDL_PREFIX that `init_schema` writes before returning
        // early WITHOUT verifying. Every later open compares the CONST. Bump one
        // and not the other and every freshly-initialized camp writes the old
        // version and then fails to open on its very next process — and no test
        // that only ever inits would notice.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.db");
        {
            let _l = Ledger::open(&path).unwrap();
        }
        let reopened = Ledger::open(&path);
        assert!(
            reopened.is_ok(),
            "a camp initialized by this binary must reopen with it: {:?}",
            reopened.err()
        );
    }

    #[test]
    fn bead_created_carries_metadata_and_bead_updated_sets_and_unsets_it() {
        let (_d, mut l) = temp_ledger();
        mk_bead(
            &mut l,
            "gc-1",
            serde_json::json!({
                "title": "t",
                "metadata": {"gc.run_target": "superpowers.implementer", "gc.kind": "drain"},
            }),
        );
        let md = l.bead_metadata("gc-1").unwrap();
        assert_eq!(md.get("gc.kind").map(String::as_str), Some("drain"));
        assert_eq!(
            md.get("gc.run_target").map(String::as_str),
            Some("superpowers.implementer")
        );

        // Set a new key and overwrite an existing one.
        update(
            &mut l,
            "gc-1",
            serde_json::json!({"metadata": {"gc.kind": "cleanup", "gc.on_fail": "abort"}}),
        )
        .unwrap();
        let md = l.bead_metadata("gc-1").unwrap();
        assert_eq!(md.get("gc.kind").map(String::as_str), Some("cleanup"));
        assert_eq!(md.get("gc.on_fail").map(String::as_str), Some("abort"));

        // null UNSETS.
        update(
            &mut l,
            "gc-1",
            serde_json::json!({"metadata": {"gc.on_fail": null}}),
        )
        .unwrap();
        let md = l.bead_metadata("gc-1").unwrap();
        assert!(!md.contains_key("gc.on_fail"), "{md:?}");
        assert!(md.contains_key("gc.kind"), "only the named key is unset");
    }

    #[test]
    fn bead_updated_still_requires_at_least_one_field() {
        let (_d, mut l) = temp_ledger();
        mk_bead(&mut l, "gc-1", serde_json::json!({"title": "t"}));
        let err = update(&mut l, "gc-1", serde_json::json!({})).unwrap_err();
        assert!(
            matches!(&err, CoreError::InvalidEventData { reason, .. }
                     if reason.contains("title and/or description and/or metadata")),
            "{err:?}"
        );
        // But metadata ALONE is now enough — the reservation rides this event.
        update(
            &mut l,
            "gc-1",
            serde_json::json!({"metadata": {"gc.k": "v"}}),
        )
        .unwrap();
    }

    #[test]
    fn a_second_drain_cannot_reserve_a_held_member() {
        let (_d, mut l) = temp_ledger();
        mk_bead(&mut l, "gc-1", serde_json::json!({"title": "member"}));
        let reserve =
            |drain: &str| serde_json::json!({"metadata": {EXCLUSIVE_DRAIN_RESERVATION: drain}});

        update(&mut l, "gc-1", reserve("drain-a")).unwrap();
        assert_eq!(
            l.bead_metadata("gc-1")
                .unwrap()
                .get(EXCLUSIVE_DRAIN_RESERVATION)
                .map(String::as_str),
            Some("drain-a")
        );

        // A DIFFERENT drain conflicts, and the error NAMES the holder.
        let err = update(&mut l, "gc-1", reserve("drain-b")).unwrap_err();
        assert!(
            matches!(&err, CoreError::InvalidEventData { reason, .. }
                     if reason.contains("drain-a") && reason.contains("drain-b")),
            "the conflict must name both drains: {err:?}"
        );
        // The rejected append appended NOTHING — the reservation is untouched.
        assert_eq!(
            l.bead_metadata("gc-1")
                .unwrap()
                .get(EXCLUSIVE_DRAIN_RESERVATION)
                .map(String::as_str),
            Some("drain-a"),
            "a rejected CAS must not have mutated the holder"
        );

        // The SAME holder re-reserving is idempotent (campd may replay its own
        // queued drain after a restart).
        update(&mut l, "gc-1", reserve("drain-a")).unwrap();

        // Release, then a different drain CAN take it.
        update(
            &mut l,
            "gc-1",
            serde_json::json!({"metadata": {EXCLUSIVE_DRAIN_RESERVATION: null}}),
        )
        .unwrap();
        update(&mut l, "gc-1", reserve("drain-b")).unwrap();
        assert_eq!(
            l.bead_metadata("gc-1")
                .unwrap()
                .get(EXCLUSIVE_DRAIN_RESERVATION)
                .map(String::as_str),
            Some("drain-b")
        );
    }

    #[test]
    fn a_reserve_BATCH_with_one_conflict_appends_ZERO_ROWS() {
        // ⭐ V-1 — the assertion BD4's all-or-nothing reserve actually rests on, and
        // the one the daemon suite CANNOT make.
        //
        // At the daemon level a losing drain always conflicts on member[0] (both
        // drains enumerate through the same `run_members` ordering and campd runs
        // them serially), so a single-event batch and rev-2's incremental
        // per-event loop are INDISTINGUISHABLE — the BD4 mutant survives the whole
        // daemon suite even with two members. The property is a LEDGER property, so
        // it is asserted at the ledger.
        //
        // A batch that reserves m1 and then conflicts on m2 must append NOTHING —
        // not "m1 and then stop". Under the incremental shape m1 stays reserved,
        // item-run 1 is already cooked and dispatchable on it, and releasing it
        // later would let a SECOND drain cook its own item run over the same bead:
        // two drains mutating one bead, the exact thing the reservation prevents.
        let (_d, mut l) = temp_ledger();
        mk_bead(&mut l, "gc-1", serde_json::json!({"title": "m1"}));
        mk_bead(&mut l, "gc-2", serde_json::json!({"title": "m2"}));
        // m2 is already held by another drain.
        update(
            &mut l,
            "gc-2",
            serde_json::json!({"metadata": {EXCLUSIVE_DRAIN_RESERVATION: "drain-other"}}),
        )
        .unwrap();

        let reserve = |bead: &str| EventInput {
            kind: EventType::BeadUpdated,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some(bead.to_owned()),
            data: serde_json::json!({
                "metadata": {EXCLUSIVE_DRAIN_RESERVATION: "drain-mine"}
            }),
        };
        let err = l
            .append_batch(vec![reserve("gc-1"), reserve("gc-2")])
            .unwrap_err();
        assert!(
            matches!(&err, CoreError::InvalidEventData { reason, .. }
                     if reason.contains("drain-other")),
            "the conflict names the holder: {err:?}"
        );

        // ⭐ ZERO ROWS. m1 — which the batch reserved BEFORE it hit the conflict —
        // must carry NO reservation. "Rejections appended nothing."
        assert!(
            !l.bead_metadata("gc-1")
                .unwrap()
                .contains_key(EXCLUSIVE_DRAIN_RESERVATION),
            "a rejected batch must append NOTHING — m1 was reserved before the conflict"
        );
        // …and the prior holder is untouched.
        assert_eq!(
            l.bead_metadata("gc-2")
                .unwrap()
                .get(EXCLUSIVE_DRAIN_RESERVATION)
                .map(String::as_str),
            Some("drain-other")
        );
    }

    #[test]
    fn a_metadata_key_with_a_dedicated_column_is_projected_at_read_and_refused_at_write() {
        let (_d, mut l) = temp_ledger();
        mk_bead(
            &mut l,
            "gc-1",
            serde_json::json!({"title": "t", "assignee": "superpowers.implementer"}),
        );

        // PROJECTED at read: the column shows up as its gc metadata key, so a
        // reader sees one complete map and never has to know where a fact lives.
        let md = l.bead_metadata("gc-1").unwrap();
        assert_eq!(
            md.get("gc.routed_to").map(String::as_str),
            Some("superpowers.implementer")
        );

        // REFUSED at write, NAMING the column. compat-3 stamps `gc.routed_to`
        // when it dispatches; if metadata could also hold it, a bead could carry
        // two different routes and nothing would be wrong enough to fail.
        for (key, column) in PROJECTED_METADATA {
            let err =
                update(&mut l, "gc-1", serde_json::json!({"metadata": {*key: "x"}})).unwrap_err();
            assert!(
                matches!(&err, CoreError::InvalidEventData { reason, .. }
                         if reason.contains(column)),
                "{key} must be refused naming `beads.{column}`: {err:?}"
            );
        }
    }
}

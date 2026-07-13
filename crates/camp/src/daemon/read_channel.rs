//! cp-0 (control-plane spec §2.3): the campd read channel — per-session
//! byte-offset tailing of each worker's stdout file, drained to EOF on
//! every campd wake. The notify watcher is a latency-only wake-up; the
//! correctness rule is "drain all tailed files on every wake, any token."
//! Partial lines are buffered (a notify event can land mid-line; a partial
//! JSON line is never parsed). Offsets persist in the `stream_cursors` table
//! only after the line's ledger effect commits, so a campd restart resumes
//! from the exact byte the last life consumed. A `max_stream_bytes` breach
//! is a loud, evented session failure (§2.3).

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use camp_core::event::{Event, EventType};
use camp_core::ledger::Ledger;

use super::spawn::munge;

/// The per-session byte ceiling on the stream file (§2.3). Generous
/// default — a stream-json session file grows ~KB/min. Configurability is
/// deferred to a phase that owns `config.rs` (compat-1 owns it in W1);
/// phase 0 exposes the cap test-injectably via `max_stream_bytes_from_env`
/// so a real §8 ceiling integration test with a small cap exercises the
/// full path.
pub const MAX_STREAM_BYTES_DEFAULT: u64 = 256 * 1024 * 1024;

/// cp-0 (note 1): the cap is test-injectable via the `CAMP_MAX_STREAM_BYTES`
/// env var (a test-only override; production uses `MAX_STREAM_BYTES_DEFAULT`
/// until `config.rs` gains a `[control]` field in a phase that owns it).
/// Fail fast: a malformed override is an error, never silently ignored.
pub fn max_stream_bytes_from_env(default: u64) -> Result<u64> {
    match std::env::var("CAMP_MAX_STREAM_BYTES") {
        Ok(raw) => {
            let n: u64 = raw
                .parse()
                .with_context(|| format!("CAMP_MAX_STREAM_BYTES={raw:?} is not a u64"))?;
            if n == 0 {
                anyhow::bail!("CAMP_MAX_STREAM_BYTES must be > 0");
            }
            Ok(n)
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(v)) => {
            anyhow::bail!("CAMP_MAX_STREAM_BYTES={v:?} is not valid UTF-8");
        }
    }
}

/// The per-session tail state: the in-memory byte offset (persisted to
/// `stream_cursors` by `drain_all` after each line's ledger effect commits),
/// the buffered trailing partial line, and the held file handle (reused
/// across drains; reopen-after-restart is a fresh register).
struct Tailed {
    stdout_path: PathBuf,
    offset: u64,
    /// review fix 9: the offset last written to `stream_cursors`. The
    /// UPSERT is skipped when `offset == persisted_offset` (a quiescent
    /// tailed session costs ZERO ledger writes per wake — the drain block
    /// runs on every wake, so an unconditional UPSERT was N SQLite writes
    /// per wake with N workers).
    persisted_offset: u64,
    partial: Vec<u8>,
    /// None until the first drain opens the file; reused thereafter.
    file: Option<std::fs::File>,
    /// cp-0 §2.3: a `max_stream_bytes` breach was surfaced for this session.
    /// review fix 2: this is a HARD STOP, not an event-dedupe flag — a
    /// capped session is not read AT ALL until it is unregistered. The
    /// original code gated the two OOM guards on `!capped`, which DISABLED
    /// them on the exact path they exist for: once capped, the pre-read
    /// guard fell through to the read loop and the in-loop guard let
    /// `partial` extend without bound, so a re-drained capped session read
    /// its whole over-cap file into memory. Refusing to read is both the
    /// RSS bound and the breach-dedupe.
    capped: bool,
}

/// The watch filter (the patrol mold): shared with the notify callback
/// thread. `rescan` is set on a Rescan / empty-paths / unknown-kind event
/// (§2.3: rev 2's handler discarded these by iterating `event.paths`).
#[derive(Debug, Default)]
pub struct ReadFilter {
    pub registered: std::collections::HashSet<PathBuf>,
    pub rescan: bool,
    pub error: Option<String>,
}

pub struct ReadChannelRuntime {
    sessions_dir: PathBuf,
    max_stream_bytes: u64,
    tailed: HashMap<String, Tailed>,
    /// Queued register/unregister ops (applied outside the cursor txn —
    /// the patrol `track_ops` mold).
    track_ops: Vec<TrackOp>,
    /// review fix 1 (CRITICAL): sessions whose unregister is DEFERRED until
    /// after this wake's final drain. `apply_tracking` used to execute the
    /// Unregister immediately — and it runs inside `settle`, which runs
    /// BEFORE the event loop's drain block. So a reaped session was dropped
    /// from `tailed` and its stream file unlinked while the drain block
    /// still had to run: every byte the worker wrote between the last drain
    /// and its exit was deleted UNREAD. That voids §2.3's "drained to EOF on
    /// EVERY wake … never lost" at the exact moment it matters most — the
    /// session's LAST output (in phase 1 those final bytes carry the
    /// terminal `result`, the `control_response` to an interrupt, and any
    /// late `can_use_tool`). The session now stays in `tailed` through the
    /// drain and is disposed only by `apply_pending_unregisters`, which the
    /// event loop calls as the LAST step of the drain block.
    pending_unregisters: Vec<String>,
    filter: std::sync::Arc<std::sync::Mutex<ReadFilter>>,
    /// Complete JSON lines parsed (consumed) per session this runtime life
    /// — test observable.
    parsed_counts: HashMap<String, usize>,
    /// Surfaced parse failures (fail fast — §2.3: an unparsable line is
    /// never silently dropped). Drained into durable events by
    /// `take_parse_error_events`.
    parse_errors: Vec<ParseError>,
    /// cp-0 §2.3: sessions whose stdout file crossed `max_stream_bytes`
    /// this drain — surfaced by `take_cap_breaches` for the event loop to
    /// append `session.stream_capped` + kill the worker.
    cap_breaches: Vec<CapBreach>,
    /// cp-0 fix 8: per-session drain (open/seek/read) errors, captured
    /// non-fatally so one bad stream does not crash campd or stop the drain
    /// of the other tailed sessions. Drained into durable
    /// `patrol.degraded` events by `take_drain_error_events`.
    drain_errors: Vec<DrainError>,
    /// The notify watcher on `sessions/` (held for liveness; the drain-
    /// all-on-every-wake rule makes it latency-only — §2.3).
    watcher: Option<notify::RecommendedWatcher>,
}

/// cp-0 §2.3: a stream file that crossed `max_stream_bytes` this drain —
/// the loud session-failure cause. The event loop appends
/// `session.stream_capped` from a breach, then kills the worker.
#[derive(Debug, Clone)]
pub struct CapBreach {
    pub session: String,
    pub bead: Option<String>,
    pub file: PathBuf,
    pub file_size: u64,
    pub cap_bytes: u64,
}

/// A non-JSON line surfaced from a drain (fail fast). The caller turns it
/// into a durable `patrol.degraded` event (the read-channel source named in
/// the error string — fix 1: the `PatrolDegraded` fold struct is
/// deny_unknown_fields with only `error`/`session`, so the source/offset/
/// line ride the `error` string, not separate keys).
#[derive(Debug, Clone)]
pub struct ParseError {
    pub session: String,
    pub line: String,
    pub offset: u64,
    pub error: String,
}

/// cp-0 fix 8: a per-session drain (open/seek/read/stat) error, captured
/// non-fatally. The caller turns it into a durable `patrol.degraded` event.
#[derive(Debug, Clone)]
pub struct DrainError {
    pub session: String,
    pub error: String,
}

#[derive(Debug)]
enum TrackOp {
    Register(String),
    Unregister(String),
}

impl ReadChannelRuntime {
    pub fn new(sessions_dir: PathBuf, max_stream_bytes: u64) -> Result<Self> {
        // The sessions dir must exist to be watchable (the patrol mold:
        // "the project dir must exist to be watchable"). Created ahead of
        // any spawn; idempotent.
        std::fs::create_dir_all(&sessions_dir)
            .with_context(|| format!("creating {}", sessions_dir.display()))?;
        Ok(ReadChannelRuntime {
            sessions_dir,
            max_stream_bytes,
            tailed: HashMap::new(),
            track_ops: Vec::new(),
            pending_unregisters: Vec::new(),
            filter: std::sync::Arc::new(std::sync::Mutex::new(ReadFilter::default())),
            parsed_counts: HashMap::new(),
            parse_errors: Vec::new(),
            cap_breaches: Vec::new(),
            drain_errors: Vec::new(),
            watcher: None,
        })
    }

    /// The slot the notify callback closure captures (the patrol mold).
    pub fn filter_slot(&self) -> std::sync::Arc<std::sync::Mutex<ReadFilter>> {
        self.filter.clone()
    }

    /// Observe a ledger event on the campd processing path (the patrol
    /// `observe` mold): session.woke queues a register; session.stopped /
    /// session.crashed queues an unregister. Memory-only — applied in
    /// `apply_tracking` outside the cursor txn.
    pub fn observe(&mut self, event: &Event) {
        match event.kind {
            EventType::SessionWoke => {
                // cp-0: tail only campd-spawned workers (actor "campd").
                // Hook-registered attended sessions (actor "hook:...") have
                // no stdout file — tailing them would error every wake.
                if event.actor == "campd"
                    && let Some(name) = event.data["name"].as_str()
                {
                    self.track_ops.push(TrackOp::Register(name.to_owned()));
                }
            }
            EventType::SessionStopped | EventType::SessionCrashed => {
                if let Some(name) = event.data["name"].as_str() {
                    self.track_ops.push(TrackOp::Unregister(name.to_owned()));
                }
            }
            _ => {}
        }
    }

    /// Execute queued register/unregister ops (the patrol `apply_tracking`
    /// mold — outside the cursor txn). Idempotent: `track_ops` is drained
    /// by `mem::take`, so a second call after a settle that already applied
    /// the same ops is a no-op (the deliberate safety net for the
    /// no-settle-ran case — see the event_loop wiring).
    ///
    /// review fix 1 (CRITICAL): a Register is applied NOW (so a session
    /// woken by this settle is drained on this same wake — no lag), but an
    /// Unregister is only QUEUED. Executing it here would drop the session
    /// from `tailed` and unlink its stream file BEFORE the event loop's
    /// drain block runs, destroying the worker's final output unread. The
    /// event loop calls `apply_pending_unregisters` as the LAST step of the
    /// drain block, so a reaped session is drained to EOF one final time
    /// (its last bytes become durable ledger events) and only then disposed.
    pub fn apply_tracking(&mut self, ledger: &mut Ledger) -> Result<()> {
        let ops = std::mem::take(&mut self.track_ops);
        for op in ops {
            match op {
                TrackOp::Register(name) => self.register(ledger, &name)?,
                TrackOp::Unregister(name) => self.pending_unregisters.push(name),
            }
        }
        Ok(())
    }

    /// review fix 1: execute the deferred unregisters — called by the event
    /// loop as the LAST step of the drain block, AFTER `drain_all` has read
    /// each reaped session's stream file to EOF, after its parse/drain fault
    /// events are appended, and after `persist_offsets`. Only here is the
    /// session dropped from `tailed` and its stream file disposed.
    ///
    /// Idempotent: the queue is drained by `mem::take`, and `unregister` is
    /// a no-op for a session that is no longer tailed.
    pub fn apply_pending_unregisters(&mut self, ledger: &mut Ledger) -> Result<()> {
        let pending = std::mem::take(&mut self.pending_unregisters);
        for session in pending {
            self.unregister(ledger, &session)?;
        }
        Ok(())
    }

    /// review fix 3: queue a session for disposal from outside the observe
    /// path — used when a cap-breach kill could NOT be delivered (no live
    /// child for that session, e.g. an adopted worker from a previous campd
    /// life, or one already reaped). Such a session must stop being tailed;
    /// otherwise it sits capped forever with a `session.stream_capped`
    /// event that had no effect.
    pub fn queue_unregister(&mut self, session: &str) {
        self.pending_unregisters.push(session.to_owned());
    }

    /// Register a session for tailing: derive its stdout path, load the
    /// persisted byte offset (0 if new), and insert the in-memory state.
    /// Public for startup seeding (the adoption mold — seed from the
    /// ledger's live sessions after `patrol::adopt`).
    pub fn register(&mut self, ledger: &mut Ledger, session: &str) -> Result<()> {
        if self.tailed.contains_key(session) {
            return Ok(()); // idempotent — the same woke row re-observed
        }
        let stdout_path = self.sessions_dir.join(format!("{}.json", munge(session)));
        let offset = ledger.stream_cursor(session)?;
        lock_unpoisoned(&self.filter)
            .registered
            .insert(stdout_path.clone());
        self.tailed.insert(
            session.to_owned(),
            Tailed {
                stdout_path,
                offset,
                // review fix 9: the loaded offset IS the persisted one, so a
                // session that never advances costs zero cursor writes.
                persisted_offset: offset,
                partial: Vec::new(),
                file: None,
                capped: false,
            },
        );
        Ok(())
    }

    /// Unregister a session: drop the in-memory state, clear the persisted
    /// offset row, and best-effort dispose the stream file (§2.3: "stream
    /// files append-only until reap"; fix 10: the reap-time unlink). The
    /// unlink is best-effort — a just-killed worker may still hold the open
    /// fd on Unix; unlink removes the directory entry (the inode persists
    /// until the worker exits and the fd closes, which is fine).
    pub fn unregister(&mut self, ledger: &mut Ledger, session: &str) -> Result<()> {
        if let Some(t) = self.tailed.remove(session) {
            lock_unpoisoned(&self.filter)
                .registered
                .remove(&t.stdout_path);
            if let Err(e) = std::fs::remove_file(&t.stdout_path) {
                eprintln!(
                    "campd: stream file disposal of {}: {e}",
                    t.stdout_path.display()
                );
            }
        }
        ledger.clear_stream_cursor(session)?;
        Ok(())
    }

    /// Test observable: the set of tailed session names.
    #[allow(dead_code)] // test observable (the unit tests in this file)
    pub fn tailed_sessions(&self) -> Vec<String> {
        self.tailed.keys().cloned().collect()
    }

    /// Test observable: the in-memory offset for a session.
    #[allow(dead_code)] // test observable (the unit tests in this file)
    pub fn offset_of(&self, session: &str) -> Option<u64> {
        self.tailed.get(session).map(|t| t.offset)
    }

    /// Drain EVERY tailed session's stdout file to EOF (§2.3: "on EVERY
    /// campd wake — any poll token — campd drains every tailed stream file
    /// to EOF before going back to sleep"). For each session: open-or-reuse
    /// the fd, seek to the offset, read to EOF, split complete lines on
    /// `\n`, buffer the trailing partial line, parse each complete line
    /// as JSON (validating — phase 1+ acts on control messages; phase 0
    /// validates only), and advance the in-memory offset past each complete
    /// line. A parse failure is surfaced via `take_parse_errors` (fail
    /// fast) but does NOT stop the drain. A drain (open/seek/read) error
    /// is captured per-session (fix 8: non-fatal — campd keeps draining the
    /// other tailed sessions and stays up). The in-memory offset is
    /// persisted AFTER the drain by `persist_offsets` (fix 7: persist after
    /// the line's ledger effect commits — phase 0 has no per-line effect, so
    /// after the drain block; phase 1+ reorders to after the
    /// `permission.pending` event's transaction).
    pub fn drain_all(&mut self, ledger: &mut Ledger) -> Result<()> {
        let sessions: Vec<String> = self.tailed.keys().cloned().collect();
        for session in sessions {
            self.drain_one(ledger, &session)?;
        }
        Ok(())
    }

    fn drain_one(&mut self, _ledger: &mut Ledger, session: &str) -> Result<()> {
        let Some(t) = self.tailed.get_mut(session) else {
            return Ok(());
        };
        // review fix 2: a capped session is a HARD STOP — do not read it at
        // all (not even open it) until it is unregistered. The `capped` flag
        // used to gate the two OOM guards below (`&& !t.capped`), which
        // inverted their meaning: once a breach set the flag, the pre-read
        // guard stopped firing and the in-loop guard stopped bounding
        // `partial`, so the very next drain of a capped session read its
        // entire over-cap file into memory — the unbounded read the guards
        // exist to prevent. Refusing to read IS the RSS bound, and it also
        // dedupes the breach (no duplicate `session.stream_capped`).
        if t.capped {
            return Ok(());
        }
        // Open-or-reuse the fd. A missing file is NOT a hard fault (the
        // reap-race window: a just-crashed worker's stream file is unlinked
        // before the unregister lands) — skip it. Any other open error is
        // captured per-session (fix 8: non-fatal) and surfaced as a durable
        // patrol.degraded event — one bad stream never crashes campd.
        let file = match t.file.as_mut() {
            Some(f) => f,
            None => match std::fs::OpenOptions::new().read(true).open(&t.stdout_path) {
                Ok(f) => t.file.insert(f),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => {
                    self.drain_errors.push(DrainError {
                        session: session.to_owned(),
                        error: format!("opening {}: {e}", t.stdout_path.display()),
                    });
                    return Ok(());
                }
            },
        };
        if let Err(e) = file.seek(SeekFrom::Start(t.offset)) {
            self.drain_errors.push(DrainError {
                session: session.to_owned(),
                error: format!("seeking {}: {e}", t.stdout_path.display()),
            });
            return Ok(());
        }
        // cp-0 fix 9 (OOM-before-cap): the PRE-read cap check. A file
        // already over the cap (e.g. a previous append) breaches WITHOUT
        // reading a single byte — RSS stays bounded. file_size is read
        // once here; the in-loop check below covers a file that grows past
        // the cap during the read.
        let file_size = match file.metadata() {
            Ok(m) => m.len(),
            Err(e) => {
                self.drain_errors.push(DrainError {
                    session: session.to_owned(),
                    error: format!("stat {}: {e}", t.stdout_path.display()),
                });
                return Ok(());
            }
        };
        // review fix 2: NO `&& !t.capped` here — a capped session already
        // returned above. The guard is unconditional.
        if file_size > self.max_stream_bytes {
            t.capped = true;
            self.cap_breaches.push(CapBreach {
                session: session.to_owned(),
                bead: None, // the event loop fills the bead from the session registry
                file: t.stdout_path.clone(),
                file_size,
                cap_bytes: self.max_stream_bytes,
            });
            return Ok(()); // no read — RSS-bounded
        }
        // The trailing partial from a previous drain is still in the file
        // at [t.offset..] (the stream file is append-only, never truncated
        // — §2.3), so re-reading from `t.offset` re-reads it. Clear the
        // in-memory partial so it is not double-counted; the bytes are
        // re-read fresh from the file below.
        t.partial.clear();
        let mut buf = [0u8; 8192];
        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.drain_errors.push(DrainError {
                        session: session.to_owned(),
                        error: format!("reading {}: {e}", t.stdout_path.display()),
                    });
                    return Ok(());
                }
            };
            // cp-0 fix 9 (in-loop OOM-before-cap): a newline-less line that
            // would push `partial` past the cap breaches NOW, BEFORE the
            // extend — so `partial` never exceeds the cap (RSS-bounded).
            // (A partial buffer only holds an incomplete line; if adding
            // the new chunk crosses the cap, the line is over-cap → a loud
            // breach is correct.)
            // review fix 2: NO `&& !t.capped` here either — this is the
            // guard that keeps `partial` bounded, and gating it on the flag
            // is what let a capped session accumulate a whole over-cap
            // newline-less blob into memory.
            if (t.partial.len() + n) as u64 > self.max_stream_bytes {
                t.capped = true;
                self.cap_breaches.push(CapBreach {
                    session: session.to_owned(),
                    bead: None,
                    file: t.stdout_path.clone(),
                    file_size,
                    cap_bytes: self.max_stream_bytes,
                });
                break; // do NOT extend — partial stays <= cap
            }
            t.partial.extend_from_slice(&buf[..n]);
            // Split complete lines on `\n`; keep the trailing partial.
            while let Some(pos) = t.partial.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = t.partial.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
                let line = line.trim_end_matches('\r');
                let new_offset = t.offset + line_bytes.len() as u64;
                if line.trim().is_empty() {
                    t.offset = new_offset;
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(_v) => {
                        self.parsed_counts
                            .entry(session.to_owned())
                            .and_modify(|c| *c += 1)
                            .or_insert(1);
                    }
                    Err(e) => {
                        self.parse_errors.push(ParseError {
                            session: session.to_owned(),
                            line: line.to_owned(),
                            offset: t.offset,
                            error: format!("{e}"),
                        });
                    }
                }
                t.offset = new_offset;
            }
        }
        Ok(())
    }

    /// cp-0 fix 7: persist every tailed session's in-memory offset to the
    /// `stream_cursors` table. Called by the event loop as the LAST step of
    /// the drain block — AFTER `take_parse_error_events` are appended AND
    /// `take_cap_breaches` are processed — so the offset commits after the
    /// line's ledger effect (phase 1+: the `permission.pending` event's
    /// transaction; phase 0: the drain). A crash between read and persist
    /// re-reads from the last persisted offset (no loss, no silent dup —
    /// the ledger dedupes by request_id in phase 1+).
    /// review fix 9: only sessions whose offset actually MOVED are written.
    /// The drain block runs on EVERY wake, so an unconditional UPSERT cost
    /// one SQLite write per tailed session per wake (N writes/wake with N
    /// workers) even when nothing was read.
    pub fn persist_offsets(&mut self, ledger: &mut Ledger) -> Result<()> {
        for (session, t) in &mut self.tailed {
            if t.offset == t.persisted_offset {
                continue; // nothing was consumed — no cursor write
            }
            ledger.set_stream_cursor(session, t.offset)?;
            t.persisted_offset = t.offset;
        }
        Ok(())
    }

    /// Test observable: complete JSON lines parsed (consumed) for a session
    /// this runtime life.
    #[allow(dead_code)] // test observable (the unit tests in this file)
    pub fn parsed_lines(&self, session: &str) -> usize {
        self.parsed_counts.get(session).copied().unwrap_or(0)
    }

    /// Drain the surfaced parse errors (fail fast — the caller appends them
    /// as durable events in Task 5; phase 0 surfaces them for the test).
    pub fn take_parse_errors(&mut self) -> Vec<ParseError> {
        std::mem::take(&mut self.parse_errors)
    }

    /// cp-0 §2.3: drain the cap breaches surfaced this drain — the event
    /// loop appends `session.stream_capped` from each and kills the worker.
    pub fn take_cap_breaches(&mut self) -> Vec<CapBreach> {
        std::mem::take(&mut self.cap_breaches)
    }

    /// cp-0 fix 8: drain the per-session drain (open/seek/read) errors
    /// captured non-fatally this drain — the event loop appends each as a
    /// durable `patrol.degraded` event (the read-channel source + session
    /// named in the `error` string; fix 1 shape — only `error`/`session`
    /// ride the `PatrolDegraded` fold struct).
    pub fn take_drain_error_events(&mut self) -> Vec<camp_core::event::EventInput> {
        std::mem::take(&mut self.drain_errors)
            .into_iter()
            .map(|de| camp_core::event::EventInput {
                kind: camp_core::event::EventType::PatrolDegraded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": de.session,
                    "error": format!("read_channel: stream drain: {}", de.error),
                }),
            })
            .collect()
    }

    /// Hold the notify watcher for liveness (the patrol mold).
    pub fn set_watcher(&mut self, watcher: notify::RecommendedWatcher) {
        self.watcher = Some(watcher);
    }

    /// Drain a stored watcher error into its durable event (the
    /// patrol::take_watch_error_events mold — a dead watcher is a durable,
    /// evented fault, never just a stderr line). Reuses `patrol.degraded`;
    /// the read-channel source is named IN the `error` string
    /// (`read_channel: ...`) because the `patrol.degraded` schema is
    /// `deny_unknown_fields` (only `error` + `session`) — a phase that
    /// wants a dedicated `read_channel.degraded` event can split it later.
    pub fn take_watch_error_events(&mut self) -> Vec<camp_core::event::EventInput> {
        let mut out = Vec::new();
        if let Some(msg) = lock_unpoisoned(&self.filter).error.take() {
            out.push(camp_core::event::EventInput {
                kind: camp_core::event::EventType::PatrolDegraded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "error": format!("read_channel: stream watcher error: {msg}"),
                }),
            });
        }
        out
    }

    /// Drain surfaced parse errors into durable events (fail fast — §2.3:
    /// an unparsable line is never silently dropped). The caller appends
    /// them to the ledger. Reuses `patrol.degraded` with the session in its
    /// `session` audit field and the read-channel source named in `error`.
    pub fn take_parse_error_events(&mut self) -> Vec<camp_core::event::EventInput> {
        self.take_parse_errors()
            .into_iter()
            .map(|pe| camp_core::event::EventInput {
                kind: camp_core::event::EventType::PatrolDegraded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": pe.session,
                    "error": format!(
                        "read_channel: non-JSON line in stream at offset {}: {}: {}",
                        pe.offset, pe.error, pe.line
                    ),
                }),
            })
            .collect()
    }
}

/// A poisoned mutex still yields its data (the patrol mold): the callback
/// holds the lock only for inserts, and campd must not die over a poisoned
/// filter.
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The notify callback body (runs on the watcher's thread — the
/// patrol::on_watch_event mold): a Rescan, an empty-path event, or any
/// unrecognized event kind ⇒ set the `rescan` flag (§2.3: rev 2's
/// `event.paths` iteration discarded the Rescan). Per-path dispatch is
/// an optimization applied only to well-formed events; phase 0 drains all
/// on every wake, so the flag is forward-compatible. Always signal — the
/// drain-all-on-every-wake rule makes the watch a latency-only wake.
pub fn on_watch_event(
    result: notify::Result<notify::Event>,
    sender: Option<&mio::unix::pipe::Sender>,
    filter: &Mutex<ReadFilter>,
) {
    let signal = match result {
        Ok(event) => {
            let mut f = lock_unpoisoned(filter);
            // §2.3: a Rescan (empty paths) or any non-modify kind ⇒ drain all.
            let well_formed_modify = matches!(
                event.kind,
                notify::EventKind::Modify(_) | notify::EventKind::Access(_)
            );
            if event.paths.is_empty() || !well_formed_modify {
                f.rescan = true;
            }
            true
        }
        Err(e) => {
            lock_unpoisoned(filter).error = Some(format!("{e}"));
            true
        }
    };
    if signal && let Some(sender) = sender {
        let _ = (&*sender).write(&[1]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::event::EventInput;
    use camp_core::ledger::Ledger;

    fn woke_input(name: &str, bead: &str) -> EventInput {
        EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some(bead.into()),
            data: serde_json::json!({ "name": name, "agent": "dev", "bead": bead }),
        }
    }

    /// cp-0 fix 5: a SessionWoke with a custom actor (for the attended-
    /// session filter test — actor "hook:session-start" must NOT register).
    fn woke_input_with_actor(name: &str, bead: &str, actor: &str) -> EventInput {
        EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: actor.into(),
            bead: Some(bead.into()),
            data: serde_json::json!({ "name": name, "agent": "dev", "bead": bead }),
        }
    }

    fn stopped_input(name: &str) -> EventInput {
        EventInput {
            kind: EventType::SessionStopped,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({ "name": name }),
        }
    }

    /// observe + apply_tracking registers a tailed session on session.woke.
    #[test]
    fn observe_woke_then_apply_registers_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let mut rc = ReadChannelRuntime::new(sessions_dir.clone(), 256 * 1024 * 1024).unwrap();
        // Append a woke event and observe it through the real event shape.
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        ledger.append(woke_input("t/dev/1", "gc-1")).unwrap();
        let event = ledger.events_range(1, None).unwrap().pop().unwrap();
        rc.observe(&event);
        assert!(rc.tailed_sessions().is_empty(), "queued, not applied yet");
        rc.apply_tracking(&mut ledger).unwrap();
        assert_eq!(rc.tailed_sessions(), vec!["t/dev/1".to_string()]);
        assert_eq!(rc.offset_of("t/dev/1"), Some(0), "new session => offset 0");
    }

    /// A stopped/crashed session unregisters and clears the offset row —
    /// but only once the DEFERRED unregister is applied (review fix 1).
    /// `apply_tracking` merely queues it, so the event loop's drain block
    /// still sees the session tailed and reads its final bytes.
    #[test]
    fn observe_stopped_then_apply_defers_the_unregister_until_after_the_drain() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        ledger.append(woke_input("t/dev/1", "gc-1")).unwrap();
        let woke = ledger.events_range(1, None).unwrap().pop().unwrap();
        rc.observe(&woke);
        rc.apply_tracking(&mut ledger).unwrap();
        ledger.set_stream_cursor("t/dev/1", 4096).unwrap();
        // Now stop the session.
        ledger.append(stopped_input("t/dev/1")).unwrap();
        let stopped = ledger.events_range(2, None).unwrap().pop().unwrap();
        rc.observe(&stopped);
        rc.apply_tracking(&mut ledger).unwrap();
        // review fix 1: STILL TAILED — the drain block has not run yet, and
        // dropping it here would delete the worker's final output unread.
        assert_eq!(
            rc.tailed_sessions(),
            vec!["t/dev/1".to_string()],
            "the unregister is DEFERRED — the session is still tailed for the final drain"
        );
        // The event loop applies it as the LAST step of the drain block.
        rc.apply_pending_unregisters(&mut ledger).unwrap();
        assert!(rc.tailed_sessions().is_empty(), "unregistered");
        assert_eq!(
            ledger.stream_cursor("t/dev/1").unwrap(),
            0,
            "offset row cleared"
        );
    }

    /// review fix 1 (CRITICAL), the unit-level proof: a worker's FINAL bytes
    /// — written after the reap event was observed but before disposal —
    /// are drained. The reap-ordering the event loop implements is:
    ///   observe(stopped) → apply_tracking (defers) → drain_all (reads the
    ///   final bytes) → persist_offsets → apply_pending_unregisters (disposes)
    /// Executing the unregister inside `apply_tracking` (the original code)
    /// unlinked the file before `drain_all` ever saw it.
    #[test]
    fn a_reaped_sessions_final_bytes_are_drained_before_the_file_is_disposed() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(&stdout, "{\"type\":\"assistant\"}\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        ledger.append(woke_input("t/dev/1", "gc-1")).unwrap();
        let woke = ledger.events_range(1, None).unwrap().pop().unwrap();
        rc.observe(&woke);
        rc.apply_tracking(&mut ledger).unwrap();
        rc.drain_all(&mut ledger).unwrap();
        assert_eq!(rc.parsed_lines("t/dev/1"), 1, "the first line is consumed");
        // The worker writes its LAST line and exits. campd reaps it: the
        // session.stopped is observed and tracking applied — all before the
        // event loop's drain block runs.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&stdout)
            .unwrap()
            .write_all(b"{\"type\":\"result\",\"subtype\":\"success\"}\n")
            .unwrap();
        ledger.append(stopped_input("t/dev/1")).unwrap();
        let stopped = ledger.events_range(2, None).unwrap().pop().unwrap();
        rc.observe(&stopped);
        rc.apply_tracking(&mut ledger).unwrap();
        // The drain block: the final line MUST still be readable here.
        rc.drain_all(&mut ledger).unwrap();
        assert_eq!(
            rc.parsed_lines("t/dev/1"),
            2,
            "the worker's FINAL line was drained before disposal — not deleted unread"
        );
        rc.persist_offsets(&mut ledger).unwrap();
        rc.apply_pending_unregisters(&mut ledger).unwrap();
        assert!(
            !stdout.exists(),
            "and only THEN is the stream file disposed"
        );
        assert!(rc.tailed_sessions().is_empty());
    }

    /// review fix 2: a capped session is a HARD STOP — a second drain does
    /// NOT read the over-cap file (the guards used to be gated on
    /// `!capped`, so once capped they stopped firing and the next drain
    /// read the whole over-cap file into `partial` — unbounded RSS).
    #[test]
    fn a_capped_session_is_not_read_again_and_partial_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        let cap: u64 = 128;
        // A newline-less blob far over the cap.
        std::fs::write(&stdout, vec![b'x'; 64 * 1024]).unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, cap).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        assert_eq!(rc.take_cap_breaches().len(), 1, "the breach is surfaced");
        // The kill-in-flight window: campd wakes again before the reap lands.
        rc.drain_all(&mut ledger).unwrap();
        rc.drain_all(&mut ledger).unwrap();
        assert!(
            rc.take_cap_breaches().is_empty(),
            "no duplicate breach events"
        );
        let partial_len = rc.tailed.get("t/dev/1").map(|t| t.partial.len()).unwrap();
        assert_eq!(
            partial_len, 0,
            "a capped session is never read again — partial stays empty (RSS-bounded)"
        );
    }

    /// review fix 9: a quiescent tailed session costs ZERO cursor writes.
    /// `persist_offsets` used to UPSERT every tailed session on every wake.
    #[test]
    fn persist_offsets_skips_sessions_whose_offset_did_not_move() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(&stdout, "{\"type\":\"a\"}\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        rc.persist_offsets(&mut ledger).unwrap();
        let persisted = ledger.stream_cursor("t/dev/1").unwrap();
        assert!(persisted > 0, "the consumed line was persisted");
        // Corrupt the row behind the runtime's back: a second persist with an
        // unmoved offset must NOT rewrite it (proving no write happened).
        ledger.set_stream_cursor("t/dev/1", 999_999).unwrap();
        rc.drain_all(&mut ledger).unwrap(); // no new bytes — offset unmoved
        rc.persist_offsets(&mut ledger).unwrap();
        assert_eq!(
            ledger.stream_cursor("t/dev/1").unwrap(),
            999_999,
            "no cursor write for a session whose offset did not move"
        );
    }

    /// A restart resumes from the persisted offset (§8 append-only-cursors):
    /// register loads the offset the prior campd life persisted.
    #[test]
    fn register_loads_the_persisted_offset() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        // The prior campd life persisted this offset.
        ledger.set_stream_cursor("t/dev/1", 8192).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        assert_eq!(
            rc.offset_of("t/dev/1"),
            Some(8192),
            "resumed from the persisted offset"
        );
    }

    /// drain_all reads to EOF from offset 0 and persists the offset at the
    /// file size. Two complete lines => offset advances past both.
    #[test]
    fn drain_all_reads_complete_lines_and_persists_the_offset() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(
            &stdout,
            "{\"type\":\"assistant\",\"text\":\"hi\"}\n{\"type\":\"result\",\"text\":\"ok\"}\n",
        )
        .unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir.clone(), 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        // cp-0 fix 7: drain_all advances the in-memory offset only;
        // persist_offsets writes it to the stream_cursors table.
        rc.persist_offsets(&mut ledger).unwrap();
        let file_len = std::fs::metadata(&stdout).unwrap().len();
        assert_eq!(rc.offset_of("t/dev/1"), Some(file_len), "offset at EOF");
        assert_eq!(
            ledger.stream_cursor("t/dev/1").unwrap(),
            file_len,
            "persisted"
        );
        assert_eq!(rc.parsed_lines("t/dev/1"), 2, "two complete lines consumed");
    }

    /// A trailing partial line is buffered, NOT parsed, and the offset
    /// stays at the last complete line's end (§2.3: a notify event can land
    /// mid-line; a partial JSON line is never parsed). The next drain
    /// completes it.
    #[test]
    fn drain_all_buffers_a_trailing_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        let complete = b"{\"type\":\"assistant\"}\n";
        let partial = b"{\"type\":\"result\",\"text\":\"op";
        std::fs::write(&stdout, [complete.as_ref(), partial.as_ref()].concat()).unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir.clone(), 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        let offset = rc.offset_of("t/dev/1").unwrap();
        assert_eq!(
            offset,
            complete.len() as u64,
            "offset at the last complete line end"
        );
        assert_eq!(
            rc.parsed_lines("t/dev/1"),
            1,
            "the partial line was NOT parsed"
        );
        // Append the rest of the line + a newline.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&stdout)
            .unwrap();
        use std::io::Write;
        file.write_all(b"en\"}\n").unwrap();
        drop(file);
        rc.drain_all(&mut ledger).unwrap();
        assert_eq!(
            rc.parsed_lines("t/dev/1"),
            2,
            "the completed line is now parsed"
        );
        let file_len = std::fs::metadata(&stdout).unwrap().len();
        assert_eq!(
            rc.offset_of("t/dev/1"),
            Some(file_len),
            "offset at EOF after completion"
        );
    }

    /// A second drain with no new data is a no-op (idempotent): the offset
    /// does not move, no line is re-parsed.
    #[test]
    fn drain_all_with_no_new_data_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(&stdout, "{\"type\":\"assistant\"}\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        let offset = rc.offset_of("t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        assert_eq!(rc.offset_of("t/dev/1"), Some(offset), "no movement");
        assert_eq!(rc.parsed_lines("t/dev/1"), 1, "no re-parse");
    }

    /// drain_all resumes from the persisted offset after a restart: a fresh
    /// runtime, register loads the prior offset, drain reads ONLY the new
    /// bytes (no loss, no duplication) (§8 append-only-cursors).
    #[test]
    fn drain_all_resumes_from_the_persisted_offset_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(&stdout, "{\"type\":\"a\"}\n{\"type\":\"b\"}\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        // First life: drain both lines, persist the offset.
        let mut rc1 = ReadChannelRuntime::new(sessions_dir.clone(), 256 * 1024 * 1024).unwrap();
        rc1.register(&mut ledger, "t/dev/1").unwrap();
        rc1.drain_all(&mut ledger).unwrap();
        // cp-0 fix 7: persist the in-memory offset so the second life
        // registers from it (the append-only-cursors resumption).
        rc1.persist_offsets(&mut ledger).unwrap();
        let persisted = ledger.stream_cursor("t/dev/1").unwrap();
        assert_eq!(rc1.parsed_lines("t/dev/1"), 2);
        // Append a third line after the "crash".
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&stdout)
            .unwrap();
        use std::io::Write;
        file.write_all(b"{\"type\":\"c\"}\n").unwrap();
        drop(file);
        // Second life: fresh runtime, register loads the persisted offset.
        let mut rc2 = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc2.register(&mut ledger, "t/dev/1").unwrap();
        assert_eq!(
            rc2.offset_of("t/dev/1"),
            Some(persisted),
            "resumed from persisted"
        );
        rc2.drain_all(&mut ledger).unwrap();
        assert_eq!(
            rc2.parsed_lines("t/dev/1"),
            1,
            "only the NEW line — no duplication"
        );
        let file_len = std::fs::metadata(&stdout).unwrap().len();
        assert_eq!(
            rc2.offset_of("t/dev/1"),
            Some(file_len),
            "no loss — offset at EOF"
        );
    }

    /// A non-JSON line surfaces as a durable error event (fail fast, §2.3:
    /// an unparsable line is never silently dropped). The drain CONTINUES
    /// past it (the offset advances); the error is collected for the
    /// caller to append. This test asserts the offset advances AND the
    /// error is captured (the ledger append is wired in Task 5).
    #[test]
    fn drain_all_surfaces_a_non_json_line_as_an_error_and_advances() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(&stdout, "not json at all\n{\"type\":\"ok\"}\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        let file_len = std::fs::metadata(&stdout).unwrap().len();
        assert_eq!(
            rc.offset_of("t/dev/1"),
            Some(file_len),
            "the bad line's offset advances"
        );
        assert!(
            !rc.take_parse_errors().is_empty(),
            "the parse error is surfaced"
        );
        // the good line after it is still consumed
        assert_eq!(
            rc.parsed_lines("t/dev/1"),
            1,
            "the valid line after the bad one is parsed"
        );
    }

    use mio::unix::pipe;

    /// A well-formed event on a registered path signals the self-pipe.
    #[test]
    fn on_watch_event_signals_on_a_registered_path() {
        let (sender, mut receiver) = pipe::new().unwrap();
        let rc = ReadChannelRuntime::new(std::env::temp_dir(), 256 * 1024 * 1024).unwrap();
        let filter = rc.filter_slot();
        let path = std::env::temp_dir().join("t-dev-1.json");
        lock_unpoisoned(&filter).registered.insert(path.clone());
        let mut event = notify::Event::new(notify::EventKind::Modify(
            notify::event::ModifyKind::Data(notify::event::DataChange::Any),
        ));
        event.paths.push(path);
        on_watch_event(Ok(event), Some(&sender), &filter);
        let mut buf = [0u8; 1];
        assert_eq!(receiver.read(&mut buf).unwrap(), 1, "signaled");
    }

    /// §2.3 / §8: a Rescan (empty paths) event MUST signal — rev 2's
    /// `event.paths` iteration discarded it. The drain-all-on-every-wake
    /// rule covers correctness; this test pins that the callback does not
    /// drop the event.
    #[test]
    fn on_watch_event_signals_on_a_rescan_empty_paths_event() {
        let (sender, mut receiver) = pipe::new().unwrap();
        let rc = ReadChannelRuntime::new(std::env::temp_dir(), 256 * 1024 * 1024).unwrap();
        let filter = rc.filter_slot();
        // notify's documented inotify-overflow shape: EventKind::Other with
        // an EMPTY paths vec.
        let event = notify::Event::new(notify::EventKind::Other);
        assert!(event.paths.is_empty(), "the Rescan has empty paths");
        on_watch_event(Ok(event), Some(&sender), &filter);
        let mut buf = [0u8; 1];
        assert_eq!(receiver.read(&mut buf).unwrap(), 1, "the Rescan signaled");
        assert!(lock_unpoisoned(&filter).rescan, "the rescan flag is set");
    }

    /// A watcher error is stored for its durable event and signals.
    #[test]
    fn on_watch_event_stores_a_watcher_error_and_signals() {
        let (sender, mut receiver) = pipe::new().unwrap();
        let rc = ReadChannelRuntime::new(std::env::temp_dir(), 256 * 1024 * 1024).unwrap();
        let filter = rc.filter_slot();
        on_watch_event(
            Err(notify::Error::generic("inotify watch limit reached")),
            Some(&sender),
            &filter,
        );
        let mut buf = [0u8; 1];
        assert_eq!(receiver.read(&mut buf).unwrap(), 1, "signaled");
        assert!(
            lock_unpoisoned(&filter)
                .error
                .as_ref()
                .unwrap()
                .contains("inotify watch limit reached"),
            "error stored"
        );
    }

    /// A stream file crossing max_stream_bytes surfaces a cap breach (the
    /// offset still advances to EOF — the breach is loud, not a silent
    /// truncation; invariant 5).
    #[test]
    fn drain_all_surfaces_a_cap_breach_when_the_file_exceeds_max_stream_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        // A small cap so the test is fast.
        let cap: u64 = 64;
        std::fs::write(&stdout, "{\"type\":\"assistant\"}\n").unwrap();
        // Grow the file past the cap.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&stdout)
            .unwrap();
        use std::io::Write;
        file.write_all(&[b' '; 128]).unwrap();
        drop(file);
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, cap).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        let breaches = rc.take_cap_breaches();
        assert_eq!(breaches.len(), 1, "one breach surfaced");
        assert_eq!(breaches[0].session, "t/dev/1");
        assert!(breaches[0].file_size > cap, "the file exceeded the cap");
    }

    /// cp-0 fix 5: a hook-registered attended session (actor
    /// "hook:session-start") queues NO Register — attended sessions have
    /// no campd-created stdout file, so tailing them would crash campd via
    /// drain_one's open. A campd-actor SessionWoke queues Register.
    #[test]
    fn observe_filters_attended_sessions_and_registers_only_campd_workers() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        // attended (hook-registered) session.woke → NO register.
        ledger
            .append(woke_input_with_actor(
                "t/attended/1",
                "gc-1",
                "hook:session-start",
            ))
            .unwrap();
        let attended = ledger.events_range(1, None).unwrap().pop().unwrap();
        rc.observe(&attended);
        rc.apply_tracking(&mut ledger).unwrap();
        assert!(
            rc.tailed_sessions().is_empty(),
            "attended sessions are not tailed"
        );
        // campd-spawned worker session.woke → register.
        ledger.append(woke_input("t/dev/1", "gc-2")).unwrap();
        let woke = ledger.events_range(2, None).unwrap().pop().unwrap();
        rc.observe(&woke);
        rc.apply_tracking(&mut ledger).unwrap();
        assert_eq!(
            rc.tailed_sessions(),
            vec!["t/dev/1".to_string()],
            "campd-spawned workers are tailed"
        );
    }

    /// cp-0 fix 9 (OOM-before-cap), PRE-READ half: a file already over the
    /// cap breaches WITHOUT reading a byte — `partial` stays 0.
    ///
    /// review fix 6: this test is NAMED for the in-loop guard but only ever
    /// exercised the pre-read one — it writes a static 500-byte file against
    /// a 200-byte cap, so the pre-read check fires and `drain_one` returns
    /// before the read loop runs. `partial` is 0, making `partial <= cap`
    /// trivially true; deleting the in-loop guard left it green. The in-loop
    /// guard is now driven by its own test below (it is UNREACHABLE for a
    /// static file: the pre-read check compares the absolute file size, so
    /// any file with more than `cap` bytes past the offset breaches before
    /// the read loop — only a file that GROWS mid-drain reaches it).
    #[test]
    fn drain_all_pre_read_cap_breach_does_not_read_the_over_cap_file_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        let cap: u64 = 200;
        // A single newline-less line larger than the cap.
        std::fs::write(&stdout, vec![b'x'; 500]).unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, cap).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        let breaches = rc.take_cap_breaches();
        assert_eq!(breaches.len(), 1, "the huge line breached the cap");
        // The pre-read check breached before reading a single byte.
        let partial_len = rc.tailed.get("t/dev/1").map(|t| t.partial.len()).unwrap();
        assert_eq!(
            partial_len, 0,
            "the pre-read guard breached WITHOUT reading — partial is untouched"
        );
    }

    /// cp-0 fix 9 (OOM-before-cap), IN-LOOP half — review fix 6.
    ///
    /// The in-loop guard bounds `partial` when the file GROWS DURING the
    /// drain: the pre-read stat saw a size under the cap, so the read loop
    /// runs, and a concurrent worker keeps appending newline-less bytes past
    /// the cap while we read. Without the before-extend check, `partial`
    /// absorbs the whole over-cap blob (unbounded RSS — the exact OOM fix 9
    /// exists to prevent).
    ///
    /// Reaching this guard REQUIRES concurrency (see the pre-read test's
    /// note), so the scenario is retried until the race lands the drain in
    /// the read loop — proven by `partial > 0`, which a pre-read breach can
    /// never produce. Delete the in-loop guard and this test fails: `partial`
    /// grows to the whole multi-MiB file, blowing the `<= cap` bound.
    #[test]
    fn drain_all_in_loop_guard_bounds_partial_when_the_file_grows_during_the_drain() {
        const CAP: u64 = 1 << 20; // 1 MiB
        for attempt in 0..25 {
            let dir = tempfile::tempdir().unwrap();
            let sessions_dir = dir.path().join("sessions");
            std::fs::create_dir_all(&sessions_dir).unwrap();
            let stdout = sessions_dir.join("t-dev-1.json");
            // Start UNDER the cap so the pre-read stat lets the read loop run.
            std::fs::write(&stdout, vec![b'x'; (CAP as usize) - 4096]).unwrap();
            let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
            let mut rc = ReadChannelRuntime::new(sessions_dir, CAP).unwrap();
            rc.register(&mut ledger, "t/dev/1").unwrap();
            // A "worker" appending newline-less bytes far past the cap while
            // the drain reads.
            let path = stdout.clone();
            let writer = std::thread::spawn(move || {
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap();
                for _ in 0..512 {
                    if f.write_all(&[b'y'; 8192]).is_err() {
                        return;
                    }
                }
            });
            rc.drain_all(&mut ledger).unwrap();
            writer.join().unwrap();
            let partial_len = rc.tailed.get("t/dev/1").map(|t| t.partial.len()).unwrap();
            if partial_len == 0 {
                // The writer won the race and pushed the file over the cap
                // before the stat: the PRE-read guard breached instead. Not
                // the path under test — retry.
                continue;
            }
            // The read loop ran (partial > 0) — so the in-loop guard is the
            // only thing standing between `partial` and the whole file.
            assert!(
                partial_len as u64 <= CAP,
                "attempt {attempt}: the in-loop guard did not bound partial: \
                 partial={partial_len} exceeds cap={CAP} (unbounded read — OOM)"
            );
            assert_eq!(
                rc.take_cap_breaches().len(),
                1,
                "attempt {attempt}: the in-loop guard breached loudly"
            );
            return; // the in-loop path was exercised and held
        }
        panic!("the in-loop cap guard was never exercised in 25 attempts");
    }

    /// cp-0 fix 10 (reap-time stream-file disposal): unregister best-effort
    /// unlinks the session's stdout file (§2.3: "stream files append-only
    /// until reap"). After unregister, the file no longer exists.
    #[test]
    fn unregister_disposes_the_session_stream_file() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(&stdout, b"{\"type\":\"assistant\"}\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        assert!(stdout.exists(), "the stream file exists while tailed");
        rc.unregister(&mut ledger, "t/dev/1").unwrap();
        assert!(
            !stdout.exists(),
            "the stream file is disposed at unregister"
        );
        assert!(rc.tailed_sessions().is_empty(), "unregistered");
        assert_eq!(
            ledger.stream_cursor("t/dev/1").unwrap(),
            0,
            "offset row cleared"
        );
    }
}

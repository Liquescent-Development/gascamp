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
    partial: Vec<u8>,
    /// None until the first drain opens the file; reused thereafter.
    file: Option<std::fs::File>,
    /// cp-0 §2.3: set after a `max_stream_bytes` breach is surfaced for this
    /// session, so the kill-in-flight window (breach → SIGCHLD → reap →
    /// unregister) does not re-surface the breach on every intervening
    /// wake and append duplicate `session.stream_capped` events.
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
/// into a durable `patrol.degraded` event (the read-channel component).
#[derive(Debug, Clone)]
pub struct ParseError {
    pub session: String,
    pub line: String,
    pub offset: u64,
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
            filter: std::sync::Arc::new(std::sync::Mutex::new(ReadFilter::default())),
            parsed_counts: HashMap::new(),
            parse_errors: Vec::new(),
            cap_breaches: Vec::new(),
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
    pub fn apply_tracking(&mut self, ledger: &mut Ledger) -> Result<()> {
        let ops = std::mem::take(&mut self.track_ops);
        for op in ops {
            match op {
                TrackOp::Register(name) => self.register(ledger, &name)?,
                TrackOp::Unregister(name) => self.unregister(ledger, &name)?,
            }
        }
        Ok(())
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
        lock_unpoisoned(&self.filter).registered.insert(stdout_path.clone());
        self.tailed.insert(
            session.to_owned(),
            Tailed {
                stdout_path,
                offset,
                partial: Vec::new(),
                file: None,
                capped: false,
            },
        );
        Ok(())
    }

    /// Unregister a session: drop the in-memory state and clear the
    /// persisted offset row (the stream file is disposed at reap, §2.3).
    pub fn unregister(&mut self, ledger: &mut Ledger, session: &str) -> Result<()> {
        if let Some(t) = self.tailed.remove(session) {
            lock_unpoisoned(&self.filter).registered.remove(&t.stdout_path);
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
    /// validates only), advance the offset past each complete line, and
    /// persist the offset. A parse failure is surfaced via
    /// `take_parse_errors` (fail fast) but does NOT stop the drain.
    pub fn drain_all(&mut self, ledger: &mut Ledger) -> Result<()> {
        let sessions: Vec<String> = self.tailed.keys().cloned().collect();
        for session in sessions {
            self.drain_one(ledger, &session)?;
        }
        Ok(())
    }

    fn drain_one(&mut self, ledger: &mut Ledger, session: &str) -> Result<()> {
        let Some(t) = self.tailed.get_mut(session) else {
            return Ok(());
        };
        // Open-or-reuse the fd at the offset. A missing file is NOT a
        // hard fault: a just-crashed worker's stream file is reaped at the
        // SIGCHLD reap, and the unregister lands only on the next settle —
        // so a drain in that window sees the file gone. Skip it; the
        // unregister will remove the session from the tailed set.
        let file = match t.file.as_mut() {
            Some(f) => f,
            None => match std::fs::OpenOptions::new().read(true).open(&t.stdout_path) {
                Ok(f) => t.file.insert(f),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("opening {}", t.stdout_path.display()))
                }
            },
        };
        file.seek(SeekFrom::Start(t.offset))
            .with_context(|| format!("seeking {}", t.stdout_path.display()))?;
        // The trailing partial from a previous drain is still in the file
        // at [t.offset..] (the stream file is append-only, never
        // truncated — §2.3), so re-reading from `t.offset` re-reads it.
        // Clear the in-memory partial so it is not double-counted; the
        // bytes are re-read fresh from the file below.
        t.partial.clear();
        let mut buf = [0u8; 8192];
        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(e).with_context(|| format!("reading {}", t.stdout_path.display()))
                }
            };
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
            // Persist the offset after each read chunk. The offset is at
            // the last complete line's end; the partial buffer is held in
            // memory and re-read from `t.offset` on the next drain.
            //
            // cp-0 phase-1+ obligation: once consumed lines become
            // `permission.pending` events with their own ledger effect,
            // this persist must move to AFTER that effect's transaction
            // commits (persist-after-event-commit), not after each read
            // chunk — so a crash between parse and persist re-reads
            // (dedup by request_id) rather than silently skipping. Phase 0
            // has no per-line ledger effect, so persisting after the drain
            // chunk is correct today.
            ledger.set_stream_cursor(session, t.offset)?;
        }
        // §2.3: max_stream_bytes ceiling — a loud session failure. The
        // offset has already advanced to EOF (the breach is loud, not a
        // silent truncation; invariant 5); the event loop appends
        // session.stream_capped from the breach and kills the worker.
        let file_size = t
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0);
        if file_size > self.max_stream_bytes && !t.capped {
            t.capped = true;
            self.cap_breaches.push(CapBreach {
                session: session.to_owned(),
                bead: None, // the event loop fills the bead from the session registry
                file: t.stdout_path.clone(),
                file_size,
                cap_bytes: self.max_stream_bytes,
            });
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

    /// A stopped/crashed session unregisters and clears the offset row.
    #[test]
    fn observe_stopped_then_apply_unregisters_and_clears_the_offset() {
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
        assert!(rc.tailed_sessions().is_empty(), "unregistered");
        assert_eq!(ledger.stream_cursor("t/dev/1").unwrap(), 0, "offset row cleared");
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
        assert_eq!(rc.offset_of("t/dev/1"), Some(8192), "resumed from the persisted offset");
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
        let file_len = std::fs::metadata(&stdout).unwrap().len();
        assert_eq!(rc.offset_of("t/dev/1"), Some(file_len), "offset at EOF");
        assert_eq!(ledger.stream_cursor("t/dev/1").unwrap(), file_len, "persisted");
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
        assert_eq!(offset, complete.len() as u64, "offset at the last complete line end");
        assert_eq!(rc.parsed_lines("t/dev/1"), 1, "the partial line was NOT parsed");
        // Append the rest of the line + a newline.
        let mut file = std::fs::OpenOptions::new().append(true).open(&stdout).unwrap();
        use std::io::Write;
        file.write_all(b"en\"}\n").unwrap();
        drop(file);
        rc.drain_all(&mut ledger).unwrap();
        assert_eq!(rc.parsed_lines("t/dev/1"), 2, "the completed line is now parsed");
        let file_len = std::fs::metadata(&stdout).unwrap().len();
        assert_eq!(rc.offset_of("t/dev/1"), Some(file_len), "offset at EOF after completion");
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
        let persisted = ledger.stream_cursor("t/dev/1").unwrap();
        assert_eq!(rc1.parsed_lines("t/dev/1"), 2);
        // Append a third line after the "crash".
        let mut file = std::fs::OpenOptions::new().append(true).open(&stdout).unwrap();
        use std::io::Write;
        file.write_all(b"{\"type\":\"c\"}\n").unwrap();
        drop(file);
        // Second life: fresh runtime, register loads the persisted offset.
        let mut rc2 = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc2.register(&mut ledger, "t/dev/1").unwrap();
        assert_eq!(rc2.offset_of("t/dev/1"), Some(persisted), "resumed from persisted");
        rc2.drain_all(&mut ledger).unwrap();
        assert_eq!(rc2.parsed_lines("t/dev/1"), 1, "only the NEW line — no duplication");
        let file_len = std::fs::metadata(&stdout).unwrap().len();
        assert_eq!(rc2.offset_of("t/dev/1"), Some(file_len), "no loss — offset at EOF");
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
        assert_eq!(rc.offset_of("t/dev/1"), Some(file_len), "the bad line's offset advances");
        assert!(!rc.take_parse_errors().is_empty(), "the parse error is surfaced");
        // the good line after it is still consumed
        assert_eq!(rc.parsed_lines("t/dev/1"), 1, "the valid line after the bad one is parsed");
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
        let mut file = std::fs::OpenOptions::new().append(true).open(&stdout).unwrap();
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
}
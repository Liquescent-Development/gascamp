//! cp-0 (control-plane spec §2.3): the campd read channel — per-session
//! byte-offset tailing of each worker's stdout file, drained to EOF on
//! every campd wake. The notify watcher is a latency-only wake-up; the
//! correctness rule is "drain all tailed files on every wake, any token."
//! Partial lines are buffered (a notify event can land mid-line; a partial
//! JSON line is never parsed). Offsets persist in the `stream_cursors` table
//! only after the line's ledger effect commits, so a campd restart resumes
//! from the exact byte the last life consumed. A `max_stream_bytes` breach
//! is a loud, evented session failure (§2.3).
//!
//! WIP allow: this runtime is built up over Tasks 3-6 and only wired into
//! the daemon in Task 5 (event_loop/orders/mod.rs) with the cap closed in
//! Task 6. Until then several items are test-only or forward-declared;
//! the module-level `dead_code` allow is removed in Task 6 once every
//! item is reached by the wiring + the cap block.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use camp_core::event::{Event, EventType};
use camp_core::ledger::Ledger;

use super::spawn::munge;

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
                if let Some(name) = event.data["name"].as_str() {
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
    pub fn tailed_sessions(&self) -> Vec<String> {
        self.tailed.keys().cloned().collect()
    }

    /// Test observable: the in-memory offset for a session.
    pub fn offset_of(&self, session: &str) -> Option<u64> {
        self.tailed.get(session).map(|t| t.offset)
    }
}

/// A poisoned mutex still yields its data (the patrol mold): the callback
/// holds the lock only for inserts, and campd must not die over a poisoned
/// filter.
fn lock_unpoisoned<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
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
}
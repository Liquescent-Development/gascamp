//! campd's exactly-once event consumption (spec §7.3): the `cursors` row
//! 'campd' marks the last processed seq; catch-up replays everything past
//! it through an `EventProcessor`, and the cursor advances in the same
//! transaction as the processor's ledger effects (the
//! `Ledger::process_past_cursor` mechanism).

use camp_core::Seq;
use camp_core::error::CoreError;
use camp_core::event::{Event, EventType};
use camp_core::ledger::Ledger;
use rusqlite::Connection;

/// campd's row in the `cursors` table.
pub const CAMPD_CURSOR: &str = "campd";

/// What campd runs over each committed event, in seq order. Ledger writes
/// must go through `conn` — the open cursor transaction — so they commit
/// atomically with the cursor advance. Phase 8 plugs dispatch in here.
pub trait EventProcessor {
    fn process(&mut self, conn: &Connection, event: &Event) -> Result<(), CoreError>;
}

/// Phase 7's processor: readiness bookkeeping only (spec §7.3 — recompute
/// the affected subgraph on each close). Phase 8's dispatcher consumes
/// `take_pending`; until then the list is the observable proof that the
/// recompute runs on the processing path.
#[derive(Default)]
pub struct ReadinessProcessor {
    pending: Vec<String>,
}

impl ReadinessProcessor {
    /// Drain the beads made ready by processed closes, in processing order.
    pub fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }
}

impl EventProcessor for ReadinessProcessor {
    fn process(&mut self, conn: &Connection, event: &Event) -> Result<(), CoreError> {
        if event.kind == EventType::BeadClosed {
            let bead = event
                .bead
                .as_deref()
                .ok_or_else(|| CoreError::InvalidEventData {
                    event_type: event.kind.as_str().to_owned(),
                    reason: "bead.closed event without a bead id".to_owned(),
                })?;
            self.pending
                .extend(camp_core::readiness::newly_ready(conn, bead)?);
        }
        Ok(())
    }
}

/// Process everything past campd's cursor, to a fixpoint: a processor may
/// itself append events (Phase 8 dispatch); re-checking until the cursor
/// stops moving drains those too. Bounded by the backlog — convergence,
/// not polling. Returns the final cursor position.
pub fn catch_up(
    ledger: &mut Ledger,
    processor: &mut dyn EventProcessor,
) -> Result<Seq, CoreError> {
    loop {
        let before = ledger.cursor(CAMPD_CURSOR)?;
        let after = ledger.process_past_cursor(CAMPD_CURSOR, &mut |conn, event| {
            processor.process(conn, event)
        })?;
        if after == before {
            return Ok(after);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::event::{EventInput, EventType};
    use camp_core::ledger::Ledger;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        (dir, ledger)
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
    fn catch_up_records_newly_ready_beads_from_pass_closes() {
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "pass");

        let mut processor = ReadinessProcessor::default();
        let end = catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(end, 3);
        assert_eq!(l.cursor(CAMPD_CURSOR).unwrap(), 3);
        assert_eq!(processor.take_pending(), vec!["gc-2".to_owned()]);
        // take_pending drains
        assert!(processor.take_pending().is_empty());
    }

    #[test]
    fn a_fail_close_unblocks_nothing() {
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "fail");

        let mut processor = ReadinessProcessor::default();
        catch_up(&mut l, &mut processor).unwrap();
        assert!(processor.take_pending().is_empty());
    }

    #[test]
    fn catch_up_is_exactly_once_across_calls() {
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "pass");

        let mut processor = ReadinessProcessor::default();
        catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(processor.take_pending(), vec!["gc-2".to_owned()]);

        // a second catch-up with no new events reprocesses nothing
        catch_up(&mut l, &mut processor).unwrap();
        assert!(processor.take_pending().is_empty());

        // new events after the cursor are picked up from there only
        create(&mut l, "gc-3", &[]);
        let end = catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(end, 4);
        assert!(
            processor.take_pending().is_empty(),
            "a create is not a close"
        );
    }

    /// Phase 8's dispatch appends events *while processing* (e.g.
    /// session.woke); catch_up drains from the cursor onward on every call,
    /// so nothing is skipped and nothing repeated across calls.
    #[test]
    fn catch_up_drains_events_appended_between_calls() {
        #[derive(Default)]
        struct Recorder {
            seen: Vec<i64>,
        }
        impl EventProcessor for Recorder {
            fn process(
                &mut self,
                _conn: &rusqlite::Connection,
                event: &camp_core::event::Event,
            ) -> Result<(), camp_core::error::CoreError> {
                self.seen.push(event.seq);
                Ok(())
            }
        }
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        let mut processor = Recorder::default();
        catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(processor.seen, vec![1]);
        // an append landing after the first fixpoint is drained by the next
        // catch_up from the cursor onward — nothing skipped, nothing repeated
        create(&mut l, "gc-2", &[]);
        catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(processor.seen, vec![1, 2]);
    }
}

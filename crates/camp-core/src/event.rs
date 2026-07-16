//! The canonical event model (spec §7.2): the shape of an `events` row and
//! what `camp events --json` emits.

use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use crate::Seq;
use crate::error::CoreError;

/// Every event type camp emits. Names follow gc's `noun.verb` convention;
/// `vocab.rs` partitions them into gc-mirrored and camp-specific and tests
/// enforce the naming law (spec §15.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    BeadCreated,
    BeadClaimed,
    BeadUpdated,
    BeadClosed,
    SessionWoke,
    /// The worker's OS pid, appended once the child exists (issue #99):
    /// `session.woke` is committed BEFORE the spawn, so its pid is unknowable
    /// then. Additive, camp-specific — an OS/adoption detail gc does not track.
    SessionPid,
    SessionStopped,
    SessionCrashed,
    SessionNudged,
    CampdStarted,
    CampdStopped,
    /// HISTORICAL — no producer since the CLI became a pure socket client
    /// (campd-service-management design §4.3): the removed CLI-spawn path
    /// recorded which verb had spawned campd. The type STAYS: `EventType::parse`
    /// rejects unknown names and every read path goes through it, so dropping
    /// this variant would make any ledger that carries one unreadable
    /// (`camp events`, the fold, `refold`). Invariant 3 — the ledger tells the
    /// whole story, old ones included.
    CampdAutostarted,
    RigAdded,
    RunCooked,
    OrderFired,
    OrderCompleted,
    OrderFailed,
    ConfigChanged,
    WorkerMilestone,
    WorktreeKept,
    BeadWorktreeReaped,
    DispatchFailed,
    DispatchLiveTree,
    DispatchRearmed,
    CheckPassed,
    CheckFailed,
    RunFinalized,
    AgentStalled,
    PatrolDegraded,
    /// cp-0 (control-plane spec §2.3): a worker's stdout stream file crossed
    /// `max_stream_bytes`. Declarative — the cause event; the reap appends
    /// `session.crashed` with `cause_seq` pointing here, and the bead
    /// re-hooks via the patrol restart path. The event NAMES the cap
    /// (greppable, invariant 3: the ledger tells the whole story).
    SessionStreamCapped,
    /// cp-1 (§4.1): campd delivered an `interrupt` control request into a
    /// session's held stdin. `{session, request_id}`. The ACK half of D1's
    /// ack-then-async: the worker's answer arrives later as
    /// `control.responded`, and THIS event is what a restart rebuilds the
    /// pending table from (B6).
    SessionInterrupted,
    /// cp-1 (§2.1): a worker answered one of camp's control requests.
    /// `{session, request_id, verb, ok, detail, late}`. `late: true` is C11's
    /// CORRECTION: the answer arrived after campd had already declared the
    /// request unanswered, so that `control.failed` was PREMATURE — and
    /// saying so is the difference between a self-repairing fault and a lie.
    ControlResponded,
    /// cp-1 (§2.1): "a control response that never arrives is an evented,
    /// operator-visible fault — never a swallowed timeout."
    /// `{session?, request_id?, verb?, cause, reason}`.
    ///
    /// `cause` is a MACHINE-READABLE DISCRIMINANT, not decoration: rehydration
    /// ROUTES on it. `silence_timeout`/`ceiling_timeout` mean an answer may
    /// still arrive and must still correct; every other cause is TERMINAL.
    /// Prose cannot carry that distinction, and a prose-matching contract is
    /// not one to hand a later phase (invariant 3).
    ControlFailed,
    /// cp-1 (§4.4): a subscriber's peer stopped reading — its socket accepted
    /// ZERO bytes for `SUBSCRIBER_STALL_TIMEOUT` with data buffered — so campd
    /// dropped it. `{session, subscription, buffered_bytes, cap_bytes}`.
    /// Loud, naming the high-water mark: campd never blocks and never
    /// silently discards a stream.
    SubscriberDropped,
    /// compat §7: a pack import was added (audit-only — no state fold).
    ImportAdded,
    /// compat §5.4: a pack/agent key was refused (audit-only — no state fold).
    ImportRefused,
    /// compat §4 rule 1: a formula named a Gas City construct camp does not
    /// implement, so camp refused to load it rather than approximate its
    /// semantics. Audit-only — no state fold. The event NAMES the key
    /// (invariant 3: the ledger tells the whole story), because "the formula
    /// did not load" is not an answer an operator can act on.
    FormulaRefused,
    /// compat §6 (worker contract): a gc/bd shim was handed a verb or flag camp
    /// does not serve. FAIL FAST + EVENTED — a silently-ignored shim call is a
    /// corrupted ledger (§6). Audit-only — no state fold. NAMES the refused verb
    /// (invariant 3). `{binding?, agent?, verb, detail}`.
    ShimRefused,
    /// compat §6.2 (worker contract): a gc worker acknowledged its drain — the
    /// release signal. This is campd's PROMPT-KILL trigger for the already-
    /// released worker (Task 10), NOT gc's `session.drain_acked_with_assigned_work`
    /// (gc's acked-while-still-holding-work anomaly, which has no camp counterpart
    /// because camp truncates gc's continuation loop: one bead per session, no
    /// assigned work remaining at ack). Audit-only — no state fold. `{session}`.
    WorkerDrainAcked,
    /// cp-3 (control-plane §5.3): a worker asked permission to use a tool and
    /// is now BLOCKED awaiting an operator decision. Folds a `permissions` row
    /// (`{session, request_id, tool_name}`).
    PermissionPending,
    /// cp-3 (§5.3/§9): an operator answered a `permission.pending`. Folds the
    /// row to `decided`; a second decision for the same request is REFUSED
    /// (first-answer-wins, a fold invariant). `decided_by` records who.
    PermissionDecided,
    /// cp-3 (§5.3.2): the count of BLOCKED sessions crossed `max_blocked` —
    /// a loud, operator-visible saturation fault. Audit-only — no state fold.
    PermissionSaturated,
}

impl EventType {
    pub const ALL: &'static [EventType] = &[
        EventType::BeadCreated,
        EventType::BeadClaimed,
        EventType::BeadUpdated,
        EventType::BeadClosed,
        EventType::SessionWoke,
        EventType::SessionPid,
        EventType::SessionStopped,
        EventType::SessionCrashed,
        EventType::SessionNudged,
        EventType::CampdStarted,
        EventType::CampdStopped,
        EventType::CampdAutostarted,
        EventType::RigAdded,
        EventType::RunCooked,
        EventType::OrderFired,
        EventType::OrderCompleted,
        EventType::OrderFailed,
        EventType::ConfigChanged,
        EventType::WorkerMilestone,
        EventType::WorktreeKept,
        EventType::BeadWorktreeReaped,
        EventType::DispatchFailed,
        EventType::DispatchLiveTree,
        EventType::DispatchRearmed,
        EventType::CheckPassed,
        EventType::CheckFailed,
        EventType::RunFinalized,
        EventType::AgentStalled,
        EventType::PatrolDegraded,
        EventType::SessionStreamCapped,
        EventType::SessionInterrupted,
        EventType::ControlResponded,
        EventType::ControlFailed,
        EventType::SubscriberDropped,
        EventType::ImportAdded,
        EventType::ImportRefused,
        EventType::FormulaRefused,
        EventType::ShimRefused,
        EventType::WorkerDrainAcked,
        EventType::PermissionPending,
        EventType::PermissionDecided,
        EventType::PermissionSaturated,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventType::BeadCreated => "bead.created",
            EventType::BeadClaimed => "bead.claimed",
            EventType::BeadUpdated => "bead.updated",
            EventType::BeadClosed => "bead.closed",
            EventType::SessionWoke => "session.woke",
            EventType::SessionPid => "session.pid",
            EventType::SessionStopped => "session.stopped",
            EventType::SessionCrashed => "session.crashed",
            EventType::SessionNudged => "session.nudged",
            EventType::CampdStarted => "campd.started",
            EventType::CampdStopped => "campd.stopped",
            EventType::CampdAutostarted => "campd.autostarted",
            EventType::RigAdded => "rig.added",
            EventType::RunCooked => "run.cooked",
            EventType::OrderFired => "order.fired",
            EventType::OrderCompleted => "order.completed",
            EventType::OrderFailed => "order.failed",
            EventType::ConfigChanged => "config.changed",
            EventType::WorkerMilestone => "worker.milestone",
            EventType::WorktreeKept => "worktree.kept",
            EventType::BeadWorktreeReaped => "bead.worktree.reaped",
            EventType::DispatchFailed => "dispatch.failed",
            EventType::DispatchLiveTree => "dispatch.live_tree",
            EventType::DispatchRearmed => "dispatch.rearmed",
            EventType::CheckPassed => "check.passed",
            EventType::CheckFailed => "check.failed",
            EventType::RunFinalized => "run.finalized",
            EventType::AgentStalled => "agent.stalled",
            EventType::PatrolDegraded => "patrol.degraded",
            EventType::SessionStreamCapped => "session.stream_capped",
            EventType::SessionInterrupted => "session.interrupted",
            EventType::ControlResponded => "control.responded",
            EventType::ControlFailed => "control.failed",
            EventType::SubscriberDropped => "subscriber.dropped",
            EventType::ImportAdded => "import.added",
            EventType::ImportRefused => "import.refused",
            EventType::FormulaRefused => "formula.refused",
            EventType::ShimRefused => "shim.refused",
            EventType::WorkerDrainAcked => "worker.drain_acked",
            EventType::PermissionPending => "permission.pending",
            EventType::PermissionDecided => "permission.decided",
            EventType::PermissionSaturated => "permission.saturated",
        }
    }

    pub fn parse(s: &str) -> Result<Self, CoreError> {
        EventType::ALL
            .iter()
            .find(|k| k.as_str() == s)
            .copied()
            .ok_or_else(|| CoreError::UnknownEventType(s.to_owned()))
    }
}

impl Serialize for EventType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        EventType::parse(&s).map_err(D::Error::custom)
    }
}

/// A committed event: one `events` row. Serde field order is the canonical
/// JSON form from spec §7.2 — do not reorder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: Seq,
    pub ts: String,
    #[serde(rename = "type")]
    pub kind: EventType,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rig: Option<String>,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bead: Option<String>,
    pub data: serde_json::Value,
}

/// An event about to be appended: `seq` and `ts` are assigned by the ledger
/// inside the write transaction.
#[derive(Debug, Clone)]
pub struct EventInput {
    pub kind: EventType,
    pub rig: Option<String>,
    pub actor: String,
    pub bead: Option<String>,
    pub data: serde_json::Value,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn spec_example() -> Event {
        Event {
            seq: 412,
            ts: "2026-07-05T21:14:03Z".into(),
            kind: EventType::BeadClosed,
            rig: Some("gascity".into()),
            actor: "session:8f3c2e01".into(),
            bead: Some("gc-142".into()),
            data: serde_json::json!({"outcome": "pass"}),
        }
    }

    #[test]
    fn canonical_json_matches_spec_section_7_2_example_exactly() {
        assert_eq!(
            serde_json::to_string(&spec_example()).unwrap(),
            r#"{"seq":412,"ts":"2026-07-05T21:14:03Z","type":"bead.closed","rig":"gascity","actor":"session:8f3c2e01","bead":"gc-142","data":{"outcome":"pass"}}"#
        );
    }

    #[test]
    fn none_rig_and_bead_are_omitted() {
        let event = Event {
            seq: 1,
            ts: "2026-07-05T21:14:03Z".into(),
            kind: EventType::CampdStarted,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({}),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"seq":1,"ts":"2026-07-05T21:14:03Z","type":"campd.started","actor":"campd","data":{}}"#
        );
    }

    #[test]
    fn json_round_trips() {
        let event = spec_example();
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn every_event_type_round_trips_through_its_name() {
        for kind in EventType::ALL {
            assert_eq!(EventType::parse(kind.as_str()).unwrap(), *kind);
        }
    }

    #[test]
    fn unknown_event_type_is_an_error() {
        assert!(EventType::parse("bogus.event").is_err());
    }

    #[test]
    fn rig_added_round_trips_through_its_name() {
        assert_eq!(EventType::parse("rig.added").unwrap(), EventType::RigAdded);
        assert_eq!(EventType::RigAdded.as_str(), "rig.added");
    }
    #[test]
    fn dispatch_rearmed_round_trips_through_its_name() {
        assert_eq!(EventType::DispatchRearmed.as_str(), "dispatch.rearmed");
        assert_eq!(
            EventType::parse("dispatch.rearmed").unwrap(),
            EventType::DispatchRearmed
        );
        assert!(EventType::ALL.contains(&EventType::DispatchRearmed));
    }

    #[test]
    fn import_events_roundtrip_and_are_camp_specific() {
        assert_eq!(EventType::ImportAdded.as_str(), "import.added");
        assert_eq!(EventType::ImportRefused.as_str(), "import.refused");
        assert_eq!(
            EventType::parse("import.added").unwrap(),
            EventType::ImportAdded
        );
        assert_eq!(
            EventType::parse("import.refused").unwrap(),
            EventType::ImportRefused
        );
        assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&"import.added"));
        assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&"import.refused"));
    }

    #[test]
    fn shim_and_drain_ack_events_roundtrip_and_are_camp_specific() {
        // compat §6 (worker contract). Neither name exists in gc's registry:
        // `worker.drain_acked` is camp's release trigger (gc's only `worker.*`
        // is `worker.operation`), `shim.refused` follows import/formula.refused
        // (gc has no `shim.*`). Invariant 7 — additive, never a redefinition.
        for (variant, name) in [
            (EventType::ShimRefused, "shim.refused"),
            (EventType::WorkerDrainAcked, "worker.drain_acked"),
        ] {
            assert_eq!(variant.as_str(), name);
            assert_eq!(EventType::parse(name).unwrap(), variant);
            assert!(EventType::ALL.contains(&variant));
            assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&name));
            assert!(!crate::vocab::GC_MIRRORED_EVENTS.contains(&name));
        }
    }

    #[test]
    fn permission_events_roundtrip_and_are_camp_specific() {
        // cp-3 (control-plane §5.3/§9). gc's registry carries no `permission.*`
        // event, so all three are additive (invariant 7).
        for (variant, name) in [
            (EventType::PermissionPending, "permission.pending"),
            (EventType::PermissionDecided, "permission.decided"),
            (EventType::PermissionSaturated, "permission.saturated"),
        ] {
            assert_eq!(variant.as_str(), name);
            assert_eq!(EventType::parse(name).unwrap(), variant);
            assert!(EventType::ALL.contains(&variant));
            assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&name));
            assert!(!crate::vocab::GC_MIRRORED_EVENTS.contains(&name));
        }
    }

    #[test]
    fn formula_refused_roundtrips_and_is_camp_specific() {
        // compat §4 rule 1. gc's 71-event vocabulary has NO `formula.*` event,
        // so this is additive, never a redefinition (invariant 7).
        assert_eq!(EventType::FormulaRefused.as_str(), "formula.refused");
        assert_eq!(
            EventType::parse("formula.refused").unwrap(),
            EventType::FormulaRefused
        );
        assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&"formula.refused"));
        assert!(!crate::vocab::GC_MIRRORED_EVENTS.contains(&"formula.refused"));
        assert!(!crate::vocab::GC_MIRRORED_EVENTS.contains(&"import.added"));
    }
}

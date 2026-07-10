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
    SessionStopped,
    SessionCrashed,
    SessionNudged,
    CampdStarted,
    CampdStopped,
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
    CheckPassed,
    CheckFailed,
    RunFinalized,
    AgentStalled,
    PatrolDegraded,
}

impl EventType {
    pub const ALL: &'static [EventType] = &[
        EventType::BeadCreated,
        EventType::BeadClaimed,
        EventType::BeadUpdated,
        EventType::BeadClosed,
        EventType::SessionWoke,
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
        EventType::CheckPassed,
        EventType::CheckFailed,
        EventType::RunFinalized,
        EventType::AgentStalled,
        EventType::PatrolDegraded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventType::BeadCreated => "bead.created",
            EventType::BeadClaimed => "bead.claimed",
            EventType::BeadUpdated => "bead.updated",
            EventType::BeadClosed => "bead.closed",
            EventType::SessionWoke => "session.woke",
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
            EventType::CheckPassed => "check.passed",
            EventType::CheckFailed => "check.failed",
            EventType::RunFinalized => "run.finalized",
            EventType::AgentStalled => "agent.stalled",
            EventType::PatrolDegraded => "patrol.degraded",
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
}

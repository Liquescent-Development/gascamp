//! Property test for the refold equivalence (spec §16): for any accepted
//! event sequence, incremental fold state ≡ refolded state, and two ledgers
//! fed the same sequence are byte-identical.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Create {
        id: u8,
        with_needs: bool,
    },
    Claim {
        id: u8,
        session: u8,
    },
    Update {
        id: u8,
    },
    Close {
        id: u8,
        pass: bool,
    },
    Woke {
        session: u8,
    },
    Stop {
        session: u8,
    },
    Crash {
        session: u8,
    },
    /// compat §6.1 — plain metadata on a bead.
    SetMeta {
        id: u8,
        key: u8,
    },
    /// The exclusive drain reservation. `drain` is drawn from a SMALL range on
    /// purpose: two different drains WILL contend for one member, so the
    /// generator really produces the CAS conflict the fold must reject. A
    /// rejected append must append NOTHING, and the replay must land on an
    /// identical state — that is the property, and without a conflicting op it
    /// is never exercised.
    Reserve {
        id: u8,
        drain: u8,
    },
    /// Release — an UNSET (null), including of a key that is not held.
    Release {
        id: u8,
    },
}

fn bead_id(i: u8) -> String {
    format!("bead-{i}")
}

fn session_name(i: u8) -> String {
    format!("camp/dev/{i}")
}

fn to_input(op: &Op) -> EventInput {
    let (kind, bead, data) = match op {
        Op::Create { id, with_needs } => {
            let mut data = serde_json::json!({
                "title": format!("task {id}"),
                "description": format!("body of task {id}"),
                "labels": ["prop"],
            });
            if *with_needs {
                data["needs"] = serde_json::json!([bead_id(id.wrapping_add(1) % 8)]);
            }
            (EventType::BeadCreated, Some(bead_id(*id)), data)
        }
        Op::Claim { id, session } => (
            EventType::BeadClaimed,
            Some(bead_id(*id)),
            serde_json::json!({"session": session_name(*session)}),
        ),
        Op::Update { id } => (
            EventType::BeadUpdated,
            Some(bead_id(*id)),
            serde_json::json!({"title": format!("task {id} (updated)")}),
        ),
        Op::Close { id, pass } => (
            EventType::BeadClosed,
            Some(bead_id(*id)),
            serde_json::json!({
                "outcome": if *pass { "pass" } else { "fail" },
                "reason": format!("closed task {id}"),
            }),
        ),
        Op::Woke { session } => (
            EventType::SessionWoke,
            None,
            serde_json::json!({"name": session_name(*session), "agent": "dev", "rig": "gc"}),
        ),
        Op::Stop { session } => (
            EventType::SessionStopped,
            None,
            serde_json::json!({"name": session_name(*session)}),
        ),
        Op::Crash { session } => (
            EventType::SessionCrashed,
            None,
            serde_json::json!({"name": session_name(*session)}),
        ),
        Op::SetMeta { id, key } => (
            EventType::BeadUpdated,
            Some(bead_id(*id)),
            serde_json::json!({"metadata": {format!("gc.k{key}"): format!("v{key}")}}),
        ),
        Op::Reserve { id, drain } => (
            EventType::BeadUpdated,
            Some(bead_id(*id)),
            serde_json::json!({
                "metadata": {
                    camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION: format!("drain-{drain}"),
                }
            }),
        ),
        Op::Release { id } => (
            EventType::BeadUpdated,
            Some(bead_id(*id)),
            serde_json::json!({
                "metadata": { camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION: serde_json::Value::Null }
            }),
        ),
    };
    EventInput {
        kind,
        rig: Some("gc".into()),
        actor: "prop".into(),
        bead,
        data,
    }
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..8, any::<bool>()).prop_map(|(id, with_needs)| Op::Create { id, with_needs }),
        (0u8..8, 0u8..4).prop_map(|(id, session)| Op::Claim { id, session }),
        (0u8..8).prop_map(|id| Op::Update { id }),
        (0u8..8, any::<bool>()).prop_map(|(id, pass)| Op::Close { id, pass }),
        (0u8..4).prop_map(|session| Op::Woke { session }),
        (0u8..4).prop_map(|session| Op::Stop { session }),
        (0u8..4).prop_map(|session| Op::Crash { session }),
        (0u8..8, 0u8..3).prop_map(|(id, key)| Op::SetMeta { id, key }),
        // 2 drains over 8 beads: conflicts are frequent, not incidental.
        (0u8..8, 0u8..2).prop_map(|(id, drain)| Op::Reserve { id, drain }),
        (0u8..8).prop_map(|id| Op::Release { id }),
    ]
}

/// Apply candidate ops, skipping the invalid ones — validity rules are unit-
/// tested elsewhere; the property under test is fold determinism. Returns how
/// many were accepted.
fn feed(ledger: &mut Ledger, ops: &[Op]) -> u64 {
    let mut accepted = 0;
    for op in ops {
        if ledger.append(to_input(op)).is_ok() {
            accepted += 1;
        }
    }
    accepted
}

const DUMPS: &[(&str, &str)] = &[
    (
        "beads",
        "id, rig, type, title, description, status, assignee, claimed_by, outcome, close_reason, labels, run_id, step_id, created_ts, updated_ts, closed_ts",
    ),
    ("bead_meta", "bead_id, key, value"),
    ("deps", "bead_id, needs_id"),
    (
        "sessions",
        "name, agent, rig, claude_session_id, transcript_path, pid, status, bead, spawned_ts, ended_ts",
    ),
    ("search", "bead_id, kind, content"),
    ("counters", "prefix, high"),
    ("events", "seq, ts, type, rig, actor, bead, data"),
];

fn dump_state(db: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut out = Vec::new();
    for (table, cols) in DUMPS {
        let quoted: Vec<String> = cols.split(", ").map(|c| format!("quote({c})")).collect();
        let sql = format!(
            "SELECT {} FROM {table} ORDER BY {cols}",
            quoted.join(" || '|' || ")
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| format!("{table}: {}", r.unwrap()))
            .collect();
        out.extend(rows);
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn refold_matches_incremental_fold(ops in prop::collection::vec(op_strategy(), 0..80)) {
        let dir = tempfile::tempdir().unwrap();
        let db_a = dir.path().join("a.db");
        let db_b = dir.path().join("b.db");

        let mut ledger_a =
            Ledger::open_with_clock(&db_a, Box::new(FixedClock::new("2026-07-05T21:14:03Z")))
                .unwrap();
        let mut ledger_b =
            Ledger::open_with_clock(&db_b, Box::new(FixedClock::new("2026-07-05T21:14:03Z")))
                .unwrap();

        let accepted_a = feed(&mut ledger_a, &ops);
        let accepted_b = feed(&mut ledger_b, &ops);
        prop_assert_eq!(accepted_a, accepted_b, "acceptance must be deterministic");

        // Property 1: state ≡ fold(event log).
        let report = ledger_a.refold_check().unwrap();
        prop_assert_eq!(report.events_replayed, accepted_a);
        prop_assert!(report.drift.is_empty(), "drift: {:?}", report.drift);

        // Property 3: id allocation is folded state — refold_repair preserves
        // the next id for the prefix ("bead") every Create op used.
        let before = ledger_a.next_bead_id("bead").unwrap();
        ledger_a.refold_repair().unwrap();
        let after = ledger_a.next_bead_id("bead").unwrap();
        prop_assert_eq!(before, after);

        // Property 2: two ledgers fed the same sequence are identical.
        drop(ledger_a);
        drop(ledger_b);
        prop_assert_eq!(dump_state(&db_a), dump_state(&db_b));
    }
}

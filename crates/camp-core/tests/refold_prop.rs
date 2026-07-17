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
        /// Index into `CAMP_FINAL_DISPOSITIONS`, or `None` for a close that
        /// carries no disposition. A FAILING close may carry one (the fold
        /// requires `outcome = "fail"`), which is what actually populates
        /// `beads.final_disposition` — deriving the compared columns makes the
        /// property STRUCTURALLY cover a column, but only an op that writes a
        /// non-NULL value makes the coverage real. NULL == NULL proves nothing.
        disposition: Option<usize>,
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
    /// cp-3 (§5.3): a worker's permission request. `req` is drawn from a SMALL
    /// range so two pendings WILL contend for one request_id — the PK conflict
    /// the fold must reject (append nothing). A pending against a non-live
    /// session is also refused; both exercise the rejection-appends-nothing arm.
    PermissionPending {
        session: u8,
        req: u8,
    },
    /// cp-3 (§9): a decision on a request. A decide against an unknown or
    /// already-decided request_id changes zero rows → refused (first-answer-
    /// wins), and the harness tolerates the `Err` exactly like other rejections.
    PermissionDecide {
        req: u8,
        deny: bool,
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
        Op::Close {
            id,
            pass,
            disposition,
        } => {
            let mut data = serde_json::json!({
                "outcome": if *pass { "pass" } else { "fail" },
                "reason": format!("closed task {id}"),
            });
            // Only a failing close may carry one; a passing close with a
            // disposition is REFUSED by the fold, and `append` rolls back, so
            // emitting it there would just shrink the accepted-op count.
            if !*pass && let Some(i) = disposition {
                data["final_disposition"] =
                    serde_json::json!(camp_core::vocab::CAMP_FINAL_DISPOSITIONS[*i]);
            }
            (EventType::BeadClosed, Some(bead_id(*id)), data)
        }
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
        Op::PermissionPending { session, req } => (
            EventType::PermissionPending,
            None,
            serde_json::json!({
                "session": session_name(*session),
                "request_id": format!("req-{req}"),
                "tool_name": "Bash",
            }),
        ),
        Op::PermissionDecide { req, deny } => (
            EventType::PermissionDecided,
            None,
            if *deny {
                serde_json::json!({
                    "session": session_name(0),
                    "request_id": format!("req-{req}"),
                    "decision": "deny",
                    "decided_by": "op",
                    "reason": "denied by prop",
                })
            } else {
                serde_json::json!({
                    "session": session_name(0),
                    "request_id": format!("req-{req}"),
                    "decision": "allow",
                    "decided_by": "op",
                })
            },
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
        (
            0u8..8,
            any::<bool>(),
            proptest::option::of(0..camp_core::vocab::CAMP_FINAL_DISPOSITIONS.len()),
        )
            .prop_map(|(id, pass, disposition)| Op::Close {
                id,
                pass,
                disposition,
            }),
        (0u8..4).prop_map(|session| Op::Woke { session }),
        (0u8..4).prop_map(|session| Op::Stop { session }),
        (0u8..4).prop_map(|session| Op::Crash { session }),
        (0u8..8, 0u8..3).prop_map(|(id, key)| Op::SetMeta { id, key }),
        // 2 drains over 8 beads: conflicts are frequent, not incidental.
        (0u8..8, 0u8..2).prop_map(|(id, drain)| Op::Reserve { id, drain }),
        (0u8..8).prop_map(|id| Op::Release { id }),
        // req over 0..8: two pendings contend for a request_id (PK conflict),
        // and a decide often lands on an already-decided/unknown id (first-
        // answer-wins refusal) — both rejection arms are exercised.
        (0u8..4, 0u8..8).prop_map(|(session, req)| Op::PermissionPending { session, req }),
        (0u8..8, any::<bool>()).prop_map(|(req, deny)| Op::PermissionDecide { req, deny }),
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

/// The tables this property compares — columns are DERIVED from the schema
/// (`pragma_table_info`), never hand-listed.
///
/// This list used to carry its own copy of every column name, which made it a
/// THIRD hand-maintained transcript of the `beads` columns (after the DDL and
/// `refold::STATE_TABLES`) — and it had already rotted: `work_outcome`,
/// `work_commit`, `work_branch` and `dispatch_failure` were all missing, so the
/// property silently stopped covering them, and #122's `final_disposition`
/// would have been the fifth. A property that quietly ignores new columns is
/// how a fold bug reaches an operator. Deriving the columns means every column
/// a state table ever grows is compared the day it is added, with nothing to
/// remember.
///
/// Deriving buys STRUCTURAL coverage only: a column no op ever populates is
/// compared NULL to NULL, which proves nothing about the fold. `Op::Close`
/// therefore emits a real `final_disposition`. The work axis
/// (`work_outcome`/`work_commit`/`work_branch`) and `dispatch_failure` are
/// still NULL-to-NULL here — no op writes them — so their real coverage lives
/// in the daemon suite, not in this property.
const DUMPS: &[&str] = &[
    "beads",
    "bead_meta",
    "deps",
    "sessions",
    "search",
    "counters",
    "permissions",
    "events",
];

fn dump_state(db: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut out = Vec::new();
    for table in DUMPS {
        let mut info = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .unwrap();
        let names: Vec<String> = info
            .query_map([table], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|c| c.unwrap())
            .collect();
        assert!(
            !names.is_empty(),
            "{table}: pragma_table_info returned nothing — is the table named right?"
        );
        let cols = names.join(", ");
        let quoted: Vec<String> = names.iter().map(|c| format!("quote({c})")).collect();
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

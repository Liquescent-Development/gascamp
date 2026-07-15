//! compat §6.1/§6.2 — `gc hook --claim --json`: the worker's ONLY discovery
//! source. It claims the one bead campd pinned to this session (`$CAMP_BEAD`),
//! flips it open → in_progress, and prints the work JSON; on a closed/already-
//! gone bead it prints the drain JSON. The route in the JSON comes from the
//! BEAD ROW (via `claim_projection`), never from `$GC_AGENT` env (round-1 B1).
//!
//! The JSON shape (fields `action`/`bead_id`/`assignee`/`route`, exit work=0/
//! drain=1) is confirmed against Task 1's measurement in
//! `ci/gc-compat/fixtures/gc-role-worker.observed.json` (`hook_json_fields`,
//! `exit_contract`). The fragment keys on `action` before consulting the exit
//! code, so the drain exit is not itself load-bearing — but we emit 1 to mirror
//! gc.

use anyhow::{Context, Result};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use serde_json::Value;

use super::project::claim_projection;
use super::{ShimExit, refuse};
use crate::campdir::CampDir;

/// `gc hook <flags>` entry point. Reads `$CAMP_BEAD`/`$CAMP_SESSION` (campd set
/// them, Task 9), runs the claim decision, and prints the JSON when `--json`.
pub fn run(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    let mut want_claim = false;
    let mut want_json = false;
    let mut drain_ack = false;
    for flag in args {
        match flag.as_str() {
            "--claim" => want_claim = true,
            "--json" => want_json = true,
            "--drain-ack" => drain_ack = true,
            other => {
                return refuse(camp, "hook", &format!("unknown hook flag {other:?}"));
            }
        }
    }
    if !want_claim {
        return refuse(camp, "hook", "only `hook --claim` is served");
    }

    let bead = std::env::var("CAMP_BEAD").context("CAMP_BEAD not set in worker env")?;
    let session = std::env::var("CAMP_SESSION").context("CAMP_SESSION not set in worker env")?;
    let mut ledger = Ledger::open(&camp.db_path())?;
    let (json, exit) = claim_hook(camp, &mut ledger, &bead, &session, drain_ack)?;
    if want_json {
        println!("{}", serde_json::to_string(&json)?);
    }
    Ok(exit)
}

/// The claim decision, split out so it is testable without env or stdout.
/// `open` → claim + work; `in_progress` by this session → idempotent re-hook
/// (work again, no re-claim); anything else (closed / another session / gone)
/// → drain. **The route in the work JSON is read from the BEAD via
/// `claim_projection` — there is no env read here, so a route re-derivation
/// from `GC_AGENT` is structurally impossible.**
pub(crate) fn claim_hook(
    camp: &CampDir,
    ledger: &mut Ledger,
    bead: &str,
    session: &str,
    drain_ack: bool,
) -> Result<(Value, ShimExit)> {
    let status = ledger.bead_row(bead)?.map(|r| (r.status, r.claimed_by));
    match status.as_ref().map(|(s, c)| (s.as_str(), c.as_deref())) {
        Some(("open", _)) => {
            let seq = ledger.append(EventInput {
                kind: EventType::BeadClaimed,
                rig: None,
                actor: "gc-shim".into(),
                bead: Some(bead.to_owned()),
                data: serde_json::json!({
                    "session": session,
                    "work_branch": format!("camp/{bead}"),
                }),
            })?;
            crate::daemon::socket::poke_best_effort(camp, seq);
            Ok((work_json(bead, ledger)?, ShimExit(0)))
        }
        // Idempotent re-hook: same session already holds it.
        Some(("in_progress", Some(who))) if who == session => {
            Ok((work_json(bead, ledger)?, ShimExit(0)))
        }
        _ => Ok((
            drain_json("no routed work"),
            ShimExit(if drain_ack { 0 } else { 1 }),
        )),
    }
}

fn work_json(bead: &str, ledger: &Ledger) -> Result<Value> {
    let proj = claim_projection(ledger, bead)?;
    Ok(serde_json::json!({
        "schema_version": 1,
        "ok": true,
        "action": "work",
        "reason": Value::Null,
        "bead_id": bead,
        "assignee": proj.assignee, // the session (claimed_by)
        "route": proj.route,       // the BEAD's gc.routed_to — never env
    }))
}

fn drain_json(reason: &str) -> Value {
    serde_json::json!({
        "schema_version": 1,
        "ok": true,
        "action": "drain",
        "reason": reason,
        "assignee": Value::Null,
        "route": Value::Null,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::event::EventType;

    fn temp_camp() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        drop(Ledger::open(&camp.db_path()).unwrap());
        (dir, camp)
    }

    /// Seed an OPEN bead whose `assignee` column (the route → `gc.routed_to`)
    /// is `route`. BeadCreated's `assignee` field sets exactly that column.
    fn seed_open_bead(camp: &CampDir, id: &str, route: &str) {
        let mut l = Ledger::open(&camp.db_path()).unwrap();
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some(id.into()),
            data: serde_json::json!({ "title": "work", "assignee": route }),
        })
        .unwrap();
    }

    fn close_bead(camp: &CampDir, id: &str) {
        let mut l = Ledger::open(&camp.db_path()).unwrap();
        l.append(EventInput {
            kind: EventType::BeadClosed,
            rig: None,
            actor: "cli".into(),
            bead: Some(id.into()),
            data: serde_json::json!({ "outcome": "pass" }),
        })
        .unwrap();
    }

    #[test]
    fn hook_claim_returns_work_and_projects_the_bead_route_not_the_env() {
        let (_d, camp) = temp_camp();
        seed_open_bead(&camp, "gc-2", "gc.publisher"); // the COOKED route
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();

        let (v, exit) = claim_hook(&camp, &mut ledger, "gc-2", "t/gc.publisher/1", false).unwrap();
        assert_eq!(exit, ShimExit(0));
        assert_eq!(v["action"], "work");
        assert_eq!(v["bead_id"], "gc-2");
        assert_eq!(v["assignee"], "t/gc.publisher/1"); // the session (claimed_by)
        assert_eq!(v["route"], "gc.publisher"); // the BEAD's route — claim_hook reads no env

        let row = ledger.bead_row("gc-2").unwrap().unwrap();
        assert_eq!(row.status, "in_progress");
        assert_eq!(row.claimed_by.as_deref(), Some("t/gc.publisher/1"));
        assert_eq!(row.assignee.as_deref(), Some("gc.publisher")); // route intact
        let meta = ledger.bead_metadata("gc-2").unwrap();
        assert_eq!(meta.get("gc.work_branch").map(String::as_str), Some("camp/gc-2"));
    }

    #[test]
    fn hook_claim_on_a_closed_bead_returns_drain_exit_1() {
        let (_d, camp) = temp_camp();
        seed_open_bead(&camp, "gc-2", "gc.publisher");
        close_bead(&camp, "gc-2");
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();

        let (v, exit) = claim_hook(&camp, &mut ledger, "gc-2", "t/gc.publisher/1", false).unwrap();
        assert_eq!(exit, ShimExit(1));
        assert_eq!(v["action"], "drain");
        assert_eq!(v["assignee"], Value::Null);
        assert_eq!(v["route"], Value::Null);
    }

    #[test]
    fn hook_claim_drain_with_drain_ack_flag_exits_0() {
        let (_d, camp) = temp_camp();
        seed_open_bead(&camp, "gc-2", "gc.publisher");
        close_bead(&camp, "gc-2");
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();

        let (v, exit) = claim_hook(&camp, &mut ledger, "gc-2", "t/gc.publisher/1", true).unwrap();
        assert_eq!(exit, ShimExit(0));
        assert_eq!(v["action"], "drain");
    }

    #[test]
    fn hook_reclaim_by_the_same_session_is_idempotent() {
        let (_d, camp) = temp_camp();
        seed_open_bead(&camp, "gc-2", "gc.publisher");
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();
        claim_hook(&camp, &mut ledger, "gc-2", "t/gc.publisher/1", false).unwrap();
        // second hook while still in_progress by us: work again, no error.
        let (v, exit) = claim_hook(&camp, &mut ledger, "gc-2", "t/gc.publisher/1", false).unwrap();
        assert_eq!(exit, ShimExit(0));
        assert_eq!(v["action"], "work");
        assert_eq!(v["route"], "gc.publisher");
    }
}

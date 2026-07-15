//! compat §6.2 — `gc runtime drain-ack` (the release signal) and
//! `gc convoy status --json` (a worker-facing read).
//!
//! `runtime drain-ack` appends `worker.drain_acked{session}` and pokes campd:
//! that event is campd's PROMPT-KILL trigger for the already-released worker
//! (Task 10). No new poll — the poke rides the existing socket (invariant 1).
//! `convoy status --json` reports the session's claimed bead; the real
//! gc-role-worker fragment never calls it (Task 1), so its shape is camp's own
//! minimal projection, not a measured contract.

use anyhow::{Context, Result};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::readiness::ListFilter;
use serde_json::Value;

use super::{ShimExit, refuse};
use crate::campdir::CampDir;

/// `gc runtime <subverb> …`.
pub fn run_runtime(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    match args.first().map(String::as_str) {
        Some("drain-ack") => {
            let session = std::env::var("CAMP_SESSION").context("CAMP_SESSION not set in worker env")?;
            let mut ledger = Ledger::open(&camp.db_path())?;
            drain_ack(camp, &mut ledger, &session)
        }
        other => refuse(
            camp,
            "runtime",
            &format!("runtime {:?} is not served", other.unwrap_or("")),
        ),
    }
}

/// Append the release signal. Testable without env/stdout.
pub(crate) fn drain_ack(camp: &CampDir, ledger: &mut Ledger, session: &str) -> Result<ShimExit> {
    let seq = ledger.append(EventInput {
        kind: EventType::WorkerDrainAcked,
        rig: None,
        actor: "gc-shim".into(),
        bead: None,
        data: serde_json::json!({ "session": session }),
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    Ok(ShimExit(0))
}

/// `gc convoy <subverb> …`.
pub fn run_convoy(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    match args.first().map(String::as_str) {
        Some("status") => {
            let mut want_json = false;
            for a in &args[1..] {
                if a == "--json" {
                    want_json = true;
                } else if a.starts_with('-') {
                    return refuse(camp, "convoy", &format!("unknown convoy status flag {a:?}"));
                }
            }
            let session =
                std::env::var("CAMP_SESSION").context("CAMP_SESSION not set in worker env")?;
            let ledger = Ledger::open(&camp.db_path())?;
            let v = convoy_status(&ledger, &session)?;
            if want_json {
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!("{v}");
            }
            Ok(ShimExit(0))
        }
        other => refuse(
            camp,
            "convoy",
            &format!("convoy {:?} is not served", other.unwrap_or("")),
        ),
    }
}

/// The session's claimed bead(s), as a small JSON object. Testable.
pub(crate) fn convoy_status(ledger: &Ledger, session: &str) -> Result<Value> {
    let beads = ledger.list_beads(&ListFilter {
        rig: None,
        mine: Some(session),
    })?;
    let items: Vec<Value> = beads
        .iter()
        .map(|b| serde_json::json!({ "bead_id": b.id, "status": b.status }))
        .collect();
    Ok(serde_json::json!({ "session": session, "beads": items }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cmd::shim::hook::claim_hook;

    fn temp_camp() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        drop(Ledger::open(&camp.db_path()).unwrap());
        (dir, camp)
    }

    fn seed_and_claim(camp: &CampDir, id: &str, route: &str, session: &str) {
        let mut l = Ledger::open(&camp.db_path()).unwrap();
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some(id.into()),
            data: serde_json::json!({ "title": "work", "assignee": route }),
        })
        .unwrap();
        claim_hook(camp, &mut l, id, session, false).unwrap();
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn runtime_drain_ack_appends_worker_drain_acked_and_exits_0() {
        let (_d, camp) = temp_camp();
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();
        let code = drain_ack(&camp, &mut ledger, "t/gc.publisher/1").unwrap();
        assert_eq!(code, ShimExit(0));
        assert!(
            ledger
                .events_of_type(EventType::WorkerDrainAcked)
                .unwrap()
                .iter()
                .any(|e| e.data["session"] == "t/gc.publisher/1")
        );
    }

    #[test]
    fn convoy_status_json_reports_the_sessions_bead() {
        let (_d, camp) = temp_camp();
        seed_and_claim(&camp, "gc-2", "gc.publisher", "t/gc.publisher/1");
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let v = convoy_status(&ledger, "t/gc.publisher/1").unwrap();
        assert_eq!(v["session"], "t/gc.publisher/1");
        assert_eq!(v["beads"][0]["bead_id"], "gc-2");
        assert_eq!(v["beads"][0]["status"], "in_progress");
    }

    #[test]
    fn runtime_unknown_subcommand_is_refused() {
        let (_d, camp) = temp_camp();
        let err = run_runtime(&camp, &s(&["foo"])).unwrap_err();
        assert!(format!("{err:#}").contains("runtime") || format!("{err:#}").contains("foo"));
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert!(
            ledger
                .events_of_type(EventType::ShimRefused)
                .unwrap()
                .iter()
                .any(|e| e.data["verb"] == "runtime")
        );
    }

    #[test]
    fn convoy_unknown_subcommand_is_refused() {
        let (_d, camp) = temp_camp();
        let err = run_convoy(&camp, &s(&["teardown"])).unwrap_err();
        assert!(format!("{err:#}").contains("convoy") || format!("{err:#}").contains("teardown"));
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert!(
            ledger
                .events_of_type(EventType::ShimRefused)
                .unwrap()
                .iter()
                .any(|e| e.data["verb"] == "convoy")
        );
    }
}

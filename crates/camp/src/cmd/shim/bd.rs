//! compat §6.1 — the `bd` data plane the worker uses after a claim.
//!
//! Confirmed against Task 1's measurement
//! (`ci/gc-compat/fixtures/gc-role-worker.observed.json`): the real
//! gc-role-worker fragment uses `bd show <id> [--json]`, `bd update <id>
//! --set-metadata k=v`, and a BARE `bd close <id> [--reason r]` — it sets
//! `gc.outcome` via `bd update` and then closes, NOT `bd close --status`
//! (that is the spec §6.1 EXCERPT's shape, which the recording corrects). So
//! `bd close` reads the just-set `gc.outcome` and maps it to camp's close
//! outcome. Every non-recorded verb/flag is refused loudly (§6).
//!
//! `bd show --json`'s top-level `assignee` is camp's `claimed_by` (the
//! SESSION) — the camp↔gc column inversion — and its `metadata` map comes
//! WHOLESALE from `readiness::bead_metadata` (the one formatter), so
//! `gc.routed_to`/`gc.work_branch`/`gc.root_bead_id`/… are never re-derived in
//! shim code (B5).

use anyhow::{Context, Result, anyhow, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use serde_json::Value;

use super::{ShimExit, refuse};
use crate::campdir::CampDir;

/// `camp bd-shim <verb> …` dispatch.
pub fn run(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    match args.first().map(String::as_str) {
        Some("show") => show(camp, &args[1..]),
        Some("update") => update(camp, &args[1..]),
        Some("close") => close(camp, &args[1..]),
        _ => refuse(
            camp,
            args.first().map(String::as_str).unwrap_or(""),
            "bd shim does not serve this verb",
        ),
    }
}

/// The first non-flag argument (the bead id). gc's role-worker always passes
/// exactly one explicit id.
fn bead_id<'a>(args: &'a [String], camp: &CampDir, verb: &str) -> Result<&'a str> {
    match args.iter().find(|a| !a.starts_with('-')) {
        Some(id) => Ok(id),
        None => {
            refuse(camp, verb, "no bead id")?;
            unreachable!("refuse returns Err")
        }
    }
}

fn show(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    let mut want_json = false;
    for a in args {
        if a == "--json" {
            want_json = true;
        } else if a.starts_with('-') {
            return refuse(camp, "show", &format!("unknown bd show flag {a:?}"));
        }
    }
    let id = bead_id(args, camp, "show")?.to_owned();
    let ledger = Ledger::open(&camp.db_path())?;
    if want_json {
        println!("{}", serde_json::to_string(&show_json(&ledger, &id)?)?);
    } else {
        let row = ledger
            .bead_row(&id)?
            .ok_or_else(|| anyhow!("no such bead {id}"))?;
        println!(
            "{} {} {} {}",
            row.id,
            row.status,
            row.claimed_by.as_deref().unwrap_or("-"),
            row.title
        );
    }
    Ok(ShimExit(0))
}

/// The `bd show --json` projection. Top-level `assignee` = `claimed_by` (the
/// session); `metadata` = `readiness::bead_metadata` (the one formatter).
pub(crate) fn show_json(ledger: &Ledger, id: &str) -> Result<Value> {
    let row = ledger
        .bead_row(id)?
        .ok_or_else(|| anyhow!("no such bead {id}"))?;
    let meta = ledger.bead_metadata(id)?;
    Ok(serde_json::json!({
        "id": row.id,
        "status": row.status,
        "title": row.title,
        "assignee": row.claimed_by, // gc's assignee = camp's claimed_by (the session)
        "metadata": meta,           // gc.routed_to / gc.work_branch / … from the one formatter
    }))
}

fn update(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    let id = bead_id(args, camp, "update")?.to_owned();
    let mut metadata = serde_json::Map::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--set-metadata" => {
                let kv = it
                    .next()
                    .ok_or_else(|| anyhow!("--set-metadata needs a key=value argument"))?;
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| anyhow!("--set-metadata {kv:?} is not key=value"))?;
                metadata.insert(k.to_owned(), Value::String(v.to_owned()));
            }
            other if other == id => {} // the positional bead id
            other if other.starts_with('-') => {
                return refuse(camp, "update", &format!("unknown bd update flag {other:?}"));
            }
            _ => {}
        }
    }
    if metadata.is_empty() {
        bail!("bd update {id}: nothing to set");
    }
    let mut ledger = Ledger::open(&camp.db_path())?;
    let seq = ledger.append(EventInput {
        kind: EventType::BeadUpdated,
        rig: None,
        actor: "gc-shim".into(),
        bead: Some(id),
        data: serde_json::json!({ "metadata": metadata }),
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    Ok(ShimExit(0))
}

fn close(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    let id = bead_id(args, camp, "close")?.to_owned();
    let mut reason: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--reason" => {
                reason = Some(
                    it.next()
                        .ok_or_else(|| anyhow!("--reason needs an argument"))?
                        .clone(),
                );
            }
            other if other == id => {}
            other if other.starts_with('-') => {
                // The measured fragment uses a BARE `bd close` (+ optional
                // --reason); a --status/other flag is not on any recorded
                // branch — refuse loudly (§6), never silently ignore.
                return refuse(camp, "close", &format!("unknown bd close flag {other:?}"));
            }
            _ => {}
        }
    }
    // The worker set gc.outcome via `bd update` before closing; map it to
    // camp's control outcome. Absent → fail fast (a close with no recorded
    // outcome is not a contract the fragment expresses).
    let outcome = {
        let ledger = Ledger::open(&camp.db_path())?;
        ledger
            .bead_metadata(&id)?
            .get("gc.outcome")
            .cloned()
            .with_context(|| {
                format!(
                    "bd close {id}: gc.outcome not set — the worker must \
                     `bd update {id} --set-metadata gc.outcome=…` before closing"
                )
            })?
    };
    // Reuse camp's own close path (its shipped-commit gate, its vocabulary
    // validation) — the fragment never ships, so no work_outcome/commit.
    crate::cmd::close::run(camp, id, outcome, reason, false, None, None, None, None)?;
    Ok(ShimExit(0))
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
    fn bd_show_json_assignee_is_the_session_and_metadata_comes_from_bead_metadata() {
        let (_d, camp) = temp_camp();
        seed_and_claim(&camp, "gc-2", "gc.publisher", "t/gc.publisher/1");
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let v = show_json(&ledger, "gc-2").unwrap();
        assert_eq!(v["assignee"], "t/gc.publisher/1"); // gc's assignee = the session
        assert_eq!(v["metadata"]["gc.routed_to"], "gc.publisher"); // from bead_metadata
        assert_eq!(v["metadata"]["gc.work_branch"], "camp/gc-2");
        assert_eq!(v["id"], "gc-2");
        assert_eq!(v["status"], "in_progress");
    }

    #[test]
    fn bd_update_set_metadata_writes_through_bead_updated() {
        let (_d, camp) = temp_camp();
        seed_and_claim(&camp, "gc-2", "gc.publisher", "t/gc.publisher/1");
        run(
            &camp,
            &s(&["update", "gc-2", "--set-metadata", "gc.custom=x"]),
        )
        .unwrap();
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert_eq!(
            ledger.bead_metadata("gc-2").unwrap().get("gc.custom").map(String::as_str),
            Some("x")
        );
    }

    #[test]
    fn bd_close_maps_gc_outcome_to_camps_close_vocabulary() {
        let (_d, camp) = temp_camp();
        seed_and_claim(&camp, "gc-2", "gc.publisher", "t/gc.publisher/1");
        // the worker's real sequence: set gc.outcome, then BARE close.
        run(
            &camp,
            &s(&["update", "gc-2", "--set-metadata", "gc.outcome=pass"]),
        )
        .unwrap();
        run(&camp, &s(&["close", "gc-2"])).unwrap();
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let row = ledger.bead_row("gc-2").unwrap().unwrap();
        assert_eq!(row.status, "closed");
        assert_eq!(row.outcome.as_deref(), Some("pass"));
    }

    #[test]
    fn bd_close_fail_with_reason_maps_to_outcome_fail() {
        let (_d, camp) = temp_camp();
        seed_and_claim(&camp, "gc-2", "gc.publisher", "t/gc.publisher/1");
        run(
            &camp,
            &s(&["update", "gc-2", "--set-metadata", "gc.outcome=fail"]),
        )
        .unwrap();
        run(&camp, &s(&["close", "gc-2", "--reason", "work failed"])).unwrap();
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let row = ledger.bead_row("gc-2").unwrap().unwrap();
        assert_eq!(row.status, "closed");
        assert_eq!(row.outcome.as_deref(), Some("fail"));
    }

    #[test]
    fn bd_unknown_subcommand_is_refused() {
        let (_d, camp) = temp_camp();
        let err = run(&camp, &s(&["mol", "current"])).unwrap_err();
        assert!(format!("{err:#}").contains("mol"));
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert!(
            ledger
                .events_of_type(EventType::ShimRefused)
                .unwrap()
                .iter()
                .any(|e| e.data["verb"] == "mol")
        );
    }

    #[test]
    fn bd_update_unknown_flag_is_refused_not_silently_ignored() {
        // Task 5's flag-refusal test, sited with the served handler that owns
        // the flag grammar (as the plan directs). A fall-through no-op on an
        // unknown flag is a corrupted ledger (§6).
        let (_d, camp) = temp_camp();
        seed_and_claim(&camp, "gc-1", "gc.publisher", "t/gc.publisher/1");
        let err = run(
            &camp,
            &s(&[
                "update",
                "gc-1",
                "--set-metadata",
                "gc.outcome=pass",
                "--frobnicate",
            ]),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("--frobnicate"));
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert!(
            ledger
                .events_of_type(EventType::ShimRefused)
                .unwrap()
                .iter()
                .any(|e| e.data["verb"] == "update")
        );
    }
}

use anyhow::{Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request};

/// `camp retry <bead>` (issue #83): re-arm a bead whose dispatch failed,
/// keeping its id and history. A PURE CLIENT (design §4.3) in the `camp
/// sling` shape — append the durable `dispatch.rearmed` fact (which the fold
/// uses to clear `beads.dispatch_failure`), then poke a RUNNING campd so it
/// re-dispatches. campd is the sole dispatcher; there is no second path.
///
/// The re-arm is an EXPLICIT operator action, never a timer (invariant 1).
/// A bead with no failed dispatch is a loud, actionable error — re-arming
/// clean work would be a lie about state (invariant 5).
pub fn run(camp: &CampDir, bead: String) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let row = ledger
        .get_bead(&bead)?
        .ok_or_else(|| anyhow::anyhow!("no such bead: {bead}"))?;
    let Some(previous_reason) = row.dispatch_failure.clone() else {
        bail!(
            "{bead} has no failed dispatch to retry (its dispatch_failure marker is clear). \
             `camp show {bead}` shows its current state; `camp top` counts stuck beads."
        );
    };
    let seq = ledger.append(EventInput {
        kind: EventType::DispatchRearmed,
        rig: Some(row.rig.clone()),
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data: serde_json::json!({ "previous_reason": previous_reason }),
    })?;
    drop(ledger); // campd may need the write lock immediately

    // The re-arm is DURABLE now: print it before the poke, so a campd that
    // cannot serve us costs the operator the dispatch, never the re-arm.
    println!("re-armed {bead} (was: {previous_reason})");
    socket::require(camp, &Request::Poke { seq }).map_err(|e| {
        e.context(format!(
            "{bead} is re-armed and durable, but NOT dispatched — only a healthy, running \
             campd dispatches; it runs as soon as one is (campd catches up from its cursor \
             on start)"
        ))
    })?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn camp_with_ledger() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        // touch the ledger so it exists
        drop(Ledger::open(&camp.db_path()).unwrap());
        (dir, camp)
    }

    #[test]
    fn retry_on_an_unknown_bead_bails() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let (_d, camp) = camp_with_ledger();
        let err = run(&camp, "gc-404".into()).unwrap_err();
        assert!(
            format!("{err:#}").contains("no such bead"),
            "err was: {err:#}"
        );
    }

    #[test]
    fn retry_on_a_bead_with_no_failure_bails_before_any_event() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let (_d, camp) = camp_with_ledger();
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({ "title": "healthy work" }),
            })
            .unwrap();
        drop(ledger);

        let err = run(&camp, "gc-1".into()).unwrap_err();
        assert!(
            format!("{err:#}").contains("no failed dispatch to retry"),
            "err was: {err:#}"
        );
        // no dispatch.rearmed was appended
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert_eq!(
            ledger
                .events_of_type(EventType::DispatchRearmed)
                .unwrap()
                .len(),
            0
        );
    }
}

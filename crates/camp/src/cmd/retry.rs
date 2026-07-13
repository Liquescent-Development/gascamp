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
/// clean work would be a lie about state (invariant 5). So are a non-open
/// bead (a closed bead's stale marker is history, not a promise — review
/// F2) and a worker-cap deferral (campd retries that itself — review F1).
pub fn run(camp: &CampDir, bead: String) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let row = ledger
        .get_bead(&bead)?
        .ok_or_else(|| anyhow::anyhow!("no such bead: {bead}"))?;
    if row.status != "open" {
        bail!(
            "{bead} is {} — only an open bead can be re-armed; re-arming would promise \
             a dispatch that can never happen (its dispatch_failure marker, if any, is \
             stale history)",
            row.status
        );
    }
    let Some(previous_reason) = row.dispatch_failure.clone() else {
        bail!(
            "{bead} has no failed dispatch to retry (its dispatch_failure marker is clear). \
             `camp show {bead}` shows its current state; `camp top` counts stuck beads."
        );
    };
    if camp_core::readiness::is_deferred_dispatch_failure(&previous_reason) {
        bail!(
            "{bead}'s dispatch is deferred, not dead: {previous_reason}. campd retries it \
             itself when a worker slot frees; there is nothing to re-arm"
        );
    }
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

    /// Issue #83 review F2: a closed bead can carry a stale marker (nothing
    /// clears it on close). "Re-arming" it would print success for a bead
    /// that can never dispatch — a lie about state (invariant 5).
    #[test]
    fn retry_on_a_closed_bead_with_a_stale_marker_bails() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let (_d, camp) = camp_with_ledger();
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();
        for (kind, data) in [
            (
                EventType::BeadCreated,
                serde_json::json!({ "title": "went nowhere" }),
            ),
            (
                EventType::DispatchFailed,
                serde_json::json!({ "reason": "rig path is not a directory" }),
            ),
            (
                EventType::BeadClosed,
                serde_json::json!({ "outcome": "fail" }),
            ),
        ] {
            ledger
                .append(EventInput {
                    kind,
                    rig: Some("gc".into()),
                    actor: "cli".into(),
                    bead: Some("gc-1".into()),
                    data,
                })
                .unwrap();
        }
        // precondition: the marker really is stale on the closed bead
        let row = ledger.get_bead("gc-1").unwrap().unwrap();
        assert_eq!(row.status, "closed");
        assert!(row.dispatch_failure.is_some());
        drop(ledger);

        let err = run(&camp, "gc-1".into()).unwrap_err();
        assert!(
            format!("{err:#}").contains("only an open bead"),
            "err was: {err:#}"
        );
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        assert_eq!(
            ledger
                .events_of_type(EventType::DispatchRearmed)
                .unwrap()
                .len(),
            0
        );
    }

    /// Issue #83 review F1: a worker-cap deferral is campd's OWN retry (the
    /// pending_respawns queue) — `camp retry` re-arming it would be a silent
    /// no-op (the bead is ever-sessioned, outside `dispatchable_beads`), so
    /// it is a loud error instead.
    #[test]
    fn retry_on_a_cap_deferred_bead_bails_campd_owns_that_retry() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let (_d, camp) = camp_with_ledger();
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();
        for (kind, data) in [
            (
                EventType::BeadCreated,
                serde_json::json!({ "title": "capped" }),
            ),
            (
                EventType::DispatchFailed,
                serde_json::json!({ "reason": format!(
                    "{} worker cap reached; will retry when a slot frees",
                    camp_core::readiness::DEFERRED_DISPATCH_PREFIX
                ) }),
            ),
        ] {
            ledger
                .append(EventInput {
                    kind,
                    rig: Some("gc".into()),
                    actor: "campd".into(),
                    bead: Some("gc-1".into()),
                    data,
                })
                .unwrap();
        }
        drop(ledger);

        let err = run(&camp, "gc-1".into()).unwrap_err();
        assert!(format!("{err:#}").contains("deferred"), "err was: {err:#}");
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

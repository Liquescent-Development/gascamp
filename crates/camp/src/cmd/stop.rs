use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};
use crate::service::{self, Supervisor, SystemProbe, SystemRunner};

/// `camp stop`: graceful daemon shutdown over the socket. Never auto-starts
/// (stopping nothing is an error, not a no-op).
///
/// On a SUPERVISED camp it refuses instead (operator decision, 2026-07-10):
/// the supervisor's KeepAlive / Restart=always would bring campd straight back,
/// so a socket stop that printed "campd stopped" would be a lie about the
/// verb's effect. Fail fast, name the remedy (invariants 3 and 5).
pub fn run(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = service::host_supervisor(&probe, &runner)?;
    run_with(camp, supervisor.as_deref())
}

/// The testable core: the supervisor is injected, so both branches (supervised
/// and not) are unit-tested without a live service manager.
fn run_with(camp: &CampDir, supervisor: Option<&dyn Supervisor>) -> Result<()> {
    if let Some(supervisor) = supervisor
        && let Some(unit) = crate::cmd::service::managed_unit(supervisor, &camp.root)?
    {
        bail!(
            "campd for this camp is supervised by {} (unit {}, {}) — a socket stop would be \
             restarted immediately.\n       To stop it:      camp service stop\n       \
             To un-manage it: camp service uninstall",
            supervisor.name(),
            unit.name,
            supervisor.restart_policy()
        );
    }
    stop_over_socket(camp)
}

/// Unchanged from before this phase: the socket stop for an unsupervised camp.
fn stop_over_socket(camp: &CampDir) -> Result<()> {
    // A wedge is not "not running" (issue #55): the CampdUnresponsive
    // error already carries the truth (pid + kill -9 remedy) — layering
    // "campd is not running" over it would misdiagnose a live-but-stuck
    // daemon as an absent one.
    let response = socket::request(camp, &Request::Stop).map_err(|e| {
        if e.downcast_ref::<socket::CampdUnresponsive>().is_some() {
            e
        } else {
            e.context("campd is not running")
        }
    })?;
    match response {
        Response::Ok { .. } => {
            println!("campd stopped");
            Ok(())
        }
        other => bail!("unexpected response to stop: {other:?}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::launchd::Launchd;
    use crate::service::runner::fake::FakeRunner;
    use std::path::Path;

    /// Operator decision (2026-07-10): on a SUPERVISED camp, `camp stop`
    /// refuses. A socket stop would succeed and the supervisor would restart
    /// campd within moments — so "campd stopped" would be a lie, and no verb
    /// may lie about its effect (invariants 3 and 5). The error names the
    /// supervisor, the unit, the always-on mechanism, and BOTH remedies.
    #[test]
    fn stop_refuses_on_a_supervised_camp_and_sends_no_socket_request() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        crate::cmd::service::install(&launchd, &camp.root, Path::new("/usr/local/bin/camp"))
            .unwrap();

        let err = run_with(&camp, Some(&launchd)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("supervised by launchd"), "{msg}");
        assert!(
            msg.contains("com.gascamp.campd."),
            "must name the unit: {msg}"
        );
        assert!(
            msg.contains("KeepAlive"),
            "must name the always-on mechanism: {msg}"
        );
        assert!(
            msg.contains("camp service stop"),
            "must name the remedy: {msg}"
        );
        assert!(
            msg.contains("camp service uninstall"),
            "must name the un-manage remedy: {msg}"
        );
        // And it must not have been a socket error dressed up: there is no
        // campd on this temp camp's socket at all — the refusal came FIRST.
        assert!(
            !msg.contains("not running"),
            "the refusal precedes any socket attempt: {msg}"
        );
    }

    /// An UNSUPERVISED camp (a container, CI, a camp nobody installed a unit
    /// for) keeps today's behavior exactly: the socket stop is attempted, and
    /// with no campd listening it is the same loud "campd is not running".
    #[test]
    fn stop_on_an_unsupervised_camp_is_unchanged() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        // No unit installed → the supervised check passes through…
        let err = run_with(&camp, Some(&launchd)).unwrap_err();
        assert!(
            format!("{err:#}").contains("campd is not running"),
            "the socket stop must still be attempted: {err:#}"
        );
        assert_eq!(
            fake.call_count(),
            0,
            "no unit file, nothing to ask the manager"
        );

        // …and so does a host with no service manager at all (a container).
        let err = run_with(&camp, None).unwrap_err();
        assert!(
            format!("{err:#}").contains("campd is not running"),
            "{err:#}"
        );
    }
}

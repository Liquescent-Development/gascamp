use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};
use crate::service::{self, Supervisor, SystemProbe, SystemRunner};

/// `camp stop`: graceful daemon shutdown over the socket. Stopping nothing is
/// an error, not a no-op — the CLI never starts campd, so it never un-stops it.
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
        // Keyed on the ONE question the refusal's justification rests on: will
        // this supervisor put campd back if we stop it? Not on the unit file
        // existing (round 1's bug), and not on `loaded` either (round 2's) —
        // `loaded` means "bootstrapped" to launchd but merely "the unit file
        // parsed" to systemd, so a `loaded` gate refused forever on Linux, on
        // units systemd had long since let die. Only the supervisor knows its
        // own restart semantics; it answers in `will_restart_campd`.
        //
        // When it says no, nothing will undo a socket stop, so `camp stop`
        // falls through and is the honest verb for exactly the campd the
        // supervisor does not own.
        && supervisor.state(&unit.id)?.will_restart_campd
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
    use crate::service::systemd::Systemd;
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
        // Two calls: the install's bootstrap, then the LOADED check the refusal
        // is now keyed on — a loaded unit is the only state whose KeepAlive
        // would really undo a socket stop, and so the only one worth refusing.
        let install_runner = FakeRunner::new(vec![
            FakeRunner::ok(""),
            FakeRunner::ok("service = {\n\tstate = running\n}\n"),
        ]);
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

    /// CRITICAL (review round 1). The refusal's ENTIRE justification is that
    /// `KeepAlive` / `Restart=always` would restart campd immediately, making a
    /// socket stop a lie. That is true only of a LOADED unit. Once the unit is
    /// booted out — exactly the state `camp service stop` leaves behind — the
    /// supervisor will not restart anything, so a socket stop is honest and
    /// `camp stop` is the right verb for it.
    ///
    /// Keyed on the unit FILE merely existing, `camp stop` instead refused and
    /// sent the operator to `camp service stop`, which (see the twin test in
    /// cmd::service) did nothing and reported success — so a campd that a
    /// hand-run `camp daemon` (or, before it was removed, the CLI-spawn path)
    /// had left listening could not be stopped by ANY camp verb.
    #[test]
    fn stop_does_the_socket_stop_when_the_unit_is_installed_but_not_loaded() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        crate::cmd::service::install(&launchd, &camp.root, Path::new("/usr/local/bin/camp"))
            .unwrap();

        // A campd is listening, and launchd has booted the unit out: nothing
        // will undo a socket stop, so `camp stop` must simply do it.
        let campd = socket::fake_campd::serve(&camp, vec![socket::fake_campd::stopped()]);
        let runner = FakeRunner::new(vec![FakeRunner::fail(113, "Could not find service\n")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &runner);

        run_with(&camp, Some(&launchd))
            .expect("an unloaded unit restarts nothing — the socket stop is honest, not a lie");
        assert_eq!(
            campd.served(),
            1,
            "camp stop must have actually stopped it over the socket"
        );
    }

    /// CRITICAL (review round 2). The launchd twin above passes, and that is
    /// exactly why this went unseen: every test of this verb wired `Launchd`.
    ///
    /// `UnitState.loaded` does not mean the same thing in the two supervisors.
    /// On launchd it means BOOTSTRAPPED — and with `KeepAlive`, bootstrapped
    /// really does mean "campd comes back if you stop it". On systemd it means
    /// `LoadState=loaded`, which only says the unit FILE parsed and is in
    /// systemd's memory: it is `loaded` for a unit that is inactive, dead,
    /// stopped or failed. It is, near enough, "the unit file exists" — the very
    /// predicate round 1 ordered this refusal to STOP using.
    ///
    /// So on Linux the refusal never opened: `camp stop` refused on any camp
    /// with an installed unit forever, naming `camp service stop`, which
    /// no-op'd its `systemctl stop` and bailed back naming `camp stop`. Round
    /// 1's un-stoppable ping-pong, alive verbatim, on the whole of Linux.
    #[test]
    fn stop_does_the_socket_stop_when_the_systemd_unit_is_loaded_but_inactive() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![
            FakeRunner::ok(""), // daemon-reload
            FakeRunner::ok(""), // enable --now
        ]);
        let systemd = Systemd::new(units.path().to_path_buf(), &install_runner);
        crate::cmd::service::install(&systemd, &camp.root, Path::new("/usr/local/bin/camp"))
            .unwrap();

        // Literally what `systemctl show` prints for a stopped unit: still
        // LOADED, but inactive and dead. `Restart=always` applies only to a
        // unit that is running — systemd will restart NOTHING here, so a
        // socket stop is honest and `camp stop` is the verb for it.
        let campd = socket::fake_campd::serve(&camp, vec![socket::fake_campd::stopped()]);
        let runner = FakeRunner::new(vec![FakeRunner::ok(
            "LoadState=loaded\nActiveState=inactive\nSubState=dead\n",
        )]);
        let systemd = Systemd::new(units.path().to_path_buf(), &runner);

        run_with(&camp, Some(&systemd))
            .expect("an inactive systemd unit restarts nothing — the socket stop is honest");
        assert_eq!(
            campd.served(),
            1,
            "camp stop must have actually stopped it over the socket"
        );
    }

    /// The systemd unit that IS running must still be refused: `Restart=always`
    /// on an active unit really would undo a socket stop.
    #[test]
    fn stop_refuses_on_an_active_systemd_unit() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let runner = FakeRunner::new(vec![
            FakeRunner::ok(""), // daemon-reload
            FakeRunner::ok(""), // enable --now
            FakeRunner::ok("LoadState=loaded\nActiveState=active\nSubState=running\n"),
        ]);
        let systemd = Systemd::new(units.path().to_path_buf(), &runner);
        crate::cmd::service::install(&systemd, &camp.root, Path::new("/usr/local/bin/camp"))
            .unwrap();

        let err = run_with(&camp, Some(&systemd)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("supervised by systemd"), "{msg}");
        assert!(msg.contains("Restart=always"), "{msg}");
        assert!(msg.contains("camp service stop"), "{msg}");
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

//! Host service management (design §5/§6): campd is a supervised foreground
//! process, and the supervisor is environment-provided. This module is the
//! SEAM that makes that pluggable and testable.
//!
//! Three ports, so no flow needs a live service manager to be tested:
//!   - `CommandRunner` (runner.rs) — the only place a process is spawned.
//!   - `HostProbe` (detect.rs) — the only place the environment is read.
//!   - `Supervisor` (supervisor.rs) — one impl per service manager. Unit-file
//!     GENERATION is pure; only load/unload/restart/state touch the manager,
//!     and they do it through the runner.
//!
//! A third supervisor is a new `impl Supervisor` and one arm in
//! `supervisor_for`. Nothing above the trait changes.

pub mod camp_id;
pub mod detect;
pub mod launchd;
pub mod runner;
pub mod supervisor;
pub mod systemd;

use std::path::PathBuf;

use anyhow::{Context, Result};

pub use camp_id::CampId;
pub use detect::{HostProbe, Manager, SystemProbe, detect};
pub use runner::{CommandRunner, SystemRunner};
pub use supervisor::Supervisor;

/// The supervisor for `manager`, wired to THIS host's unit directory (and,
/// for launchd, this user's uid — its domain target needs one).
pub fn supervisor_for<'a>(
    manager: Manager,
    probe: &dyn HostProbe,
    runner: &'a dyn CommandRunner,
) -> Result<Box<dyn Supervisor + 'a>> {
    match manager {
        Manager::Launchd => {
            let unit_dir = home(probe)?.join("Library").join("LaunchAgents");
            let uid = runner::current_uid(runner)?;
            Ok(Box::new(launchd::Launchd::new(unit_dir, uid, runner)))
        }
        Manager::Systemd => {
            let config = match probe.env("XDG_CONFIG_HOME") {
                Some(dir) => PathBuf::from(dir),
                None => home(probe)?.join(".config"),
            };
            let unit_dir = config.join("systemd").join("user");
            Ok(Box::new(systemd::Systemd::new(unit_dir, runner)))
        }
    }
}

/// The host's supervisor, or None when no host service manager is usable (a
/// container, CI, a minimal box). None is a normal answer, not an error — the
/// CALLER decides what it means (`camp init` hands off; `camp service
/// install` fails loudly).
pub fn host_supervisor<'a>(
    probe: &dyn HostProbe,
    runner: &'a dyn CommandRunner,
) -> Result<Option<Box<dyn Supervisor + 'a>>> {
    match detect(probe) {
        Some(manager) => Ok(Some(supervisor_for(manager, probe, runner)?)),
        None => Ok(None),
    }
}

fn home(probe: &dyn HostProbe) -> Result<PathBuf> {
    probe
        .env("HOME")
        .map(PathBuf::from)
        .context("$HOME is not set — cannot locate the user's unit directory")
}

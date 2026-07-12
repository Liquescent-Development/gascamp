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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

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

/// What the operator asked `camp init` to do about the host service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceChoice {
    /// Default: install where a manager exists; hand off where none does.
    Auto,
    /// `--service`: install, or fail loudly.
    Force,
    /// `--no-service`: never install.
    Skip,
}

/// What `camp init` will DO. Pure — `(choice, detection) → decision` — so
/// every environment is a unit test (design §9), and the IO-shaped half stays
/// a thin shell over a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Install(Manager),
    SkipByFlag,
    SkipNoManager,
    FailNoManager,
}

pub fn decide(choice: ServiceChoice, detected: Option<Manager>) -> Decision {
    match (choice, detected) {
        (ServiceChoice::Skip, _) => Decision::SkipByFlag,
        (_, Some(manager)) => Decision::Install(manager),
        (ServiceChoice::Force, None) => Decision::FailNoManager,
        (ServiceChoice::Auto, None) => Decision::SkipNoManager,
    }
}

fn home(probe: &dyn HostProbe) -> Result<PathBuf> {
    probe
        .env("HOME")
        .map(PathBuf::from)
        .context("$HOME is not set — cannot locate the user's unit directory")
}

/// The boundary gate for everything that enters a unit file.
///
/// A unit is TEXT: a launchd plist is XML, a systemd unit is line-oriented
/// INI. A path that is not valid UTF-8 (legal on macOS and Linux), or that
/// carries a control character, cannot be written into either without
/// corrupting it — and a corrupt unit the manager still ACCEPTS is the worst
/// outcome available: `install` prints "now supervised", and the supervisor
/// respawn-throttles a campd that can never open its camp. `to_string_lossy`
/// would do exactly that (U+FFFD for the unrepresentable bytes), which is the
/// silent-fallback pattern invariant 5 exists to forbid. So we refuse HERE,
/// loudly, before a single byte of unit text is generated — and `unit_text`
/// takes `&str`, so no generator can reintroduce the lossy path.
pub fn unit_safe_str<'a>(path: &'a Path, what: &str) -> Result<&'a str> {
    let text = path.to_str().with_context(|| {
        format!(
            "the {what} path is not valid UTF-8 ({}) — no service unit can name it; \
             move the camp to a UTF-8 path, or run `camp daemon --camp <dir>` under \
             your own supervisor",
            path.display()
        )
    })?;
    if let Some(bad) = text.chars().find(|c| c.is_control()) {
        bail!(
            "the {what} path contains a control character ({bad:?}) — no service unit can \
             name it (a launchd plist is XML; a systemd unit is line-oriented): {}",
            path.display()
        );
    }
    Ok(text)
}

/// The same gate for a value that is already a `str` — the PATH we bake into
/// the unit. A newline in it would end the `Environment=` line early and hand
/// systemd a unit that means something else entirely; a control character in a
/// plist is no better.
pub fn unit_safe_value<'a>(text: &'a str, what: &str) -> Result<&'a str> {
    if let Some(bad) = text.chars().find(|c| c.is_control()) {
        bail!(
            "the {what} contains a control character ({bad:?}) — no service unit can carry \
             it (a launchd plist is XML; a systemd unit is line-oriented): {text:?}"
        );
    }
    Ok(text)
}

/// The PATH a supervised campd will run with, and why the unit must say it.
///
/// A supervisor does NOT give campd your shell's environment. launchd hands a
/// LaunchAgent `PATH=/usr/bin:/bin:/usr/sbin:/sbin`; a systemd user service gets
/// `/usr/local/bin:/usr/bin:/bin:…`. Neither contains `~/.local/bin` — which is
/// where Claude Code installs `claude`, the very thing campd spawns to do the
/// work. Before campd was supervised the CLI started it, so it inherited the
/// shell that ran the verb and this never came up. Supervision took that away,
/// and a campd that cannot find `claude` cannot dispatch anything: every bead
/// lands as `session.crashed  spawn failed: spawning claude: No such file or
/// directory`, which is the whole product, silently broken.
///
/// So the PATH is captured HERE — from the environment that ran the install,
/// which is demonstrably one where the operator's tools resolve — and written
/// into the unit, where it is visible (`camp service status` prints the unit's
/// path; the file itself is human-readable). It is a snapshot, not a live link:
/// change your PATH and you re-run `camp service install` to re-capture it. That
/// is stated at install rather than left to be discovered.
pub fn campd_path() -> Result<String> {
    let path = std::env::var("PATH").context(
        "this environment has no PATH, so the unit could not give campd one — and campd \
         needs it to find the worker command (`claude`) and `git`",
    )?;
    unit_safe_value(&path, "PATH")?;
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The PATH goes into the unit as text, so it gets the same gate the camp
    /// path gets. A newline is the one that matters: a systemd unit is
    /// line-oriented, so `PATH=/a\nExecStart=/bin/sh` would not be a broken unit
    /// — it would be a DIFFERENT one, which systemd would accept and run.
    #[test]
    fn a_path_with_a_control_character_is_refused_before_any_unit_is_written() {
        let err = unit_safe_value("/usr/bin:/tmp\nExecStart=/bin/sh", "PATH").unwrap_err();
        let shown = format!("{err:#}");
        assert!(
            shown.contains("control character"),
            "a newline in the PATH must be a hard error, not a corrupt unit: {shown}"
        );
        assert!(unit_safe_value("/usr/bin:/bin", "PATH").is_ok());
    }

    /// A unit file is TEXT — a launchd plist is XML, a systemd unit is
    /// line-oriented INI. A path that is not valid UTF-8 (legal on macOS and
    /// Linux), or that carries a control character, cannot be written into
    /// either without corrupting it — and a corrupt unit the manager still
    /// ACCEPTS is the worst outcome available: `install` prints "now
    /// supervised", and the supervisor respawn-throttles a campd that can
    /// never open its camp. `to_string_lossy` would do exactly that (U+FFFD
    /// for the unrepresentable bytes), which is the silent-fallback pattern
    /// invariant 5 exists to forbid. So we refuse HERE, loudly.
    #[test]
    fn a_path_that_cannot_be_written_into_a_unit_is_a_loud_error() {
        assert_eq!(
            unit_safe_str(Path::new("/Users/x/camps/dev/.camp"), "camp").unwrap(),
            "/Users/x/camps/dev/.camp"
        );

        // Not valid UTF-8 (legal on macOS and Linux alike).
        use std::os::unix::ffi::OsStrExt as _;
        let raw = std::ffi::OsStr::from_bytes(b"/tmp/caf\xFF/.camp");
        let err = unit_safe_str(Path::new(raw), "camp").unwrap_err();
        assert!(format!("{err:#}").contains("not valid UTF-8"), "{err:#}");

        // A control character would structurally corrupt either unit format.
        let err = unit_safe_str(Path::new("/tmp/two\nlines/.camp"), "camp").unwrap_err();
        assert!(format!("{err:#}").contains("control character"), "{err:#}");
    }

    /// Design §6: detection decides, the flags override. Nine cells (3
    /// choices × 3 detections), all pinned.
    #[test]
    fn the_init_service_decision_is_a_pure_table() {
        // Default: a host with a manager gets a supervised campd…
        assert_eq!(
            decide(ServiceChoice::Auto, Some(Manager::Launchd)),
            Decision::Install(Manager::Launchd)
        );
        assert_eq!(
            decide(ServiceChoice::Auto, Some(Manager::Systemd)),
            Decision::Install(Manager::Systemd)
        );
        // …and a container/CI box gets a VISIBLE hand-off, not a failure.
        assert_eq!(decide(ServiceChoice::Auto, None), Decision::SkipNoManager);

        // --service forces it, and is a HARD ERROR where it cannot be honored.
        assert_eq!(
            decide(ServiceChoice::Force, Some(Manager::Launchd)),
            Decision::Install(Manager::Launchd)
        );
        assert_eq!(
            decide(ServiceChoice::Force, Some(Manager::Systemd)),
            Decision::Install(Manager::Systemd)
        );
        assert_eq!(decide(ServiceChoice::Force, None), Decision::FailNoManager);

        // --no-service skips, manager or not.
        assert_eq!(
            decide(ServiceChoice::Skip, Some(Manager::Launchd)),
            Decision::SkipByFlag
        );
        assert_eq!(
            decide(ServiceChoice::Skip, Some(Manager::Systemd)),
            Decision::SkipByFlag
        );
        assert_eq!(decide(ServiceChoice::Skip, None), Decision::SkipByFlag);
    }
}

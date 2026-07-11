//! `camp service` (design §5): the control surface over the host's service
//! manager. Every flow takes the `Supervisor` PORT, so each is tested against
//! a real unit directory (a tempdir) with a faked process runner — no live
//! service manager anywhere in unit CI.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::campdir::CampDir;
use crate::service::{self, CampId, Supervisor, SystemProbe, SystemRunner};

/// `camp service list`: every camp with a managed unit, and its state. The
/// unit DIRECTORY is the registry (design §5) — there is no status file, no
/// registry file. Needs no camp: it is the "manage everything" view.
pub fn list(supervisor: Option<&dyn Supervisor>) -> Result<String> {
    let Some(supervisor) = supervisor else {
        return Ok(
            "no host service manager detected (container/CI?) — no managed units\n".to_owned(),
        );
    };
    let units = supervisor.installed()?;
    if units.is_empty() {
        return Ok(format!(
            "no camps have a managed {} unit\n",
            supervisor.name()
        ));
    }
    let mut report = String::new();
    for unit in units {
        let state = supervisor.state(&unit.id)?;
        let mark = match (state.loaded, state.running) {
            (true, true) => "running",
            (true, false) => "loaded",
            (false, _) => "not loaded",
        };
        report.push_str(&format!(
            "{}  {}  {}\n  unit: {}  [{}]\n",
            unit.id,
            mark,
            unit.camp_root.display(),
            unit.unit_path.display(),
            state.detail
        ));
    }
    Ok(report)
}

/// The wiring: the real host, the real process runner.
pub fn run_list() -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = service::host_supervisor(&probe, &runner)?;
    print!("{}", list(supervisor.as_deref())?);
    Ok(())
}

/// The unit installed for THIS camp — identity verified.
pub(crate) struct ManagedUnit {
    pub id: CampId,
    /// The manager's own name for it (a launchd label; a systemd unit name).
    pub name: String,
    pub path: PathBuf,
}

/// Is this camp managed, and is the unit at its path really ITS unit?
///
/// The one place any verb answers "is this camp supervised?" — `install`'s
/// clobber check, `uninstall`, `status`, `restart`, `stop`, `start`, and
/// `camp stop`'s refusal all go through here.
///
/// `<camp-id>` is `<slug>-<32 bits of digest>`: collision is vanishingly
/// unlikely, but "the file exists" alone would let a colliding camp operate on
/// ANOTHER camp's unit — and `uninstall` would remove it. So we do not trust
/// the path; we ASK the unit which camp it names (the unit is the source of
/// truth, design §5) and refuse loudly on a mismatch.
pub(crate) fn managed_unit(
    supervisor: &dyn Supervisor,
    camp_root: &Path,
) -> Result<Option<ManagedUnit>> {
    let id = CampId::for_camp(camp_root)?;
    let path = supervisor.unit_path(&id);
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let named = supervisor.parse_camp_root(&text)?;
    let canonical = std::fs::canonicalize(camp_root)
        .with_context(|| format!("resolving the camp path {}", camp_root.display()))?;
    if named != canonical {
        bail!(
            "the {} unit {} names a DIFFERENT camp ({}) than this one ({}) — the camp id \
             {} collides. Refusing to act on another camp's daemon; move or rename this camp.",
            supervisor.name(),
            path.display(),
            named.display(),
            canonical.display(),
            id
        );
    }
    Ok(Some(ManagedUnit {
        name: supervisor.unit_name(&id),
        id,
        path,
    }))
}

/// `camp service install` (design §5): generate the unit, then load it.
/// macOS → a KeepAlive LaunchAgent bootstrapped into `gui/$UID`; Linux → a
/// `Restart=always` systemd user unit, `enable --now`.
pub fn install(supervisor: &dyn Supervisor, camp_root: &Path, exe: &Path) -> Result<String> {
    // Never a silent overwrite — and if the unit at our path belongs to a
    // different camp, `managed_unit` refuses rather than let us clobber it.
    if let Some(existing) = managed_unit(supervisor, camp_root)? {
        bail!(
            "a {} unit for this camp is already installed ({} at {}) — \
             `camp service restart` cycles it, `camp service uninstall` removes it",
            supervisor.name(),
            existing.name,
            existing.path.display()
        );
    }
    let id = CampId::for_camp(camp_root)?;
    // The unit must name the camp's REAL path: a supervisor runs campd from
    // its own cwd, and a relative path would resolve somewhere else entirely.
    let root = std::fs::canonicalize(camp_root)
        .with_context(|| format!("resolving the camp path {}", camp_root.display()))?;
    // The gate (invariant 5): a path no unit file could name is a hard error
    // HERE — before any text is generated, any file is written, and any
    // manager is told a camp is supervised.
    let root_text = service::unit_safe_str(&root, "camp")?;
    let exe_text = service::unit_safe_str(exe, "camp binary")?;

    let unit_path = supervisor.unit_path(&id);
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&unit_path, supervisor.unit_text(&id, root_text, exe_text))
        .with_context(|| format!("writing {}", unit_path.display()))?;
    if let Err(reload_error) = supervisor.reload_units() {
        // Fail fast, no half state: a unit the manager could not even be
        // told about must not be left on disk pretending to be installed —
        // the next `install` would otherwise refuse with "already installed"
        // for a camp that is neither installed nor loaded. Same rollback as
        // a failed `load`, one line later.
        let error = reload_error.context(format!(
            "reloading {} after writing the unit {} ({})",
            supervisor.name(),
            supervisor.unit_name(&id),
            unit_path.display()
        ));
        return Err(rollback_unit_file(supervisor, &unit_path, error));
    }

    if let Err(load_error) = supervisor.load(&id) {
        // Fail fast, no half state: a unit the manager refused must not be
        // left on disk pretending to be installed — and the MANAGER must be
        // told too (systemd keeps a failed unit in memory until the next
        // daemon-reload). Every error is reported; none is swallowed.
        let error = load_error.context(format!(
            "loading the {} unit {} ({})",
            supervisor.name(),
            supervisor.unit_name(&id),
            unit_path.display()
        ));
        return Err(rollback_unit_file(supervisor, &unit_path, error));
    }
    Ok(format!(
        "installed {} unit {} ({})\ncampd for {} is now supervised — it restarts on crash \
         and at login\nto stop it: `camp service stop`; to un-manage it: \
         `camp service uninstall`; to cycle it after an upgrade: `camp service restart`\n",
        supervisor.name(),
        supervisor.unit_name(&id),
        unit_path.display(),
        root.display()
    ))
}

/// After the unit file has been written, undo it: no failure between "the
/// file is on disk" and "install reports success" may leave that file
/// behind (invariant 5, no half state) — reachable from a failed
/// `reload_units` (just after the write) or a failed `load` (one line
/// later), so both go through here. The ORIGINAL error is never swallowed: a
/// failed rollback is folded INTO it (both failures visible), never
/// replaces it.
fn rollback_unit_file(
    supervisor: &dyn Supervisor,
    unit_path: &Path,
    error: anyhow::Error,
) -> anyhow::Error {
    match std::fs::remove_file(unit_path) {
        Err(e) => error.context(format!(
            "and the unit file could not be rolled back: removing {} ({e})",
            unit_path.display()
        )),
        Ok(()) => match supervisor.reload_units() {
            Err(e) => error.context(format!(
                "and the manager could not be reloaded after the rollback: {e:#}"
            )),
            Ok(()) => error,
        },
    }
}

/// The managed unit, or the loud "this camp is not managed" error. `remedy` is
/// the verb that WOULD help — every one of these errors is actionable.
/// (Shared by `uninstall`, `restart`, `stop` and `start`: four verbs, one
/// sentence about what "not installed" means.)
pub(crate) fn require_managed_unit(
    supervisor: &dyn Supervisor,
    camp_root: &Path,
    remedy: &str,
) -> Result<ManagedUnit> {
    match managed_unit(supervisor, camp_root)? {
        Some(unit) => Ok(unit),
        None => {
            let id = CampId::for_camp(camp_root)?;
            bail!(
                "no {} unit is installed for this camp ({} does not exist) — {remedy}",
                supervisor.name(),
                supervisor.unit_path(&id).display()
            )
        }
    }
}

/// `camp service uninstall` (design §5): stop + unload + remove the unit.
pub fn uninstall(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String> {
    let unit = require_managed_unit(supervisor, camp_root, "nothing to uninstall")?;
    supervisor.unload(&unit.id)?;
    std::fs::remove_file(&unit.path)
        .with_context(|| format!("removing {}", unit.path.display()))?;
    supervisor.reload_units()?;
    Ok(format!(
        "uninstalled {} unit {} ({})\n",
        supervisor.name(),
        unit.name,
        unit.path.display()
    ))
}

/// The `camp` binary a unit must run: the running executable's REAL absolute
/// path. A unit naming a relative path breaks the moment the supervisor's cwd
/// differs from yours (it always does).
pub(crate) fn camp_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the camp binary")?;
    std::fs::canonicalize(&exe).with_context(|| format!("resolving {}", exe.display()))
}

/// The host's supervisor, or the loud, actionable error for a host that has
/// none (a container, CI) — where installing a unit is impossible, not
/// merely inconvenient.
fn require_supervisor<'a>(
    probe: &dyn service::HostProbe,
    runner: &'a dyn service::CommandRunner,
) -> Result<Box<dyn Supervisor + 'a>> {
    service::host_supervisor(probe, runner)?.context(
        "no host service manager detected (macOS launchd, or a reachable systemd --user) — \
         run `camp daemon --camp <dir>` under your supervisor (e.g. the container runtime)",
    )
}

pub fn run_install(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!(
        "{}",
        install(supervisor.as_ref(), &camp.root, &camp_binary()?)?
    );
    Ok(())
}

pub fn run_uninstall(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", uninstall(supervisor.as_ref(), &camp.root)?);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::launchd::Launchd;
    use crate::service::runner::fake::FakeRunner;
    use crate::service::systemd::Systemd;
    use std::path::Path;

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/camp</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>/Users/x/camps/dev/.camp</string>
  </array>
</dict>
</plist>
"#;

    #[test]
    fn list_reports_every_managed_camp_and_its_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("com.gascamp.campd.dev-f9481b53.plist"),
            PLIST,
        )
        .unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::ok(
            "com.gascamp.campd.dev-f9481b53 = {\n\tstate = running\n}\n",
        )]);
        let launchd = Launchd::new(dir.path().to_path_buf(), 501, &fake);

        let report = list(Some(&launchd)).unwrap();
        assert!(report.contains("dev-f9481b53"), "{report}");
        assert!(report.contains("running"), "{report}");
        assert!(report.contains("/Users/x/camps/dev/.camp"), "{report}");
        assert!(
            report.contains("com.gascamp.campd.dev-f9481b53.plist"),
            "{report}"
        );
    }

    #[test]
    fn list_with_no_managed_camps_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(dir.path().to_path_buf(), 501, &fake);
        assert!(
            list(Some(&launchd)).unwrap().contains("no camps"),
            "must state the empty case"
        );
    }

    /// A container/CI box: no host service manager. Reporting that is the
    /// honest answer to the query — not a silent empty list.
    #[test]
    fn list_with_no_host_service_manager_says_so() {
        let report = list(None).unwrap();
        assert!(report.contains("no host service manager"), "{report}");
    }

    /// The full install flow against a REAL unit directory (a tempdir) with a
    /// faked service manager: the unit lands on disk with the camp's real
    /// (canonicalized) path, and the manager is asked to load it.
    #[test]
    fn install_writes_the_unit_then_loads_it() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]); // bootstrap
        let launchd = Launchd::new(units.path().join("LaunchAgents"), 501, &fake);

        let report = install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        let unit_path = launchd.unit_path(&id);
        assert!(
            unit_path.exists(),
            "the unit must be on disk: {}",
            unit_path.display()
        );
        let text = std::fs::read_to_string(&unit_path).unwrap();
        let canonical = std::fs::canonicalize(camp.path()).unwrap();
        assert_eq!(launchd.parse_camp_root(&text).unwrap(), canonical);
        assert!(text.contains("<key>KeepAlive</key>"), "{text}");
        assert!(
            fake.call(0).starts_with("launchctl bootstrap gui/501 "),
            "{}",
            fake.call(0)
        );
        assert!(report.contains("installed"), "{report}");
    }

    /// Never a silent overwrite: an existing unit is a hard error naming the
    /// two verbs that CAN act on it.
    #[test]
    fn install_refuses_to_clobber_an_existing_unit() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let fake2 = FakeRunner::new(vec![]);
        let launchd2 = Launchd::new(units.path().to_path_buf(), 501, &fake2);
        let err = install(&launchd2, camp.path(), Path::new("/usr/local/bin/camp")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already installed"), "{msg}");
        assert!(
            msg.contains("camp service restart"),
            "must name the remedy: {msg}"
        );
        assert_eq!(fake2.call_count(), 0, "a refused install touches nothing");
    }

    /// Fail fast, no half state: a unit the manager REFUSES to load must not be
    /// left on disk pretending to be installed.
    #[test]
    fn a_failed_load_rolls_the_unit_file_back() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::fail(5, "Bootstrap failed: 5\n")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        let err = install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Bootstrap failed"),
            "must carry the manager's words: {msg}"
        );

        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        assert!(
            !launchd.unit_path(&id).exists(),
            "a unit that would not load must not survive the failed install"
        );
    }

    #[test]
    fn uninstall_unloads_then_removes_the_unit() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();
        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        let unit_path = launchd.unit_path(&id);

        let uninstall_runner = FakeRunner::new(vec![
            FakeRunner::ok("service = {\n\tstate = running\n}\n"), // state: loaded
            FakeRunner::ok(""),                                    // bootout
        ]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &uninstall_runner);
        let report = uninstall(&launchd, camp.path()).unwrap();

        assert!(
            uninstall_runner.call(1).starts_with("launchctl bootout "),
            "{}",
            uninstall_runner.call(1)
        );
        assert!(!unit_path.exists(), "the unit file must be gone");
        assert!(report.contains("uninstalled"), "{report}");
    }

    /// Uninstalling what is not installed is an error, not a no-op (fail fast).
    #[test]
    fn uninstall_without_a_unit_is_a_loud_error() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        let err = uninstall(&launchd, camp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("no launchd unit"), "{err:#}");
        assert_eq!(fake.call_count(), 0);
    }

    /// B2, the launchd half: a camp path that cannot be written into a unit is
    /// refused BEFORE anything is generated, loaded, or reported as installed.
    /// (A newline is valid UTF-8 and a legal directory name on both macOS and
    /// Linux, so this is creatable everywhere; the non-UTF-8 half of the gate
    /// is pinned purely in `service::tests` — APFS refuses to create such a
    /// directory, so it cannot be exercised through the filesystem on macOS.)
    #[test]
    fn install_refuses_a_camp_path_no_unit_could_name_launchd() {
        let parent = tempfile::tempdir().unwrap();
        let camp = parent.path().join("two\nlines");
        std::fs::create_dir(&camp).unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        let err = install(&launchd, &camp, Path::new("/usr/local/bin/camp")).unwrap_err();
        assert!(format!("{err:#}").contains("control character"), "{err:#}");
        assert_eq!(fake.call_count(), 0, "nothing may be loaded");
        assert!(
            std::fs::read_dir(units.path()).unwrap().next().is_none(),
            "no unit file may be written"
        );
    }

    /// B2, the systemd half: same gate, same refusal.
    #[test]
    fn install_refuses_a_camp_path_no_unit_could_name_systemd() {
        let parent = tempfile::tempdir().unwrap();
        let camp = parent.path().join("two\nlines");
        std::fs::create_dir(&camp).unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(units.path().to_path_buf(), &fake);

        let err = install(&systemd, &camp, Path::new("/usr/local/bin/camp")).unwrap_err();
        assert!(format!("{err:#}").contains("control character"), "{err:#}");
        assert_eq!(fake.call_count(), 0, "nothing may be loaded");
        assert!(
            std::fs::read_dir(units.path()).unwrap().next().is_none(),
            "no unit file may be written"
        );
    }

    /// Note 3: the rollback tells the MANAGER too — systemd keeps a failed
    /// unit in memory until the next daemon-reload. (launchd's `reload_units`
    /// is a documented no-op: it reads the plist at bootstrap.)
    #[test]
    fn a_failed_load_rolls_back_the_file_and_the_manager() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![
            FakeRunner::ok(""),                             // daemon-reload (after write)
            FakeRunner::fail(1, "Failed to enable unit\n"), // enable --now
            FakeRunner::ok(""),                             // daemon-reload (after rollback)
        ]);
        let systemd = Systemd::new(units.path().to_path_buf(), &fake);

        let err = install(&systemd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap_err();
        assert!(
            format!("{err:#}").contains("Failed to enable unit"),
            "{err:#}"
        );
        assert_eq!(fake.call(0), "systemctl --user daemon-reload");
        assert_eq!(fake.call(2), "systemctl --user daemon-reload");
        assert!(
            std::fs::read_dir(units.path()).unwrap().next().is_none(),
            "the unit file must not survive a failed load"
        );
    }

    /// Finding 2 fix: the FIRST `reload_units` call — right after the unit
    /// file is written, before `load` is ever attempted — must roll the file
    /// back on failure exactly like a failed `load` does. Without this, a
    /// transient manager failure here (e.g. a bus hiccup) leaves the unit
    /// file on disk, and the next `install` refuses with "already installed"
    /// for a camp that was never actually loaded — the operator has to run
    /// `uninstall` just to recover from a FAILED install.
    #[test]
    fn a_failed_reload_before_load_rolls_the_unit_file_back() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![
            FakeRunner::fail(1, "Failed to connect to bus\n"), // daemon-reload (after write)
            FakeRunner::ok(""),                                // daemon-reload (after rollback)
        ]);
        let systemd = Systemd::new(units.path().to_path_buf(), &fake);

        let err = install(&systemd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Failed to connect to bus"),
            "must carry the manager's own words: {msg}"
        );

        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        assert!(
            !systemd.unit_path(&id).exists(),
            "a unit whose reload failed must not survive the failed install"
        );
    }

    /// Note 2: `<camp-id>` is `<slug>-<32 bits>`, so a collision — however
    /// unlikely — must never let one camp's verb act on ANOTHER camp's unit.
    /// The unit is the source of truth, so we ASK it which camp it names.
    /// (The collision is simulated by rewriting the installed unit's camp
    /// path: an id collision is exactly "the unit at my path names someone
    /// else's camp", and that is the state the guard must catch.)
    #[test]
    fn a_unit_that_names_another_camp_is_never_acted_on() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        let unit_path = launchd.unit_path(&id);
        let text = std::fs::read_to_string(&unit_path).unwrap();
        let hijacked = text.replace(
            &std::fs::canonicalize(camp.path())
                .unwrap()
                .display()
                .to_string(),
            "/Users/someone/else/.camp",
        );
        std::fs::write(&unit_path, hijacked).unwrap();

        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        let err = uninstall(&launchd, camp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("/Users/someone/else/.camp"),
            "must name the other camp: {msg}"
        );
        assert_eq!(
            fake.call_count(),
            0,
            "another camp's daemon is never touched"
        );
        assert!(
            unit_path.exists(),
            "and another camp's unit is never removed"
        );
    }
}

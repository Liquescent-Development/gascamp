//! `camp service` (design §5): the control surface over the host's service
//! manager. Every flow takes the `Supervisor` PORT, so each is tested against
//! a real unit directory (a tempdir) with a faked process runner — no live
//! service manager anywhere in unit CI.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};
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
            indented_detail(&state.detail, "        ")
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

/// The manager's own words, kept verbatim (invariant 3) but kept INSIDE the
/// report's shape.
///
/// `launchctl print`'s failure stderr is several lines ("Bad request." then
/// the real reason), so interpolating it raw drops every line after the first
/// to column 0 and breaks the alignment of a report whose whole job is to be
/// read. Indent the continuation lines under the first instead: nothing is
/// dropped, nothing is summarized, and the block still reads as one field.
///
/// The indent is the CALLER's, because `status` and `list` set their fields at
/// different columns (m4) — a single hard-coded indent lines up under one of
/// them and not the other.
fn indented_detail(detail: &str, indent: &str) -> String {
    detail
        .lines()
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
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
    // Before ANY unit text is generated, any file is written, or any manager is
    // told this camp is supervised: a campd already on the socket means the
    // supervised one could never bind, and would be respawn-throttled forever
    // while we reported success.
    refuse_if_a_campd_holds_the_socket(supervisor, camp_root, "install")?;
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
    let mut report = format!(
        "uninstalled {} unit {} ({})\n",
        supervisor.name(),
        unit.name,
        unit.path.display()
    );
    // m5: uninstall took its own word for it too. Unloading stops the campd the
    // manager owned — but a campd it never started survives untouched, and the
    // camp is now unsupervised, so saying only "uninstalled" would leave a live
    // daemon unmentioned. Not an error (the unit really is gone, and `camp stop`
    // now works on it): a stated fact, with the remedy.
    if let Some(pid) = listening_campd_pid(camp_root)? {
        report.push_str(&format!(
            "note: a campd is still listening on this camp's socket (pid {pid}) — {} did not \
             start it, and this camp is now unsupervised. `camp stop` stops it.\n",
            supervisor.name()
        ));
    }
    Ok(report)
}

/// `camp service status` (design §5): the unit's load/run state, PLUS the
/// campd liveness answer. Two independent truths — a loaded unit whose campd
/// does not answer is precisely the fault worth seeing.
pub fn status(supervisor: Option<&dyn Supervisor>, camp: &CampDir) -> Result<String> {
    let mut report = String::new();
    match supervisor {
        None => report.push_str("unit:  no host service manager detected (container/CI?)\n"),
        // `managed_unit` — not a bare `unit_path.exists()` — so a unit that
        // names a different camp is reported as the loud collision it is,
        // rather than as this camp's state.
        Some(supervisor) => match managed_unit(supervisor, &camp.root)? {
            Some(unit) => {
                let state = supervisor.state(&unit.id)?;
                report.push_str(&format!(
                    "unit:  {} ({}, {})\n       loaded={} running={}  [{}]\n",
                    unit.name,
                    supervisor.name(),
                    unit.path.display(),
                    state.loaded,
                    state.running,
                    indented_detail(&state.detail, "       ")
                ));
            }
            None => {
                let id = CampId::for_camp(&camp.root)?;
                report.push_str(&format!(
                    "unit:  not installed ({} does not exist) — `camp service install`\n",
                    supervisor.unit_path(&id).display()
                ));
            }
        },
    }
    // Liveness is an ANSWERED REQUEST (spec §5 as amended by issue #55), never
    // a bare connect: a wedged campd's listen backlog accepts connections its
    // event loop never serves. This never auto-starts; a campd that accepts
    // and does not answer surfaces as the loud CampdUnresponsive error.
    match socket::request_if_up(camp, &Request::Status) {
        Ok(Some(Response::Status {
            summary,
            red,
            campd_pid,
            ..
        })) => report.push_str(&format!(
            "campd: listening (pid {campd_pid}) — {} live sessions, {} ready, {} red\n",
            summary.live_sessions.len(),
            summary.ready,
            red
        )),
        Ok(Some(other)) => bail!("unexpected response to status: {other:?}"),
        Ok(None) => report.push_str(&format!(
            "campd: not listening ({})\n",
            camp.socket_path().display()
        )),
        // A wedged campd (issue #55: accepts, never answers) must still fail
        // this command loudly (invariant 5 — never downgrade to `Ok`, a
        // script must not read exit 0 from a wedged daemon). But the unit
        // half of the report above is already fully built and true — losing
        // it to a bare `?` would hide `loaded=true running=true` behind the
        // campd fault, from the very command whose job is to show both. Fold
        // the report INTO the error instead: both truths reach the operator,
        // the campd fault (and its remedy) survives verbatim as the error's
        // cause, and the non-zero exit is untouched.
        //
        // F4 fix: `report` ends in `\n` (every line above is pushed with a
        // trailing newline), so handing it to `.context()` unchanged makes
        // anyhow's `: `-joined chain render a line that starts bare with
        // `: campd (pid …) …` — the wedge error, the flagship error this
        // whole feature exists to surface, reading as garbage. Trim the
        // trailing newline first so the chain joins onto the report's last
        // real line instead.
        Err(campd_error) => return Err(campd_error.context(report.trim_end().to_owned())),
    }
    Ok(report)
}

/// `camp service restart` (design §5): cycle the daemon — the post-upgrade
/// path (`launchctl kickstart -k` / `systemctl --user restart`).
pub fn restart(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String> {
    let unit = require_managed_unit(supervisor, camp_root, "`camp service install` first")?;
    supervisor.restart(&unit.id)?;
    Ok(format!(
        "restarted {} unit {} ({})\n",
        supervisor.name(),
        unit.name,
        unit.path.display()
    ))
}

/// Is a campd ANSWERING on this camp's socket, and at what pid?
///
/// The only honest test of whether a stop took effect. A unit's state is what
/// the MANAGER believes, and the manager only knows about campds it started —
/// one launched by hand (`camp daemon`, which is what `camp init` prints on a
/// manager-less host) is invisible to it and survives a stop of the unit
/// untouched. A wedged campd (accepts, never answers) surfaces as the loud
/// `CampdUnresponsive` error rather than as "gone": still not stopped.
fn listening_campd_pid(camp_root: &Path) -> Result<Option<u32>> {
    let camp = CampDir {
        root: camp_root.to_path_buf(),
    };
    match socket::request_if_up(&camp, &Request::Status)? {
        Some(Response::Status { campd_pid, .. }) => Ok(Some(campd_pid)),
        Some(other) => bail!("unexpected response to status: {other:?}"),
        None => Ok(None),
    }
}

/// Never hand the supervisor a camp whose socket another campd already owns.
///
/// `socket::bind_or_replace` is explicit: a socket that ACCEPTS means a live
/// campd, so a second one exits(1) rather than take it over. Under `KeepAlive`
/// / `Restart=always` the supervisor then respawns that doomed campd forever —
/// launchd every ~10s on an idle machine (invariant 1), systemd straight into
/// `failed` — while the verb that started it reported success. So both verbs
/// that hand campd to the supervisor ASK first, and refuse before touching the
/// unit directory or the manager.
///
/// This is not a hypothetical: it is the UPGRADE path. Every camp that exists
/// today has an auto-started, unsupervised campd, or was created with
/// `--no-service` and is running one from the `camp daemon` hand-off.
fn refuse_if_a_campd_holds_the_socket(
    supervisor: &dyn Supervisor,
    camp_root: &Path,
    verb: &str,
) -> Result<()> {
    if let Some(pid) = listening_campd_pid(camp_root)? {
        bail!(
            "a campd is already listening on this camp's socket (pid {pid}), and it is not one \
             {mgr} started. A supervised campd cannot take over a socket another campd owns — it \
             would exit immediately, and {mgr} would respawn it forever ({policy}) while this \
             command told you the camp was supervised.\n       Stop it first: camp stop\n       \
             Then: camp service {verb}",
            mgr = supervisor.name(),
            policy = supervisor.restart_policy(),
        );
    }
    Ok(())
}

/// `camp service stop` (operator decision, 2026-07-10): stop the supervised
/// campd — the verb `camp stop` sends a supervised operator to. The unit stays
/// INSTALLED; `camp service start` brings it back, `camp service uninstall`
/// removes it for good.
///
/// It VERIFIES its own effect, because two different things could otherwise
/// make it lie (§4.10: no verb may lie about its effect):
///
/// 1. `Supervisor::stop` is a no-op on a unit that is not loaded — launchd
///    cannot boot out a label it never bootstrapped, and `systemctl stop`
///    exits 0 on an inactive unit. Printing "stopped" for that is a claim of
///    an action that never happened, so the manager is asked FIRST.
/// 2. Stopping the unit cannot stop a campd the manager never started. If one
///    is still listening afterwards, this is the verb `camp stop` sent the
///    operator to — so it must not send them away satisfied. It names the pid
///    and the verb that CAN stop it.
pub fn stop(supervisor: &dyn Supervisor, camp: &CampDir) -> Result<String> {
    let unit = require_managed_unit(supervisor, &camp.root, "nothing to stop")?;
    // `will_restart_campd`, not `loaded`: on systemd `loaded` is true of a unit
    // that is inactive, dead or failed, so a `loaded` gate meant this verb
    // always ran a `systemctl stop` that did nothing and always printed
    // "stopped" — the claim-of-an-action-that-never-happened this check exists
    // to remove, on the whole of Linux. Only a supervisor that will actually
    // put campd back has anything here to stop.
    let was_supervised = supervisor.state(&unit.id)?.will_restart_campd;
    if was_supervised {
        supervisor.stop(&unit.id)?;
    }
    if let Some(pid) = listening_campd_pid(&camp.root)? {
        bail!(
            "the {mgr} unit {name} is {unit_state}, but a campd is STILL listening on this \
             camp's socket (pid {pid}) — stopping the unit did not stop it, so it is not the \
             campd {mgr} manages (a hand-run `camp daemon`, or one auto-started before the unit \
             was installed).\n       To stop it: camp stop",
            mgr = supervisor.name(),
            name = unit.name,
            unit_state = if was_supervised {
                "stopped"
            } else {
                "not running"
            },
        );
    }
    let headline = if was_supervised {
        format!(
            "stopped {} unit {} ({})",
            supervisor.name(),
            unit.name,
            unit.path.display()
        )
    } else {
        format!(
            "already stopped: the {} unit {} ({}) is not running",
            supervisor.name(),
            unit.name,
            unit.path.display()
        )
    };
    // The durability caveat is part of the effect, so it is stated: neither
    // manager forgets a stopped-but-installed unit across a login.
    Ok(format!(
        "{headline}\nthe unit is still installed — `camp service start` brings campd back, and \
         the host starts it again at your next login; `camp service uninstall` removes it for \
         good\n"
    ))
}

/// `camp service start` (operator decision, 2026-07-10): start a stopped but
/// still-installed unit.
pub fn start(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String> {
    let unit = require_managed_unit(supervisor, camp_root, "`camp service install` first")?;
    // Same reason as `install`: starting the unit while another campd holds the
    // socket produces a supervised campd that can never bind, respawned
    // forever, under a "started" the operator was told to trust.
    refuse_if_a_campd_holds_the_socket(supervisor, camp_root, "start")?;
    supervisor.start(&unit.id)?;
    Ok(format!(
        "started {} unit {} ({})\n",
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

pub fn run_status(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    // No supervisor is a normal state for `status` (a container still has a
    // campd to report on) — it is only fatal for the MUTATING verbs.
    let supervisor = service::host_supervisor(&probe, &runner)?;
    print!("{}", status(supervisor.as_deref(), camp)?);
    Ok(())
}

pub fn run_restart(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", restart(supervisor.as_ref(), &camp.root)?);
    Ok(())
}

pub fn run_stop(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", stop(supervisor.as_ref(), camp)?);
    Ok(())
}

pub fn run_start(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", start(supervisor.as_ref(), &camp.root)?);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::launchd::Launchd;
    use crate::service::runner::fake::FakeRunner;
    use crate::service::systemd::Systemd;
    use std::os::unix::net::UnixListener;
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
        // F5 fix (mirrors the `status` twin fix, finding 2): `report.contains
        // ("running")` is near-vacuous — `state.detail` carries launchd's
        // raw "state = running" text regardless of what `mark` computed, so
        // a `mark` bug that always rendered "loaded"/"not loaded" would
        // still leave a "running" substring sitting in the detail bracket.
        // Assert the computed mark AND the manager's own detail separately,
        // so a broken loaded/running parse fails this test.
        assert!(
            report.contains("dev-f9481b53  running  "),
            "the computed mark must be exactly \"running\": {report}"
        );
        assert!(
            report.contains("[state = running]"),
            "the manager's own detail: {report}"
        );
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

    /// Design §5: status is TWO independent truths — the unit's load/run state
    /// AND the campd liveness answer. A loaded unit whose campd does not
    /// answer is exactly the fault this command exists to show.
    #[test]
    fn status_reports_the_unit_and_the_campd_liveness_answer() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        let status_runner =
            FakeRunner::new(vec![FakeRunner::ok("service = {\n\tstate = running\n}\n")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &status_runner);
        let report = status(Some(&launchd), &camp).unwrap();

        // Finding 2 fix: `report.contains("running")` is near-vacuous — the
        // report line is `loaded={} running={}`, so a STOPPED unit
        // (running=false) also contains the substring "running". Assert the
        // actual state instead, plus the manager's own detail line, so a
        // broken `launchd::state` parse (e.g. `running` wrongly hardcoded)
        // fails this test.
        assert!(
            report.contains("loaded=true running=true"),
            "the unit's actual state: {report}"
        );
        assert!(
            report.contains("[state = running]"),
            "the manager's own detail: {report}"
        );
        // No campd is listening on this temp camp's socket — and that is a
        // REPORTED state, not an error, and never an auto-start.
        assert!(report.contains("campd: not listening"), "{report}");
    }

    /// M2 (review round 1): `launchctl print`'s failure stderr is MULTI-LINE
    /// ("Bad request." then the real reason). Interpolated raw, every line after
    /// the first dropped to column 0 and broke the shape of the very report this
    /// command exists to render. The manager's words must survive verbatim
    /// (invariant 3) — but INSIDE the report.
    #[test]
    fn status_keeps_a_multi_line_manager_detail_inside_the_report_shape() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        let runner = FakeRunner::new(vec![FakeRunner::fail(
            113,
            "Bad request.\nCould not find service in domain for user gui: 501\n",
        )]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &runner);
        let report = status(Some(&launchd), &camp).unwrap();

        // Nothing the manager said is dropped or summarized…
        assert!(report.contains("Bad request."), "{report}");
        assert!(
            report.contains("Could not find service in domain"),
            "{report}"
        );
        // …and no continuation line escapes to column 0.
        for line in report.lines() {
            assert!(
                line.starts_with("unit:")
                    || line.starts_with("campd:")
                    || line.starts_with(' ')
                    || line.is_empty(),
                "a continuation line must not drop to column 0: {line:?}\nin:\n{report}"
            );
        }
    }

    #[test]
    fn status_without_a_unit_says_so_and_names_the_remedy() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        let report = status(Some(&launchd), &camp).unwrap();
        assert!(report.contains("not installed"), "{report}");
        assert!(
            report.contains("camp service install"),
            "must name the remedy: {report}"
        );
        assert_eq!(
            fake.call_count(),
            0,
            "no unit file, nothing to ask the manager"
        );
    }

    /// In a container there is no unit — but campd's liveness is still the
    /// half of the answer that matters there.
    #[test]
    fn status_with_no_host_service_manager_still_answers_for_campd() {
        let camp_dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let report = status(None, &camp).unwrap();
        assert!(report.contains("no host service manager"), "{report}");
        assert!(report.contains("campd: not listening"), "{report}");
    }

    /// Finding 1 (fix wave 1 review): a WEDGED campd (accepts the
    /// connection, never answers — the shape `daemon/socket.rs`'s wedge
    /// tests simulate with a bare bound listener) must not make the
    /// already-built unit half of the report vanish. `status` must still
    /// fail loudly (non-zero exit — invariant 5) but the error it returns
    /// must carry BOTH truths: the unit's loaded/running state AND the
    /// campd fault text, remedy included.
    #[test]
    fn status_keeps_the_unit_half_when_campd_is_wedged() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        let status_runner =
            FakeRunner::new(vec![FakeRunner::ok("service = {\n\tstate = running\n}\n")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &status_runner);

        // The wedge simulator (daemon/socket.rs, issue #55): a bound
        // listener whose kernel backlog accepts the connection but whose
        // event loop never answers — exactly a campd stuck mid-syscall.
        let _wedged = UnixListener::bind(camp.socket_path()).unwrap();

        let err = status(Some(&launchd), &camp).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("loaded=true running=true"),
            "the unit half must survive a wedged campd: {msg}"
        );
        assert!(
            msg.contains("wedged") && msg.contains("kill -9"),
            "the campd fault must still be reported, with its remedy: {msg}"
        );
        // F4 fix: `report` (the unit half) ends in `\n`; folded into the
        // error unchanged, anyhow's `: `-joined chain would render a line
        // starting bare with `: campd (pid …) …` — the flagship wedge error
        // reading as garbage. `report.trim_end()` before `.context()` must
        // leave no such dangling separator.
        assert!(
            !msg.contains("\n: "),
            "the report's trailing newline must not leave a chain separator \
             starting a bare line: {msg}"
        );
    }

    #[test]
    fn restart_cycles_an_installed_unit_and_refuses_a_missing_one() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let missing = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &missing);
        let err = restart(&launchd, camp_dir.path()).unwrap_err();
        assert!(
            format!("{err:#}").contains("camp service install"),
            "{err:#}"
        );
        assert_eq!(missing.call_count(), 0);

        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp_dir.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let restart_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &restart_runner);
        let report = restart(&launchd, camp_dir.path()).unwrap();
        assert!(
            restart_runner
                .call(0)
                .starts_with("launchctl kickstart -k "),
            "{}",
            restart_runner.call(0)
        );
        assert!(report.contains("restarted"), "{report}");
    }

    /// `camp service stop` / `start` (operator decision, 2026-07-10): the
    /// supervisor-level verbs that `camp stop` points a supervised operator at.
    /// The unit STAYS installed — that is the whole difference from uninstall.
    #[test]
    fn stop_and_start_act_on_the_installed_unit_and_leave_it_installed() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp_dir.path(), Path::new("/usr/local/bin/camp")).unwrap();
        let id = crate::service::CampId::for_camp(camp_dir.path()).unwrap();
        let unit_path = launchd.unit_path(&id);

        // Three manager calls now: `stop` asks the state BEFORE it acts (so it
        // can only say "stopped" for a stop that really happened), then
        // `Launchd::stop` → `unload` re-checks and boots out. No campd is
        // listening afterwards, so the effect check passes and it reports the
        // stop it actually performed.
        let stop_runner = FakeRunner::new(vec![
            FakeRunner::ok("service = {\n\tstate = running\n}\n"),
            FakeRunner::ok("service = {\n\tstate = running\n}\n"),
            FakeRunner::ok(""),
        ]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &stop_runner);
        let report = stop(&launchd, &camp).unwrap();
        assert!(report.contains("stopped"), "{report}");
        assert!(
            !report.contains("already stopped"),
            "it really did stop a loaded unit: {report}"
        );
        assert!(
            stop_runner.call(2).starts_with("launchctl bootout "),
            "a loaded unit must actually be booted out: {}",
            stop_runner.call(2)
        );
        assert!(
            unit_path.exists(),
            "stop must NOT remove the unit (that is uninstall)"
        );

        let start_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &start_runner);
        let report = start(&launchd, camp_dir.path()).unwrap();
        assert!(
            start_runner.call(0).starts_with("launchctl bootstrap "),
            "{}",
            start_runner.call(0)
        );
        assert!(report.contains("started"), "{report}");
    }

    /// CRITICAL (review round 1). `camp service stop` must never report success
    /// while the campd it claims to have stopped is still answering.
    ///
    /// The state reproduced here is the one `camp service stop` itself leaves
    /// behind: the unit FILE stays on disk, booted out. A campd the supervisor
    /// never started can nonetheless be listening — a hand-run `camp daemon`,
    /// the very thing `camp init` prints on a manager-less host. Stopping the
    /// unit cannot stop THAT campd, because launchd never owned it: `unload`
    /// sees `loaded=false` and early-returns `Ok`, and the verb printed
    /// "stopped …" over the top of a daemon that never died. That is a verb
    /// lying about its effect — precisely what this branch's own §4.10 ruling
    /// forbids — and it strands the operator, because `camp stop` sends them
    /// to exactly this verb.
    #[test]
    fn service_stop_never_reports_success_while_a_campd_still_answers() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        // A campd the supervisor does not own, alive on this camp's socket.
        let campd = socket::fake_campd::serve(&camp, vec![socket::fake_campd::status(49602)]);
        // …and launchd has booted the unit out: it does not know the label.
        let runner = FakeRunner::new(vec![FakeRunner::fail(113, "Could not find service\n")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &runner);

        let result = stop(&launchd, &camp);
        let shown = match &result {
            Ok(report) => report.clone(),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            result.is_err(),
            "must not claim success while a campd still answers: {shown}"
        );
        assert!(
            shown.contains("49602"),
            "the error must name the still-live campd pid: {shown}"
        );
        assert_eq!(campd.served(), 1, "stop must ASK the socket, not assume");
    }

    /// The milder half of the same lie, and it needs no orphan at all: stopping
    /// an already-stopped unit must not claim to have stopped anything. The
    /// unit's END state is reported truthfully either way — what may not happen
    /// is a claim of an ACTION that did not occur.
    #[test]
    fn service_stop_on_an_already_stopped_unit_says_so_rather_than_claiming_a_stop() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        // Booted out already, and no campd anywhere: the honest answer is
        // "already stopped", and the manager must not be asked to stop again.
        let runner = FakeRunner::new(vec![FakeRunner::fail(113, "Could not find service\n")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &runner);

        let report = stop(&launchd, &camp).unwrap();
        assert!(
            report.contains("already stopped"),
            "must not claim an action it did not take: {report}"
        );
        assert_eq!(
            runner.call_count(),
            1,
            "only the state query — nothing to boot out: {report}"
        );
    }

    /// IMPORTANT 2 (review round 2). `install` is the UPGRADE PATH, and it
    /// needs no operator error to go wrong: every camp that exists today has an
    /// auto-started, unsupervised campd (or was created `--no-service`, where
    /// the README hands off to `camp daemon`). Installing a unit for such a
    /// camp hands the supervisor a socket another campd already owns —
    /// `bind_or_replace` makes the supervised campd exit(1) on a socket that
    /// accepts — and `KeepAlive`/`Restart=always` then respawns it forever
    /// (launchd: a standing spawn every ~10s on an idle machine, invariant 1;
    /// systemd: straight into `failed`), while `install` printed "campd for …
    /// is now supervised". Nothing is supervised. So: ASK first, refuse loudly,
    /// and touch neither the unit dir nor the manager.
    #[test]
    fn install_refuses_when_a_campd_is_already_listening() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let campd = socket::fake_campd::serve(&camp, vec![socket::fake_campd::status(4242)]);
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        let err = install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("4242"),
            "must name the campd already holding the socket: {msg}"
        );
        assert!(
            msg.contains("camp stop"),
            "must name the verb that frees the socket: {msg}"
        );
        assert_eq!(campd.served(), 1, "install must ASK the socket");
        assert_eq!(fake.call_count(), 0, "the manager must not be touched");
        assert!(
            std::fs::read_dir(units.path()).unwrap().next().is_none(),
            "no unit file may be written"
        );
    }

    /// IMPORTANT 2, the twin: `camp service start` has the identical shape —
    /// it printed "started …" without ever asking whether campd could come up.
    #[test]
    fn start_refuses_when_a_foreign_campd_is_listening() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        let campd = socket::fake_campd::serve(&camp, vec![socket::fake_campd::status(4242)]);
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        let err = start(&launchd, &camp.root).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("4242"), "must name the live campd: {msg}");
        assert_eq!(campd.served(), 1, "start must ASK the socket");
        assert_eq!(
            fake.call_count(),
            0,
            "the manager must not be asked to start a campd that cannot bind"
        );
    }

    /// CRITICAL (review round 2): the systemd twin of the "already stopped"
    /// test above. `LoadState=loaded` is true of an inactive, dead, stopped or
    /// failed unit — so keying on `loaded`, this verb always believed the unit
    /// was running, always ran a `systemctl stop` that did nothing (exit 0 on
    /// an inactive unit), and always printed "stopped systemd unit …". The
    /// "already stopped" branch was UNREACHABLE on the whole of Linux.
    #[test]
    fn service_stop_on_an_already_stopped_systemd_unit_says_so_rather_than_claiming_a_stop() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok(""), FakeRunner::ok("")]);
        let systemd = Systemd::new(units.path().to_path_buf(), &install_runner);
        install(&systemd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        // Exactly what `systemctl show` prints for a stopped unit.
        let runner = FakeRunner::new(vec![FakeRunner::ok(
            "LoadState=loaded\nActiveState=inactive\nSubState=dead\n",
        )]);
        let systemd = Systemd::new(units.path().to_path_buf(), &runner);

        let report = stop(&systemd, &camp).unwrap();
        assert!(
            report.contains("already stopped"),
            "must not claim an action it did not take: {report}"
        );
        assert_eq!(
            runner.call_count(),
            1,
            "an inactive unit needs no `systemctl stop`: {report}"
        );
    }

    /// The systemd twin of the orphan test: a campd the supervisor never
    /// started, still answering after the unit's stop. This one already held
    /// before round 2 (the socket check catches it either way) — it is here so
    /// that it KEEPS holding now that the predicate underneath it changed.
    #[test]
    fn service_stop_on_systemd_never_reports_success_while_a_campd_still_answers() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok(""), FakeRunner::ok("")]);
        let systemd = Systemd::new(units.path().to_path_buf(), &install_runner);
        install(&systemd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        let campd = socket::fake_campd::serve(&camp, vec![socket::fake_campd::status(31337)]);
        let runner = FakeRunner::new(vec![FakeRunner::ok(
            "LoadState=loaded\nActiveState=inactive\nSubState=dead\n",
        )]);
        let systemd = Systemd::new(units.path().to_path_buf(), &runner);

        let err = stop(&systemd, &camp).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("31337"), "must name the live campd pid: {msg}");
        assert_eq!(campd.served(), 1, "stop must ASK the socket, not assume");
    }

    /// An ACTIVE systemd unit really is stopped by `systemctl stop`, and the
    /// verb may say so.
    #[test]
    fn service_stop_on_an_active_systemd_unit_really_stops_it() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok(""), FakeRunner::ok("")]);
        let systemd = Systemd::new(units.path().to_path_buf(), &install_runner);
        install(&systemd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        let runner = FakeRunner::new(vec![
            FakeRunner::ok("LoadState=loaded\nActiveState=active\nSubState=running\n"),
            FakeRunner::ok(""), // systemctl --user stop
        ]);
        let systemd = Systemd::new(units.path().to_path_buf(), &runner);

        let report = stop(&systemd, &camp).unwrap();
        assert!(report.contains("stopped"), "{report}");
        assert!(!report.contains("already stopped"), "{report}");
        assert!(
            runner.call(1).starts_with("systemctl --user stop "),
            "an active unit must really be stopped: {}",
            runner.call(1)
        );
    }

    /// Stopping/starting what was never installed is an error, not a no-op.
    #[test]
    fn stop_and_start_without_a_unit_are_loud_errors() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        assert!(format!("{:#}", stop(&launchd, &camp).unwrap_err()).contains("no launchd unit"));
        assert!(
            format!("{:#}", start(&launchd, camp_dir.path()).unwrap_err())
                .contains("no launchd unit")
        );
        assert_eq!(fake.call_count(), 0);
    }
}

//! The supervisor port. One implementation per service manager. Everything
//! above this trait (the seven `camp service` flows, `camp init`, `camp stop`) is written
//! ONCE and works for every supervisor; adding a third (a container
//! supervisor, a BSD rc) is a new `impl Supervisor` plus one arm in
//! `supervisor_for` — nothing else moves.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::CampId;

/// The service manager's view of one unit. `detail` is the manager's OWN
/// words, printed verbatim (invariant 3: nothing hidden).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitState {
    /// The manager knows this unit. It does NOT mean the same thing in both
    /// managers, and it is not the predicate any verb should make a decision
    /// on — see `will_restart_campd`. launchd: the label is bootstrapped in
    /// `gui/<uid>`. systemd: `LoadState=loaded`, which says only that the unit
    /// FILE parsed and sits in systemd's memory — true of a unit that is
    /// inactive, dead, stopped, or failed.
    pub loaded: bool,
    /// campd is up, under the manager.
    pub running: bool,
    /// **The predicate the verbs decide on: will this supervisor put campd
    /// BACK if something stops it out from under the manager?**
    ///
    /// This is the whole question `camp stop`'s refusal and `camp service
    /// stop`'s "did I actually stop anything" both ask, and only the
    /// supervisor can answer it, because the two managers restart on entirely
    /// different conditions:
    ///
    /// - **launchd** — `KeepAlive` is unconditional: a bootstrapped job is
    ///   respawned whenever it exits, so BOOTSTRAPPED (`loaded`) is the answer.
    /// - **systemd** — `Restart=always` applies only to a unit that is running:
    ///   an inactive/dead/failed unit stays down. So ACTIVE (or activating) is
    ///   the answer, and `loaded` is nearly "the unit file exists".
    ///
    /// Keying the verbs on `loaded` therefore worked on launchd and was inert
    /// on systemd — `camp stop` refused forever on any camp with an installed
    /// unit, and `camp service stop` claimed a stop it never performed. Each
    /// supervisor computes this from its OWN raw output, where the distinction
    /// is still visible; nothing above this trait may re-derive it.
    pub will_restart_campd: bool,
    pub detail: String,
}

/// One installed unit, read back from the unit directory — which IS the
/// registry (design §5: no registry file, no status file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledUnit {
    pub id: CampId,
    pub camp_root: PathBuf,
    pub unit_path: PathBuf,
}

pub trait Supervisor {
    /// "launchd" | "systemd" — for operator-facing messages.
    fn name(&self) -> &'static str;

    /// The directory this supervisor's unit files live in — the shared base
    /// the default `unit_path` and `installed` below are built from.
    fn unit_dir(&self) -> &Path;

    /// The filename prefix common to every unit this supervisor manages
    /// (`com.gascamp.campd.` for launchd, `campd-` for systemd).
    fn unit_prefix(&self) -> &str;

    /// The filename suffix common to every unit this supervisor manages
    /// (`.plist` for launchd, `.service` for systemd).
    fn unit_suffix(&self) -> &str;

    /// PURE: where this camp's unit file lives —
    /// `unit_dir`/`unit_prefix``id``unit_suffix`. The same shape for every
    /// supervisor, so a third (a container supervisor, a BSD rc) needs only
    /// the three accessors above, per this module's doc: a default built
    /// from them, so no impl repeats it.
    fn unit_path(&self, id: &CampId) -> PathBuf {
        let (prefix, suffix) = (self.unit_prefix(), self.unit_suffix());
        self.unit_dir().join(format!("{prefix}{id}{suffix}"))
    }

    /// PURE: the camp root recorded in an installed unit's text — the exact
    /// inverse of `unit_text`. The unit is the source of truth.
    fn parse_camp_root(&self, unit_text: &str) -> Result<PathBuf>;

    /// The service manager's load/run state for this unit.
    fn state(&self, id: &CampId) -> Result<UnitState>;

    /// Every camp unit installed for this user, read from the unit directory.
    /// A default built from `scan_units` plus the three accessors above —
    /// every supervisor's `installed` was byte-identical in shape; this is
    /// now the one place that shape lives. `unit_path` is recomputed via the
    /// trait method, not the raw scan result: `unit_path` IS the source of
    /// truth for where a unit lives (it is also how `install`/`uninstall`
    /// find it), and `scan_units` necessarily agrees since it matches files
    /// by this same prefix/suffix.
    fn installed(&self) -> Result<Vec<InstalledUnit>> {
        scan_units(self.unit_dir(), self.unit_prefix(), self.unit_suffix())?
            .into_iter()
            .map(|(id, unit_path, text)| {
                let camp_root = self
                    .parse_camp_root(&text)
                    .with_context(|| format!("reading {}", unit_path.display()))?;
                Ok(InstalledUnit {
                    unit_path: self.unit_path(&id),
                    id,
                    camp_root,
                })
            })
            .collect()
    }

    /// PURE: the service manager's OWN name for this unit — a launchd label
    /// (`com.gascamp.campd.<id>`), a systemd unit name (`campd-<id>.service`).
    /// Operator-facing: every message about a unit names it.
    fn unit_name(&self, id: &CampId) -> String;

    /// PURE: the unit's text. `(camp id, camp root, camp binary) → plist /
    /// unit file`. No IO, no environment — this is the function design §9
    /// requires to be unit-tested without a live service manager.
    ///
    /// The paths arrive as `&str`, NOT `&Path`: `service::unit_safe_str` has
    /// already proven they are representable in a unit file. A generator that
    /// took `&Path` would need a lossy conversion, and a lossy conversion here
    /// produces a "successfully installed" unit pointing at a directory that
    /// does not exist (invariant 5).
    /// `path` is the PATH campd will run with. It is not optional and it is not
    /// cosmetic: a supervisor gives campd a minimal environment (launchd:
    /// `/usr/bin:/bin:/usr/sbin:/sbin`), and campd spawns `claude` and `git` by
    /// name. Without it a supervised campd cannot dispatch a single bead — see
    /// `service::campd_path`.
    fn unit_text(&self, id: &CampId, camp_root: &str, exe: &str, path: &str) -> String;

    /// Tell the service manager the unit DIRECTORY changed. Called after a
    /// unit file is written and after one is removed. launchd reads the plist
    /// at bootstrap, so it is a no-op there; systemd needs `daemon-reload`.
    fn reload_units(&self) -> Result<()>;

    /// Load + start an already-written unit.
    fn load(&self, id: &CampId) -> Result<()>;

    /// Stop + unload a unit. Its file is removed by the caller (the unit
    /// directory is the registry, and the flow that owns it does the IO).
    fn unload(&self, id: &CampId) -> Result<()>;

    /// Cycle the service (the post-upgrade path: a running campd keeps
    /// executing the OLD binary until it is restarted — design §1).
    fn restart(&self, id: &CampId) -> Result<()>;

    /// Stop the service, leaving the unit INSTALLED (operator decision,
    /// 2026-07-10: this is what `camp stop` points a supervised operator at —
    /// a socket stop would just be undone by the supervisor).
    fn stop(&self, id: &CampId) -> Result<()>;

    /// Start a stopped, still-installed unit.
    fn start(&self, id: &CampId) -> Result<()>;

    /// The always-on mechanism this supervisor uses to keep campd alive — the
    /// reason a socket-level `camp stop` would be undone. Operator-facing:
    /// `camp stop`'s refusal names it, so the operator can see WHY.
    fn restart_policy(&self) -> &'static str;
}

/// Shared by every supervisor: the unit DIRECTORY is the registry. Returns
/// `(id, unit path, unit text)` for every file named `<prefix><id><suffix>`,
/// sorted by id (stable output). A missing directory means zero units — not
/// an error; any other IO failure is loud.
pub fn scan_units(
    dir: &Path,
    prefix: &str,
    suffix: &str,
) -> Result<Vec<(CampId, PathBuf, String)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut units = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(rest) = name.strip_prefix(prefix) else {
            continue;
        };
        let Some(slug) = rest.strip_suffix(suffix) else {
            continue;
        };
        let id = CampId::from_slug(slug)?;
        let path = entry.path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        units.push((id, path, text));
    }
    units.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(units)
}

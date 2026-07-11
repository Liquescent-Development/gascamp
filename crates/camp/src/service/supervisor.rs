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
    pub loaded: bool,
    pub running: bool,
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

    /// PURE: where this camp's unit file lives.
    fn unit_path(&self, id: &CampId) -> PathBuf;

    /// PURE: the camp root recorded in an installed unit's text — the exact
    /// inverse of `unit_text`. The unit is the source of truth.
    fn parse_camp_root(&self, unit_text: &str) -> Result<PathBuf>;

    /// The service manager's load/run state for this unit.
    fn state(&self, id: &CampId) -> Result<UnitState>;

    /// Every camp unit installed for this user, read from the unit directory.
    fn installed(&self) -> Result<Vec<InstalledUnit>>;
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

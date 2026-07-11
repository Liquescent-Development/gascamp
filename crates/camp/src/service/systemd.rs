//! systemd (Linux): a per-user unit in the `--user` manager, at
//! `$XDG_CONFIG_HOME/systemd/user/campd-<camp-id>.service`
//! (default `~/.config/systemd/user`).

use std::ffi::OsStr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::CampId;
use super::runner::{CommandRunner, run_checked};
use super::supervisor::{InstalledUnit, Supervisor, UnitState, scan_units};

/// Every camp unit's name starts with this — `camp service list` finds
/// managed camps by it (design §5).
pub const UNIT_PREFIX: &str = "campd-";
const UNIT_SUFFIX: &str = ".service";

pub struct Systemd<'a> {
    unit_dir: PathBuf,
    runner: &'a dyn CommandRunner,
}

impl<'a> Systemd<'a> {
    pub fn new(unit_dir: PathBuf, runner: &'a dyn CommandRunner) -> Systemd<'a> {
        Systemd { unit_dir, runner }
    }

    fn unit_name(&self, id: &CampId) -> String {
        format!("{UNIT_PREFIX}{id}{UNIT_SUFFIX}")
    }
}

impl Supervisor for Systemd<'_> {
    fn name(&self) -> &'static str {
        "systemd"
    }

    fn unit_path(&self, id: &CampId) -> PathBuf {
        self.unit_dir.join(self.unit_name(id))
    }

    fn parse_camp_root(&self, unit_text: &str) -> Result<PathBuf> {
        let exec = unit_text
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .context("this unit has no ExecStart= line")?;
        let args = split_exec(exec);
        let root = args
            .iter()
            .position(|arg| arg == "--camp")
            .and_then(|i| args.get(i + 1))
            .context("this unit's ExecStart has no `--camp <dir>`")?;
        Ok(PathBuf::from(root))
    }

    fn state(&self, id: &CampId) -> Result<UnitState> {
        // One machine-readable call. `show` exits 0 even for a unit systemd
        // has never heard of (LoadState=not-found), so this is a state query,
        // not a failure path.
        let name = self.unit_name(id);
        let out = run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("show"),
                OsStr::new(&name),
                OsStr::new("--property=LoadState"),
                OsStr::new("--property=ActiveState"),
                OsStr::new("--property=SubState"),
            ],
        )?;
        let value = |key: &str| -> String {
            out.stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let load = value("LoadState=");
        let active = value("ActiveState=");
        let sub = value("SubState=");
        Ok(UnitState {
            loaded: load == "loaded",
            running: active == "active",
            detail: format!("LoadState={load} ActiveState={active} SubState={sub}"),
        })
    }

    fn installed(&self) -> Result<Vec<InstalledUnit>> {
        scan_units(&self.unit_dir, UNIT_PREFIX, UNIT_SUFFIX)?
            .into_iter()
            .map(|(id, unit_path, text)| {
                let camp_root = self
                    .parse_camp_root(&text)
                    .with_context(|| format!("reading {}", unit_path.display()))?;
                // Recomputed via the trait method, not the raw scan result:
                // `unit_path` IS the source of truth for where a unit lives
                // (it is also how a future `install`/`uninstall` will find
                // it), and scan_units necessarily agrees since it matches
                // files by this same prefix/suffix.
                Ok(InstalledUnit {
                    unit_path: self.unit_path(&id),
                    id,
                    camp_root,
                })
            })
            .collect()
    }
}

/// systemd's `ExecStart` quoting, in reverse: double-quoted arguments (a camp
/// path may contain spaces) with `\"` and `\\` escapes; bare arguments split
/// on whitespace.
fn split_exec(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' if !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::runner::fake::FakeRunner;

    const UNIT: &str = "[Unit]\nDescription=Gas Camp daemon (campd)\n\n[Service]\nType=simple\nExecStart=\"/usr/local/bin/camp\" daemon --camp \"/home/x/my camps/.camp\"\nRestart=always\n";

    fn id() -> CampId {
        CampId::from_slug("dev-f9481b53").unwrap()
    }

    #[test]
    fn unit_path_is_the_user_unit() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/home/x/.config/systemd/user"), &fake);
        assert_eq!(
            systemd.unit_path(&id()),
            PathBuf::from("/home/x/.config/systemd/user/campd-dev-f9481b53.service")
        );
    }

    #[test]
    fn parse_camp_root_reads_exec_start_through_its_quoting() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        assert_eq!(
            systemd.parse_camp_root(UNIT).unwrap(),
            PathBuf::from("/home/x/my camps/.camp")
        );
        assert!(
            systemd
                .parse_camp_root("[Service]\nExecStart=/bin/true\n")
                .is_err(),
            "a unit with no --camp is a loud error, never a guess"
        );
    }

    #[test]
    fn state_reads_systemctl_show() {
        let fake = FakeRunner::new(vec![FakeRunner::ok(
            "LoadState=loaded\nActiveState=active\nSubState=running\n",
        )]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let state = systemd.state(&id()).unwrap();
        assert_eq!(
            state,
            UnitState {
                loaded: true,
                running: true,
                detail: "LoadState=loaded ActiveState=active SubState=running".to_owned()
            }
        );
        assert_eq!(
            fake.call(0),
            "systemctl --user show campd-dev-f9481b53.service \
             --property=LoadState --property=ActiveState --property=SubState"
        );

        let unknown = FakeRunner::new(vec![FakeRunner::ok(
            "LoadState=not-found\nActiveState=inactive\nSubState=dead\n",
        )]);
        let systemd = Systemd::new(PathBuf::from("/units"), &unknown);
        let state = systemd.state(&id()).unwrap();
        assert!(!state.loaded && !state.running, "{state:?}");
    }

    #[test]
    fn installed_enumerates_the_unit_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("campd-dev-f9481b53.service"), UNIT).unwrap();
        std::fs::write(dir.path().join("pipewire.service"), "[Unit]\n").unwrap();

        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(dir.path().to_path_buf(), &fake);
        let units = systemd.installed().unwrap();
        assert_eq!(units.len(), 1, "only camp units: {units:?}");
        assert_eq!(units[0].id, id());
        assert_eq!(units[0].camp_root, PathBuf::from("/home/x/my camps/.camp"));
    }
}

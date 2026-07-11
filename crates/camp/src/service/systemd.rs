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

    fn unit_name(&self, id: &CampId) -> String {
        format!("{UNIT_PREFIX}{id}{UNIT_SUFFIX}")
    }

    fn unit_text(&self, _id: &CampId, camp_root: &str, exe: &str) -> String {
        // Restart=always (design §4.2, always-on). Output goes to the journal
        // (`journalctl --user -u campd-<id>`): visible, not swallowed. The
        // paths are `&str` that `unit_safe_str` vouched for — control-character
        // free, so neither the unquoted Description= nor the line-oriented
        // parse can be structurally corrupted by a path.
        //
        // systemd expands `%`-specifiers (e.g. `%h` → the invoking user's
        // home directory) EVERYWHERE in a unit file, not only `ExecStart` —
        // `Description=` included. A literal `%` in the camp path or the
        // binary path must be escaped `%%` in every field it is interpolated
        // into, or systemd substitutes something else entirely (or, for a
        // specifier it does not recognize, refuses to load the unit at all).
        // `escape_percent` runs BEFORE `quote`, so the doubled `%%` is itself
        // quoted verbatim in `ExecStart`; `split_exec` undoes the quoting
        // first and `unescape_percent` undoes this last, making
        // `parse_camp_root` the exact inverse of this function.
        //
        // `camp_root` is escaped once and reused for both `Description=`
        // (unquoted) and `ExecStart` (quoted): every place this function
        // interpolates the camp root into a field systemd expands specifiers
        // in must see the escaped form.
        let camp_root = escape_percent(camp_root);
        format!(
            "[Unit]\n\
             Description=Gas Camp daemon (campd) for {camp_root}\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={exe} daemon --camp {camp}\n\
             Restart=always\n\
             RestartSec=1\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            exe = quote(&escape_percent(exe)),
            camp = quote(&camp_root),
        )
    }

    fn reload_units(&self) -> Result<()> {
        run_checked(
            self.runner,
            "systemctl",
            &[OsStr::new("--user"), OsStr::new("daemon-reload")],
        )?;
        Ok(())
    }

    fn load(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("enable"),
                OsStr::new("--now"),
                OsStr::new(&name),
            ],
        )?;
        Ok(())
    }

    fn unload(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("disable"),
                OsStr::new("--now"),
                OsStr::new(&name),
            ],
        )?;
        Ok(())
    }

    fn restart(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("restart"),
                OsStr::new(&name),
            ],
        )?;
        Ok(())
    }

    fn stop(&self, id: &CampId) -> Result<()> {
        // Unlike launchd, systemd separates the service from the unit: `stop`
        // leaves it ENABLED (it returns at the next login), `disable --now`
        // (our `unload`) does not.
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[OsStr::new("--user"), OsStr::new("stop"), OsStr::new(&name)],
        )?;
        Ok(())
    }

    fn start(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[OsStr::new("--user"), OsStr::new("start"), OsStr::new(&name)],
        )?;
        Ok(())
    }

    fn restart_policy(&self) -> &'static str {
        "Restart=always"
    }
}

/// systemd's `ExecStart` quoting, in reverse: double-quoted arguments (a camp
/// path may contain spaces) with `\"` and `\\` escapes; bare arguments split
/// on whitespace. The LAST step undoes `escape_percent` (`%%` → `%`) — the
/// inverse must run after quote-unescaping, since `%` plays no part in
/// systemd's quoting and quote-unescaping never touches it.
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
                    args.push(unescape_percent(&std::mem::take(&mut current)));
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
        args.push(unescape_percent(&current));
    }
    args
}

/// systemd's `ExecStart` quoting: every argument double-quoted, with `\` and
/// `"` escaped — a camp path with a space must reach campd verbatim. The
/// inverse of `split_exec`.
fn quote(arg: &str) -> String {
    let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// `%` → `%%`: systemd treats a lone `%` in `ExecStart` as the start of a
/// specifier (`%h`, `%u`, …) and substitutes it; a literal `%` must be
/// doubled to survive. Every `%` is doubled, so the mapping is a bijection —
/// `unescape_percent` (non-overlapping `%%` → `%`) is its exact inverse for
/// any run length, including a source path that itself contains `%%`.
fn escape_percent(text: &str) -> String {
    text.replace('%', "%%")
}

/// The inverse of `escape_percent`.
fn unescape_percent(text: &str) -> String {
    text.replace("%%", "%")
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

    /// Design §5: `ExecStart=camp daemon --camp <dir>`, `Restart=always`.
    /// PURE, and pinned as a golden.
    #[test]
    fn unit_text_is_the_restart_always_user_unit() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let text = systemd.unit_text(&id(), "/home/x/camps/dev/.camp", "/usr/local/bin/camp");
        assert_eq!(
            text,
            "[Unit]\n\
             Description=Gas Camp daemon (campd) for /home/x/camps/dev/.camp\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart=\"/usr/local/bin/camp\" daemon --camp \"/home/x/camps/dev/.camp\"\n\
             Restart=always\n\
             RestartSec=1\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );
    }

    /// A camp path may contain spaces or a quote; systemd's ExecStart quoting
    /// must survive the round trip back to the exact path.
    #[test]
    fn unit_text_quotes_exec_start_and_round_trips() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let root = "/home/x/my \"camps\"/.camp";
        let text = systemd.unit_text(&id(), root, "/usr/local/bin/camp");
        assert_eq!(systemd.parse_camp_root(&text).unwrap(), PathBuf::from(root));
    }

    /// systemd expands `%` specifiers (e.g. `%h` → the invoking user's home
    /// directory) in `ExecStart`. A literal `%` in a camp path must be
    /// escaped `%%`, or systemd substitutes something else entirely, the
    /// unit names a directory that does not exist, `install` reports
    /// success, and campd crash-loops forever under `Restart=always`. The
    /// round trip through `parse_camp_root` must return the path UNCHANGED.
    #[test]
    fn unit_text_escapes_percent_specifiers_and_round_trips() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let root = "/home/x/100%h/.camp";
        let text = systemd.unit_text(&id(), root, "/usr/local/bin/camp");
        assert!(
            text.contains("100%%h"),
            "a literal `%` must be escaped to `%%` in ExecStart: {text}"
        );
        assert_eq!(systemd.parse_camp_root(&text).unwrap(), PathBuf::from(root));
    }

    /// systemd expands `%`-specifiers in `Description=` too, not only
    /// `ExecStart=`. A camp root containing a literal `%` (e.g. `50%off`) is a
    /// perfectly legal path, but `%o` is not a valid specifier: an unescaped
    /// `Description=` makes systemd refuse to load the WHOLE unit, so
    /// `install` fails and the camp becomes permanently uninstallable via a
    /// cryptic systemd error. The escaping must cover every generated field,
    /// not just `ExecStart=`.
    #[test]
    fn unit_text_escapes_percent_in_description_too() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let root = "/home/x/50%off/.camp";
        let text = systemd.unit_text(&id(), root, "/usr/local/bin/camp");
        let description = text
            .lines()
            .find(|line| line.starts_with("Description="))
            .expect("unit text must have a Description= line");
        assert!(
            description.contains("50%%off"),
            "a literal % in Description= must be escaped to %%, or systemd's \
             specifier expansion corrupts or refuses the whole unit: {text}"
        );
        assert!(
            !description.contains("50%off"),
            "a raw, unescaped % must never survive into Description=: {text}"
        );
    }

    /// The escape/unescape pair must be a true round trip through the WHOLE
    /// generated unit (Description= included), for every shape of `%` a camp
    /// path might contain: a lone `%`, an already-doubled `%%`, a trailing
    /// lone `%`, and a real specifier-looking `%h`.
    #[test]
    fn unit_text_round_trips_every_percent_shape() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        for root in [
            "/home/x/50%off/.camp",
            "/home/x/50%%off/.camp",
            "/home/x/trailing%/.camp",
            "/home/x/%h/.camp",
        ] {
            let text = systemd.unit_text(&id(), root, "/usr/local/bin/camp");
            assert_eq!(
                systemd.parse_camp_root(&text).unwrap(),
                PathBuf::from(root),
                "round trip broke for root {root:?}: {text}"
            );
        }
    }

    #[test]
    fn unit_name_is_the_systemd_unit_name() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        assert_eq!(systemd.unit_name(&id()), "campd-dev-f9481b53.service");
    }

    #[test]
    fn load_enables_and_starts_the_unit() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.load(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "systemctl --user enable --now campd-dev-f9481b53.service"
        );
    }

    #[test]
    fn unload_disables_and_stops_the_unit() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.unload(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "systemctl --user disable --now campd-dev-f9481b53.service"
        );
    }

    #[test]
    fn reload_units_tells_systemd_the_unit_dir_changed() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.reload_units().unwrap();
        assert_eq!(fake.call(0), "systemctl --user daemon-reload");
    }

    /// Design §5: restart = `systemctl --user restart`.
    #[test]
    fn restart_restarts_the_unit() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.restart(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "systemctl --user restart campd-dev-f9481b53.service"
        );
    }

    /// The operator's remedy (2026-07-10). Unlike launchd, systemd separates
    /// "stop the service" from "unload the unit": `stop` leaves it enabled
    /// (so it returns at login), `disable --now` (that is `unload`) does not.
    #[test]
    fn stop_and_start_are_the_unit_level_verbs() {
        let stopping = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &stopping);
        systemd.stop(&id()).unwrap();
        assert_eq!(
            stopping.call(0),
            "systemctl --user stop campd-dev-f9481b53.service"
        );

        let starting = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &starting);
        systemd.start(&id()).unwrap();
        assert_eq!(
            starting.call(0),
            "systemctl --user start campd-dev-f9481b53.service"
        );
    }
}

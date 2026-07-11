//! launchd (macOS): a per-user LaunchAgent in the `gui/<uid>` domain, at
//! `~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist`.

use std::ffi::OsStr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::CampId;
use super::runner::{CommandRunner, run_checked};
use super::supervisor::{InstalledUnit, Supervisor, UnitState, scan_units};

/// Every camp unit's label starts with this — `camp service list` finds
/// managed camps by it (design §5).
pub const LABEL_PREFIX: &str = "com.gascamp.campd.";
const PLIST_SUFFIX: &str = ".plist";

pub struct Launchd<'a> {
    unit_dir: PathBuf,
    uid: u32,
    runner: &'a dyn CommandRunner,
}

impl<'a> Launchd<'a> {
    pub fn new(unit_dir: PathBuf, uid: u32, runner: &'a dyn CommandRunner) -> Launchd<'a> {
        Launchd {
            unit_dir,
            uid,
            runner,
        }
    }

    fn label(&self, id: &CampId) -> String {
        format!("{LABEL_PREFIX}{id}")
    }

    /// launchd's service target: `gui/<uid>/<label>`.
    fn service_target(&self, id: &CampId) -> String {
        format!("gui/{}/{}", self.uid, self.label(id))
    }

    /// launchd's per-user domain target.
    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }
}

impl Supervisor for Launchd<'_> {
    fn name(&self) -> &'static str {
        "launchd"
    }

    fn unit_path(&self, id: &CampId) -> PathBuf {
        self.unit_dir
            .join(format!("{LABEL_PREFIX}{id}{PLIST_SUFFIX}"))
    }

    fn parse_camp_root(&self, unit_text: &str) -> Result<PathBuf> {
        // ProgramArguments IS the truth: the <string> after the "--camp"
        // <string>. No duplicated marker to drift out of sync.
        let args: Vec<String> = unit_text
            .split("<string>")
            .skip(1)
            .filter_map(|chunk| chunk.split("</string>").next())
            .map(xml_unescape)
            .collect();
        let root = args
            .iter()
            .position(|arg| arg == "--camp")
            .and_then(|i| args.get(i + 1))
            .context("this unit has no `--camp <dir>` in its ProgramArguments")?;
        Ok(PathBuf::from(root))
    }

    fn state(&self, id: &CampId) -> Result<UnitState> {
        let target = self.service_target(id);
        let out = self
            .runner
            .run("launchctl", &[OsStr::new("print"), OsStr::new(&target)])?;
        if !out.success() {
            // launchd does not know this label: the plist may exist while the
            // unit is booted out. A STATE, not an error.
            return Ok(UnitState {
                loaded: false,
                running: false,
                detail: out.stderr.trim().to_owned(),
            });
        }
        let state_line = out
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("state = "))
            .unwrap_or("state = unknown");
        Ok(UnitState {
            loaded: true,
            running: state_line == "state = running",
            detail: state_line.to_owned(),
        })
    }

    fn installed(&self) -> Result<Vec<InstalledUnit>> {
        scan_units(&self.unit_dir, LABEL_PREFIX, PLIST_SUFFIX)?
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
        self.label(id)
    }

    fn unit_text(&self, id: &CampId, camp_root: &str, exe: &str) -> String {
        // KeepAlive (design §4.2, always-on): the supervisor keeps campd
        // alive; a crash is restarted. StandardErrorPath is the camp's own
        // campd.log (CampDir::log_path) — a supervised daemon's stderr is
        // never swallowed (invariant 3). No lossy conversion anywhere: the
        // caller passed strings `unit_safe_str` already vouched for.
        let log = format!("{camp_root}/campd.log");
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>{root}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/dev/null</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
            label = xml_escape(&self.label(id)),
            exe = xml_escape(exe),
            root = xml_escape(camp_root),
            log = xml_escape(&log),
        )
    }

    fn reload_units(&self) -> Result<()> {
        // launchd reads the plist at bootstrap time: there is nothing to
        // reload. Stated, not silently skipped.
        Ok(())
    }

    fn load(&self, id: &CampId) -> Result<()> {
        let unit_path = self.unit_path(id);
        run_checked(
            self.runner,
            "launchctl",
            &[
                OsStr::new("bootstrap"),
                OsStr::new(&self.domain()),
                unit_path.as_os_str(),
            ],
        )?;
        Ok(())
    }

    fn unload(&self, id: &CampId) -> Result<()> {
        // `bootout` on a label launchd never bootstrapped fails. We do not
        // guess and we do not silence a failure: we ASK for the state and act
        // on the answer. A bootout of a LOADED unit that fails is still loud.
        if !self.state(id)?.loaded {
            return Ok(());
        }
        run_checked(
            self.runner,
            "launchctl",
            &[OsStr::new("bootout"), OsStr::new(&self.service_target(id))],
        )?;
        Ok(())
    }

    fn restart(&self, id: &CampId) -> Result<()> {
        run_checked(
            self.runner,
            "launchctl",
            &[
                OsStr::new("kickstart"),
                OsStr::new("-k"),
                OsStr::new(&self.service_target(id)),
            ],
        )?;
        Ok(())
    }

    fn stop(&self, id: &CampId) -> Result<()> {
        // launchd has no "stop but stay bootstrapped" for a KeepAlive agent:
        // `launchctl kill` sends a signal and KeepAlive restarts it. Stopping
        // IS booting out of the gui domain — the plist stays on disk, which is
        // exactly what separates this from `uninstall`. Same operation as
        // `unload`, stated rather than aliased so the intent is readable.
        self.unload(id)
    }

    fn start(&self, id: &CampId) -> Result<()> {
        // …and starting is bootstrapping the still-present plist back in.
        self.load(id)
    }

    fn restart_policy(&self) -> &'static str {
        "KeepAlive"
    }
}

/// A camp path may legally contain `&` or `<`; an escaped plist must survive
/// the round trip back to the real path.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// XML text escaping for the plist. A camp path may legally contain `&` or
/// `<`; an unescaped one is a corrupt plist launchd refuses to load. `&` FIRST
/// (it is the escape introducer), the inverse of `xml_unescape`.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::runner::fake::FakeRunner;

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.gascamp.campd.dev-f9481b53</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/camp</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>/Users/x/camps/My Camp &amp; Co/.camp</string>
  </array>
</dict>
</plist>
"#;

    fn id() -> CampId {
        CampId::from_slug("dev-f9481b53").unwrap()
    }

    #[test]
    fn unit_path_is_the_launch_agent_plist() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/Users/x/Library/LaunchAgents"), 501, &fake);
        assert_eq!(
            launchd.unit_path(&id()),
            PathBuf::from("/Users/x/Library/LaunchAgents/com.gascamp.campd.dev-f9481b53.plist")
        );
    }

    /// The unit is the source of truth (design §5: no registry file). The
    /// camp root is read back out of ProgramArguments — the real datum, not
    /// a duplicated marker — and XML-unescaped.
    #[test]
    fn parse_camp_root_reads_the_program_arguments() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        assert_eq!(
            launchd.parse_camp_root(PLIST).unwrap(),
            PathBuf::from("/Users/x/camps/My Camp & Co/.camp")
        );
        assert!(
            launchd.parse_camp_root("<plist></plist>").is_err(),
            "a plist with no --camp is a loud error, never a guess"
        );
    }

    #[test]
    fn state_reads_launchctl_print() {
        let running = FakeRunner::new(vec![FakeRunner::ok(
            "com.gascamp.campd.dev-f9481b53 = {\n\tstate = running\n\tpid = 4242\n}\n",
        )]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &running);
        let state = launchd.state(&id()).unwrap();
        assert_eq!(
            state,
            UnitState {
                loaded: true,
                running: true,
                detail: "state = running".to_owned()
            }
        );
        assert_eq!(
            running.call(0),
            "launchctl print gui/501/com.gascamp.campd.dev-f9481b53"
        );

        // Booted out: launchctl does not know the label. A STATE, not an error.
        let absent = FakeRunner::new(vec![FakeRunner::fail(113, "Could not find service\n")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &absent);
        let state = launchd.state(&id()).unwrap();
        assert!(!state.loaded && !state.running, "{state:?}");
        assert_eq!(state.detail, "Could not find service");
    }

    /// `list`'s source of truth: the unit DIRECTORY. Files that are not ours
    /// are ignored; a missing directory means zero units, not an error.
    #[test]
    fn installed_enumerates_the_unit_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("com.gascamp.campd.dev-f9481b53.plist"),
            PLIST,
        )
        .unwrap();
        std::fs::write(dir.path().join("com.apple.something.plist"), "<plist/>").unwrap();

        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(dir.path().to_path_buf(), 501, &fake);
        let units = launchd.installed().unwrap();
        assert_eq!(
            units.len(),
            1,
            "only camp units, and every camp unit: {units:?}"
        );
        assert_eq!(units[0].id, id());
        assert_eq!(
            units[0].camp_root,
            PathBuf::from("/Users/x/camps/My Camp & Co/.camp")
        );
        assert_eq!(
            units[0].unit_path,
            dir.path().join("com.gascamp.campd.dev-f9481b53.plist")
        );

        let missing = Launchd::new(dir.path().join("nope"), 501, &fake);
        assert!(
            missing.installed().unwrap().is_empty(),
            "no unit dir = no units"
        );
    }

    /// Design §5: `ProgramArguments = camp daemon --camp <dir>`, `RunAtLoad`
    /// plus `KeepAlive`. PURE: a path in, the plist text out. Pinned as a
    /// golden: a supervisor's unit file is an operator-visible artifact.
    /// Note the `&str` parameters: `unit_safe_str` has ALREADY proven the
    /// paths are representable, so no lossy conversion can hide in here.
    #[test]
    fn unit_text_is_the_keepalive_launch_agent() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let text = launchd.unit_text(&id(), "/Users/x/camps/dev/.camp", "/usr/local/bin/camp");
        assert_eq!(
            text,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.gascamp.campd.dev-f9481b53</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/camp</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>/Users/x/camps/dev/.camp</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/dev/null</string>
  <key>StandardErrorPath</key>
  <string>/Users/x/camps/dev/.camp/campd.log</string>
</dict>
</plist>
"#
        );
    }

    /// A camp path may contain XML metacharacters. An unescaped `&` is a
    /// corrupt plist launchd refuses — and generation must survive the round
    /// trip back to the exact path.
    #[test]
    fn unit_text_escapes_xml_and_round_trips() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let root = "/Users/x/camps/R&D <beta>/.camp";
        let text = launchd.unit_text(&id(), root, "/usr/local/bin/camp");
        assert!(text.contains("R&amp;D &lt;beta&gt;"), "{text}");
        assert!(
            !text.contains("R&D <beta>"),
            "raw metacharacters leaked: {text}"
        );
        assert_eq!(launchd.parse_camp_root(&text).unwrap(), PathBuf::from(root));
    }

    #[test]
    fn unit_name_is_the_launchd_label() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        assert_eq!(launchd.unit_name(&id()), "com.gascamp.campd.dev-f9481b53");
    }

    #[test]
    fn load_bootstraps_the_agent_into_the_gui_domain() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        launchd.load(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "launchctl bootstrap gui/501 /units/com.gascamp.campd.dev-f9481b53.plist"
        );
    }

    /// A launchctl failure is LOUD, carrying launchd's own words.
    #[test]
    fn a_failed_bootstrap_is_a_loud_error() {
        let fake = FakeRunner::new(vec![FakeRunner::fail(5, "Input/output error\n")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let err = launchd.load(&id()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Input/output error"),
            "must carry launchd's stderr: {msg}"
        );
    }

    /// `bootout` on a unit launchd never bootstrapped fails. We do not guess
    /// and we do not silence: we ASK (`state`) and act on the answer.
    #[test]
    fn unload_boots_out_a_loaded_unit_and_skips_an_unloaded_one() {
        let loaded = FakeRunner::new(vec![
            FakeRunner::ok("com.gascamp.campd.dev-f9481b53 = {\n\tstate = running\n}\n"),
            FakeRunner::ok(""),
        ]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &loaded);
        launchd.unload(&id()).unwrap();
        assert_eq!(
            loaded.call(1),
            "launchctl bootout gui/501/com.gascamp.campd.dev-f9481b53"
        );

        let absent = FakeRunner::new(vec![FakeRunner::fail(113, "Could not find service\n")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &absent);
        launchd.unload(&id()).unwrap();
        assert_eq!(
            absent.call_count(),
            1,
            "nothing to boot out: only the state query"
        );
    }

    /// Design §5: restart = `launchctl kickstart -k` (the post-upgrade cycle).
    #[test]
    fn restart_kickstarts_the_service() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        launchd.restart(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "launchctl kickstart -k gui/501/com.gascamp.campd.dev-f9481b53"
        );
    }

    /// The operator's remedy (2026-07-10). launchd has no "stop but stay
    /// bootstrapped" for a KeepAlive agent — `launchctl kill` would just be
    /// restarted — so stopping IS booting out of the domain, and starting IS
    /// bootstrapping back in. The plist stays on disk (that is what makes this
    /// `stop`, not `uninstall`).
    #[test]
    fn stop_boots_the_agent_out_and_start_bootstraps_it_back() {
        let stopping = FakeRunner::new(vec![
            FakeRunner::ok("service = {\n\tstate = running\n}\n"), // state: loaded
            FakeRunner::ok(""),                                    // bootout
        ]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &stopping);
        launchd.stop(&id()).unwrap();
        assert_eq!(
            stopping.call(1),
            "launchctl bootout gui/501/com.gascamp.campd.dev-f9481b53"
        );

        let starting = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &starting);
        launchd.start(&id()).unwrap();
        assert_eq!(
            starting.call(0),
            "launchctl bootstrap gui/501 /units/com.gascamp.campd.dev-f9481b53.plist"
        );
    }
}

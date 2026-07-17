//! launchd (macOS): a per-user LaunchAgent in the `gui/<uid>` domain, at
//! `~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::CampId;
use super::runner::{CommandRunner, run_checked};
use super::supervisor::{Supervisor, UnitState};

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

    fn unit_dir(&self) -> &Path {
        &self.unit_dir
    }

    fn unit_prefix(&self) -> &str {
        LABEL_PREFIX
    }

    fn unit_suffix(&self) -> &str {
        PLIST_SUFFIX
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

    fn parse_path(&self, unit_text: &str) -> Option<String> {
        // The `<string>` right after `<key>PATH</key>` — the exact inverse of
        // what `unit_text` writes. A unit installed before campd's PATH was
        // baked in has no such key at all, and says so by returning None.
        let after_key = unit_text.split("<key>PATH</key>").nth(1)?;
        let value = after_key
            .split("<string>")
            .nth(1)?
            .split("</string>")
            .next()?;
        Some(xml_unescape(value))
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
                // Booted out: launchd is not holding this job at all, so
                // nothing will bring campd back.
                will_restart_campd: false,
                detail: out.stderr.trim().to_owned(),
            });
        }
        // F2 fix: a successful `launchctl print` with no `state = ` line is
        // not a real answer to fabricate a placeholder for — `detail`'s
        // whole contract is the manager's OWN words, verbatim (invariant 3),
        // and a synthesized "state = unknown" would be handed back as if
        // launchd had said it. launchd always prints `state = ` for a label
        // it recognizes (the `!out.success()` branch above already covers
        // "launchd doesn't know this label"), so a successful print with no
        // such line is a genuine surprise — a launchd output-format change,
        // or a test double that got the shape wrong — worth failing loudly
        // on, never guessing past (invariant 5).
        let state_line = out
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("state = "))
            .with_context(|| {
                format!(
                    "launchctl print {target} succeeded but printed no `state = ` line:\n{}",
                    out.stdout
                )
            })?;
        Ok(UnitState {
            loaded: true,
            running: state_line == "state = running",
            // `KeepAlive` is UNCONDITIONAL (see `unit_text`): launchd respawns
            // a bootstrapped job whenever it exits. So for launchd, and only
            // for launchd, "bootstrapped" IS "will restart campd" — which is
            // why keying the verbs on `loaded` happened to work here, and did
            // not work at all on systemd.
            will_restart_campd: true,
            detail: state_line.to_owned(),
        })
    }

    fn unit_name(&self, id: &CampId) -> String {
        self.label(id)
    }

    fn unit_text(&self, id: &CampId, camp_root: &str, exe: &str, path: &str) -> String {
        // KeepAlive (design §4.2, always-on): the supervisor keeps campd
        // alive; a crash is restarted. StandardErrorPath is the camp's own
        // campd.log (CampDir::log_path) — a supervised daemon's stderr is
        // never swallowed (invariant 3). No lossy conversion anywhere: the
        // caller passed strings `unit_safe_str` already vouched for.
        //
        // EnvironmentVariables/PATH is load-bearing, not decoration: launchd
        // hands a LaunchAgent PATH=/usr/bin:/bin:/usr/sbin:/sbin, which does not
        // contain ~/.local/bin — where `claude` lives. Without this, campd
        // spawns nothing and every bead dies as `spawn failed: spawning claude:
        // No such file or directory`. See service::campd_path.
        //
        // AbandonProcessGroup=true is the macOS half of issue #119, and it is
        // the mechanism nobody looked for. `man 5 launchd.plist`: "When a job
        // dies, launchd kills any remaining processes with the same process
        // group ID as the job. Setting this key to true disables that behavior."
        // The default is FALSE — the sweep is ON — and `daemon/spawn.rs` sets no
        // `process_group`, so every `claude -p` worker inherits campd's pgid.
        // Without this key `camp service stop` (a `bootout`) kills campd and
        // launchd then sweeps the group, killing every in-flight worker: the
        // exact shape of systemd's `KillMode=control-group` bug, on the platform
        // that was assumed immune to it.
        //
        // "macOS has no cgroup to sweep" is TRUE and was the wrong reason to
        // feel safe: launchd sweeps the PROCESS GROUP instead. Measured on macOS
        // 2026-07-16 — a LaunchAgent mirroring this plist, job spawns a child
        // the way spawn.rs does, `launchctl bootout`: the child DIED. With this
        // key, it survived. So parity with systemd's `KillMode=process` is
        // something camp now DOES, not something it inherits.
        //
        // The fix is here and not `.process_group(0)` in `spawn()`: that would
        // change worker signal/reaping semantics globally, and `tests/e2e.rs`
        // group-kills to reap the whole worker tree, which works precisely
        // BECAUSE workers share campd's group.
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
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>AbandonProcessGroup</key>
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
            path = xml_escape(path),
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
                // KeepAlive is unconditional: bootstrapped IS "will restart".
                will_restart_campd: true,
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

    /// F2 fix: a successful `launchctl print` with no `state = ` line must
    /// never be papered over with a fabricated "state = unknown" — `detail`'s
    /// whole contract (supervisor.rs doc) is the manager's OWN words,
    /// verbatim. This case is a genuine surprise (launchd always prints
    /// `state = ` for a label it recognizes; the "doesn't know this label"
    /// case is the separate `!out.success()` branch), so it bails loudly
    /// instead of guessing (invariant 5).
    #[test]
    fn state_bails_loudly_when_launchctl_print_has_no_state_line() {
        let weird = FakeRunner::new(vec![FakeRunner::ok(
            "com.gascamp.campd.dev-f9481b53 = {\n\tpid = 4242\n}\n",
        )]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &weird);
        let err = launchd.state(&id()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no `state = ` line"),
            "must fail loudly, not fabricate a placeholder: {msg}"
        );
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
        let text = launchd.unit_text(
            &id(),
            "/Users/x/camps/dev/.camp",
            "/usr/local/bin/camp",
            "/usr/local/bin:/usr/bin:/bin",
        );
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
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>AbandonProcessGroup</key>
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

    /// Issue #119 on macOS — and the claim this file's first fix got WRONG.
    ///
    /// The plist MUST set `AbandonProcessGroup=true`. `man 5 launchd.plist`:
    /// *"When a job dies, launchd kills any remaining processes with the same
    /// process group ID as the job. Setting this key to true disables that
    /// behavior."* The default is FALSE, so the sweep is ON by default — and
    /// `daemon/spawn.rs` sets no `process_group`, so every `claude -p` worker
    /// inherits campd's pgid. Without this key, `camp service stop` (a
    /// `launchctl bootout`) kills campd and then launchd sweeps the process
    /// group, taking every in-flight worker with it: exactly systemd's
    /// `KillMode=control-group` bug, on the other platform.
    ///
    /// This was NOT a theory. Measured on macOS 2026-07-16 with a LaunchAgent
    /// mirroring this plist (RunAtLoad + KeepAlive, no `AbandonProcessGroup`)
    /// whose job spawned a child the way `spawn.rs` does: after `launchctl
    /// bootout` the child was DEAD. With this key set, it survived.
    ///
    /// The reasoning this replaces — "macOS has no cgroup to sweep, so bootout
    /// signals campd alone" — is a non-sequitur, and it is why the bug hid: the
    /// absence of cgroups does not imply the absence of a sweep. launchd has a
    /// different mechanism for the same job, and it was never checked.
    #[test]
    fn unit_text_abandons_the_process_group_so_stopping_campd_leaves_the_workers_alone() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let text = launchd.unit_text(
            &id(),
            "/Users/x/camps/dev/.camp",
            "/usr/local/bin/camp",
            "/usr/local/bin:/usr/bin:/bin",
        );
        // Pin the VALUE, not the key's presence: `<false/>` here IS the default,
        // i.e. the bug, and a plist naming the key while disabling it would read
        // as though the choice had been made.
        assert!(
            text.contains("<key>AbandonProcessGroup</key>\n  <true/>"),
            "the plist must set AbandonProcessGroup=true — without it launchd \
             sweeps campd's process group on bootout and kills every in-flight \
             worker (#119, measured on macOS): {text}"
        );
        assert!(
            !text.contains("<key>AbandonProcessGroup</key>\n  <false/>"),
            "AbandonProcessGroup=false is the default, and the default is the \
             bug: {text}"
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
        let text = launchd.unit_text(
            &id(),
            root,
            "/usr/local/bin/camp",
            "/usr/local/bin:/usr/bin:/bin",
        );
        assert!(text.contains("R&amp;D &lt;beta&gt;"), "{text}");
        assert!(
            !text.contains("R&D <beta>"),
            "raw metacharacters leaked: {text}"
        );
        assert_eq!(launchd.parse_camp_root(&text).unwrap(), PathBuf::from(root));
    }

    /// The unit MUST carry a PATH, and it must be the one it was handed.
    ///
    /// launchd gives a LaunchAgent `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which
    /// does not contain `~/.local/bin` — where Claude Code installs `claude`,
    /// the process campd spawns to do all of the work. A plist with no
    /// EnvironmentVariables/PATH produced a campd that came up healthy, served
    /// its socket, accepted beads, and then failed EVERY dispatch with
    /// `spawn failed: spawning claude: No such file or directory`. It reached a
    /// user that way. This assertion is what stops it coming back.
    #[test]
    fn unit_text_carries_the_path_campd_will_run_with() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let path = "/Users/x/.local/bin:/usr/bin:/bin";
        let text = launchd.unit_text(&id(), "/Users/x/c/.camp", "/usr/local/bin/camp", path);
        assert!(
            text.contains("<key>EnvironmentVariables</key>"),
            "the plist must set an environment at all: {text}"
        );
        assert!(
            text.contains("<key>PATH</key>") && text.contains(&format!("<string>{path}</string>")),
            "the plist must carry the PATH campd runs with — without it campd finds no \
             `claude` and dispatches nothing: {text}"
        );
    }

    /// `parse_path` is the exact inverse of what `unit_text` wrote — including
    /// through XML escaping — and it answers `None` for a unit that carries no
    /// PATH at all. That `None` is what lets `camp service status` tell an
    /// operator their pre-fix unit is the reason nothing dispatches, instead of
    /// reporting it as healthy.
    #[test]
    fn parse_path_round_trips_and_reports_a_unit_that_has_none() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let path = "/opt/R&D <b>/bin:/Users/x/.local/bin:/usr/bin";
        let text = launchd.unit_text(&id(), "/c/.camp", "/usr/local/bin/camp", path);
        assert_eq!(launchd.parse_path(&text).as_deref(), Some(path));

        // A unit from before the PATH was baked in: no EnvironmentVariables.
        let old = text
            .split("  <key>EnvironmentVariables</key>")
            .next()
            .unwrap()
            .to_owned()
            + "</dict>\n</plist>\n";
        assert_eq!(
            launchd.parse_path(&old),
            None,
            "a unit with no PATH must report NONE, not a wrong answer"
        );
    }

    /// A PATH is XML too — and it must not be able to corrupt the
    /// ProgramArguments the camp registry is read back out of.
    #[test]
    fn a_hostile_path_is_escaped_and_cannot_break_the_camp_root_round_trip() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let root = "/Users/x/camps/dev/.camp";
        // XML metacharacters, and a `--camp` inside the PATH itself: the parser
        // takes the FIRST `--camp` (in ProgramArguments, above this), so a PATH
        // cannot hijack which camp the unit claims to be for.
        let path = "/opt/R&D <b>/bin:/x/--camp/bin:/usr/bin";
        let text = launchd.unit_text(&id(), root, "/usr/local/bin/camp", path);
        assert!(
            !text.contains("R&D <b>"),
            "raw metacharacters leaked from the PATH into the plist: {text}"
        );
        assert!(text.contains("R&amp;D &lt;b&gt;"), "{text}");
        assert_eq!(
            launchd.parse_camp_root(&text).unwrap(),
            PathBuf::from(root),
            "a PATH must not be able to change which camp this unit resolves to"
        );
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

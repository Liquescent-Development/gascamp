//! launchd (macOS): a per-user LaunchAgent in the `gui/<uid>` domain, at
//! `~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist`.

use std::ffi::OsStr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::CampId;
use super::runner::CommandRunner;
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
}

/// A camp path may legally contain `&` or `<`; an escaped plist must survive
/// the round trip back to the real path.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
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
}

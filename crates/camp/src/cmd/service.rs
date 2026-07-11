//! `camp service` (design §5): the control surface over the host's service
//! manager. Every flow takes the `Supervisor` PORT, so each is tested against
//! a real unit directory (a tempdir) with a faked process runner — no live
//! service manager anywhere in unit CI.

use anyhow::Result;

use crate::service::{self, Supervisor, SystemProbe, SystemRunner};

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::launchd::Launchd;
    use crate::service::runner::fake::FakeRunner;

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
}

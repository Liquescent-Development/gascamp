#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 2 (campd service management): the `camp service` control surface.
//!
//! The tests in unit CI are READ-ONLY and must pass on a host with a service
//! manager (macOS) and one without (a Linux CI runner): they never install,
//! start or remove a unit. The full lifecycle against the host's REAL service
//! manager is the `#[ignore]`d, CAMP_SERVICE_E2E-gated test added in Task 6.

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// `camp service list` is a pure query over the unit directory — the one
/// `camp service` verb that needs no camp at all (design §5: it is the
/// "manage everything" view across every managed camp). It must succeed
/// everywhere, mutate nothing, and print SOMETHING (an answer, never silence).
#[test]
fn service_list_is_a_read_only_query_that_needs_no_camp() {
    let dir = tempfile::tempdir().unwrap();
    let out = camp()
        .current_dir(dir.path())
        .args(["service", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        !String::from_utf8_lossy(&out).trim().is_empty(),
        "list must answer the query (managed units, or why there are none)"
    );
}

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Fail-loud env gate (no silent skip), mirroring `e2e.rs::require_e2e_env`.
fn require_service_e2e() {
    assert_eq!(
        std::env::var("CAMP_SERVICE_E2E").as_deref(),
        Ok("1"),
        "the service lifecycle test is opt-in and LOCAL-ONLY: set CAMP_SERVICE_E2E=1 \
         (use `make service-e2e`). It installs, starts, restarts and removes a REAL \
         unit in YOUR service manager."
    );
}

/// Always remove the unit, even if an assertion below blows up: a leaked
/// LaunchAgent/systemd unit would keep a campd alive on a temp directory.
///
/// Drop does NOT run on Ctrl-C or a hard kill. If you interrupt this test, the
/// unit survives — pointing at a tempdir that no longer exists, which the
/// supervisor will respawn-throttle forever. Clean it up by hand:
///
///     camp service list                       # find the orphan's camp id
///     # macOS:
///     launchctl bootout gui/$UID/com.gascamp.campd.<camp-id>
///     rm ~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist
///     # Linux:
///     systemctl --user disable --now campd-<camp-id>.service
///     rm ~/.config/systemd/user/campd-<camp-id>.service && systemctl --user daemon-reload
struct Uninstall(PathBuf);

impl Drop for Uninstall {
    fn drop(&mut self) {
        let _ = std::process::Command::new(assert_cmd::cargo::cargo_bin("camp"))
            .args(["--camp"])
            .arg(&self.0)
            .args(["service", "uninstall"])
            .status();
    }
}

/// Block until campd answers on this camp's socket (test-side polling is
/// sanctioned for harnesses — campd itself never polls — invariant 1).
fn wait_for_campd(camp: &Path, want_listening: bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("camp"))
            .args(["--camp"])
            .arg(camp)
            .args(["service", "status"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let listening = text.contains("campd: listening");
        if listening == want_listening {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "campd never reached listening={want_listening}; last status was:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Design §9: the `camp service` lifecycle against the HOST's REAL service
/// manager — `camp init` installs → status shows running → list finds it →
/// restart → `camp stop` REFUSES (the 2026-07-10 operator ruling) → service
/// stop → service start → uninstall. OPT-IN and LOCAL-ONLY (`make
/// service-e2e`): it writes a real LaunchAgent / systemd user unit and starts a
/// real campd, then removes both. CI never runs it.
#[test]
#[ignore = "installs a REAL host service unit: run via `make service-e2e` (CAMP_SERVICE_E2E=1)"]
fn service_lifecycle_against_the_real_host_manager() {
    require_service_e2e();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".camp");

    // `camp init` with NO flag: the environment-aware default (design §6) —
    // on a host with a manager it installs and starts the unit itself. This
    // bare init is the THING UNDER TEST, and it is safe only because this test
    // is #[ignore]d AND gated on CAMP_SERVICE_E2E=1 (see the marker below; the
    // no_bare_camp_init gate documents when that marker is legitimate).
    let init = camp()
        .current_dir(dir.path())
        .args(["--camp"])
        .arg(&root)
        .arg("init") // real-manager: deliberate bare `camp init` — #[ignore]d + CAMP_SERVICE_E2E-gated
        .assert()
        .success();
    let _cleanup = Uninstall(root.clone());
    let init_out = String::from_utf8_lossy(&init.get_output().stdout).into_owned();
    assert!(
        init_out.contains("installed"),
        "on a host WITH a service manager, `camp init` installs the unit: {init_out}"
    );

    // The supervisor started campd; status shows BOTH truths.
    wait_for_campd(&root, true);
    let status = camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "status"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = String::from_utf8_lossy(&status).into_owned();
    assert!(
        status.contains("running=true"),
        "the unit must be running: {status}"
    );
    assert!(
        status.contains("campd: listening"),
        "campd must answer: {status}"
    );

    // The fleet view finds this camp.
    let list = camp()
        .args(["service", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list = String::from_utf8_lossy(&list).into_owned();
    let canonical = std::fs::canonicalize(&root).unwrap();
    assert!(
        list.contains(&canonical.display().to_string()),
        "`camp service list` must name this camp: {list}"
    );

    // The post-upgrade cycle: campd comes back.
    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "restart"])
        .assert()
        .success();
    wait_for_campd(&root, true);

    // The operator's ruling (2026-07-10), end to end: `camp stop` REFUSES on a
    // supervised camp — and the remedy it names actually works.
    let refusal = camp()
        .args(["--camp"])
        .arg(&root)
        .arg("stop")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refusal = String::from_utf8_lossy(&refusal).into_owned();
    assert!(refusal.contains("supervised by"), "{refusal}");
    assert!(
        refusal.contains("camp service stop"),
        "must name the remedy: {refusal}"
    );
    wait_for_campd(&root, true); // …and the refusal stopped nothing.

    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "stop"])
        .assert()
        .success();
    wait_for_campd(&root, false); // the supervisor did NOT bring it back
    assert!(
        std::fs::canonicalize(&root).is_ok(),
        "a stopped camp is still a camp"
    );

    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "start"])
        .assert()
        .success();
    wait_for_campd(&root, true);

    // And it all comes out again.
    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "uninstall"])
        .assert()
        .success();
    wait_for_campd(&root, false);
    let after = camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "status"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let after = String::from_utf8_lossy(&after).into_owned();
    assert!(
        after.contains("not installed"),
        "the unit must be gone: {after}"
    );
}

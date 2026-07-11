#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 1 (campd service management): campd shuts down gracefully on
//! SIGTERM *and* SIGINT — the same clean stop as the socket `Request::Stop`
//! (append `campd.stopped`, unlink the socket, exit 0; spec §7, §9) — so it
//! is a well-behaved supervised process. launchd/systemd/the container
//! runtime all stop a service with SIGTERM; SIGINT is Ctrl-C on a foreground
//! `camp daemon`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// Repo precedent (`daemon_lifecycle.rs:11-12`). `camp` is a bin-only crate
// (no `src/lib.rs`), so `daemon::READY_PREFIX` cannot be imported into an
// integration test — it is re-declared here, exactly as daemon_lifecycle does.
const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

/// The whole phase contract, once. Both signal tests call THIS — so the
/// "identical outcome" in spec §9 is enforced by construction rather than
/// asserted twice and allowed to drift.
fn graceful_stop_on(signal: &str) {
    let dir = tempfile::tempdir().unwrap();

    // A minimal camp: `camp init` writes ./.camp/{camp.toml,camp.db}.
    let init = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir.path())
        .arg("init")
        .status()
        .unwrap();
    assert!(init.success(), "camp init failed");
    let camp_root = dir.path().join(".camp");

    // Spawn the long-lived daemon; capture stdout for the readiness line.
    let mut child = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["daemon", "--camp"])
        .arg(&camp_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    // Block until campd announces readiness — an OS pipe read, not a
    // sleep/retry loop. Assert the PREFIX, not merely that some bytes
    // arrived: that distinguishes "campd is up and listening" from "campd
    // printed something and died".
    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    assert!(
        line.starts_with(READY_PREFIX),
        "unexpected first line from campd: {line:?}"
    );

    // Signal the child's POSITIVE pid. `Command::spawn` does not put the
    // child in a new process group (nothing in this repo sets setsid /
    // process_group for it), so it shares the test runner's pgroup — a
    // negative-pgid form would signal the test harness itself.
    // kill(1) rather than a libc dep: `Child::kill` is SIGKILL-only.
    let sent = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(sent.success(), "kill -{signal} failed to send");

    // (1 of 3) It exits CLEANLY — not terminated by the signal's default action.
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("campd did not exit within 10s of SIG{signal}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        exit.success(),
        "SIG{signal} must cause a clean exit(0), got {exit:?}"
    );

    // (2 of 3) The graceful stop is DURABLE — the same event as `camp stop`.
    let ledger = camp_core::ledger::Ledger::open_read_only(&camp_root.join("camp.db")).unwrap();
    let stopped = ledger
        .events_of_type(camp_core::event::EventType::CampdStopped)
        .unwrap();
    assert!(
        !stopped.is_empty(),
        "a graceful SIG{signal} stop must record campd.stopped"
    );

    // (3 of 3) The socket is DROPPED. This is the part that bites under a
    // KeepAlive/Restart=always supervisor: the restart hits
    // `socket::bind_or_replace` against a stale socket. The Request::Stop
    // test asserts exactly this (`daemon/mod.rs:300`), so the signal path is
    // held to the same standard as the path it claims to be identical to.
    assert!(
        !camp_root.join("campd.sock").exists(),
        "a graceful signal stop must unlink the socket, exactly like Request::Stop"
    );
}

#[test]
fn campd_stops_gracefully_on_sigterm() {
    graceful_stop_on("TERM");
}

#[test]
fn campd_stops_gracefully_on_sigint() {
    graceful_stop_on("INT");
}

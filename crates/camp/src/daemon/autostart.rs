//! Auto-start (spec §5): a verb that needs the daemon connects; on failure
//! it records campd.autostarted (the trail carries the cause, spec §13.3),
//! spawns `camp daemon` detached, blocks on the daemon's readiness line —
//! an OS pipe read, not a sleep/retry loop — and retries the request
//! exactly ONCE. Fail fast after that.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use super::READY_PREFIX;
use super::socket::{self, Request, Response};
use crate::campdir::CampDir;

pub fn request_with_autostart(camp: &CampDir, request: &Request, verb: &str) -> Result<Response> {
    let sock = camp.socket_path();
    // Probe first: only an unreachable socket triggers auto-start; a live
    // daemon's protocol errors surface as themselves.
    if UnixStream::connect(&sock).is_ok() {
        return socket::request(&sock, request);
    }
    start_detached(camp, verb)?;
    socket::request(&sock, request).with_context(|| {
        format!(
            "campd did not come up after auto-start; see {}",
            camp.log_path().display()
        )
    })
}

// The daemon is detached BY DESIGN (spec §5): it must outlive this CLI
// process, which exits immediately; init reaps it. Never waited on.
#[allow(clippy::zombie_processes)]
fn start_detached(camp: &CampDir, verb: &str) -> Result<()> {
    // Cause before effect (spec §13.3): the trail reads
    // campd.autostarted → campd.started.
    let mut ledger = Ledger::open(&camp.db_path())?;
    ledger.append(EventInput {
        kind: EventType::CampdAutostarted,
        rig: None,
        actor: "cli".into(),
        bead: None,
        data: serde_json::json!({ "verb": verb }),
    })?;
    drop(ledger);

    let exe = std::env::current_exe().context("locating the camp binary")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(camp.log_path())
        .with_context(|| format!("opening {}", camp.log_path().display()))?;
    use std::os::unix::process::CommandExt as _;
    let mut child = Command::new(exe)
        .arg("daemon")
        .arg("--camp")
        .arg(&camp.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .process_group(0) // its own group: detached from the CLI's terminal
        .spawn()
        .context("spawning camp daemon")?;

    // Block on the readiness line. EOF without it = the daemon failed and
    // its stderr is in campd.log.
    let stdout = child.stdout.take().context("daemon stdout unavailable")?;
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .context("reading campd's readiness line")?;
    if !line.starts_with(READY_PREFIX) {
        // Our child may have lost the start race to another daemon that is
        // now live: a campd whose bind is refused exits without a readiness
        // line (PR #8 review finding 2). The socket, not our child, is the
        // truth (spec §5) — re-probe before declaring failure.
        if UnixStream::connect(camp.socket_path()).is_ok() {
            return Ok(());
        }
        bail!(
            "campd failed to start (no readiness line); see {}",
            camp.log_path().display()
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// PR #8 review finding 2: when our spawned campd loses the start race
    /// to another daemon, it exits without a readiness line. The socket,
    /// not our child, is the truth (spec §5): a live socket after the
    /// child's EOF is a won race, not "campd failed to start".
    #[test]
    fn start_detached_recognizes_a_lost_race() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let camp = CampDir { root };
        // Another daemon already owns the socket (a bound listener accepts
        // via its backlog). Any child spawned below therefore exits without
        // printing a readiness line. (The child here is this test binary
        // rejecting bogus args — the same observable shape as a campd that
        // lost the bind race: stdout EOF, no line.)
        let _winner = UnixListener::bind(camp.socket_path()).unwrap();

        let result = start_detached(&camp, "test");
        assert!(
            result.is_ok(),
            "a live socket after child EOF is a won race, not a failure: {:?}",
            result.err()
        );
        // the causal trail is still recorded
        let ledger = camp_core::ledger::Ledger::open(&camp.db_path()).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.kind.as_str() == "campd.autostarted")
        );
    }
}

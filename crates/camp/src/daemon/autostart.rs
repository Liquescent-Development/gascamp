//! Auto-start (spec §5): a verb that needs the daemon sends its request —
//! the request is the liveness probe (issue #55; an event-loop
//! round-trip, never a bare connect, which a wedged daemon's listen
//! backlog can fool). Only a refused/absent socket triggers the spawn: it
//! records campd.autostarted (the trail carries the cause, spec §13.3),
//! spawns `camp daemon` detached, blocks on the daemon's readiness line —
//! an OS pipe read, not a sleep/retry loop — and retries the request
//! exactly ONCE. Fail fast after that; an unanswered request is the loud
//! CampdUnresponsive error, never a second daemon.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use super::READY_PREFIX;
use super::socket::{self, Request, Response};
use crate::campdir::CampDir;

pub fn request_with_autostart(camp: &CampDir, request: &Request, verb: &str) -> Result<Response> {
    // The request IS the liveness probe (issue #55), judged on the SAME
    // connection that carries it (the PR #51 finding 1 law). A wedged
    // campd's listen backlog accepts connects while its event loop never
    // serves them, so a bare-connect pre-probe reads wedged as alive; a
    // round-trip cannot be fooled. Only a refused/absent socket — campd
    // genuinely not running — triggers auto-start. An unanswered request
    // surfaces as the loud CampdUnresponsive error instead: something
    // owns the socket, and a second daemon would only mask it.
    if let Some(response) = socket::request_if_up(camp, request)? {
        return Ok(response);
    }
    start_detached(camp, verb)?;
    socket::request(camp, request).with_context(|| {
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
        // truth (spec §5) — and the WINNER must ANSWER, not merely accept
        // (issue #55): a round-trip re-probe reports a wedged socket
        // holder as CampdUnresponsive instead of blaming our child's log.
        return match socket::request_if_up(camp, &Request::Status)? {
            Some(_) => Ok(()), // a live winner answered: a won race
            None => bail!(
                "campd failed to start (no readiness line); see {}",
                camp.log_path().display()
            ),
        };
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Issue #55 scope 3, pinned: liveness is an event-loop ROUND-TRIP on
    /// the same connection that carries the request — never a bare
    /// connect. A wedged campd's kernel backlog accepts connects while its
    /// loop never serves them, so a bare-connect probe reads wedged as
    /// alive. The verb must (a) open exactly ONE connection (the request
    /// IS the probe — the PR #51 finding 1 law), (b) fail loudly with the
    /// actionable wedge error, and (c) NEVER auto-start a second daemon —
    /// something owns the socket, and a second campd would only mask it.
    #[test]
    fn a_wedged_socket_holder_fails_the_verb_loudly_and_never_autostarts() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        let mut ledger = Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(EventInput {
                kind: EventType::CampdStarted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({ "pid": 424242 }),
            })
            .unwrap();
        drop(ledger);
        // The wedge simulator: accepts (and counts) connections, then
        // holds them open without ever reading or answering.
        let listener = UnixListener::bind(camp.socket_path()).unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepts);
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                counter.fetch_add(1, Ordering::SeqCst);
                held.push(stream); // keep the connection open, serve nothing
            }
        });

        let start = std::time::Instant::now();
        let err = request_with_autostart(&camp, &Request::Status, "test").unwrap_err();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(12),
            "bounded, never a hang: took {elapsed:?}"
        );
        let msg = format!("{err:#}");
        assert!(
            err.downcast_ref::<socket::CampdUnresponsive>().is_some(),
            "the wedge must surface as CampdUnresponsive: {msg}"
        );
        assert!(msg.contains("424242"), "must name the campd pid: {msg}");
        assert!(msg.contains("kill -9"), "must name the remedy: {msg}");
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "exactly ONE connection: the request is the probe (no bare-connect \
             pre-probe that the backlog can fool)"
        );
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        let autostarted = ledger
            .events_of_type(EventType::CampdAutostarted)
            .unwrap()
            .len();
        assert_eq!(
            autostarted, 0,
            "a wedged socket-holder must never trigger auto-start"
        );
    }

    /// Issue #55 scope 3: the lost-race re-probe is a round-trip too. A
    /// winner that holds the socket but never answers is reported as the
    /// actionable wedge error — NOT as "campd failed to start", which
    /// would misdirect the operator at the log of a child that lost a
    /// race to a stuck daemon.
    #[test]
    fn start_detached_reports_a_wedged_winner_as_unresponsive() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let camp = CampDir { root };
        // the "winner": owns the socket, accepts via its backlog, answers
        // nothing — the wedge shape
        let _wedged = UnixListener::bind(camp.socket_path()).unwrap();

        let err = start_detached(&camp, "test").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            err.downcast_ref::<socket::CampdUnresponsive>().is_some(),
            "a wedged winner is a wedge, not a start failure: {msg}"
        );
        assert!(
            !msg.contains("failed to start"),
            "must not misdiagnose the wedge as a start failure: {msg}"
        );
    }

    /// PR #8 review finding 2: when our spawned campd loses the start race
    /// to another daemon, it exits without a readiness line. The socket,
    /// not our child, is the truth (spec §5): a live socket after the
    /// child's EOF is a won race, not "campd failed to start".
    #[test]
    fn start_detached_recognizes_a_lost_race() {
        // start_detached forks a child: serialized against the socket
        // probe tests (round-2 review finding; see spawn_probe_guard) so
        // the fork cannot land inside another test's socket()/FD_CLOEXEC
        // window on macOS.
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let camp = CampDir { root };
        // Another daemon already owns the socket AND ANSWERS (issue #55:
        // the lost-race re-probe is a status round-trip, not a bare
        // connect — a backlog alone no longer counts as a winner). Any
        // child spawned below exits without printing a readiness line.
        // (The child here is this test binary rejecting bogus args — the
        // same observable shape as a campd that lost the bind race:
        // stdout EOF, no line.)
        let winner = UnixListener::bind(camp.socket_path()).unwrap();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = winner.accept() {
                let mut line = String::new();
                let mut reader = BufReader::new(match stream.try_clone() {
                    Ok(clone) => clone,
                    Err(_) => continue,
                });
                if reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let _ = stream.write_all(b"{\"ok\":true}\n");
                }
            }
        });

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

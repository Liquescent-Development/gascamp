//! The campd socket protocol (master plan Phase 7, pinned): newline-delimited
//! JSON over `<camp>/campd.sock`. Liveness is an ANSWERED REQUEST (spec §5
//! as amended by issue #55): alive means an event-loop round-trip, because
//! the kernel's listen backlog accepts connections even when the loop is
//! wedged. Bind-conflict detection is the narrower accept test: a stale
//! file that refuses connections is unlinked and rebound; a live listener
//! makes a second daemon refuse to start (wedged or not — replacing a
//! wedged daemon is the operator's kill -9, never a silent takeover).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use camp_core::Seq;
use camp_core::ledger::StatusSummary;
use serde::{Deserialize, Serialize};

use crate::campdir::CampDir;

/// One request line. Internally tagged on `op`; an unknown op is a parse
/// error (there is no wildcard arm to hide behind).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Post-commit poke (spec §7.2). `seq` is advisory: catch-up processes
    /// everything past the cursor regardless. The `{"ok":true}` reply is an
    /// ACK — campd is awake and will process this wake — not a completion
    /// signal (PR #14 review finding 2, ack-before-settle).
    Poke {
        seq: Seq,
    },
    Status,
    Stop,
    /// Reconcile the session registry against reality (spec §8.5) — the
    /// same routine campd runs at startup, on demand (Phase 11).
    Adopt,
    /// Deliver one user turn into a live worker's campd-held stdin pipe
    /// (dispatch-lifecycle Phase 1, #29 — the converse verb's live path).
    Nudge {
        session: String,
        text: String,
    },
}

/// One response line. Untagged: variant order matters for deserialization
/// (Status needs its fields, Error needs `error`, Ok is the fallback).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Status {
        ok: bool,
        #[serde(flatten)]
        summary: StatusSummary,
        /// Live sessions currently stalled by patrol — the `✖red` of the
        /// fleet badge (spec §10/§11, Phase 12 Decision D2). Serialized
        /// after the flattened summary, before `campd_pid`.
        red: u64,
        campd_pid: u32,
    },
    Adopt {
        ok: bool,
        crashed: usize,
        rearmed: usize,
        released: usize,
        swept: usize,
        kept: usize,
    },
    /// Nudge disposition (dispatch-lifecycle Phase 1): via="stdin" means
    /// the turn is in the held pipe; via="none" means no held pipe for the
    /// session (released, Null-mode, exited, or not campd's child) — the
    /// caller converses over the resume path instead. Must precede Ok in
    /// this untagged enum so {"ok":..,"via":..} resolves here.
    Nudge {
        ok: bool,
        via: String,
    },
    Error {
        ok: bool,
        error: String,
    },
    Ok {
        ok: bool,
    },
}

/// Bind the daemon socket under spec §5's liveness rules: fresh path →
/// bind; existing file that refuses connections (stale, e.g. after
/// kill -9) → unlink and rebind; existing file that accepts → another
/// campd is alive → hard error.
///
/// The whole negotiation is serialized with an exclusive advisory lock on
/// `<socket>.lock` (PR #8 review finding 1): without it, two daemons racing
/// past a stale socket can both probe-refuse, both unlink, both bind — the
/// loser ends up live but orphaned on an unlinked inode (split brain). The
/// lock releases on drop / process exit, so a crash never wedges startup
/// (crash-only, spec §5). The lock file is a serialization primitive, not
/// status: bind-conflict detection stays "the socket accepts" (a wedged
/// daemon still owns its socket — issue #55 puts liveness-for-service at
/// an answered request, but replacing a stuck daemon is the operator's
/// kill -9, never a silent takeover here) — no pidfiles, no
/// lockfiles-as-status.
pub fn bind_or_replace(path: &Path) -> Result<UnixListener> {
    // Literally `<socket>.lock` (campd.sock.lock) — appended, not
    // with_extension, which would silently rename to campd.lock and
    // contradict every doc that names this file (re-review finding 2).
    let lock_path = {
        let mut os = path.as_os_str().to_owned();
        os.push(".lock");
        std::path::PathBuf::from(os)
    };
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening bind lock {}", lock_path.display()))?;
    lock_file
        .lock()
        .with_context(|| format!("locking {}", lock_path.display()))?;
    // Lock held for the whole probe → unlink → rebind section; released on
    // return (drop). A loser blocks here, then sees the winner's live
    // socket and refuses cleanly.
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                bail!(
                    "campd is already running (socket {} accepts connections)",
                    path.display()
                );
            }
            std::fs::remove_file(path)
                .with_context(|| format!("removing stale socket {}", path.display()))?;
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
        }
        Err(e) => Err(e).with_context(|| format!("binding {}", path.display())),
    }
}

/// How long one CLI request may wait on campd before the CLI declares
/// the daemon wedged (issue #55). A bound on one operation, not a wakeup
/// — the daemon's own poll timeout stays None (invariant 1). Because
/// every daemon-needing verb sends a real request, every operator
/// interaction doubles as an event-loop liveness probe at zero standing
/// cost.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The wedge shape (issue #55): the kernel's listen backlog accepted the
/// connection — that happens even when the event loop never runs accept —
/// but no response line arrived within REQUEST_TIMEOUT. Typed so callers
/// (the auto-start path) can tell "something owns the socket but does not
/// serve it" from "nothing is running"; only the latter may auto-start.
#[derive(Debug)]
pub struct CampdUnresponsive {
    /// From the ledger's last campd.started event; None when no recorded
    /// start carries one (a pre-#55 ledger, or no ledger at all).
    pub campd_pid: Option<u32>,
    pub socket: std::path::PathBuf,
}

impl std::fmt::Display for CampdUnresponsive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secs = REQUEST_TIMEOUT.as_secs();
        match self.campd_pid {
            Some(pid) => write!(
                f,
                "campd (pid {pid}, per the ledger's last campd.started) accepted the \
                 connection but sent no response within {secs}s — its event loop is \
                 wedged or stuck in one long operation; `kill -9 {pid}` is a supported \
                 shutdown (crash-only: the ledger keeps the whole story), then rerun \
                 the verb to start a fresh campd"
            ),
            None => write!(
                f,
                "campd accepted the connection but sent no response within {secs}s — \
                 its event loop is wedged or stuck in one long operation (pid unknown: \
                 no campd.started event records one; `lsof {}` finds the holder); \
                 kill -9 it — a supported shutdown (crash-only: the ledger keeps the \
                 whole story) — then rerun the verb to start a fresh campd",
                self.socket.display()
            ),
        }
    }
}

impl std::error::Error for CampdUnresponsive {}

/// Send one request, read one response line. REQUEST_TIMEOUT bounds the
/// operation against a wedged daemon — not a wakeup; the daemon's own
/// poll timeout stays None (invariant 1). A timeout surfaces as the
/// actionable `CampdUnresponsive` error naming the campd pid (issue #55).
pub fn request(camp: &CampDir, request: &Request) -> Result<Response> {
    let path = camp.socket_path();
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("connecting to campd at {}", path.display()))?;
    request_on(stream, request).map_err(|e| mark_wedge(camp, e))
}

/// Like `request`, but campd-not-listening is a NORMAL state, not an error
/// (dispatch-lifecycle Phase 1: the converse verb's resume path; the same
/// designed degrade as `camp top --statusline`). Liveness is judged on the
/// SAME connection that carries the request — a separate probe connect
/// would leave a window where campd stops between probe and request and
/// the designed degrade becomes a hard error (PR #51 review finding 1).
/// Only an absent/refusing socket maps to Ok(None); a campd that accepts
/// and then misbehaves (or answers an Error line) still surfaces as Err —
/// fail fast.
pub fn request_if_up(camp: &CampDir, request: &Request) -> Result<Option<Response>> {
    use std::io::ErrorKind;
    let path = camp.socket_path();
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
            return Ok(None);
        }
        Err(e) => {
            return Err(e).with_context(|| format!("connecting to campd at {}", path.display()));
        }
    };
    request_on(stream, request)
        .map(Some)
        .map_err(|e| mark_wedge(camp, e))
}

/// Reclassify a timed-out request as the actionable wedge error (issue
/// #55): the root cause of a read/write deadline is the raw io error
/// (WouldBlock on macOS, TimedOut elsewhere) — cryptic and inactionable
/// ("Resource temporarily unavailable"). Everything else passes through
/// untouched.
fn mark_wedge(camp: &CampDir, err: anyhow::Error) -> anyhow::Error {
    use std::io::ErrorKind;
    let timed_out = err
        .root_cause()
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| matches!(io.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut));
    if !timed_out {
        return err;
    }
    anyhow::Error::new(CampdUnresponsive {
        campd_pid: last_recorded_campd_pid(camp),
        socket: camp.socket_path(),
    })
}

/// The pid from the ledger's last campd.started event — recorded at every
/// daemon start precisely because a WEDGED campd cannot be asked (the
/// status op is the only other place its pid lives). Option, not Result:
/// this decorates an error that is already being reported; a ledger that
/// cannot yield the pid downgrades the message to "pid unknown" (stated,
/// not silent), never masks the wedge.
fn last_recorded_campd_pid(camp: &CampDir) -> Option<u32> {
    let ledger = camp_core::ledger::Ledger::open_read_only(&camp.db_path()).ok()?;
    let starts = ledger
        .events_of_type(camp_core::event::EventType::CampdStarted)
        .ok()?;
    u32::try_from(starts.last()?.data.get("pid")?.as_u64()?).ok()
}

/// The shared request body: one line out, one line back, on an
/// already-open connection.
fn request_on(mut stream: UnixStream, request: &Request) -> Result<Response> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut response_line = String::new();
    BufReader::new(stream).read_line(&mut response_line)?;
    if response_line.is_empty() {
        bail!("campd closed the connection without responding");
    }
    let response: Response = serde_json::from_str(response_line.trim_end())
        .with_context(|| format!("campd sent a malformed response: {response_line:?}"))?;
    if let Response::Error { error, .. } = &response {
        bail!("campd: {error}");
    }
    Ok(response)
}

/// Post-commit poke: fire-and-forget BY DESIGN (spec §7.2 — "if campd is
/// down, writes still succeed and it catches up from its processed-cursor
/// on start"). This is the one sanctioned ignore-the-error site in camp;
/// a poke never auto-starts the daemon.
pub fn poke_best_effort(camp: &CampDir, seq: Seq) {
    let _ = request(camp, &Request::Poke { seq });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::ledger::StatusSummary;
    use std::os::unix::net::UnixListener;

    #[test]
    fn request_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Request::Poke { seq: 412 }).unwrap(),
            r#"{"op":"poke","seq":412}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Status).unwrap(),
            r#"{"op":"status"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Stop).unwrap(),
            r#"{"op":"stop"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Adopt).unwrap(),
            r#"{"op":"adopt"}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"adopt"}"#).unwrap(),
            Request::Adopt
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"poke","seq":412}"#).unwrap(),
            Request::Poke { seq: 412 }
        );
    }

    #[test]
    fn unknown_op_is_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"dance"}"#).is_err());
    }

    /// The converse verb's wire op (dispatch-lifecycle Phase 1, #29).
    #[test]
    fn nudge_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Request::Nudge {
                session: "camp/dev/1".into(),
                text: "status?".into()
            })
            .unwrap(),
            r#"{"op":"nudge","session":"camp/dev/1","text":"status?"}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"nudge","session":"s","text":"t"}"#).unwrap(),
            Request::Nudge {
                session: "s".into(),
                text: "t".into()
            }
        );
        // Response: untagged — the Nudge variant must win for {"ok":..,"via":..}
        assert_eq!(
            serde_json::to_string(&Response::Nudge {
                ok: true,
                via: "stdin".into()
            })
            .unwrap(),
            r#"{"ok":true,"via":"stdin"}"#
        );
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true,"via":"none"}"#).unwrap(),
            Response::Nudge { via, .. } if via == "none"
        ));
    }

    /// Review finding 1 (PR #51): liveness must be judged on the SAME
    /// connection that carries the request. A separate probe connect left
    /// a window where campd stops between probe and request, turning the
    /// designed degrade (Ok(None) → resume path) into a hard error. The
    /// accept queue is FIFO, so by the time the request is answered every
    /// earlier probe connection has been accepted and counted.
    #[test]
    fn request_if_up_uses_a_single_connection() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        let path = camp.socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepts);
        // Serve every connection: count it, answer any request line with a
        // plain ok. The thread parks in accept() after the test's last
        // connection; the test process exits regardless (harness-only).
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                counter.fetch_add(1, Ordering::SeqCst);
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

        let response = request_if_up(&camp, &Request::Status).unwrap();
        assert!(matches!(response, Some(Response::Ok { ok: true })));
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "request_if_up must open exactly ONE connection (no separate liveness probe)"
        );
    }

    /// Issue #55 scope 2: a campd whose event loop never serves the
    /// request fails the CLI verb loudly WITHIN ITS BOUND — naming the
    /// campd pid recorded in the ledger and the kill -9 remedy — never
    /// hangs. The bare bound listener IS the wedge simulator: its kernel
    /// backlog accepts the connect, but nothing ever reads or answers,
    /// exactly like a daemon stuck mid-syscall.
    #[test]
    fn a_wedged_campd_fails_the_request_loudly_within_its_bound() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        // the ledger knows the daemon's pid: campd.started records it
        let mut ledger = camp_core::ledger::Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::CampdStarted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({ "pid": 424242 }),
            })
            .unwrap();
        drop(ledger);
        let _wedged = UnixListener::bind(camp.socket_path()).unwrap();

        let start = std::time::Instant::now();
        let err = request(&camp, &Request::Status).unwrap_err();
        let elapsed = start.elapsed();
        assert!(
            elapsed < REQUEST_TIMEOUT * 2 + Duration::from_secs(2),
            "bounded, never a hang: took {elapsed:?}"
        );
        let msg = format!("{err:#}");
        assert!(msg.contains("424242"), "must name the campd pid: {msg}");
        assert!(msg.contains("kill -9"), "must name the remedy: {msg}");
        assert!(
            err.downcast_ref::<CampdUnresponsive>().is_some(),
            "typed, so the auto-start path can tell a wedge from a refusal: {msg}"
        );
    }

    /// The pid-unknown flavor (no campd.started carries one — e.g. a
    /// pre-#55 ledger): still loud, still bounded, still actionable; the
    /// missing pid is stated, never silently omitted.
    #[test]
    fn a_wedged_campd_without_a_recorded_pid_is_still_actionable() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        drop(camp_core::ledger::Ledger::open(&camp.db_path()).unwrap()); // empty ledger
        let _wedged = UnixListener::bind(camp.socket_path()).unwrap();

        let err = request(&camp, &Request::Status).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("kill -9"), "must name the remedy: {msg}");
        assert!(
            msg.contains("pid unknown"),
            "the missing pid must be stated: {msg}"
        );
    }

    /// campd-not-listening is a NORMAL state for the converse verb (the
    /// resume path) — Ok(None), never an error.
    #[test]
    fn request_if_up_returns_none_when_no_daemon_listens() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        // no listener at all
        assert!(request_if_up(&camp, &Request::Status).unwrap().is_none());
        // a stale file that refuses connections is also "not up"
        drop(UnixListener::bind(camp.socket_path()).unwrap());
        assert!(request_if_up(&camp, &Request::Status).unwrap().is_none());
    }

    #[test]
    fn response_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Response::Ok { ok: true }).unwrap(),
            r#"{"ok":true}"#
        );
        let status = Response::Status {
            ok: true,
            summary: StatusSummary {
                live_sessions: vec!["camp/dev/1".to_owned()],
                ready: 1,
                open: 2,
            },
            red: 1,
            campd_pid: 4242,
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"ok":true,"live_sessions":["camp/dev/1"],"ready":1,"open":2,"red":1,"campd_pid":4242}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Error {
                ok: false,
                error: "bad request".to_owned()
            })
            .unwrap(),
            r#"{"ok":false,"error":"bad request"}"#
        );
        let adopt = Response::Adopt {
            ok: true,
            crashed: 1,
            rearmed: 2,
            released: 3,
            swept: 4,
            kept: 5,
        };
        assert_eq!(
            serde_json::to_string(&adopt).unwrap(),
            r#"{"ok":true,"crashed":1,"rearmed":2,"released":3,"swept":4,"kept":5}"#
        );
        assert!(matches!(
            serde_json::from_str::<Response>(
                r#"{"ok":true,"crashed":0,"rearmed":0,"released":0,"swept":0,"kept":0}"#
            )
            .unwrap(),
            Response::Adopt { .. }
        ));
        // client-side parse resolves the right variants
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true}"#).unwrap(),
            Response::Ok { ok: true }
        ));
        assert!(matches!(
            serde_json::from_str::<Response>(
                r#"{"ok":true,"live_sessions":[],"ready":0,"open":0,"red":0,"campd_pid":1}"#
            )
            .unwrap(),
            Response::Status { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":false,"error":"x"}"#).unwrap(),
            Response::Error { .. }
        ));
    }

    #[test]
    fn fresh_path_binds() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        let listener = bind_or_replace(&path).unwrap();
        drop(listener);
        assert!(path.exists());
        // The bind lock lives literally at `<socket>.lock` — the path every
        // doc names (PR #8 re-review finding 2: with_extension put it at
        // campd.lock, a file the docs never mention).
        assert!(
            dir.path().join("campd.sock.lock").exists(),
            "bind lock must be at <socket>.lock"
        );
    }

    #[test]
    fn stale_socket_is_unlinked_and_rebound() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        // a dead daemon (kill -9) leaves the file behind, refusing connections
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let listener = bind_or_replace(&path).expect("stale socket must be replaced");
        drop(listener);
    }

    #[test]
    fn live_socket_refuses_a_second_daemon() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        let _keep = bind_or_replace(&path).unwrap();
        let err = bind_or_replace(&path).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "error was: {err:#}"
        );
    }

    /// PR #8 review finding 1: the stale-socket replacement critical
    /// section (probe → unlink → rebind) must be serialized. Without a
    /// guard, two daemons racing past a stale socket can both probe-refuse,
    /// both unlink, both bind — leaving one live but orphaned on an
    /// unlinked inode (split brain: the one-standing-process shape breaks).
    #[test]
    fn concurrent_bind_or_replace_elects_exactly_one_daemon() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        use std::sync::{Arc, Barrier};
        for round in 0..50 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("campd.sock");
            // a stale socket, kill -9 shape: bound, then dropped un-unlinked
            drop(UnixListener::bind(&path).unwrap());

            let barrier = Arc::new(Barrier::new(8));
            let mut handles = Vec::new();
            for _ in 0..8 {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    bind_or_replace(&path)
                }));
            }
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners = results.iter().filter(|r| r.is_ok()).count();
            assert_eq!(
                winners, 1,
                "round {round}: exactly one daemon may own the socket"
            );
            assert!(
                UnixStream::connect(&path).is_ok(),
                "round {round}: the socket path must lead to the winner after the race"
            );
        }
    }
}

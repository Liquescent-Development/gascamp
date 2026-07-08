//! The campd socket protocol (master plan Phase 7, pinned): newline-delimited
//! JSON over `<camp>/campd.sock`. Liveness IS the socket (spec §5): alive
//! means it accepts; a stale file that refuses connections is unlinked and
//! rebound; a live listener makes a second daemon refuse to start.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use camp_core::Seq;
use camp_core::ledger::StatusSummary;
use serde::{Deserialize, Serialize};

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
/// status: liveness stays "the socket accepts" — no pidfiles, no
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

/// Send one request, read one response line. The timeouts bound a single
/// CLI operation against a wedged daemon — they are not wakeups; the
/// daemon's own poll timeout stays None (invariant 1).
pub fn request(path: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("connecting to campd at {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
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
pub fn poke_best_effort(path: &Path, seq: Seq) {
    let _ = request(path, &Request::Poke { seq });
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

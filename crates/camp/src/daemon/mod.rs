//! campd: the only standing process (spec §5). Crash-only: no exclusive
//! state, `kill -9` is a supported shutdown method; on start it opens the
//! ledger, appends campd.started, catches up past its cursor, announces
//! readiness on stdout, and sleeps on the socket.

pub mod cursor;
pub mod event_loop;
pub mod socket;

use std::io::Write;

use anyhow::{Context, Result};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;
use cursor::ReadinessProcessor;

/// The single line campd prints to stdout once the socket accepts.
/// Auto-start (and the tests) block on it — an OS pipe read, not a
/// sleep/retry loop. stdout is never written again after this line.
pub const READY_PREFIX: &str = "campd listening on ";

pub fn run(camp: &CampDir) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let socket_path = camp.socket_path();
    let std_listener = socket::bind_or_replace(&socket_path)?;
    std_listener
        .set_nonblocking(true)
        .context("setting the listener non-blocking")?;
    let listener = mio::net::UnixListener::from_std(std_listener);

    ledger.append(EventInput {
        kind: EventType::CampdStarted,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({}),
    })?;

    // Startup catch-up is fatal on error: a daemon that cannot process its
    // backlog must not pretend to be up (fail fast).
    let mut processor = ReadinessProcessor::default();
    cursor::catch_up(&mut ledger, &mut processor)?;

    let mut stdout = std::io::stdout();
    writeln!(stdout, "{READY_PREFIX}{}", socket_path.display())
        .context("announcing readiness")?;
    stdout.flush().context("flushing the readiness line")?;

    event_loop::run(listener, &socket_path, &mut ledger, &mut processor)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    /// Test-harness-only readiness wait (the daemon itself never polls;
    /// out-of-process callers get the stdout readiness line instead).
    fn connect_with_retry(sock: &Path) -> UnixStream {
        for _ in 0..500 {
            if let Ok(stream) = UnixStream::connect(sock) {
                return stream;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("campd socket {} never accepted", sock.display());
    }

    fn request(stream: &mut UnixStream, line: &str) -> serde_json::Value {
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut resp = String::new();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        reader.read_line(&mut resp).unwrap();
        serde_json::from_str(resp.trim_end()).expect("campd response is JSON")
    }

    #[test]
    fn daemon_serves_status_poke_and_stop_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let camp = CampDir { root: root.clone() };
        let handle = std::thread::spawn(move || run(&camp));

        let sock = root.join("campd.sock");
        let mut stream = connect_with_retry(&sock);

        let status = request(&mut stream, r#"{"op":"status"}"#);
        assert_eq!(status["ok"], true);
        assert_eq!(status["campd_pid"], std::process::id());
        assert_eq!(status["ready"], 0);
        assert_eq!(status["open"], 0);
        assert_eq!(status["live_sessions"], serde_json::json!([]));

        let poke = request(&mut stream, r#"{"op":"poke","seq":1}"#);
        assert_eq!(poke, serde_json::json!({"ok": true}));

        // an unknown op gets a clean error response on a fresh connection
        let mut bad = UnixStream::connect(&sock).unwrap();
        let err = request(&mut bad, r#"{"op":"dance"}"#);
        assert_eq!(err["ok"], false);
        assert!(err["error"].as_str().unwrap().contains("bad request"));

        let stop = request(&mut stream, r#"{"op":"stop"}"#);
        assert_eq!(stop, serde_json::json!({"ok": true}));
        handle.join().unwrap().unwrap();
        assert!(!sock.exists(), "stop must unlink the socket");

        // the ledger tells the story and the cursor is caught up
        let ledger = Ledger::open(&root.join("camp.db")).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let types: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(types, vec!["campd.started", "campd.stopped"]);
        assert_eq!(
            ledger.cursor(cursor::CAMPD_CURSOR).unwrap(),
            1,
            "startup catch-up covered campd.started; campd.stopped (seq 2) \
             lands after the final catch-up — the next start covers it"
        );
    }
}

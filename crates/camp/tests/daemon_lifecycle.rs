#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 7 integration: campd lifecycle against the real binary (master
//! plan Phase 7 test obligations; spec §5, §13.3).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

fn camp_cmd(root: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_remove("CAMP_DIR").arg("--camp").arg(root);
    cmd
}

fn run_ok(root: &Path, args: &[&str]) -> String {
    let out = camp_cmd(root).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "camp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// camp init + one rig; returns the camp root (<tempdir>/.camp).
fn init_camp(dir: &Path) -> PathBuf {
    let status = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir)
        .arg("init")
        .status()
        .unwrap();
    assert!(status.success());
    let root = dir.join(".camp");
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let out = camp_cmd(&root)
        .args(["rig", "add"])
        .arg(&rig)
        .args(["--prefix", "gc", "--name", "gc"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    root
}

fn request(sock: &Path, line: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(sock).expect("connect to campd");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(line.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp).unwrap();
    serde_json::from_str(resp.trim_end()).expect("campd response is JSON")
}

fn campd_cursor(root: &Path) -> i64 {
    let conn = rusqlite::Connection::open(root.join("camp.db")).unwrap();
    conn.query_row("SELECT seq FROM cursors WHERE name = 'campd'", [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn max_seq(root: &Path) -> i64 {
    let conn = rusqlite::Connection::open(root.join("camp.db")).unwrap();
    conn.query_row("SELECT coalesce(max(seq), 0) FROM events", [], |r| r.get(0))
        .unwrap()
}

fn event_types(root: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(root.join("camp.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT type FROM events ORDER BY seq")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

/// A foreground daemon child. Spawn blocks until the readiness line
/// (deterministic — no connect polling); Drop SIGKILLs and reaps.
struct Daemon {
    child: Child,
    sock: PathBuf,
}

impl Daemon {
    fn spawn(root: &Path) -> Daemon {
        let mut child = Command::new(BIN)
            .env_remove("CAMP_DIR")
            .args(["daemon", "--camp"])
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon {
            child,
            sock: root.join("campd.sock"),
        }
    }

    fn request(&self, line: &str) -> serde_json::Value {
        request(&self.sock, line)
    }

    fn kill_dash_nine(&mut self) {
        self.child.kill().unwrap(); // SIGKILL on unix
        self.child.wait().unwrap();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Cleans up an auto-started (detached) daemon even when a test fails.
struct StopGuard {
    sock: PathBuf,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.sock) {
            let _ = stream.write_all(b"{\"op\":\"stop\"}\n");
            let mut resp = String::new();
            let _ = BufReader::new(stream).read_line(&mut resp);
        }
    }
}

#[test]
fn start_socket_accepts_and_status_is_sane() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    // seed: gc-1 ready; gc-2 open but blocked on gc-1
    run_ok(&root, &["create", "first"]);
    run_ok(&root, &["create", "second", "--needs", "gc-1"]);

    let daemon = Daemon::spawn(&root);
    let status = daemon.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    assert_eq!(status["campd_pid"], daemon.child.id());
    assert_eq!(status["ready"], 1);
    assert_eq!(status["open"], 2);
    assert_eq!(status["live_sessions"], serde_json::json!([]));

    assert!(event_types(&root).contains(&"campd.started".to_owned()));
    assert_eq!(
        campd_cursor(&root),
        max_seq(&root),
        "startup catch-up complete"
    );
}

/// The poke reply is an ack, not a completion signal (Phase 8, PR #14
/// review finding 2: ack-before-settle) — the cursor reaches the head
/// within the same wake, observed with a bounded test-side wait.
fn wait_cursor_at_head(root: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if campd_cursor(root) == max_seq(root) {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "cursor {} never reached head {}",
                campd_cursor(root),
                max_seq(root)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_cli_write_pokes_campd_and_the_cursor_advances() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _daemon = Daemon::spawn(&root);

    // create pokes before it exits; the settle lands in the same wake
    run_ok(&root, &["create", "poked"]);
    wait_cursor_at_head(&root);

    // the readiness recompute path runs on close, live
    run_ok(&root, &["close", "gc-1", "--outcome", "pass"]);
    wait_cursor_at_head(&root);
}

#[test]
fn kill_dash_nine_stale_socket_restart_and_exactly_once_catch_up() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let mut daemon = Daemon::spawn(&root);
    daemon.kill_dash_nine();

    let sock = root.join("campd.sock");
    assert!(
        sock.exists(),
        "SIGKILL leaves the socket file behind (stale)"
    );
    assert!(
        UnixStream::connect(&sock).is_err(),
        "stale socket refuses connections"
    );

    // the ledger keeps accepting writes while campd is dead (poke is
    // fire-and-forget; spec §7.2)
    run_ok(&root, &["create", "while dead"]);
    run_ok(&root, &["create", "also while dead", "--needs", "gc-1"]);
    run_ok(&root, &["close", "gc-1", "--outcome", "pass"]);
    let lagging = campd_cursor(&root);
    assert!(lagging < max_seq(&root), "no live campd: cursor must lag");

    // restart: the stale socket is unlinked and rebound; catch-up processes
    // the backlog exactly once (the transactional guarantee is unit-tested;
    // here the cursor lands exactly on the head and status agrees)
    let daemon2 = Daemon::spawn(&root);
    assert_eq!(campd_cursor(&root), max_seq(&root));
    let status = daemon2.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    assert_eq!(
        status["ready"], 1,
        "gc-2 was unblocked by gc-1's pass close"
    );
    assert_eq!(status["open"], 1);
}

#[test]
fn camp_stop_is_graceful() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let mut daemon = Daemon::spawn(&root);

    let out = run_ok(&root, &["stop"]);
    assert_eq!(out, "campd stopped\n");
    let status = daemon.child.wait().unwrap();
    assert!(status.success(), "graceful stop exits 0");
    assert!(!root.join("campd.sock").exists(), "stop unlinks the socket");
    assert!(event_types(&root).contains(&"campd.stopped".to_owned()));
}

#[test]
fn stop_errors_when_campd_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let out = camp_cmd(&root).arg("stop").output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("campd is not running"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn second_daemon_refuses_to_start_while_the_first_lives() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let daemon = Daemon::spawn(&root);

    let out = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["daemon", "--camp"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(!out.status.success(), "second daemon must refuse to start");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already running"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // the first daemon is unharmed
    let status = daemon.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
}

#[test]
fn camp_top_autostarts_campd_with_the_event_trail() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _guard = StopGuard {
        sock: root.join("campd.sock"),
    };

    let out = run_ok(&root, &["top"]);
    assert!(out.contains("campd pid: "), "top output: {out:?}");
    assert!(out.contains("ready: 0"), "top output: {out:?}");
    assert!(out.contains("open: 0"), "top output: {out:?}");

    // spec §13.3: the trail shows the cause — autostarted (by the cli, for
    // top) then started (by campd)
    let types = event_types(&root);
    let auto = types
        .iter()
        .position(|t| t == "campd.autostarted")
        .expect("campd.autostarted event");
    let started = types
        .iter()
        .position(|t| t == "campd.started")
        .expect("campd.started event");
    assert!(
        auto < started,
        "trail must read autostarted → started (cause before effect): {types:?}"
    );
    let events_json = run_ok(&root, &["events", "--json"]);
    assert!(
        events_json.contains(r#""type":"campd.autostarted","actor":"cli","data":{"verb":"top"}"#),
        "events: {events_json}"
    );

    // a second top finds the daemon up: no second autostart
    run_ok(&root, &["top"]);
    let autostarts = event_types(&root)
        .iter()
        .filter(|t| t.as_str() == "campd.autostarted")
        .count();
    assert_eq!(autostarts, 1);

    // graceful shutdown of the detached daemon
    run_ok(&root, &["stop"]);
}

/// PR #8 review findings 1 and 2 through the real surface: eight
/// concurrent `camp top` invocations against a daemonless camp. Every one
/// must succeed (losers of the start race must recognize the winner, not
/// error), and exactly one campd may end up owning the socket (the
/// replacement critical section is serialized — no split brain).
#[test]
fn concurrent_top_autostarts_exactly_one_campd() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _guard = StopGuard {
        sock: root.join("campd.sock"),
    };

    let children: Vec<Child> = (0..8)
        .map(|_| {
            camp_cmd(&root)
                .arg("top")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "camp top failed under concurrent auto-start: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let started = event_types(&root)
        .iter()
        .filter(|t| t.as_str() == "campd.started")
        .count();
    assert_eq!(started, 1, "exactly one campd may win the start race");

    run_ok(&root, &["stop"]);
}

#[test]
fn campd_symlink_runs_daemon_mode() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let link = dir.path().join("campd");
    std::os::unix::fs::symlink(BIN, &link).unwrap();

    let mut child = Command::new(&link)
        .env_remove("CAMP_DIR")
        .args(["--camp"])
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    assert!(
        line.starts_with(READY_PREFIX),
        "campd symlink did not enter daemon mode: {line:?}"
    );

    let mut daemon = Daemon {
        child,
        sock: root.join("campd.sock"),
    };
    let status = daemon.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    daemon.kill_dash_nine();
}

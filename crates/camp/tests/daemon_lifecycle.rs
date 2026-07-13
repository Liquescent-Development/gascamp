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
        .args(["init", "--no-service"])
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

/// The pure-client contract (design §4.3, and §9's test obligation): a
/// daemon-needing verb with campd DOWN fails loudly, names the remedy, and
/// starts NOTHING.
///
/// "Started nothing" is asserted structurally, not by scanning the process
/// table (which a parallel `cargo test` process tree would confound anyway).
/// Three independent tripwires, each of which the removed CLI-spawn path
/// would trip BEFORE the CLI could return:
///   1. `<camp>/campd.log` — the removed path opened it (create+append) BEFORE
///      it spawned the child, so this fires even on a regression that spawns a
///      daemon without blocking on its readiness line;
///   2. `<camp>/campd.sock` — a live campd binds it before serving anything;
///   3. a `campd.started` event — appended before the readiness line
///      (`daemon/mod.rs`), and the removed path BLOCKED on that line.
///
/// No sleep, no poll, no race.
///
/// `starts_before` is how many campds the TEST started by hand (a `kill -9`d
/// daemon leaves its `campd.started` in the ledger and its socket file on
/// disk — that is still "not running").
fn assert_no_campd_came_up(root: &Path, out: &std::process::Output, starts_before: usize) {
    assert!(
        !out.status.success(),
        "a daemon-needing verb must FAIL when campd is down; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !root.join("campd.log").exists(),
        "campd.log is created only by a CLI that is about to spawn a daemon: it must not exist"
    );
    let sock = root.join("campd.sock");
    assert!(
        !sock.exists() || UnixStream::connect(&sock).is_err(),
        "no campd may be listening: the CLI must never start one"
    );
    let types = event_types(root);
    assert_eq!(
        types
            .iter()
            .filter(|t| t.as_str() == "campd.started")
            .count(),
        starts_before,
        "the CLI must not have started a campd: {types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|t| t.as_str() == "campd.autostarted")
            .count(),
        0,
        "the CLI is a pure client: no campd.autostarted may ever be recorded again: {types:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in ["campd is not running", "camp service status", "camp daemon"] {
        assert!(
            stderr.contains(needle),
            "the error must name {needle:?} — the remedy IS the feature: {stderr}"
        );
    }
}

#[test]
fn start_socket_accepts_and_status_is_sane() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    // seed: gc-1 unblocked but unroutable (no assignee, rig has no
    // default_agent), so campd dispatch-fails it at startup and it becomes
    // `stuck` — no longer `ready` (issue #83); gc-2 open but blocked on gc-1
    run_ok(&root, &["create", "first"]);
    run_ok(&root, &["create", "second", "--needs", "gc-1"]);

    let daemon = Daemon::spawn(&root);
    let status = daemon.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    assert_eq!(status["campd_pid"], daemon.child.id());
    assert_eq!(status["ready"], 0);
    assert_eq!(status["open"], 2);
    assert_eq!(
        status["stuck"], 1,
        "gc-1 dispatch-failed as unroutable — stuck, not ready (issue #83)"
    );
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
    // gc-1's pass close unblocked gc-2; but gc-2 is unroutable (no assignee,
    // rig has no default_agent), so campd dispatch-fails it at startup — it
    // is `stuck`, not `ready` (issue #83). stuck==1 is itself the proof gc-2
    // was unblocked (a still-blocked bead would never be dispatch-attempted).
    assert_eq!(status["ready"], 0);
    assert_eq!(status["open"], 1);
    assert_eq!(
        status["stuck"], 1,
        "gc-2 was unblocked by gc-1's pass close (then dispatch-failed as unroutable → stuck)"
    );
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
fn camp_top_with_campd_down_fails_loudly_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());

    let out = camp_cmd(&root).arg("top").output().unwrap();

    assert_no_campd_came_up(&root, &out, 0);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no campd has ever started"),
        "a camp whose campd never ran must say so, not omit the pid: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Design §3: the down-campd error NAMES the pid from the ledger's last
/// `campd.started` — the operator's thread back to the process that died.
/// `kill -9` leaves a stale socket file; that is still "not running", never a
/// wedge, and the two errors must not be interchangeable.
#[test]
fn camp_top_after_a_kill_dash_nine_names_the_dead_campd_pid() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let mut daemon = Daemon::spawn(&root);
    let pid = daemon.child.id();
    daemon.kill_dash_nine();
    assert!(
        root.join("campd.sock").exists(),
        "kill -9 leaves the socket file behind (stale)"
    );

    let out = camp_cmd(&root).arg("top").output().unwrap();

    assert_no_campd_came_up(&root, &out, 1); // only the one WE started
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&pid.to_string()),
        "must name the dead campd's pid {pid}: {stderr}"
    );
    assert!(
        !stderr.contains("kill -9"),
        "a DEAD campd is not a wedged one: the remedies differ: {stderr}"
    );
}

/// `camp adopt` is a socket op executed BY campd (the registry and the timers
/// live in its memory), so it needs the daemon. Pure client: campd down is a
/// loud, actionable error — never a fresh daemon started behind the operator's
/// back just to answer a reconciliation request.
#[test]
fn camp_adopt_with_campd_down_fails_loudly_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());

    let out = camp_cmd(&root).arg("adopt").output().unwrap();

    assert_no_campd_came_up(&root, &out, 0);
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("adopted:"),
        "nothing was adopted: the summary line must not be printed"
    );
}

/// The happy path, unchanged: against a running campd, `camp top` is one
/// status query rendered as plain text.
#[test]
fn camp_top_against_a_running_campd_renders_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let daemon = Daemon::spawn(&root);

    let out = run_ok(&root, &["top"]);

    assert!(
        out.contains(&format!("campd pid: {}", daemon.child.id())),
        "top output: {out:?}"
    );
    assert!(out.contains("ready: 0"), "top output: {out:?}");
    assert!(out.contains("open: 0"), "top output: {out:?}");
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

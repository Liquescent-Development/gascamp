#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! cp-0 §8 state-machine tests (control-plane spec §2.3): the read channel
//! drains every tailed file to EOF on every wake; correctness never depends
//! on a delivered filesystem event. A `#!/bin/sh` fake worker holds a
//! session stdout file open; a `can_use_tool`-shaped line is appended with
//! its notify event suppressed; an UNRELATED wake (a socket poke) consumes
//! it. No real claude, no API spend.
//!
//! The stdout path is derived with the SAME `munge` the runtime uses
//! (cp-0 note 5): non-alphanumeric → '-'. The binary's `spawn::munge` is
//! not reachable from an integration test (the daemon module tree is
//! private), so the helper is mirrored here verbatim.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

/// cp-0 note 5: the exact `spawn::munge` the runtime uses to derive the
/// stdout path (`sessions/<munge(session)>.json`). Mirrored verbatim —
/// non-alphanumeric chars become '-'.
fn munge(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The stdout file path the read channel tails for `session`.
fn stdout_path(root: &Path, session: &str) -> PathBuf {
    root.join("sessions")
        .join(format!("{}.json", munge(session)))
}

fn camp(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn camp_ok(root: &Path, args: &[&str]) -> String {
    let out = camp(root, args);
    assert!(
        out.status.success(),
        "camp {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A camp with one rig + fake-agent (`isolation: none` so dispatch needs no
/// base commit) + a `dev` agent. `max_stream_env` overrides the stream cap
/// (CAMP_MAX_STREAM_BYTES) when `Some`. Returns (root, rig).
fn scaffold(dir: &Path, max_workers: usize) -> (PathBuf, PathBuf) {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\
             [dispatch]\nmax_workers = {max_workers}\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("dev.md"),
        "---\nname: dev\nisolation: none\n---\nWork.\n",
    )
    .unwrap();
    camp_ok(&root, &["events", "--json"]);
    (root, rig)
}

fn fake_agent() -> String {
    format!("{}/tests/fake-agent.sh", env!("CARGO_MANIFEST_DIR"))
}

struct Daemon {
    child: Child,
}

impl Daemon {
    /// Spawn campd with extra env vars. cp-0 note 1: pass
    /// `("CAMP_MAX_STREAM_BYTES", "64")` to inject a small stream cap; pass
    /// `("FAKE_AGENT_NUDGE_CLOSE", "1")` so the fake worker blocks on stdin
    /// (stays alive — the session stays registered for the read channel).
    fn spawn(root: &Path, envs: &[(&str, &str)]) -> Daemon {
        let mut cmd = Command::new(BIN);
        cmd.env_remove("CAMP_DIR")
            .env("CAMP_BIN", BIN)
            .args(["daemon", "--camp"])
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon { child }
    }

    /// crash-only: kill -9, no goodbye (the §8 restart test).
    fn kill9(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::forget(self); // Drop would double-kill
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect(root: &Path) -> UnixStream {
    let sock = root.join("campd.sock");
    for _ in 0..500 {
        if let Ok(s) = UnixStream::connect(&sock) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("campd socket never accepted");
}

fn request(stream: &mut UnixStream, line: &str) -> serde_json::Value {
    stream.write_all(line.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut resp = String::new();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    reader.read_line(&mut resp).unwrap();
    serde_json::from_str(resp.trim_end()).unwrap()
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    camp_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn wait_until(root: &Path, what: &str, pred: impl Fn(&[serde_json::Value]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let events = events_json(root);
        if pred(&events) {
            return;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}; events: {events:#?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// §8 read-on-wake + exit criteria: a `can_use_tool`-shaped line appended
/// to a tailed session stdout file WITH its notify event suppressed (the
/// file is written directly, not via the worker) is consumed on the next
/// UNRELATED wake (a socket poke). "Consumed" = the persisted byte offset
/// advanced past the line (the read channel read it).
#[test]
fn read_on_wake_consumes_a_line_with_the_notify_event_suppressed() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // FAKE_AGENT_NUDGE_CLOSE: the worker reads its task line then BLOCKS on
    // stdin — it stays alive, so the session stays registered for the read
    // channel to tail (otherwise the fake agent closes and exits in <1s).
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let mut stream = connect(&root);
    // Sling a bead so a worker dispatches (session.woke => the read channel
    // registers the session and tails its stdout file).
    let bead = camp_ok(&root, &["sling", "do the thing --json"])
        .trim()
        .to_owned();
    wait_until(&root, "session.woke", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == bead.as_str())
    });
    let woke = events_json(&root)
        .into_iter()
        .find(|e| e["type"] == "session.woke")
        .unwrap();
    let session = woke["data"]["name"].as_str().unwrap().to_owned();
    let stdout = stdout_path(&root, &session);
    // Append a can_use_tool-shaped line DIRECTLY to the stdout file,
    // bypassing the notify watcher (this IS the suppressed-event scenario).
    let line = "{\"type\":\"control_request\",\"request_id\":\"req-1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool\":\"Bash\"}}\n";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&stdout)
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    // Trigger an UNRELATED wake: a socket poke (not the read-channel watch).
    request(&mut stream, r#"{"op":"poke","seq":1}"#);
    // The read channel drained on the poke wake and consumed the line: the
    // persisted stream_cursor advanced past it (poll — the drain is fast).
    wait_until(&root, "the can_use_tool line consumed", |_| {
        camp_core::ledger::Ledger::open(&root.join("camp.db"))
            .unwrap()
            .stream_cursor(&session)
            .unwrap()
            >= line.len() as u64
    });
    // campd is unharmed
    let status = request(&mut stream, r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    drop(campd);
}

/// §8 Rescan drain: a synthetic Rescan / empty-paths notify event drains
/// every tailed file. Two sessions have suppressed lines appended; the
/// drain-all-on-every-wake rule means any wake (a poke) consumes BOTH.
#[test]
fn rescan_event_drains_every_tailed_file() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let mut stream = connect(&root);
    camp_ok(&root, &["sling", "task one --json"]);
    camp_ok(&root, &["sling", "task two --json"]);
    wait_until(&root, "two sessions", |e| {
        e.iter().filter(|ev| ev["type"] == "session.woke").count() == 2
    });
    let sessions: Vec<String> = events_json(&root)
        .into_iter()
        .filter(|e| e["type"] == "session.woke")
        .map(|e| e["data"]["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(sessions.len(), 2);
    // Append a line to each stdout file (suppressed).
    for session in &sessions {
        std::fs::OpenOptions::new()
            .append(true)
            .open(stdout_path(&root, session))
            .unwrap()
            .write_all(b"{\"type\":\"assistant\",\"text\":\"x\"}\n")
            .unwrap();
    }
    // The poke IS the wake — drain-all-on-every-wake drains both regardless
    // of the watch token (the Rescan/empty-paths robustness rule, §2.3). The
    // poke ack is sent BEFORE the event-loop drain block runs (the drain +
    // persist_offsets happens after the poke arm's settle, still in the same
    // wake), so poll for the persisted offsets rather than checking
    // immediately — a synchronous check races the drain on a slow/CI runner.
    request(&mut stream, r#"{"op":"poke","seq":1}"#);
    let sessions_check = sessions.clone();
    wait_until(&root, "both sessions drained", |e| {
        let _ = e; // poll the ledger directly, not the events
        let l = camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
        sessions_check
            .iter()
            .all(|s| l.stream_cursor(s).unwrap() > 0)
    });
    drop(campd);
}

/// §8 append-only cursors: kill campd mid-stream, restart, resume from the
/// persisted byte offset — no loss (the offset reaches EOF), no duplication
/// (only NEW lines are consumed after the restart).
#[test]
fn append_only_cursors_across_a_campd_restart_no_loss_no_duplication() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // FAKE_AGENT_HOLD_DIR: the worker claims then polls the filesystem for a
    // file that never appears (it does NOT read stdin to EOF). So it
    // OUTLIVES a killed campd (spec §2.3: "workers intentionally outlive a
    // killed campd") — adoption in life 2 sees it alive and re-arms, and the
    // read channel resumes from the persisted offset. (NUDGE_CLOSE would
    // EOF the worker on campd's death, letting adoption crash it.)
    let hold_dir = root.join("hold-dir");
    std::fs::create_dir_all(&hold_dir).unwrap();
    let campd1 = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_HOLD_DIR", hold_dir.to_str().unwrap())],
    );
    let mut stream = connect(&root);
    let bead = camp_ok(&root, &["sling", "restart test --json"])
        .trim()
        .to_owned();
    wait_until(&root, "session.woke", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == bead.as_str())
    });
    let session = events_json(&root)
        .into_iter()
        .find(|e| e["type"] == "session.woke")
        .unwrap()["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    let stdout = stdout_path(&root, &session);
    let line1 = b"{\"type\":\"assistant\",\"text\":\"one\"}\n";
    let line2 = b"{\"type\":\"assistant\",\"text\":\"two\"}\n";
    // Write line1, poke to drain + persist the offset.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&stdout)
        .unwrap()
        .write_all(line1)
        .unwrap();
    request(&mut stream, r#"{"op":"poke","seq":1}"#);
    wait_until(&root, "line1 consumed", |_| {
        let off = camp_core::ledger::Ledger::open(&root.join("camp.db"))
            .unwrap()
            .stream_cursor(&session)
            .unwrap();
        off >= line1.len() as u64
    });
    // Append line2 (consumed by the next drain — but we kill campd before),
    // simulating a mid-stream crash.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&stdout)
        .unwrap()
        .write_all(line2)
        .unwrap();
    drop(stream);
    campd1.kill9();
    // Life 2: restart campd. The read channel seeds from the live campd-
    // spawned worker and resumes from the persisted offset.
    let campd2 = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_HOLD_DIR", hold_dir.to_str().unwrap())],
    );
    let mut stream2 = connect(&root);
    // The startup settle + drain consumes line2 (the line past the
    // persisted offset). Poke to be sure.
    request(&mut stream2, r#"{"op":"poke","seq":1}"#);
    wait_until(&root, "line2 consumed after restart", |_| {
        let off = camp_core::ledger::Ledger::open(&root.join("camp.db"))
            .unwrap()
            .stream_cursor(&session)
            .unwrap();
        off >= (line1.len() + line2.len()) as u64
    });
    let ledger = camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
    let offset = ledger.stream_cursor(&session).unwrap();
    let file_len = std::fs::metadata(&stdout).unwrap().len();
    assert_eq!(offset, file_len, "no loss — offset at EOF after restart");
    assert_eq!(
        offset,
        (line1.len() + line2.len()) as u64,
        "no duplication — both lines consumed exactly once across the two lives"
    );
    drop(stream2);
    drop(campd2);
}

/// §8 ceiling (cp-0 note 1): a stream crossing `max_stream_bytes` fails the
/// session loudly — `session.stream_capped` (the named cause), then
/// `session.crashed` with `cause_seq` pointing at it, then the bead re-hooks
/// (a fresh session.woke for the same bead). The cap is injected small via
/// `CAMP_MAX_STREAM_BYTES` so the full path runs without writing 256 MiB.
#[test]
fn max_stream_bytes_breach_fails_the_session_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // A 64-byte cap so a small direct append breaches it.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_NUDGE_CLOSE", "1"),
            ("CAMP_MAX_STREAM_BYTES", "64"),
        ],
    );
    let mut stream = connect(&root);
    let bead = camp_ok(&root, &["sling", "ceiling test --json"])
        .trim()
        .to_owned();
    wait_until(&root, "session.woke", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == bead.as_str())
    });
    let session = events_json(&root)
        .into_iter()
        .find(|e| e["type"] == "session.woke")
        .unwrap()["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    // Grow the worker's stdout file past the 64-byte cap (suppressed write).
    std::fs::OpenOptions::new()
        .append(true)
        .open(stdout_path(&root, &session))
        .unwrap()
        .write_all(&[b' '; 128])
        .unwrap();
    // A poke wake drains, detects the breach, appends session.stream_capped,
    // and kills the worker. The reap (next SIGCHLD wake) appends
    // session.crashed with cause_seq pointing at stream_capped.
    request(&mut stream, r#"{"op":"poke","seq":1}"#);
    wait_until(&root, "session.stream_capped", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.stream_capped" && ev["data"]["session"] == session.as_str()
        })
    });
    let events = events_json(&root);
    let capped = events
        .iter()
        .find(|e| e["type"] == "session.stream_capped" && e["data"]["session"] == session.as_str())
        .unwrap();
    let cause_seq = capped["seq"].as_i64().unwrap();
    assert_eq!(capped["data"]["cap_bytes"], 64, "the event names the cap");
    // session.crashed with cause_seq pointing at stream_capped.
    wait_until(&root, "session.crashed with cause_seq", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.crashed"
                && ev["data"]["name"] == session.as_str()
                && ev["data"]["cause_seq"] == cause_seq
        })
    });
    // The bead re-hooks: a fresh session.woke for the same bead (the new
    // worker gets a new session name but the same bead).
    wait_until(&root, "the bead re-hooks", |e| {
        e.iter()
            .filter(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == bead.as_str())
            .count()
            == 2
    });
    drop(stream);
    drop(campd);
}

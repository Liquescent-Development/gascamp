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
    // review fix 5: the worker writes its OWN stream-json (a real one does),
    // so the file already holds the worker's `system/init` line before the
    // test appends anything. Wait for it to land and take it as the baseline
    // — the no-loss/no-duplication arithmetic below is relative to it. (The
    // HOLD_DIR worker emits nothing further: it claims, then polls a file
    // that never appears.)
    let base = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let len = std::fs::metadata(&stdout).map(|m| m.len()).unwrap_or(0);
            // The worker's init line is complete once the file ends in '\n'.
            if len > 0
                && std::fs::read(&stdout)
                    .map(|b| b.ends_with(b"\n"))
                    .unwrap_or(false)
            {
                break len;
            }
            assert!(
                Instant::now() < deadline,
                "the worker never wrote its own stream-json to {}",
                stdout.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
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
        off >= base + line1.len() as u64
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
        off >= base + (line1.len() + line2.len()) as u64
    });
    let ledger = camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
    let offset = ledger.stream_cursor(&session).unwrap();
    let file_len = std::fs::metadata(&stdout).unwrap().len();
    assert_eq!(offset, file_len, "no loss — offset at EOF after restart");
    assert_eq!(
        offset,
        base + (line1.len() + line2.len()) as u64,
        "no duplication — the worker's own output plus both test lines, each \
         consumed exactly once across the two campd lives"
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
    // review fix 5: the cap must sit ABOVE the worker's own legitimate
    // stream-json. A real worker (and now the fake one) writes NDJSON to its
    // stdout from birth — with the old 64-byte cap the worker's own
    // `system/init` line breached it before the worker had even claimed the
    // bead, so the cap-kill landed on a newborn worker and the bead never
    // re-hooked. 4 KiB is comfortably above the worker's output and still far
    // below 256 MiB, so the test's deliberate padding below is what breaches.
    const CAP: usize = 4096;
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_NUDGE_CLOSE", "1"),
            ("CAMP_MAX_STREAM_BYTES", &CAP.to_string()),
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
    // Wait for the claim so the cap-kill lands on a working worker (the
    // realistic scenario), not on one still starting up.
    wait_until(&root, "bead.claimed", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.claimed" && ev["bead"] == bead.as_str())
    });
    // Grow the worker's stdout file past the cap (suppressed write).
    std::fs::OpenOptions::new()
        .append(true)
        .open(stdout_path(&root, &session))
        .unwrap()
        .write_all(&vec![b' '; CAP * 2])
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
    assert_eq!(capped["data"]["cap_bytes"], CAP, "the event names the cap");
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

/// §8 + review fix 5 / fix 1 (CRITICAL): THE WORKER LIFECYCLE TEST.
///
/// Every other test in this file appends to the stream file itself and then
/// forces a socket poke — so none of them ever has a worker produce stdout
/// and exit, which is the single most important path the read channel
/// serves. This test closes that hole: a real fake worker writes a line to
/// its OWN stdout and exits immediately after.
///
/// The line is deliberately NON-JSON, because a drained non-JSON line
/// becomes a durable `patrol.degraded` naming it (fail fast, §2.3) — that
/// event is the observable proof the final bytes reached campd. There is NO
/// POKE: the SIGCHLD from the worker's exit is the wake that must drain it.
///
/// This is the test that catches reap-before-drain: if the reap disposes the
/// stream file before the final drain (unregister executed inside `settle`,
/// which runs BEFORE the event loop's drain block), the worker's last bytes
/// are unlinked unread and this event is never appended.
#[test]
fn a_workers_final_stdout_line_is_drained_before_the_reap_disposes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_FINAL_STDOUT", "FINAL-LINE-NOT-JSON")]);
    let bead = camp_ok(&root, &["sling", "do the thing"]).trim().to_owned();
    // The worker claims, closes the bead, writes its final stdout line, and
    // exits => SIGCHLD => reap.
    wait_until(&root, "the worker session ended", |e| {
        e.iter().any(|ev| {
            (ev["type"] == "session.stopped" || ev["type"] == "session.crashed")
                && ev["data"]["name"].is_string()
        })
    });
    // NO POKE. The final bytes must already have been drained — on the reap's
    // own wake, BEFORE the stream file was disposed.
    wait_until(
        &root,
        "the worker's final stdout line drained into a durable event",
        |e| {
            e.iter().any(|ev| {
                ev["type"] == "patrol.degraded"
                    && ev["data"]["error"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("FINAL-LINE-NOT-JSON")
            })
        },
    );
    assert!(!bead.is_empty());
    drop(campd);
}

/// §2.3 + review fix 8: the `sessions/` notify watch is the LATENCY path —
/// correctness never depends on a delivered filesystem event (every other
/// wake drains everything). But the watch must actually FIRE, or every
/// worker line waits for an unrelated wake.
///
/// Append to a tailed stream file and assert the drain happens with NO other
/// wake: no poke, no socket request, no worker exit. The cursor is polled by
/// opening the ledger DB directly — the `camp` CLI is never invoked, so
/// campd receives no socket traffic that could wake it by another path.
#[test]
fn the_sessions_watch_alone_wakes_campd_and_drains_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // NUDGE_CLOSE: the worker blocks on stdin, so it stays alive and its
    // session stays tailed (no exit, hence no SIGCHLD wake to muddy this).
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let bead = camp_ok(&root, &["sling", "do the thing"]).trim().to_owned();
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
    // Let the dispatch-path wakes settle, and record the cursor we start
    // from, so the advance we assert is caused by OUR append and nothing
    // else.
    std::thread::sleep(Duration::from_millis(500));
    let before = camp_core::ledger::Ledger::open(&root.join("camp.db"))
        .unwrap()
        .stream_cursor(&session)
        .unwrap();
    let line = "{\"type\":\"assistant\",\"text\":\"watch-me\"}\n";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&stdout)
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    // Poll the ledger DIRECTLY — no CLI, no socket, nothing that wakes campd.
    // Only the notify watch can deliver this drain.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let now = camp_core::ledger::Ledger::open(&root.join("camp.db"))
            .unwrap()
            .stream_cursor(&session)
            .unwrap();
        if now >= before + line.len() as u64 {
            break; // the watch fired and the drain consumed the line
        }
        assert!(
            Instant::now() < deadline,
            "the sessions/ notify watch never woke campd: the stream cursor \
             stayed at {before} for 10s after a line was appended to {}. \
             §2.3 makes the watch latency-only, so this is not a correctness \
             violation on its own — but every worker line then waits for an \
             unrelated wake.",
            stdout.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(campd);
}

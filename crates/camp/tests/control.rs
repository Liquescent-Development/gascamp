#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! cp-1 §8 state-machine tests (control-plane spec §2.1, §4.1, §4.4, §9): the
//! control plane, end to end over the REAL socket against a fake worker.
//!
//! campd writes a control_request into a worker's held stdin; the worker's
//! answer comes back as a line in its stdout file, which cp-0's read channel
//! tails; campd correlates it and appends `control.responded`. No real claude,
//! no API spend.
//!
//! The harness (munge, stdout_path, camp, camp_ok, scaffold, fake_agent,
//! Daemon, connect, request, events_json, wait_until) is mirrored VERBATIM from
//! tests/read_channel.rs — `camp` is a BINARY-only crate, so an integration test
//! cannot link `daemon::*` and each suite carries its own harness.

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
             [dispatch]\nmax_workers = {max_workers}\ncommand = \"{}\"\ndefault_agent = \"dev\"\n\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    // A DIRECTORY agent (compat §5.1): identity is the directory name, and
    // model/tools/permission are operator-owned via [agent_defaults] — camp
    // never inherits gc's unrestricted default (§5.2). The `isolation = "none"`
    // opt-out is unchanged in meaning: it is what lets dispatch run against the
    // plain rig dir with no base commit.
    let dev = root.join("agents/dev");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("agent.toml"), "isolation = \"none\"\n").unwrap();
    std::fs::write(dev.join("prompt.md"), "Work.\n").unwrap();
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

/// Wait for a ledger predicate, POKING campd on every pass.
///
/// THE POKE IS NOT INCIDENTAL — it is cp-0's contract, and every cp-0
/// integration test does the same thing. §2.3: *"the notify watcher is a
/// latency-only wake-up; the correctness rule is 'drain all tailed stream files
/// to EOF on EVERY wake — any poll token'"*. A poke IS a wake, and a wake drains
/// everything.
///
/// It is load-bearing HERE because of a measured platform property: on macOS,
/// FSEvents does NOT deliver an event for a worker's append through its
/// long-lived inherited stdout fd (a fresh open+write+close by another process
/// DOES fire one). So a worker's `control_response` may sit in its stream file,
/// unread, until campd wakes for some other reason. cp-0 designed for exactly
/// this — correctness never depends on a delivered event — and the poke is how
/// its tests force the wake deterministically instead of racing a watch that may
/// never fire.
///
/// Without the poke, the answer still lands: the pending request's own 30 s
/// deadline wakes campd, and harvest 1 ingests the answer BEFORE `expire_pending`
/// runs (B5's ordering), so no false fault fires. But the test would take 30 s
/// per assertion and would be measuring the deadline, not the round trip.
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
        // The wake. Any token drains every tailed stream file to EOF.
        if let Ok(mut s) = UnixStream::connect(root.join("campd.sock")) {
            let _ = s.write_all(b"{\"op\":\"poke\",\"seq\":1}\n");
            let mut line = String::new();
            let _ = BufReader::new(s).read_line(&mut line);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The name of the one live session (the tests scaffold exactly one).
fn live_session_name(root: &Path) -> String {
    events_json(root)
        .into_iter()
        .find(|e| e["type"] == "session.woke")
        .expect("a session must be live")["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Sling a bead, wait for its worker, and return (bead, session).
fn dispatch_one(root: &Path) -> (String, String) {
    let bead = camp_ok(root, &["sling", "do the thing --json"])
        .trim()
        .to_owned();
    wait_until(root, "session.woke", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == bead.as_str())
    });
    let session = live_session_name(root);
    (bead, session)
}

fn events_of(root: &Path, kind: &str) -> Vec<serde_json::Value> {
    events_json(root)
        .into_iter()
        .filter(|e| e["type"] == kind)
        .collect()
}

// ===== Task 6: interrupt + send_turn, end to end over the real socket ======

/// THE EXIT CRITERION: `interrupt` works end to end against a fake worker over
/// the REAL socket.
///
/// campd writes the control line into the worker's held stdin; the worker
/// answers on its stdout; cp-0's read channel tails that file; campd correlates
/// the `request_id` and appends `control.responded`. Every hop is real.
#[test]
fn interrupt_round_trips_through_the_read_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    // D1: the ACK is immediate — campd does NOT wait for the worker's answer.
    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "the interrupt was refused: {resp}");
    let request_id = resp["request_id"].as_str().unwrap().to_owned();
    assert!(
        request_id.starts_with("camp-"),
        "camp mints its own ids: {request_id}"
    );

    // D2: deliver -> record. The delivery is a ledger fact.
    wait_until(&root, "session.interrupted", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.interrupted" && ev["data"]["request_id"] == request_id.as_str()
        })
    });

    // ...and THE ANSWER COMES BACK, on the read channel, and is correlated.
    wait_until(&root, "control.responded", |e| {
        e.iter().any(|ev| {
            ev["type"] == "control.responded" && ev["data"]["request_id"] == request_id.as_str()
        })
    });
    let responded = events_of(&root, "control.responded")
        .into_iter()
        .find(|e| e["data"]["request_id"] == request_id.as_str())
        .unwrap();
    assert_eq!(responded["data"]["ok"], true);
    assert_eq!(responded["data"]["verb"], "session.interrupt");
    assert_eq!(
        responded["data"]["late"], false,
        "the answer arrived within the deadline — it is NOT a correction"
    );
    assert_eq!(responded["data"]["session"], session.as_str());

    // And nothing was faulted along the way.
    assert!(
        events_of(&root, "control.failed").is_empty(),
        "a clean round trip must produce NO control.failed: {:#?}",
        events_of(&root, "control.failed")
    );
    drop(campd);
}

/// C5, stated honestly: the answer-and-exit race is covered by HARVEST 1 under
/// MERGED LAW — the reap appends session.stopped BEFORE settle, so the
/// unregister is queued before `drain_all`, and `drain_all` reads the worker's
/// final bytes while it is still tailed.
///
/// The worker answers ONE interrupt and exits IMMEDIATELY, so the answer is its
/// LAST stdout bytes — written a breath before it dies. A reap-before-drain bug
/// unlinks that file with the answer unread, and the interrupt looks unanswered
/// forever.
///
/// THE REGRESSION SIGNAL: a future phase that moves the reap's append INSIDE
/// settle breaks the merged law; harvest 2 starts firing; cp-0's ordering guard
/// shouts; and this test goes RED on the `patrol.degraded` assertion.
#[test]
fn a_worker_that_answers_and_exits_immediately_still_yields_control_responded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_EXIT_AFTER_CONTROL", "1")]);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "{resp}");
    let request_id = resp["request_id"].as_str().unwrap().to_owned();

    // The worker answered and died. The answer must STILL have been drained.
    wait_until(&root, "control.responded despite the immediate exit", |e| {
        e.iter().any(|ev| {
            ev["type"] == "control.responded" && ev["data"]["request_id"] == request_id.as_str()
        })
    });
    assert!(
        events_of(&root, "control.failed").is_empty(),
        "the worker DID answer — campd must not fault it: {:#?}",
        events_of(&root, "control.failed")
    );

    // The ORDERING GUARD did not fire: the merged law still holds.
    let violations: Vec<_> = events_of(&root, "patrol.degraded")
        .into_iter()
        .filter(|e| {
            e["data"]["error"]
                .as_str()
                .is_some_and(|s| s.contains("ORDERING VIOLATION"))
        })
        .collect();
    assert!(
        violations.is_empty(),
        "cp-0's ordering guard fired — the reap now appends from INSIDE settle, \
         and a worker's last bytes are reaching the disposal path: {violations:#?}"
    );
    drop(campd);
}

/// C12 — THE ARM NO EARLIER REVISION SPECIFIED: a write that is ATTEMPTED and
/// FAILS.
///
/// It is NOT the same as "no pipe". Bytes may already have reached the pipe, and
/// `write_control` has torn it down — so the worker just lost its write channel.
/// That IS a campd action with a consequence, so it must be loud in BOTH channels:
/// an error to the caller AND a durable fault (§2.1 loudness; invariant 3).
///
/// The worker closes its stdin READ end and stays alive, so campd's write into the
/// pipe it still holds gets EPIPE. (A FULL pipe cannot drive this: the first write
/// that fails TEARS the pipe down, so a later interrupt would report `NoPipe`
/// instead — a different arm entirely.)
#[test]
fn an_interrupt_whose_pipe_write_fails_is_loud_in_both_the_response_and_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CLOSE_STDIN", "30")]);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    // The write is ATTEMPTED against a pipe whose reader is gone => EPIPE.
    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(
        resp["ok"], false,
        "a failed pipe write must be an ERROR to the caller, never a silent no-op: {resp}"
    );
    let error = resp["error"].as_str().unwrap();
    assert!(
        error.contains("failed"),
        "the error must say the write FAILED (not that there was no pipe): {error}"
    );

    // ...AND it is durable. An operator reading the ledger must find out that this
    // worker can no longer be sent turns or control messages.
    wait_until(&root, "a durable control.failed", |e| {
        e.iter()
            .any(|ev| ev["type"] == "control.failed" && ev["data"]["verb"] == "session.interrupt")
    });
    let failed = events_of(&root, "control.failed")
        .into_iter()
        .find(|e| e["data"]["verb"] == "session.interrupt")
        .unwrap();
    assert_eq!(
        failed["data"]["cause"], "write_failed",
        "the cause must be MACHINE-READABLE — rehydration ROUTES on it, and \
         `write_failed` is TERMINAL: no answer can ever arrive"
    );
    assert_eq!(failed["data"]["session"], session.as_str());

    // campd is unharmed. That is the whole point of the write being BOUNDED: an
    // unbounded write into a broken pipe is issue #55's wedge, on the event loop.
    let status = request(&mut connect(&root), r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true, "campd must still serve");
    drop(campd);
}

/// §4.1 `session.send_turn` (D4's replacement for the `nudge` socket verb): the
/// turn really lands in the held pipe, and the worker's blocked `read` really
/// unblocks — which is what closes the bead.
#[test]
fn send_turn_delivers_a_user_turn_into_the_held_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // The worker reads its task line, then BLOCKS on stdin until a later line.
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let mut stream = connect(&root);
    let (bead, session) = dispatch_one(&root);

    let resp = request(
        &mut stream,
        &serde_json::json!({
            "op": "session.send_turn", "session": session, "text": "status?",
        })
        .to_string(),
    );
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(
        resp["via"], "stdin",
        "the turn is in the HELD PIPE, not the resume path"
    );

    // It is a ledger fact (the merged vocabulary: a turn was injected).
    wait_until(&root, "session.nudged", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.nudged" && ev["data"]["via"] == "stdin")
    });

    // ...and the worker's blocked `read` REALLY unblocked: it closed the bead.
    // Nothing but a real delivery into a real pipe can do that.
    wait_until(&root, "the worker to close its bead", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["bead"] == bead.as_str())
    });
    drop(campd);
}

/// There is NO resume path for an interrupt. A worker campd holds no pipe to
/// simply CANNOT be interrupted — and answering `{"ok":true}` to that would be a
/// silent no-op dressed as success.
#[test]
fn interrupting_a_session_with_no_held_pipe_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[]);
    let mut stream = connect(&root);

    let resp = request(
        &mut stream,
        r#"{"op":"session.interrupt","session":"t/dev/nonexistent"}"#,
    );
    assert_eq!(resp["ok"], false, "{resp}");
    let error = resp["error"].as_str().unwrap();
    assert!(
        error.contains("no stdin pipe"),
        "the error must say WHY, in words an operator can act on: {error}"
    );

    // Nothing happened, so nothing is recorded: invariant 3 records ACTIONS,
    // and a refused verb is the caller's error, not a campd action.
    assert!(events_of(&root, "control.failed").is_empty());
    assert!(events_of(&root, "session.interrupted").is_empty());
    drop(campd);
}

/// B6: a campd restart across an in-flight interrupt INVENTS NO FAULT.
///
/// campd is killed with -9 while the interrupt is outstanding. The new campd
/// rebuilds the pending table from the LEDGER (the only thing that survives),
/// re-tails the worker's stdout from its persisted offset, reads the answer, and
/// correlates it. It must neither LIE (invent a fault for a request the worker
/// answered) nor FORGET (drop one it never did).
#[test]
fn a_campd_restart_across_an_in_flight_interrupt_invents_no_fault() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // LINGER_ON_EOF: the worker must OUTLIVE campd. campd holds the write end of
    // its stdin, so a kill -9 EOFs it — and a worker that exits there is B6's
    // NAMED residual (the session is adopted as crashed, never re-tailed, and its
    // answer is never read), not the case under test here.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("FAKE_AGENT_LINGER_ON_EOF", "30"),
        ],
    );
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "{resp}");
    let request_id = resp["request_id"].as_str().unwrap().to_owned();

    // Wait for the answer to reach the worker's STDOUT FILE — NOT the ledger.
    // That is the whole point: the bytes exist, and campd dies before it ever
    // reads them. (campd is not poked here, so it does not wake and ingest.)
    let stdout = stdout_path(&root, &session);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let content = std::fs::read_to_string(&stdout).unwrap_or_default();
        if content.contains(&request_id) && content.contains("control_response") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never answered; its stdout was: {}",
            std::fs::read_to_string(&stdout).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // The answer is on disk and campd has NOT ingested it.
    assert!(
        events_of(&root, "control.responded").is_empty(),
        "this test is only meaningful if campd dies BEFORE ingesting the answer"
    );

    // kill -9: no goodbye, no flush. Crash-only.
    campd.kill9();

    // A FRESH campd. It has never seen this request_id in memory — only in the
    // LEDGER, which is the only thing that survives a kill -9. It rebuilds the
    // pending table from `session.interrupted`, re-tails the (still live) worker's
    // stdout from its persisted byte offset, reads the answer that was already
    // sitting there, and correlates it.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("FAKE_AGENT_LINGER_ON_EOF", "30"),
        ],
    );

    wait_until(&root, "control.responded after the restart", |e| {
        e.iter().any(|ev| {
            ev["type"] == "control.responded" && ev["data"]["request_id"] == request_id.as_str()
        })
    });
    // It did not LIE: no invented fault for a request the worker really answered.
    assert!(
        events_of(&root, "control.failed").is_empty(),
        "the new campd must NOT invent a fault for a request the worker answered: {:#?}",
        events_of(&root, "control.failed")
    );
    drop(campd);
}

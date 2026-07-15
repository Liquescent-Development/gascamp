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

use std::io::{BufRead, BufReader, Read, Write};
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
    fn pid(&self) -> u32 {
        self.child.id()
    }
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

/// Wait until a session's stdout FILE contains `needle`. A worker's own stdout is
/// the only place its internal progress is observable — the ledger records what
/// CAMPD did, not what the worker has reached.
fn wait_for_stdout(root: &Path, session: &str, needle: &str) {
    let path = stdout_path(root, session);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if std::fs::read_to_string(&path)
            .unwrap_or_default()
            .contains(needle)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?} in {session}'s stdout: {:?}",
            std::fs::read_to_string(&path).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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

    // HAPPENS-BEFORE: wait until the worker has ACTUALLY closed its read end.
    // `session.woke` fires when campd SPAWNS the worker — long before the worker
    // gets there — so interrupting on that alone races the close, the write
    // succeeds, and the test flakes.
    wait_for_stdout(&root, &session, "stdin_closed");

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
    // LINGER_ON_EOF: the worker must OUTLIVE campd (campd holds the write end of
    // its stdin, so a kill -9 EOFs it — and a worker that exits there is B6's NAMED
    // residual, not the case under test).
    //
    // ANSWER_DELAY: the worker holds its answer for 3 s. That makes the race
    // DETERMINISTIC — campd is killed while the answer DOES NOT YET EXIST, so it
    // cannot possibly have ingested it, and rehydration is genuinely what reads it.
    let envs = [
        ("FAKE_AGENT_CONTROL_LOOP", "1"),
        ("FAKE_AGENT_LINGER_ON_EOF", "60"),
        ("FAKE_AGENT_CONTROL_ANSWER_DELAY", "3"),
    ];
    let campd = Daemon::spawn(&root, &envs);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "{resp}");
    let request_id = resp["request_id"].as_str().unwrap().to_owned();
    wait_until(&root, "session.interrupted", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.interrupted" && ev["data"]["request_id"] == request_id.as_str()
        })
    });

    // The worker is still holding its answer. campd CANNOT have ingested it.
    assert!(
        events_of(&root, "control.responded").is_empty(),
        "the answer does not exist yet — this test's whole point is that campd dies \
         BEFORE it can ingest one"
    );

    // kill -9: no goodbye, no flush. Crash-only.
    campd.kill9();

    // The worker now writes its answer into its stdout file, with NO campd running.
    let stdout = stdout_path(&root, &session);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let content = std::fs::read_to_string(&stdout).unwrap_or_default();
        if content.contains(&request_id) && content.contains("control_response") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never answered while campd was down: {}",
            std::fs::read_to_string(&stdout).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // A FRESH campd. It has never seen this request_id in memory — only in the
    // LEDGER, the one thing that survives a kill -9. It rebuilds the pending table
    // from `session.interrupted`, re-tails the (still live) worker's stdout from its
    // persisted byte offset, reads the answer that was already sitting there, and
    // correlates it.
    let campd = Daemon::spawn(&root, &envs);

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

// ===== Task 7: sessions.list ==============================================

/// §4.1/§4.2/§4.3: every live session, BY NAME.
///
/// It answers from the LEDGER's registry, not campd's child map — an ADOPTED
/// worker from a previous campd life is a live session too, and a fleet view
/// that could not see it would be lying by omission.
#[test]
fn sessions_list_reports_live_sessions_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let mut stream = connect(&root);
    let (bead, session) = dispatch_one(&root);

    let resp = request(&mut stream, r#"{"op":"sessions.list"}"#);
    assert_eq!(resp["ok"], true, "{resp}");
    let sessions = resp["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "one live session: {resp}");
    let s = &sessions[0];

    assert_eq!(s["name"], session.as_str());
    assert!(
        s["name"].as_str().unwrap().contains("/dev/"),
        "the NAME is the identity: {s}"
    );
    assert_eq!(s["agent"], "dev");
    assert_eq!(s["rig"], "gc");
    assert_eq!(s["bead"], bead.as_str());
    assert!(s["bead"].as_str().unwrap().starts_with("gc-"));
    assert_eq!(
        s["state"], "working",
        "cp-1's state is exactly two values: stalled | working"
    );
    assert_eq!(
        s["blocked"], false,
        "this worker asked no permission, so BLOCKED stays false (the baseline; \
         cp-3's positive case is a can_use_tool round-trip)"
    );

    // An RFC3339 timestamp campd can actually parse back.
    let ts = s["last_activity"].as_str().unwrap();
    assert!(
        ts.parse::<jiff::Timestamp>().is_ok(),
        "last_activity must be RFC3339: {ts:?}"
    );

    // §4.2: a protocol that hands out pids cannot cross a machine boundary.
    assert!(
        s.get("pid").is_none(),
        "sessions.list must NEVER carry a pid: {s}"
    );
    drop(campd);
}

// ===== Task 8: session.subscribe ==========================================

/// A subscribe connection. `camp` is a BINARY crate, so there is no
/// `socket::subscribe` to reuse — this is the harness's own idiom.
///
/// IT OWNS ITS BYTE BUFFER, AND THAT IS LOAD-BEARING. The obvious
/// implementation — `BufReader::read_line` under a socket read timeout — SILENTLY
/// LOSES DATA: when the timeout fires mid-line, the bytes already consumed are
/// discarded with the `Err`, and the next call resumes mid-frame. Under load that
/// eats whole frames (it ate `end` frames), and it makes a green test a coin
/// flip. Buffering the bytes HERE means a timeout costs nothing: the bytes stay
/// put and the next poll picks up where it left off.
#[derive(Debug)]
struct SubClient {
    stream: UnixStream,
    buf: Vec<u8>,
    eof: bool,
    #[allow(dead_code)]
    subscription: String,
    cursor: u64,
}

impl SubClient {
    fn open(root: &Path, session: &str, cursor: Option<u64>) -> std::io::Result<SubClient> {
        let stream = UnixStream::connect(root.join("campd.sock"))?;
        // The HELLO is bounded by REQUEST_TIMEOUT (5 s) — a WEDGED campd fails
        // HERE, and that is the exit criterion.
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let req = serde_json::json!({
            "op": "session.subscribe", "session": session, "cursor": cursor,
        });
        (&stream).write_all(format!("{req}\n").as_bytes())?;

        // Read the hello into OUR buffer, one byte at a time, so a timeout can
        // never swallow half of it.
        let mut buf: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = (&stream).read(&mut byte)?; // times out on a wedge
            if n == 0 {
                return Err(std::io::Error::other("campd closed before the hello"));
            }
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        let hello = String::from_utf8_lossy(&buf).into_owned();
        let v: serde_json::Value = serde_json::from_str(hello.trim_end())
            .map_err(|e| std::io::Error::other(format!("bad hello {hello:?}: {e}")))?;
        if v["ok"] != true {
            return Err(std::io::Error::other(format!("subscribe refused: {v}")));
        }
        assert_eq!(v["v"], 1, "the hello carries the protocol version");

        // §4.4: TIMEOUT-EXEMPT after the hello — a quiet stream is not a wedged
        // daemon. From here the socket is NON-BLOCKING and we poll it ourselves.
        stream.set_read_timeout(None)?;
        stream.set_nonblocking(true)?;
        Ok(SubClient {
            subscription: v["subscription"].as_str().unwrap_or_default().to_owned(),
            cursor: v["cursor"].as_u64().unwrap_or(0),
            stream,
            buf: Vec::new(),
            eof: false,
        })
    }

    /// Pop one complete line out of the owned buffer, if there is one.
    fn take_line(&mut self) -> Option<String> {
        let pos = self.buf.iter().position(|&b| b == b'\n')?;
        let line: Vec<u8> = self.buf.drain(..=pos).collect();
        Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned())
    }

    /// Read whatever is available into the owned buffer. Never loses a byte.
    fn fill(&mut self) {
        let mut chunk = [0u8; 65536];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    return;
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => {
                    self.eof = true;
                    return;
                }
            }
        }
    }

    /// The next frame, EOF, or nothing-yet — the three states a test must tell
    /// apart. "EOF" and "quiet" are the difference between "campd truncated the
    /// stream" and "nothing has happened yet", and conflating them is how a
    /// truncation test passes.
    fn read_frame_or_eof(&mut self, within: Duration) -> FrameOrEof {
        let deadline = Instant::now() + within;
        loop {
            if let Some(line) = self.take_line() {
                if line.trim().is_empty() {
                    continue;
                }
                return match serde_json::from_str(&line) {
                    Ok(v) => FrameOrEof::Frame(v),
                    Err(e) => panic!("campd put a NON-JSON line on the wire: {line:?}: {e}"),
                };
            }
            if self.eof {
                return FrameOrEof::Eof;
            }
            if Instant::now() > deadline {
                return FrameOrEof::Timeout;
            }
            self.fill();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The next frame, or None at EOF/quiet. `end` frames ARE returned — a test
    /// must be able to SEE one.
    ///
    /// EVERY subscriber test carries a HARD DEADLINE, because the §4.4 timeout
    /// exemption clears the read deadline: without one, a test that should FAIL
    /// would HANG instead, and a hang that "passes by timing out the harness" is
    /// exactly the failure mode these tests exist to make impossible.
    fn next_frame_within(&mut self, dur: Duration) -> Option<serde_json::Value> {
        match self.read_frame_or_eof(dur) {
            FrameOrEof::Frame(v) => Some(v),
            _ => None,
        }
    }

    /// The next frame, POKING campd while it waits.
    ///
    /// This is not belt-and-braces: campd only pumps a subscriber on a WAKE, and
    /// the stream watch is latency-only (§2.3) — on macOS it does not fire at all
    /// for a worker's appends through its inherited stdout fd. A subscriber whose
    /// session has produced bytes campd has not yet drained will therefore sit
    /// silent until something wakes campd. A poke IS that wake.
    fn next_frame_poking(&mut self, root: &Path, within: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(f) = self.next_frame_within(Duration::from_millis(250)) {
                return Some(f);
            }
            if self.eof || Instant::now() > deadline {
                return None;
            }
            if let Ok(mut s) = UnixStream::connect(root.join("campd.sock")) {
                let _ = s.write_all(b"{\"op\":\"poke\",\"seq\":1}\n");
                let mut line = String::new();
                let _ = BufReader::new(s).read_line(&mut line);
            }
        }
    }
}

enum FrameOrEof {
    Frame(serde_json::Value),
    Eof,
    Timeout,
}

/// THE EXIT CRITERION: a WEDGED campd fails the subscribe hello FAST.
///
/// A bare bound `UnixListener` is the wedge simulator: the kernel's listen backlog
/// ACCEPTS the connection even though no event loop will ever answer. Liveness is
/// an ANSWERED REQUEST, and `subscribe` must discover that at the hello — not hang
/// forever waiting for a stream that will never start.
#[test]
fn a_wedged_campd_fails_the_subscribe_hello_fast() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    // A socket that accepts and NEVER answers.
    let _wedged = std::os::unix::net::UnixListener::bind(root.join("campd.sock")).unwrap();

    let started = Instant::now();
    let err = SubClient::open(&root, "t/dev/1", None)
        .expect_err("a wedged campd must FAIL the hello, never hang on it");
    let elapsed = started.elapsed();

    assert!(
        matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "the hello must time out, not error some other way: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "the hello must fail INSIDE the request timeout — it took {elapsed:?}"
    );
}

/// B13/§4.4: a subscription is TIMEOUT-EXEMPT after the hello. A quiet stream is
/// not a wedged daemon, and a `camp watch` left open on an idle session must not
/// be killed for being idle.
#[test]
fn a_subscription_survives_a_quiet_period_longer_than_request_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    // Subscribe at the TAIL: nothing to stream.
    let mut sub = SubClient::open(&root, &session, None).unwrap();

    // Quiet for LONGER than REQUEST_TIMEOUT (5 s).
    std::thread::sleep(Duration::from_secs(6));

    // The subscription is still alive: make the worker speak, and a frame arrives.
    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "{resp}");
    // Force the wake (the watch is latency-only — see wait_until).
    wait_until(&root, "control.responded", |e| {
        e.iter().any(|ev| ev["type"] == "control.responded")
    });

    let frame = sub
        .next_frame_poking(&root, Duration::from_secs(30))
        .expect("the subscription must survive a quiet period longer than REQUEST_TIMEOUT");
    assert_eq!(frame["frame"], "event");
    drop(campd);
}

/// §9's RESUME PROMISE — and NOTHING tested it in five plan revisions.
///
/// Take frame K's `offset` OFF THE WIRE, reconnect with it as the `cursor`, and
/// frame K+1 must arrive FIRST, byte-identical, with NO `skipped` frame.
///
/// THIS IS THE ONLY TEST THAT CLOSES THE LOOP ON WHAT AN OFFSET *MEANS*. Every
/// other offset assertion is RELATIVE — and a drifting offset still INCREASES,
/// which is exactly why a cumulative one-byte-per-line drift survived five
/// revisions of "offsets are strictly increasing" assertions. A drifted cursor
/// lands MID-LINE and yields a `skipped{not_a_json_object}`; that is what this
/// test would catch.
#[test]
fn a_client_that_resubscribes_from_a_delivered_offset_resumes_exactly_there() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_SPAM_ON_TURN", "60"),
            ("FAKE_AGENT_SPAM_LINGER", "60"),
        ],
    );
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    // Make the worker produce a history.
    request(
        &mut stream,
        &serde_json::json!({"op":"session.send_turn","session":session,"text":"go"}).to_string(),
    );
    wait_until(&root, "the spam to be drained", |_| {
        std::fs::read_to_string(stdout_path(&root, &session))
            .unwrap_or_default()
            .matches("spam 59")
            .count()
            > 0
    });

    // FIRST subscription: read K frames and note frame K's offset AND the frame
    // that follows it.
    let mut first = SubClient::open(&root, &session, Some(0)).unwrap();
    let mut frames = Vec::new();
    for _ in 0..12 {
        frames.push(
            first
                .next_frame_poking(&root, Duration::from_secs(30))
                .expect("a frame"),
        );
    }
    let k = frames.len() - 1;
    let resume_at = frames[k]["offset"].as_u64().unwrap();
    let expected_next = first
        .next_frame_poking(&root, Duration::from_secs(30))
        .expect("frame K+1");
    drop(first);

    // SECOND subscription, resuming from the offset campd itself handed us.
    let mut second = SubClient::open(&root, &session, Some(resume_at)).unwrap();
    assert_eq!(
        second.cursor, resume_at,
        "the hello echoes the cursor it was given"
    );
    let got = second
        .next_frame_poking(&root, Duration::from_secs(30))
        .expect("a frame after resuming");

    assert_eq!(
        got, expected_next,
        "resuming from a delivered offset must yield the NEXT frame, BYTE-IDENTICAL. \
         A cursor that lands mid-line produces a `skipped` frame instead — which is \
         precisely what a per-line offset drift causes, and what no 'offsets \
         increase' assertion can ever see"
    );
    assert_ne!(
        got["frame"], "skipped",
        "a campd-issued offset must NEVER land mid-line"
    );
    drop(campd);
}

/// C6: a subscriber catching up across a LIVE BURST gets every line exactly once,
/// in order. The history must exceed MAX_PUMP_BYTES_PER_WAKE (256 KiB) or catch-up
/// finishes in ONE wake and the live-burst window never opens.
#[test]
fn a_subscriber_catching_up_across_a_live_burst_gets_every_line_exactly_once_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // 6000 lines x ~70 bytes ~= 420 KiB — comfortably past MAX_PUMP_BYTES_PER_WAKE.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_SPAM_ON_TURN", "6000"),
            ("FAKE_AGENT_SPAM_LINGER", "60"),
        ],
    );
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);
    request(
        &mut stream,
        &serde_json::json!({"op":"session.send_turn","session":session,"text":"go"}).to_string(),
    );
    wait_until(&root, "a history bigger than one pump budget", |_| {
        std::fs::metadata(stdout_path(&root, &session))
            .map(|m| m.len() > 300 * 1024)
            .unwrap_or(false)
    });

    let mut sub = SubClient::open(&root, &session, Some(0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen: Vec<u64> = Vec::new();
    let mut offsets: Vec<u64> = Vec::new();
    while seen.len() < 6000 {
        assert!(Instant::now() < deadline, "catch-up never completed");
        // Poke: any wake pumps (the watch is latency-only).
        let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
        while let Some(f) = sub.next_frame_within(Duration::from_millis(400)) {
            if f["frame"] != "event" {
                continue;
            }
            let content = f["event"]["message"]["content"].as_str().unwrap_or("");
            if let Some(n) = content.strip_prefix("spam ") {
                seen.push(n.parse().unwrap());
            }
            offsets.push(f["offset"].as_u64().unwrap());
            if seen.len() >= 6000 {
                break;
            }
        }
    }

    // EXACTLY ONCE, IN ORDER.
    let expected: Vec<u64> = (0..6000).collect();
    assert_eq!(seen, expected, "every line exactly once, in FILE ORDER");
    // ...with STRICTLY INCREASING offsets.
    assert!(
        offsets.windows(2).all(|w| w[1] > w[0]),
        "offsets must be strictly increasing"
    );
    drop(campd);
}

/// R1'S TEST — AND THE HOLE THREE PLAN REVISIONS COULD NOT SEE.
///
/// At the DEFAULT 1 MiB cap, a client READING EVERY FRAME catches up across a
/// history LARGER THAN THE CAP, and is NEVER dropped.
///
/// A cap that KILLS makes this impossible: during catch-up the producer is `pump`
/// reading a FILE (256 KiB/wake) against a socket that accepts ~8 KiB — a file
/// ALWAYS outruns a socket — so `out` hits the cap within a few wakes and the
/// client is killed HOWEVER FAST IT READS. That breaks §9's late-joiner promise for
/// any session with more than 1 MiB of stdout (which is ordinary), reports it as
/// backpressure about a client that was reading perfectly, and is PERMANENT:
/// re-subscribing re-fills and re-drops.
///
/// No earlier test could see it: a history under the cap never reaches it; a
/// non-reading client's drop is CORRECT; and an over-cap LINE is skipped, so `out`
/// stays tiny. The drop path was exercised only where dropping is right.
#[test]
fn a_reading_subscriber_survives_a_history_larger_than_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // ~2.4 MiB of DELIVERABLE lines (short, valid JSON — never skipped).
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_SPAM_ON_TURN", "35000"),
            ("FAKE_AGENT_SPAM_LINGER", "90"),
        ],
    );
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);
    request(
        &mut stream,
        &serde_json::json!({"op":"session.send_turn","session":session,"text":"go"}).to_string(),
    );
    wait_until(&root, "a history LARGER THAN THE 1 MiB CAP", |_| {
        std::fs::metadata(stdout_path(&root, &session))
            .map(|m| m.len() > 2 * 1024 * 1024)
            .unwrap_or(false)
    });

    // The DEFAULT cap (CAMP_SUBSCRIBER_BUFFER_BYTES is NOT set), cursor 0, and a
    // client that reads every frame in a tight loop.
    let mut sub = SubClient::open(&root, &session, Some(0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut seen = 0u64;
    let mut last_offset = 0u64;
    while seen < 35000 {
        assert!(
            Instant::now() < deadline,
            "a READING client never caught up (saw {seen}/35000) — it was starved or dropped"
        );
        let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
        while let Some(f) = sub.next_frame_within(Duration::from_millis(400)) {
            assert_ne!(
                f["frame"], "skipped",
                "these lines are all short and valid — none may be skipped: {f}"
            );
            if f["frame"] == "event" {
                let off = f["offset"].as_u64().unwrap();
                assert!(off > last_offset, "offsets strictly increase");
                last_offset = off;
                seen += 1;
            }
            if seen >= 35000 {
                break;
            }
        }
    }
    assert_eq!(seen, 35000, "every line arrived, exactly once");

    // THE POINT: the client was READING PERFECTLY, so it must NEVER be dropped.
    assert!(
        events_of(&root, "subscriber.dropped").is_empty(),
        "a client that read every frame was DROPPED — the cap was treated as a KILL \
         instead of a STOP, and every late joiner more than 1 MiB behind the tail is \
         now killed however fast it reads: {:#?}",
        events_of(&root, "subscriber.dropped")
    );
    drop(campd);
}

/// §8/B8/G3/R1: a subscriber that STOPS READING is dropped LOUDLY — and campd keeps
/// serving. This is the local-DoS the cap exists to prevent: 8 such connections
/// would permanently disable `subscribe` for everyone.
///
/// The drop now fires at the STALL TIMEOUT, because the peer accepted ZERO bytes —
/// which is the TRUE cause, and the one the event names.
#[test]
fn a_subscriber_that_stops_reading_is_dropped_loudly_and_campd_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_SPAM_ON_TURN", "30000"),
            ("FAKE_AGENT_SPAM_LINGER", "60"),
            // Without this the test is a mandatory 30 s wall-clock wait, and its own
            // deadline would have to exceed the hang it exists to detect.
            ("CAMP_SUBSCRIBER_STALL_TIMEOUT_MS", "300"),
        ],
    );
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    // Subscribe at the TAIL (a clean hello), then read NOTHING, ever.
    let sub = SubClient::open(&root, &session, None).unwrap();
    request(
        &mut stream,
        &serde_json::json!({"op":"session.send_turn","session":session,"text":"go"}).to_string(),
    );

    wait_until(&root, "subscriber.dropped", |e| {
        e.iter().any(|ev| ev["type"] == "subscriber.dropped")
    });
    let dropped = events_of(&root, "subscriber.dropped");
    assert_eq!(dropped.len(), 1, "{dropped:#?}");
    assert_eq!(dropped[0]["data"]["session"], session.as_str());
    assert_eq!(
        dropped[0]["data"]["cap_bytes"], 1_048_576u64,
        "the DEFAULT 1 MiB cap — the shipped configuration, not a toy"
    );
    assert!(
        dropped[0]["data"]["buffered_bytes"].as_u64().unwrap() > 0,
        "§4.4: the drop names the HIGH-WATER MARK"
    );

    // campd is unharmed — a fresh connection is answered promptly.
    let started = Instant::now();
    let status = request(&mut connect(&root), r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "campd kept serving"
    );
    drop(sub);
    drop(campd);
}

/// B7/§5.2: a NORMAL DETACH is not a fault. A client that hangs up is FORGOTTEN —
/// never libeled as backpressure.
#[test]
fn a_hung_up_subscriber_is_forgotten_and_is_never_libeled_as_backpressure() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root);

    let sub = SubClient::open(&root, &session, None).unwrap();
    drop(sub); // a normal detach

    // Drive three wakes.
    for _ in 0..3 {
        let status = request(&mut connect(&root), r#"{"op":"status"}"#);
        assert_eq!(status["ok"], true, "campd still answers promptly");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        events_of(&root, "subscriber.dropped").is_empty(),
        "a client that simply detached must NOT be recorded as dropped — that is \
         libel, and §5.2 says a normal detach is not a fault"
    );
    drop(campd);
}

/// G1 — THE TEST THE SUITE STRUCTURALLY COULD NOT CONTAIN.
///
/// One genuinely huge line (2 MiB — over the 1 MiB cap) exercises the OVERSIZE
/// SCAN and the `skipped` frame. The NEXT line must still arrive (the cursor
/// advanced past a line campd refused to buffer), and campd must NOT LIVELOCK.
///
/// Bounded by a hard deadline: a livelock manifests as a HANG, and a hanging test
/// that "passes by timing out the harness" is exactly the failure mode this test
/// exists to make impossible.
#[test]
fn a_line_larger_than_the_cap_is_skipped_and_campd_does_not_livelock() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(
        &root,
        &[
            // 8 MiB — AND THE SIZE IS THE POINT.
            //
            // This gate was sized at 2 MiB and asserted "campd answers in < 5 s".
            // The stream drain was O(n²) (B1), and 2 MiB cost only ~100 ms — so the
            // gate PASSED while campd froze for MINUTES on a bigger line. A livelock
            // gate calibrated just under the cliff is not a gate.
            //
            // 8 MiB is chosen because it is where the bug is UNMISSABLE: with the
            // quadratic drain it costs ~1.1 s in release and ~11 s in debug (CI runs
            // debug), so the 5 s deadline below FAILS. With the linear drain it costs
            // ~36 ms. That is the signal this test exists to carry.
            //
            // THE BOUND THIS TEST DEFENDS: campd stays responsive while consuming a
            // line far larger than one chunk AND larger than the subscriber cap —
            // the ordinary case on any session whose worker reads a file.
            ("FAKE_AGENT_HUGE_LINE", "8388608"),
            ("FAKE_AGENT_HUGE_LINE_LINGER", "90"),
        ],
    );
    let (_bead, session) = dispatch_one(&root);
    wait_until(&root, "the monster line to be written", |_| {
        std::fs::read_to_string(stdout_path(&root, &session))
            .unwrap_or_default()
            .contains("after the monster")
    });

    // ── THE RESPONSIVENESS MONITOR — AND IT IS THE POINT OF THIS TEST ──────────
    //
    // "campd answered a status AFTER the stream finished" CANNOT SEE THE BUG: the
    // freeze happens INSIDE the drain, and by the time the frames arrive it is long
    // over. That assertion passed at 2 MiB *and* at 8 MiB with the O(n²) drain still
    // in place — it merely took 17 s instead of 1 s. Sizing the fixture up was not
    // enough; the probe was in the wrong PLACE.
    //
    // So: hammer campd with `status` on a FRESH connection THROUGHOUT, and keep the
    // WORST round-trip. campd drains inside a single wake and answers NOTHING while
    // it does — so a freeze lands squarely on one of these probes.
    let probe_root = root.clone();
    let probing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let probing_flag = probing.clone();
    let monitor = std::thread::spawn(move || {
        let mut worst = Duration::ZERO;
        while probing_flag.load(std::sync::atomic::Ordering::Relaxed) {
            let t = Instant::now();
            if let Ok(mut s) = UnixStream::connect(probe_root.join("campd.sock")) {
                let _ = s.set_read_timeout(Some(Duration::from_secs(90)));
                if s.write_all(b"{\"op\":\"status\"}\n").is_ok() {
                    let mut line = String::new();
                    if BufReader::new(s).read_line(&mut line).is_ok() {
                        worst = worst.max(t.elapsed());
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        worst
    });

    // THE HELLO ITSELF CAN BE THE CASUALTY — and the DIAGNOSTIC is the value of this
    // gate. A campd frozen inside the drain cannot answer `session.subscribe` either,
    // so a bare `.unwrap()` here dies with an opaque `WouldBlock` and the operator
    // never sees WHY. Retry across the freeze; if it never comes back, SAY WHAT
    // HAPPENED.
    let hello_deadline = Instant::now() + Duration::from_secs(60);
    let mut sub = loop {
        match SubClient::open(&root, &session, Some(0)) {
            Ok(c) => break c,
            Err(e) => {
                assert!(
                    Instant::now() < hello_deadline,
                    "campd could not even answer the SUBSCRIBE HELLO ({e}) — it is \
                     WEDGED INSIDE THE DRAIN of a single large line, answering nothing \
                     at all: not the socket, not SIGCHLD, not a patrol timer. An O(n²) \
                     newline scan in the drain does exactly this (invariant 1, §4.3)"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut skipped: Option<serde_json::Value> = None;
    let mut after: Option<serde_json::Value> = None;
    while after.is_none() {
        assert!(
            Instant::now() < deadline,
            "campd LIVELOCKED on a line bigger than one chunk — the ordinary case on \
             any session that reads a file"
        );
        let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
        while let Some(f) = sub.next_frame_within(Duration::from_millis(500)) {
            if f["frame"] == "skipped" && f["reason"] == "over_cap" {
                skipped = Some(f);
            } else if f["frame"] == "event"
                && f["event"]["message"]["content"] == "after the monster"
            {
                after = Some(f);
                break;
            }
        }
    }

    let skipped = skipped.expect("a skipped{over_cap} frame naming the monster");
    assert!(
        skipped["bytes"].as_u64().unwrap() > 8_000_000,
        "the frame carries the line's TRUE byte count: {skipped}"
    );
    assert!(
        after.is_some(),
        "the NEXT line still arrived — the cursor advanced"
    );

    probing.store(false, std::sync::atomic::Ordering::Relaxed);
    let worst = monitor.join().unwrap();

    // ── THE GATE ───────────────────────────────────────────────────────────────
    // campd must stay RESPONSIVE THROUGHOUT. With an O(n²) newline scan in the drain,
    // an 8 MiB line costs ~11 s of 100%-CPU freeze inside ONE wake (debug) — the
    // socket, SIGCHLD and every patrol timer, all dead. With the linear scan it is
    // ~0.1 s. And the cost grows with the SQUARE of the line length, while
    // `max_stream_bytes` accepts lines 32x bigger than this one.
    assert!(
        worst < Duration::from_secs(5),
        "campd went UNRESPONSIVE for {worst:?} while draining ONE large line — it \
         answers nothing at all during a wake, so that is the socket, SIGCHLD and \
         every patrol timer frozen together (invariant 1, §4.3)"
    );

    // The client was reading perfectly — it must not be dropped.
    assert!(events_of(&root, "subscriber.dropped").is_empty());
    drop(campd);
}

/// §9: a cursor into a REAPED stream, or PAST the tail, is an EXPLICIT ERROR —
/// never an empty stream that looks like a quiet one.
#[test]
fn a_cursor_into_a_reaped_stream_or_past_the_tail_is_an_explicit_error() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root);

    // PAST THE TAIL.
    let err = SubClient::open(&root, &session, Some(999_999_999))
        .expect_err("a cursor past what campd has consumed must be refused");
    assert!(
        err.to_string().contains("past"),
        "the error must say WHY: {err}"
    );

    // A SESSION THAT WAS NEVER TAILED (a reaped stream's file is gone — §9: the
    // bytes went with it).
    let err = SubClient::open(&root, "t/dev/ghost", Some(0))
        .expect_err("a session campd is not tailing must be refused");
    assert!(
        err.to_string().contains("not tailing"),
        "the error must say WHY: {err}"
    );
    drop(campd);
}

/// B12/C7: a subscriber gets the FULL HISTORY, then an `end` frame, when its
/// session ends. Not a truncated prefix — and EOF NEVER arrives without an `end`
/// frame first.
///
/// PLUS THE ONE LINE THAT WOULD HAVE CAUGHT A PER-LINE OFFSET DRIFT:
/// the last `event` frame's offset is the byte just past that line; the `end`
/// frame's offset is `tail`. In a correct stream THEY ARE THE SAME NUMBER. Under a
/// one-byte-per-line drift they differ BY THE LINE COUNT — while every "offsets
/// strictly increase" assertion in the suite stays green.
#[test]
fn a_subscriber_gets_the_full_history_then_an_end_frame_when_its_session_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_EXIT_AFTER_CONTROL", "1")]);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    let mut sub = SubClient::open(&root, &session, Some(0)).unwrap();

    // Interrupt: the worker answers and EXITS — so the session is reaped.
    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "{resp}");

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut events: Vec<serde_json::Value> = Vec::new();
    let mut end: Option<serde_json::Value> = None;
    while end.is_none() {
        assert!(
            Instant::now() < deadline,
            "the end frame never arrived ({} events seen)",
            events.len()
        );
        let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
        loop {
            match sub.read_frame_or_eof(Duration::from_millis(500)) {
                FrameOrEof::Frame(f) => match f["frame"].as_str() {
                    Some("end") => {
                        end = Some(f);
                        break;
                    }
                    Some("event") => events.push(f),
                    _ => {}
                },
                // §9: EOF must NEVER arrive without an `end` frame first — that is a
                // silently truncated stream, and it is exactly what this test forbids.
                FrameOrEof::Eof => panic!(
                    "the subscription was CLOSED with NO end frame — a silently \
                     truncated stream (§9). {} events seen",
                    events.len()
                ),
                FrameOrEof::Timeout => break,
            }
        }
    }
    let end = end.unwrap();

    // The whole history came FIRST — the worker's init line and its
    // control_response are both in it.
    assert!(
        events.len() >= 2,
        "the FULL history must precede the end frame, not a truncated prefix: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"]["type"] == "control_response"),
        "the worker's LAST line (its answer) is part of the history"
    );

    // ── THE OFFSET-FIDELITY ASSERTION ────────────────────────────────────────
    let last_event = events.last().unwrap();
    assert_eq!(
        last_event["offset"], end["offset"],
        "the last event frame's offset and the end frame's offset MUST BE THE SAME \
         NUMBER. If they differ by the line count, `cursor` is drifting one byte per \
         line — and §9 makes these offsets the DURABLE RESUME CURSORS, so a client \
         reconnecting with one would land mid-file at the wrong byte"
    );
    // ─────────────────────────────────────────────────────────────────────────

    assert!(["stopped", "crashed"].contains(&end["reason"].as_str().unwrap()));

    // EOF FOLLOWS the end frame — and it must be a REAL EOF.
    //
    // `next_frame_within(..).is_none()` returns None for BOTH eof and a timeout, so
    // it would pass on an fd + slot LEAK (campd never closing the connection) just as
    // happily as on a correct close. Say what we mean.
    assert!(
        matches!(
            sub.read_frame_or_eof(Duration::from_secs(10)),
            FrameOrEof::Eof
        ),
        "campd must CLOSE the connection after the end frame — a timeout here is an \
         fd and a MAX_SUBSCRIBERS slot leaked, and `is_none()` cannot tell the two apart"
    );
    drop(campd);
}

/// The steady state of every long-lived watch: a subscriber CAUGHT UP AT THE TAIL,
/// with nothing buffered and nothing to pump. It must still get its `end` frame
/// when the session is reaped.
///
/// ⚠ WHAT THIS TEST DOES *NOT* PROVE — stated honestly, because an earlier plan
/// revision claimed it did: it does NOT gate the disposal ORDERING, and no
/// black-box test can. `on_watch_event` signals on EVERY arm and `unregister`'s
/// `remove_file` fires a notify, so campd always gets another wake — under a broken
/// ordering the disposed list simply persists and the NEXT wake emits the end frame
/// ONE WAKE LATE. This test is GREEN on the broken ordering. What it proves is that
/// the end frame ARRIVES for a caught-up subscriber. That is worth having, and it is
/// all it is. The ordering guarantee is STRUCTURAL (one caller, right after
/// `dispose_pending`).
#[test]
fn a_subscriber_caught_up_at_the_tail_gets_an_end_frame_when_its_session_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_EXIT_AFTER_CONTROL", "1")]);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    // Subscribe at cursor 0 and DRAIN EVERYTHING until fully caught up.
    let mut sub = SubClient::open(&root, &session, Some(0)).unwrap();
    let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
    while sub.next_frame_within(Duration::from_millis(400)).is_some() {}
    // Now the subscriber is caught up: out is empty, cursor == tail.

    // Let the worker answer and exit.
    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "{resp}");

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut end: Option<serde_json::Value> = None;
    while end.is_none() {
        assert!(
            Instant::now() < deadline,
            "a CAUGHT-UP subscriber never got its end frame — the steady state of \
             every long-lived watch"
        );
        let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
        while let Some(f) = sub.next_frame_within(Duration::from_millis(500)) {
            if f["frame"] == "end" {
                end = Some(f);
                break;
            }
        }
    }
    assert_eq!(end.unwrap()["frame"], "end");
    // ...and a REAL EOF follows (not merely silence — see above).
    assert!(
        matches!(
            sub.read_frame_or_eof(Duration::from_secs(10)),
            FrameOrEof::Eof
        ),
        "campd must CLOSE the connection after the end frame, or the fd and the \
         subscriber slot are leaked"
    );
    drop(campd);
}

/// The `HashSet<(session, offset)>` dedupe's ENTIRE REASON TO EXIST — and nothing
/// exercised it. TWO subscribers on ONE session hit the SAME over-cap line: BOTH
/// get a `skipped` frame, and EXACTLY ONE `patrol.degraded` is appended.
///
/// cp-2 inherits this dedupe.
#[test]
fn two_subscribers_on_one_session_share_an_over_cap_line_and_one_degraded_event() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HUGE_LINE", "2097152"),
            ("FAKE_AGENT_HUGE_LINE_LINGER", "60"),
        ],
    );
    let (_bead, session) = dispatch_one(&root);
    wait_until(&root, "the monster line", |_| {
        std::fs::read_to_string(stdout_path(&root, &session))
            .unwrap_or_default()
            .contains("after the monster")
    });

    let mut a = SubClient::open(&root, &session, Some(0)).unwrap();
    let mut b = SubClient::open(&root, &session, Some(0)).unwrap();

    let mut got_a = false;
    let mut got_b = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !(got_a && got_b) {
        assert!(Instant::now() < deadline, "both subscribers must be told");
        let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
        while let Some(f) = a.next_frame_within(Duration::from_millis(300)) {
            if f["frame"] == "skipped" && f["reason"] == "over_cap" {
                got_a = true;
            }
        }
        while let Some(f) = b.next_frame_within(Duration::from_millis(300)) {
            if f["frame"] == "skipped" && f["reason"] == "over_cap" {
                got_b = true;
            }
        }
    }
    assert!(got_a && got_b, "BOTH subscribers get the skipped frame");

    // ...and EXACTLY ONE durable event, not one per subscriber.
    let degraded: Vec<_> = events_of(&root, "patrol.degraded")
        .into_iter()
        .filter(|e| {
            e["data"]["error"]
                .as_str()
                .is_some_and(|s| s.contains("exceeds the subscriber buffer cap"))
        })
        .collect();
    assert_eq!(
        degraded.len(),
        1,
        "N subscribers hitting the SAME over-cap line must append ONE event, not N: \
         {degraded:#?}"
    );
    drop(campd);
}

/// §9: "durable across a campd restart for free". The subscription DIES with campd,
/// but the client's BYTE CURSOR stays valid and it resumes exactly there — no loss,
/// no duplication.
///
/// NOTE, HONESTLY: the client receives a bare EOF with NO `end` frame when campd
/// dies. That is a KNOWN GAP (recorded in the PR body); this test PINS the
/// client-visible behaviour so cp-2's `camp watch` inherits a documented contract
/// rather than a surprise.
#[test]
fn a_subscription_dies_with_campd_and_the_client_resumes_from_its_own_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("FAKE_AGENT_LINGER_ON_EOF", "60"),
        ],
    );
    let (_bead, session) = dispatch_one(&root);
    let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);

    let mut sub = SubClient::open(&root, &session, Some(0)).unwrap();
    let first = sub
        .next_frame_poking(&root, Duration::from_secs(30))
        .expect("the init line");
    assert_eq!(first["frame"], "event");
    let resume_at = first["offset"].as_u64().unwrap();

    // kill -9. Crash-only.
    campd.kill9();
    // The client sees a BARE EOF — no end frame. A known gap, pinned here.
    let tail = sub.next_frame_within(Duration::from_secs(5));
    assert!(
        tail.is_none(),
        "campd died: the client gets EOF (and, today, NO end frame): {tail:?}"
    );

    // A fresh campd. The client's cursor is still valid — it is the CLIENT's,
    // and the stream file is still on disk.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("FAKE_AGENT_LINGER_ON_EOF", "60"),
        ],
    );
    let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);

    let mut resumed = SubClient::open(&root, &session, Some(resume_at)).unwrap();
    assert_eq!(resumed.cursor, resume_at);
    // Everything from that byte on is still there — no loss, no duplication.
    if let Some(f) = resumed.next_frame_poking(&root, Duration::from_secs(30)) {
        assert_ne!(
            f["frame"], "skipped",
            "the client's own cursor must still land on a line boundary after a \
             campd restart: {f}"
        );
        assert!(f["offset"].as_u64().unwrap() > resume_at);
    }
    drop(campd);
}

/// G11: cp-0 ALREADY reports a non-JSON line as `patrol.degraded` from its drain.
/// campd must NOT report it a SECOND time from the file side — the subscriber gets
/// a `skipped{not_a_json_object}` frame and NO extra event.
#[test]
fn a_non_json_line_yields_a_skipped_frame_and_no_second_patrol_degraded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root);
    let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);

    // WAIT FOR THE WORKER'S OWN FIRST LINE BEFORE TOUCHING ITS FILE.
    //
    // campd hands the worker an fd positioned at 0 with NO O_APPEND, while this test
    // opens its own fd WITH O_APPEND. If the test writes first, the worker's next
    // write lands at ITS position — byte 0 — and the two INTERLEAVE, corrupting both
    // lines and producing a second, spurious non-JSON fault. (Exactly that happened
    // on Linux: a `non-JSON at offset 0` plus a truncated `e":"init"…` at offset 24.)
    // Once the init line is on disk the worker is blocked on stdin and writes nothing
    // more, so an append is safe.
    wait_for_stdout(&root, &session, "\"subtype\":\"init\"");

    let mut sub = SubClient::open(&root, &session, None).unwrap();

    // Append a NON-JSON line directly to the stream file.
    std::fs::OpenOptions::new()
        .append(true)
        .open(stdout_path(&root, &session))
        .unwrap()
        .write_all(b"this is not json at all\n")
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut skipped: Option<serde_json::Value> = None;
    while skipped.is_none() {
        assert!(Instant::now() < deadline, "the skipped frame never arrived");
        let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
        while let Some(f) = sub.next_frame_within(Duration::from_millis(400)) {
            if f["frame"] == "skipped" {
                skipped = Some(f);
                break;
            }
        }
    }
    assert_eq!(skipped.unwrap()["reason"], "not_a_json_object");

    // cp-0 reports it ONCE (from its own drain). The file side must NOT report it
    // again — that is the double-report the design forbids.
    let non_json: Vec<_> = events_of(&root, "patrol.degraded")
        .into_iter()
        .filter(|e| {
            e["data"]["error"]
                .as_str()
                .is_some_and(|s| s.contains("non-JSON"))
        })
        .collect();
    assert_eq!(
        non_json.len(),
        1,
        "cp-0 already owns this fault; the subscriber path must not re-report it: \
         {non_json:#?}"
    );
    drop(campd);
}

// ===== §2.1: a worker that dies with an unanswered interrupt ===============

/// The pids of THIS campd's worker children.
///
/// Scoped to one campd on purpose: `pgrep -f fake-agent.sh` would match every
/// worker of every test running in parallel, and killing those is how one test
/// silently breaks seven others.
fn worker_pids(campd_pid: u32) -> Vec<u32> {
    let out = std::process::Command::new("pgrep")
        .args(["-P", &campd_pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn kill_pids(pids: &[u32]) {
    for pid in pids {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
}

/// VT-3 — §2.1, THROUGH THE DAEMON: a worker that DIES with an unanswered
/// interrupt still faults LOUDLY.
///
/// `forget_session`'s unit test calls it directly; nothing proved its WIRING. This
/// drives the real thing: campd delivers the interrupt, the worker is killed before
/// it can answer, campd reaps it, disposes the session, and
/// `close_disposed` -> `forget_session` must append
/// `control.failed{cause:"session_ended"}`.
///
/// This is the MOST LIKELY real scenario (the interrupt worked; the worker died
/// before flushing its ack) and the request must NEVER vanish with no event.
#[test]
fn a_worker_killed_with_an_unanswered_interrupt_faults_loudly_through_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // The worker holds its answer for a long time, so it is provably UNANSWERED
    // when we kill it.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("FAKE_AGENT_CONTROL_ANSWER_DELAY", "600"),
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
    wait_until(&root, "session.interrupted", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.interrupted" && ev["data"]["request_id"] == request_id.as_str()
        })
    });
    assert!(
        events_of(&root, "control.responded").is_empty(),
        "the worker is holding its answer — the request is genuinely unanswered"
    );

    // The worker dies. campd is UP and watching.
    let workers = worker_pids(campd.pid());
    assert!(!workers.is_empty(), "campd must hold a worker child");
    kill_pids(&workers);

    wait_until(&root, "control.failed{session_ended}", |e| {
        e.iter().any(|ev| {
            ev["type"] == "control.failed"
                && ev["data"]["request_id"] == request_id.as_str()
                && ev["data"]["cause"] == "session_ended"
        })
    });
    let failed = events_of(&root, "control.failed")
        .into_iter()
        .find(|e| e["data"]["request_id"] == request_id.as_str())
        .unwrap();
    assert_eq!(failed["data"]["session"], session.as_str());
    assert_eq!(failed["data"]["verb"], "session.interrupt");
    drop(campd);
}

/// BD-1 — THE SAME SEAM, ACROSS A RESTART. §2.1's SWALLOWED FAULT.
///
/// campd delivers an interrupt, campd dies, THE WORKER DIES DURING THE OUTAGE, and
/// a fresh campd starts. Adoption marks the session `crashed`, so the session is
/// never registered and therefore never disposed — `forget_session` NEVER RUNS.
///
/// `rehydrate` is the only thing left that can speak for that request, and it must:
/// the ledger otherwise holds `session.interrupted{request_id}` with **no terminal
/// event, forever**. That is precisely the swallowed fault §2.1 forbids — and it is
/// worse than the residual the plan RECORDED for this case ("expires into a
/// control.failed whose stated cause is false"), because it produces NO EVENT AT ALL.
#[test]
fn a_restart_after_the_worker_also_died_still_faults_the_unanswered_interrupt() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let envs = [
        ("FAKE_AGENT_CONTROL_LOOP", "1"),
        ("FAKE_AGENT_CONTROL_ANSWER_DELAY", "600"),
    ];
    let campd = Daemon::spawn(&root, &envs);
    let mut stream = connect(&root);
    let (_bead, session) = dispatch_one(&root);

    let resp = request(
        &mut stream,
        &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
    );
    assert_eq!(resp["ok"], true, "{resp}");
    let request_id = resp["request_id"].as_str().unwrap().to_owned();
    wait_until(&root, "session.interrupted", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.interrupted" && ev["data"]["request_id"] == request_id.as_str()
        })
    });

    // BOTH die: campd first, then the worker, during the outage. The worker's pid
    // is captured BEFORE the kill — once campd is gone the worker is orphaned and
    // `pgrep -P` can no longer find it.
    let workers = worker_pids(campd.pid());
    assert!(!workers.is_empty(), "campd must hold a worker child");
    campd.kill9();
    kill_pids(&workers);
    std::thread::sleep(Duration::from_millis(300));

    // A fresh campd. Adoption will find the worker gone and crash the session, so it
    // is NEVER re-tailed and NEVER disposed. Only `rehydrate` can speak for the
    // request now.
    let campd = Daemon::spawn(&root, &envs);
    let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);

    wait_until(&root, "a TERMINAL event for the interrupt", |e| {
        e.iter().any(|ev| {
            (ev["type"] == "control.failed" || ev["type"] == "control.responded")
                && ev["data"]["request_id"] == request_id.as_str()
        })
    });

    let failed = events_of(&root, "control.failed")
        .into_iter()
        .find(|e| e["data"]["request_id"] == request_id.as_str())
        .expect(
            "SWALLOWED: the interrupt produced NEITHER control.responded NOR \
             control.failed. The ledger holds session.interrupted with no terminal \
             event, forever — §2.1's swallowed fault",
        );
    assert_eq!(
        failed["data"]["cause"], "session_ended",
        "the session ended with the request unanswered — byte-for-byte the event \
         forget_session produces when campd is up"
    );
    assert_eq!(failed["data"]["session"], session.as_str());
    let after_first = events_of(&root, "control.failed").len();
    assert_eq!(after_first, 1, "exactly one fault");
    drop(campd);

    // ---- THE NEW CASE THIS FIX CREATES, AND IT MUST NOT REGRESS --------------
    // `rehydrate` now APPENDS an event. An append that is not IDEMPOTENT would
    // re-fault the same request on EVERY campd start, forever — a fault storm in
    // place of a swallowed fault, which is not an improvement.
    //
    // It is idempotent BY CONSTRUCTION: the `control.failed{session_ended}` it just
    // appended is a TERMINAL cause, so the next `rehydrate` routes this id to
    // `answered` and says nothing. Restart twice and prove it.
    let campd = Daemon::spawn(&root, &envs);
    let _ = request(&mut connect(&root), r#"{"op":"poke","seq":1}"#);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        events_of(&root, "control.failed").len(),
        after_first,
        "a SECOND restart must append NOTHING — the session_ended fault is terminal, \
         so rehydrate must route this id to `answered`. A non-idempotent append would \
         re-fault the same request on every start, forever"
    );
    drop(campd);
}

// ===== cp-2: fleet.subscribe / camp watch =================================

/// Open a fleet.subscribe connection and read + assert its hello. Mirrors the
/// SubConn idiom used for session.subscribe.
fn fleet_subscribe(root: &Path) -> std::io::BufReader<UnixStream> {
    let mut stream = connect(root);
    stream.write_all(b"{\"op\":\"fleet.subscribe\"}\n").unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let mut hello = String::new();
    reader.read_line(&mut hello).unwrap();
    let v: serde_json::Value = serde_json::from_str(hello.trim_end()).unwrap();
    assert_eq!(v["ok"], true, "fleet hello: {v}");
    assert!(
        v["subscription"].as_str().is_some(),
        "fleet hello has a subscription id: {v}"
    );
    reader
}

/// THE EXIT CRITERION: the fleet renders live sessions from the socket ALONE,
/// delivered at hello time with NO client poke. Subscribe AFTER a worker is
/// live; the snapshot (its `session` frame + `synced`) arrives push-only.
#[test]
fn fleet_subscribe_delivers_a_live_session_and_synced() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root); // dispatch_one returns (bead, session)

    let mut reader = fleet_subscribe(&root);
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();

    // NO poke: the snapshot is delivered by the post-hello pump.
    let mut saw_session = false;
    let mut saw_synced = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !(saw_session && saw_synced) {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
                if v["frame"] == "session" && v["session"]["name"] == session.as_str() {
                    assert_eq!(v["session"]["state"], "working");
                    assert!(
                        !line.contains("\"pid\""),
                        "§4.2: no pid on the wire: {line}"
                    );
                    saw_session = true;
                }
                if v["frame"] == "synced" {
                    saw_synced = true;
                }
            }
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("read: {e}"),
        }
    }
    assert!(
        saw_session,
        "the live session must appear in the fleet snapshot"
    );
    assert!(saw_synced, "the snapshot must end with a synced frame");
    drop(campd);
}

/// A completion is PUSHED, not polled: a session that ends yields a `gone` frame
/// to a live fleet subscriber with NO poke OF THE FLEET CONNECTION. The frame
/// rides a genuine wake from the real reap (the worker's SIGCHLD, or the
/// interrupt connection closing) — never a fleet-connection poll. Which wake
/// carries it does not matter; that no fleet-connection poke does is the proof.
#[test]
fn fleet_subscribe_pushes_a_gone_on_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_EXIT_AFTER_CONTROL", "1")]);
    let (_bead, session) = dispatch_one(&root);

    let mut reader = fleet_subscribe(&root);
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();

    // Drain the snapshot (session + synced) so the next frame we see is the delta.
    for _ in 0..8 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if serde_json::from_str::<serde_json::Value>(line.trim_end())
            .map(|v| v["frame"] == "synced")
            .unwrap_or(false)
        {
            break;
        }
    }

    // Trigger the worker's exit: interrupt it (CAUSE of the transition), exactly
    // as a_worker_that_answers_and_exits_immediately's test does. The worker
    // answers and exits -> SIGCHLD.
    {
        let mut ctl = connect(&root);
        let _ = request(
            &mut ctl,
            &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#),
        );
    }

    // NO poke of the fleet connection: the `gone` must ride the SIGCHLD wake.
    let mut saw_gone = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !saw_gone {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
                if v["frame"] == "gone" && v["name"] == session.as_str() {
                    saw_gone = true;
                }
            }
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("read: {e}"),
        }
    }
    assert!(
        saw_gone,
        "a session that ends must PUSH a gone frame — no poke"
    );
    drop(campd);
}

/// cp-3 §5.3 EXIT CRITERION, end to end: a fake worker's can_use_tool BLOCKS,
/// SURFACES as BLOCKED, is ANSWERED over the socket, and the decision is a ledger
/// event with its cause — and the worker CONTINUES. The whole permission plane in
/// one round trip against a real campd.
#[test]
fn a_worker_blocks_on_can_use_tool_is_answered_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CAN_USE_TOOL", "1")]);
    let (bead, session) = dispatch_one(&root);

    // (1) it surfaces: sessions.list shows blocked:true, and a permission.pending
    // event carries its cause. (`wait_until` pokes campd — the drain that surfaces
    // the worker's notify-suppressed can_use_tool append on macOS.)
    wait_until(&root, "the worker BLOCKED", |e| {
        e.iter()
            .any(|ev| ev["type"] == "permission.pending" && ev["data"]["tool_name"] == "Bash")
    });
    let req = events_json(&root)
        .into_iter()
        .find(|e| e["type"] == "permission.pending")
        .unwrap()["data"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut stream = connect(&root);
    let list = request(&mut stream, r#"{"op":"sessions.list"}"#);
    assert!(
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == session.as_str() && s["blocked"] == true),
        "the fleet shows the worker BLOCKED: {list}"
    );

    // (2) a permission.pending event exists with its cause (asserted above via req)

    // (3) answer it over the socket
    let dec = request(
        &mut stream,
        &format!(
            r#"{{"op":"session.permission_decision","session":"{session}","request_id":"{req}","decision":"allow"}}"#
        ),
    );
    assert_eq!(dec["ok"], true, "{dec}");
    assert_eq!(dec["decision"], "allow", "{dec}");

    // (4) the decision is a ledger event with who/what
    wait_until(&root, "the permission.decided", |e| {
        e.iter().any(|ev| ev["type"] == "permission.decided")
    });
    let decided = events_json(&root)
        .into_iter()
        .rev()
        .find(|e| e["type"] == "permission.decided")
        .unwrap();
    assert_eq!(decided["data"]["decision"], "allow");
    assert_eq!(decided["data"]["decided_by"], "operator");
    assert_eq!(decided["data"]["request_id"], req.as_str());

    // (5) the worker CONTINUED and unblocked — it received the allow and closed
    wait_until(&root, "the worker continued past the permission", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["bead"] == bead.as_str())
    });
    drop(campd);
}

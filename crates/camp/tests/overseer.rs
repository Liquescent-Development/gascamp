#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! cp-5 exit criterion (control-plane §5.4): the overseer performs EVERY §5.4
//! action against a fake fleet THROUGH THE SOCKET ALONE, driving the exact
//! `camp` CLI verbs the operator skill names. The no-private-paths instrument
//! (Task 6) proves the socket is both NECESSARY and SUFFICIENT.
//!
//! The harness (BIN, munge, stdout_path, camp, camp_ok, scaffold, fake_agent,
//! Daemon, events_json, wait_until, live_session_name, dispatch_one,
//! wait_for_stdout) is mirrored from tests/control.rs — `camp` is a BINARY-only
//! crate, so an integration test cannot link `daemon::*` and each suite carries
//! its own harness (see control.rs).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

/// The exact `spawn::munge` the runtime uses to derive the stdout path
/// (`sessions/<munge(session)>.json`). Non-alphanumeric chars become '-'.
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

/// A camp with one rig + fake-agent (`isolation: none`) + a `dev` agent.
/// Returns (root, rig).
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

    /// crash-only: kill -9, no goodbye. Consumes self (mem::forget avoids the
    /// double-kill in Drop).
    fn kill9(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::forget(self);
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn events_json(root: &Path) -> Vec<Value> {
    camp_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Wait for a ledger predicate, POKING campd on every pass (cp-0's contract: a
/// poke IS a wake, and a wake drains every tailed stream file to EOF).
fn wait_until(root: &Path, what: &str, pred: impl Fn(&[Value]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let events = events_json(root);
        if pred(&events) {
            return;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}; events: {events:#?}");
        }
        if let Ok(mut s) = UnixStream::connect(root.join("campd.sock")) {
            let _ = s.write_all(b"{\"op\":\"poke\",\"seq\":1}\n");
            let mut line = String::new();
            let _ = BufReader::new(s).read_line(&mut line);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The name of the first live session (session.woke).
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

/// Wait until a session's stdout FILE contains `needle`.
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

// ===== Task 5: every §5.4 action against a fake fleet, over the socket =====

/// §5.4 "it can list sessions": `camp sessions --json` returns EVERY live
/// session by name — proving the overseer discovers the fleet over the socket,
/// not by reading `sessions/`.
#[test]
fn camp_sessions_lists_the_whole_fleet_by_name_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    // Two concurrent workers, each lingering in the control loop so both are
    // LIVE at the same time (cardinality >= 2 -> name-addressing is forced).
    let _d = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("FAKE_AGENT_LINGER_ON_EOF", "30"),
        ],
    );
    camp_ok(&root, &["sling", "first", "--agent", "dev"]);
    camp_ok(&root, &["sling", "second", "--agent", "dev"]);
    // Wait until the ledger shows two live sessions (both woke).
    wait_until(&root, "two live sessions", |events| {
        events.iter().filter(|e| e["type"] == "session.woke").count() >= 2
    });

    let out = camp_ok(&root, &["sessions", "--json"]);
    let sessions: Vec<Value> = serde_json::from_str(out.trim()).unwrap();
    assert!(sessions.len() >= 2, "expected >=2 live sessions, got: {out}");
    // Every row is addressed BY NAME (§4.2, `SessionInfo`'s doc comment).
    for s in &sessions {
        assert!(s["name"].as_str().is_some_and(|n| !n.is_empty()));
    }
    // FUTURE-REGRESSION TRIPWIRE, not run coverage: `SessionInfo` never
    // serializes a pid today, so this is a tautology now. It is kept ONLY to go
    // RED the day someone adds a `pid` field to the frozen wire (§4.2 rule 1) —
    // labelled so a reviewer does not count it as behavioural evidence.
    for s in &sessions {
        assert!(s.get("pid").is_none(), "SessionInfo must never carry a pid: {s}");
    }
}

/// §5.4 "send them turns": `camp nudge` injects a user turn into the live
/// worker's campd-held stdin (via=stdin) — over the socket.
#[test]
fn camp_nudge_delivers_a_turn_into_the_live_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("FAKE_AGENT_LINGER_ON_EOF", "30"),
        ],
    );
    let (_bead, session) = dispatch_one(&root);
    // The session must be live with a held pipe before we nudge.
    wait_for_stdout(&root, &session, "\"subtype\":\"init\"");
    let out = camp_ok(&root, &["nudge", &session, "status?"]);
    assert!(
        out.contains("stdin") || out.contains("held"),
        "nudge did not use the live pipe: {out}"
    );
    // Durable proof over the socket path: a session.nudged with via=stdin.
    wait_until(&root, "nudged via stdin", |events| {
        events
            .iter()
            .any(|e| e["type"] == "session.nudged" && e["data"]["via"] == "stdin")
    });
}

/// §5.4 "interrupt them": `camp interrupt` acks a request id and the worker's
/// control_response lands in the ledger — end to end over the socket.
#[test]
fn camp_interrupt_stops_the_turn_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root);
    wait_for_stdout(&root, &session, "\"subtype\":\"init\"");
    let out = camp_ok(&root, &["interrupt", &session]);
    assert!(out.contains("interrupt"), "interrupt did not ack: {out}");
    // The worker answers on the read channel -> control.responded, verb=session.interrupt.
    wait_until(&root, "control.responded for interrupt", |events| {
        events.iter().any(|e| {
            e["type"] == "control.responded" && e["data"]["verb"] == "session.interrupt"
        })
    });
}

/// §5.4/§5.3 "answer a permission", end to end over the socket ALONE and with
/// the request_id DISCOVERED — never hardcoded. A worker asks `can_use_tool`;
/// `camp sessions --json` shows it BLOCKED; `camp attach` renders the id off the
/// `session.subscribe` stream; the test parses that id and answers with `camp
/// decide`; the worker continues. If the id could not be discovered through the
/// socket, this test cannot pass — which is the falsification the gate demanded.
#[test]
fn camp_decide_answers_a_blocked_worker_with_a_socket_discovered_request_id() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    // NOTE: no FAKE_AGENT_CAN_USE_TOOL_REQ — the worker mints its own id; the
    // test must LEARN it through the socket, not know it a priori.
    let _d = Daemon::spawn(&root, &[("FAKE_AGENT_CAN_USE_TOOL", "1")]);
    let (_bead, session) = dispatch_one(&root);
    wait_until(&root, "permission.pending", |events| {
        events.iter().any(|e| e["type"] == "permission.pending")
    });

    // 1) The overseer sees WHICH session is BLOCKED — over the socket.
    let listed: Vec<Value> =
        serde_json::from_str(camp_ok(&root, &["sessions", "--json"]).trim()).unwrap();
    assert!(
        listed
            .iter()
            .any(|s| s["name"] == session.as_str() && s["blocked"] == true),
        "the blocked worker must render blocked in sessions.list: {listed:?}"
    );

    // 2) DISCOVER the request_id from `camp attach`'s BLOCKED line (a bounded
    //    child read; attach follows live, so read until the line, then kill).
    //    stdin is a HELD-OPEN pipe: attach reads stdin for steering, and a
    //    /dev/null stdin would EOF instantly, detaching before the printer
    //    thread streams the BLOCKED line. The held pipe keeps it attached.
    let mut child = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["--camp", root.to_str().unwrap(), "attach", &session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut request_id: Option<String> = None;
    let mut line = String::new();
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if line.contains("BLOCKED") && line.contains("request ") {
            // parse the token after "request " (Task 3B's stable format)
            if let Some(rest) = line.split("request ").nth(1) {
                request_id = rest.split_whitespace().next().map(str::to_owned);
            }
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let request_id = request_id.expect("must discover the request_id from attach's BLOCKED line");
    assert!(
        !request_id.is_empty() && request_id != "?",
        "discovered a bad id: {request_id:?}"
    );

    // 3) Answer with the DISCOVERED id — over the socket.
    let out = camp_ok(&root, &["decide", &session, &request_id, "allow"]);
    assert!(out.contains("allow"), "decide did not record allow: {out}");
    wait_until(&root, "permission.decided", |events| {
        events
            .iter()
            .any(|e| e["type"] == "permission.decided" && e["data"]["decision"] == "allow")
    });
    // And the worker continued (it emits an assistant line after the answer).
    wait_for_stdout(&root, &session, "continued after permission");
}

/// §5.4 "read their streams": `camp attach` renders the worker's live typed
/// events over `session.subscribe`. A `can_use_tool` worker produces a genuine
/// typed event on its stream (the control_request), which attach renders as the
/// BLOCKED line — a real event on stdout, not the (stderr) hello. Bounded child
/// read: attach, see the rendered line, kill. attach never opens the stream file
/// (its own doc, attach.rs — proven by Task 6's static tripwire).
#[test]
fn camp_attach_streams_a_workers_events_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(&root, &[("FAKE_AGENT_CAN_USE_TOOL", "1")]);
    let (_bead, session) = dispatch_one(&root);
    // A genuine typed event exists on the worker's stream: the can_use_tool
    // control_request. wait for campd to have surfaced it (permission.pending).
    wait_until(&root, "permission.pending", |events| {
        events.iter().any(|e| e["type"] == "permission.pending")
    });

    // stdin is a HELD-OPEN pipe (see the decide test): a /dev/null stdin EOFs
    // instantly and detaches attach before the printer thread streams a line.
    let mut child = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["--camp", root.to_str().unwrap(), "attach", &session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_stream = false;
    let mut line = String::new();
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        // A rendered typed event off session.subscribe — the BLOCKED line, which
        // only exists because attach decoded a control_request frame over the
        // socket (attach.rs renders `system/init` to an empty, filtered line).
        if line.contains("BLOCKED") {
            saw_stream = true;
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(saw_stream, "camp attach produced no rendered stream line");
}

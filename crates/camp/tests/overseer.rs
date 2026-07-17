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
use std::sync::mpsc::Receiver;
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

/// Wait until `path` contains `needle`, then return its whole contents. For
/// reading what attach told the OPERATOR (its stderr), which is a different
/// channel from the rendered stream on stdout.
fn wait_for_text(path: &Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?} in {}: {text:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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

/// Read `child`'s stdout lines until `pred` matches or `within` elapses, then
/// kill the child. Returns the matching line, or None on timeout/EOF.
///
/// BOUNDED BY CONSTRUCTION (CP5-1, inv-5 fail-fast): the read runs on its own
/// thread and feeds a channel, and the caller polls with `recv_timeout`, so a
/// QUIET stream lets the deadline fire on schedule. A bare `read_line` in the
/// caller's loop would block PAST the deadline (it is only re-checked at the top
/// of the loop), turning a regression — attach never rendering the awaited line
/// — into a hang to the global harness timeout instead of a clean fail at
/// `within`.
fn read_child_line_until(
    child: &mut Child,
    within: Duration,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    let lines = stream_child_stdout(child);
    let found = recv_line_until(&lines, within, pred);
    drop(lines); // the reader thread's next send fails and it returns
    let _ = child.kill();
    let _ = child.wait();
    found
}

/// Stream `child`'s stdout lines onto a channel and LEAVE THE CHILD RUNNING.
/// Split out of `read_child_line_until` (which kills the child, a one-shot
/// discovery read) because STEERING a live attach means read a line, TYPE, then
/// read more — all on ONE session that must stay attached throughout.
fn stream_child_stdout(child: &mut Child) -> Receiver<String> {
    let stdout = child.stdout.take().expect("child stdout must be piped");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF or the pipe closed on kill
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break; // the receiver went away (match found / deadline)
                    }
                }
            }
        }
    });
    rx
}

/// Poll a `stream_child_stdout` channel until `pred` matches or `within`
/// elapses. This is the half that makes the read BOUNDED BY CONSTRUCTION: the
/// reader thread owns the blocking `read_line`, so a QUIET stream still lets the
/// deadline fire on schedule.
fn recv_line_until(
    lines: &Receiver<String>,
    within: Duration,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match lines.recv_timeout(remaining) {
            Ok(line) if pred(&line) => return Some(line),
            Ok(_) => {}            // a line that did not match — keep reading
            Err(_) => return None, // timeout, or the reader closed (EOF)
        }
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
        events
            .iter()
            .filter(|e| e["type"] == "session.woke")
            .count()
            >= 2
    });

    let out = camp_ok(&root, &["sessions", "--json"]);
    let sessions: Vec<Value> = serde_json::from_str(out.trim()).unwrap();
    assert!(
        sessions.len() >= 2,
        "expected >=2 live sessions, got: {out}"
    );
    // Every row is addressed BY NAME (§4.2, `SessionInfo`'s doc comment).
    for s in &sessions {
        assert!(s["name"].as_str().is_some_and(|n| !n.is_empty()));
    }
    // FUTURE-REGRESSION TRIPWIRE, not run coverage: `SessionInfo` never
    // serializes a pid today, so this is a tautology now. It is kept ONLY to go
    // RED the day someone adds a `pid` field to the frozen wire (§4.2 rule 1) —
    // labelled so a reviewer does not count it as behavioural evidence.
    for s in &sessions {
        assert!(
            s.get("pid").is_none(),
            "SessionInfo must never carry a pid: {s}"
        );
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
        events
            .iter()
            .any(|e| e["type"] == "control.responded" && e["data"]["verb"] == "session.interrupt")
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
    // Bounded read (CP5-1): the deadline fires even while attach is quiet, so a
    // regression that stops rendering the `request <id>` token FAILS cleanly at
    // ~20s rather than hanging to the global harness timeout.
    let blocked_line = read_child_line_until(&mut child, Duration::from_secs(20), |l| {
        l.contains("BLOCKED") && l.contains("request ")
    });
    let request_id = blocked_line
        .as_deref()
        .and_then(|l| l.split("request ").nth(1)) // Task 3B's stable format
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .expect("must discover the request_id from attach's BLOCKED line within the deadline");
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

/// Issue #120 — §5.2's "From here: send a turn, interrupt, ANSWER A PERMISSION
/// REQUEST". The answer is typed INTO the view, on the SAME line loop as a turn
/// and `/interrupt`: attach renders BLOCKED, the operator types `/allow`, and the
/// WORKER CONTINUES — no second terminal, no `camp decide`, no request_id typed
/// (the view already learned it from the frame it rendered).
///
/// This is the falsification the hint could never fail: `camp decide`'s test
/// proves the OUT-OF-BAND path, and passes just as well when attach's own
/// `/allow` does nothing at all. Only driving the keypress THROUGH attach's
/// stdin can tell the two apart.
#[test]
fn camp_attach_answers_a_blocked_worker_from_its_own_line_loop() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(&root, &[("FAKE_AGENT_CAN_USE_TOOL", "1")]);
    let (_bead, session) = dispatch_one(&root);
    wait_until(&root, "permission.pending", |events| {
        events.iter().any(|e| e["type"] == "permission.pending")
    });

    let mut child = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["--camp", root.to_str().unwrap(), "attach", &session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lines = stream_child_stdout(&mut child);
    // The view must RENDER the question before the answer is typed — exactly
    // what a human waits for, and what hands attach the request_id.
    assert!(
        recv_line_until(&lines, Duration::from_secs(20), |l| l.contains("BLOCKED")).is_some(),
        "attach never rendered the BLOCKED line"
    );

    // THE KEYPRESS. Bare `/allow`: the operator names no id and opens no second
    // terminal — the whole point of #120.
    let mut stdin = child.stdin.take().expect("attach stdin must be piped");
    stdin.write_all(b"/allow\n").unwrap();
    stdin.flush().unwrap();

    // Durable proof, over the socket alone: campd RECORDED the operator's answer.
    wait_until(&root, "permission.decided", |events| {
        events.iter().any(|e| {
            e["type"] == "permission.decided"
                && e["data"]["decision"] == "allow"
                && e["data"]["decided_by"] == "operator"
        })
    });
    // ...and the WORKER CONTINUED — recording an answer nobody delivered would
    // still leave the worker blocked forever (§5.3).
    wait_for_stdout(&root, &session, "continued after permission");

    let _ = child.kill();
    let _ = child.wait();
}

/// A worker may have MORE THAN ONE question outstanding: nothing serializes
/// `can_use_tool` (parallel tool calls), the ledger keys `permissions` on
/// `request_id` with a NON-UNIQUE `session`, and `pending_permission_for_session`
/// says `LIMIT 1` — conceding the plural. A view that remembers only the NEWEST
/// answers the wrong question, or strands the older one unanswerable and pushes
/// the operator back to the second terminal #120 exists to remove.
///
/// So the view must NEVER GUESS: a bare verb REFUSES and names the ids, and
/// `/allow <id>` answers EXACTLY that one. This test answers the OLDER id on
/// purpose — the one a single-slot view has already forgotten.
#[test]
fn camp_attach_refuses_to_guess_between_two_blocked_questions() {
    const SECOND: &str = "cli-second";
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CAN_USE_TOOL", "1"),
            ("FAKE_AGENT_CAN_USE_TOOL_SECOND", SECOND),
        ],
    );
    let (bead, session) = dispatch_one(&root);
    let first = format!("cli-{bead}"); // the fake agent's default id, asked FIRST
    wait_until(&root, "two pending permissions", |events| {
        events
            .iter()
            .filter(|e| e["type"] == "permission.pending")
            .count()
            >= 2
    });

    let err_path = root.join("attach-err.txt");
    let mut child = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["--camp", root.to_str().unwrap(), "attach", &session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(std::fs::File::create(&err_path).unwrap()))
        .spawn()
        .unwrap();
    let lines = stream_child_stdout(&mut child);
    for n in 1..=2 {
        assert!(
            recv_line_until(&lines, Duration::from_secs(20), |l| l.contains("BLOCKED")).is_some(),
            "attach did not render BLOCKED line {n} of 2"
        );
    }
    let mut stdin = child.stdin.take().expect("attach stdin must be piped");

    // (1) A BARE verb must REFUSE — and name the ids it will not choose between.
    stdin.write_all(b"/allow\n").unwrap();
    stdin.flush().unwrap();
    let refusal = wait_for_text(&err_path, "name the one you mean");
    assert!(
        refusal.contains(&first) && refusal.contains(SECOND),
        "the refusal must name BOTH pending ids so the operator can choose: {refusal}"
    );

    // (2) ...and it must not have answered anything. A view that guesses is
    //     worse than one that asks: the operator would never know it guessed.
    assert_eq!(
        events_json(&root)
            .iter()
            .filter(|e| e["type"] == "permission.decided")
            .count(),
        0,
        "a bare verb GUESSED a target instead of refusing"
    );

    // (3) Naming the OLDER id answers EXACTLY that one. A single-slot view has
    //     already forgotten it and would answer the newest instead.
    stdin
        .write_all(format!("/allow {first}\n").as_bytes())
        .unwrap();
    stdin.flush().unwrap();
    wait_until(&root, "the NAMED permission decided", |events| {
        events
            .iter()
            .any(|e| e["type"] == "permission.decided" && e["data"]["request_id"] == first.as_str())
    });
    let decided: Vec<Value> = events_json(&root)
        .into_iter()
        .filter(|e| e["type"] == "permission.decided")
        .collect();
    assert_eq!(
        decided.len(),
        1,
        "answering ONE question must not answer another: {decided:#?}"
    );
    assert_eq!(
        decided[0]["data"]["request_id"],
        first.as_str(),
        "the WRONG question was answered"
    );
    // The worker asked TWO questions, so one answer must NOT resume it: that is
    // what makes step (3) evidence the NAMED question was answered, rather than
    // just "something was".
    assert!(
        !std::fs::read_to_string(stdout_path(&root, &session))
            .unwrap_or_default()
            .contains("continued after permission"),
        "the worker resumed with a question still outstanding"
    );

    // (4) With ONE question left, the bare verb's convenience COMES BACK — this
    //     is what proves the steering loop DROPS an answered question from the
    //     floor. Without that, the view would keep refusing, naming a question
    //     campd has already settled, and the operator could never finish.
    stdin.write_all(b"/allow\n").unwrap();
    stdin.flush().unwrap();
    wait_until(&root, "the last question decided", |events| {
        events
            .iter()
            .any(|e| e["type"] == "permission.decided" && e["data"]["request_id"] == SECOND)
    });
    // Both answered: the worker finally resumes.
    wait_for_stdout(&root, &session, "continued after permission");

    let _ = child.kill();
    let _ = child.wait();
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
    // Bounded read (CP5-1): a rendered typed event off session.subscribe — the
    // BLOCKED line, which exists only because attach decoded a control_request
    // frame over the socket (attach.rs renders `system/init` to an empty,
    // filtered line). A quiet stream fails at the deadline, never hangs.
    let saw_stream = read_child_line_until(&mut child, Duration::from_secs(20), |l| {
        l.contains("BLOCKED")
    })
    .is_some();
    assert!(
        saw_stream,
        "camp attach produced no rendered stream line within the deadline"
    );
}

// ===== Task 6: the no-private-paths falsification instrument (§4) ==========

/// FALSIFIER A (§4 necessity): with the ledger, the worker's stream file, and
/// the worker's pid all present on disk but campd's socket GONE, every
/// observe/steer-a-live-worker verb fails LOUDLY. A verb that read the stream
/// file or signalled the pid would SUCCEED here — this assertion is what turns
/// that regression RED. (`camp nudge` is excluded: campd-down legitimately
/// routes to its resume path — see the plan's nudge exception.)
#[test]
fn socket_is_necessary_campd_down_is_a_loud_failure_not_a_private_path_read() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    // A worker that OUTLIVES campd, so the pid + stream file are live/present
    // after we kill campd (the tempting private paths are fully populated).
    let session = {
        let d = Daemon::spawn(
            &root,
            &[
                ("FAKE_AGENT_CONTROL_LOOP", "1"),
                ("FAKE_AGENT_LINGER_ON_EOF", "60"),
            ],
        );
        let (_bead, session) = dispatch_one(&root);
        wait_for_stdout(&root, &session, "\"subtype\":\"init\"");
        // The private paths a cheating client would reach for MUST exist now:
        assert!(
            stdout_path(&root, &session).exists(),
            "stream file must be present"
        );
        // SIGKILL campd (the harness's crash-only `kill9`, which consumes `d`),
        // leaving the lingering worker + its stream file + the ledger behind.
        d.kill9();
        session
    };
    // With NO socket, each verb must fail loudly — not silently read a file.
    for args in [
        vec!["sessions"],
        vec!["sessions", "--json"],
        vec!["interrupt", session.as_str()],
        vec!["decide", session.as_str(), "cli-x", "allow"],
        vec!["attach", session.as_str()],
    ] {
        let out = camp(&root, &args);
        assert!(
            !out.status.success(),
            "verb `{args:?}` succeeded with campd DOWN — it must reach a live \
             worker only through the socket, never a file or pid"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("campd") || err.contains("socket"),
            "verb `{args:?}` failed but not with a campd/socket error: {err}"
        );
    }
}

/// FALSIFIER B (§4 sufficiency): campd UP, but the worker's stream file and
/// campd.log are chmod 000. Every overseer verb still works over the socket.
/// campd is unaffected — it holds those fds already open; only a CLIENT doing a
/// fresh open() of a forbidden file would fail here → RED. (`camp nudge` IS
/// included: its live session.send_turn path must not read those files either.)
#[cfg(unix)]
#[test]
fn socket_is_sufficient_unreadable_private_paths_do_not_stop_any_verb() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let req_id = "cli-suff";
    let _d = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CAN_USE_TOOL", "1"),
            ("FAKE_AGENT_CAN_USE_TOOL_REQ", req_id),
            ("FAKE_AGENT_LINGER_ON_EOF", "30"),
        ],
    );
    let (_bead, session) = dispatch_one(&root);
    wait_until(&root, "permission.pending", |events| {
        events.iter().any(|e| e["type"] == "permission.pending")
    });

    // Poison every private path a cheating client might read. campd already
    // holds these fds open, so its own tailing is unaffected.
    let stream = stdout_path(&root, &session);
    let log = root.join("campd.log");
    let saved: Vec<(std::path::PathBuf, std::fs::Permissions)> = [stream.clone(), log.clone()]
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| {
            let perm = std::fs::metadata(&p).unwrap().permissions();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
            (p, perm)
        })
        .collect();

    // NON-ROOT SELF-CHECK: mode 000 does not stop root. If THIS process can
    // still open the poisoned stream file, we are effectively root and the
    // whole arm is vacuous — restore and bail rather than assert a hollow pass.
    if stream.exists() && std::fs::File::open(&stream).is_ok() {
        eprintln!("skipping sufficiency arm: running as root, chmod 000 is vacuous");
        for (p, perm) in saved {
            std::fs::set_permissions(&p, perm).unwrap();
        }
        return;
    }

    // Every verb still works — over the socket alone.
    let listed: Vec<Value> =
        serde_json::from_str(camp_ok(&root, &["sessions", "--json"]).trim()).unwrap();
    assert!(
        listed
            .iter()
            .any(|s| s["name"] == session.as_str() && s["blocked"] == true)
    );
    camp_ok(&root, &["nudge", &session, "still here?"]); // live send_turn path
    camp_ok(&root, &["decide", &session, req_id, "allow"]);

    // Restore perms so tempdir teardown can clean up.
    for (p, perm) in saved {
        std::fs::set_permissions(&p, perm).unwrap();
    }
}

/// FALSIFIER C: the pure overseer clients must talk to `socket::` and NOTHING
/// that reaches a worker by file or pid. This is the compile-cheap tripwire —
/// it goes RED the instant a private-path builder is imported into a client.
/// (`cmd/nudge.rs` is excluded: its resume path is a documented, name-keyed
/// process spawn, not a stream-file tail or a pid signal — see the plan.)
#[test]
fn pure_overseer_clients_reference_only_the_socket_never_a_private_path() {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/cmd");
    let forbidden = [
        "sessions_dir",
        "stdout_path",
        "log_path",
        ".join(\"sessions\")",
        "/proc",
        "libc::kill",
        ".pid",
    ];
    for file in ["sessions.rs", "interrupt.rs", "attach.rs", "decide.rs"] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        assert!(
            text.contains("socket::"),
            "{file} must reach the worker via socket::"
        );
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{file} references a PRIVATE PATH `{needle}` — an overseer client \
                 must reach a worker only through the socket (§4)"
            );
        }
    }
}

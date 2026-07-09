#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `camp nudge` — the converse verb (dispatch-lifecycle Phase 1, #29;
//! design §9 test obligation ii). Both delivery paths end-to-end with fake
//! agents, no Claude anywhere: live over campd's held stdin pipe
//! (FAKE_AGENT_NUDGE_CLOSE proves the injected line reaches the worker),
//! and `--resume` for exited/attended sessions (claude-or-agent.sh records
//! the argv camp built and answers an F2 envelope).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

fn fake_agent() -> String {
    format!("{}/tests/fake-agent.sh", env!("CARGO_MANIFEST_DIR"))
}

fn claude_or_agent() -> String {
    format!("{}/tests/claude-or-agent.sh", env!("CARGO_MANIFEST_DIR"))
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

/// A camp with one rig and full dispatch config; `command` is the caller's
/// choice (fake-agent.sh for held-stdin tests, claude-or-agent.sh when the
/// resume role is needed too). Returns (root, rig).
fn scaffold(dir: &Path, command: &str) -> (PathBuf, PathBuf) {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
             [dispatch]\nmax_workers = 4\ncommand = \"{command}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
        ),
    )
    .unwrap();
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("dev.md"), "---\nname: dev\n---\nDo the work.\n").unwrap();
    // create the ledger so every verb (and campd) finds it
    camp_ok(&root, &["events", "--json"]);
    (root, rig)
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    camp_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Test-harness wait (camp never polls; tests may).
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

fn count(events: &[serde_json::Value], kind: &str) -> usize {
    events.iter().filter(|e| e["type"] == kind).count()
}

/// campd as a real child process. Drop kills and reaps it.
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
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The session.woke event for a bead — the registry name + claude session
/// id the nudge targets.
fn woke_of(events: &[serde_json::Value], bead: &str) -> serde_json::Value {
    events
        .iter()
        .find(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead)
        .unwrap_or_else(|| panic!("no session.woke for {bead}: {events:#?}"))
        .clone()
}

/// Obligation (ii), live half: the converse verb delivers a turn into a
/// LIVE worker over the campd-held stdin pipe. FAKE_AGENT_NUDGE_CLOSE makes
/// the fake agent read the task line then BLOCK until a later stdin line
/// arrives; the nudge is that line, and the agent then closes pass — the
/// delivery is proven by the close.
#[test]
fn nudge_delivers_into_a_live_workers_held_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), &fake_agent());
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);

    let bead = camp_ok(&root, &["sling", "hold for a nudge"])
        .trim()
        .to_owned();
    wait_until(&root, "the worker to claim", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.claimed" && ev["bead"] == bead.as_str())
    });
    let session = woke_of(&events_json(&root), &bead)["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned();

    let out = camp(&root, &["nudge", &session, "please wrap up"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Mechanism-honest wording (assessment findings A/B): the message
    // names the pipe write, not a processed outcome, and points at the
    // TRANSCRIPT for the reply (camp events records only the delivery).
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("held stdin"), "stdout: {stdout}");
    assert!(
        stdout.contains("transcript for the reply"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("`camp events` or"),
        "camp events must not be named as where the reply appears: {stdout}"
    );

    // The nudge line unblocked the worker: it closes pass and exits.
    wait_until(&root, "the nudged worker to close", |e| {
        count(e, "bead.closed") == 1
    });
    let events = events_json(&root);
    let nudged = events
        .iter()
        .find(|e| e["type"] == "session.nudged")
        .expect("session.nudged must be in the ledger (invariant 3)");
    assert_eq!(nudged["data"]["via"], "stdin");
    assert_eq!(nudged["data"]["session"], session.as_str());
    assert_eq!(nudged["data"]["text"], "please wrap up");
    assert_eq!(nudged["actor"], "campd");
    assert_eq!(nudged["bead"], bead.as_str());
    assert_eq!(
        count(&events, "session.woke"),
        1,
        "converse never dispatches"
    );
}

/// Obligation (ii), resume half: an EXITED worker is reached via
/// `<command> -p --resume <sid> <text>` run from the session's recorded
/// cwd, and the reply's result text is printed. campd is STOPPED first —
/// resume needs no daemon (A4/F6).
#[test]
fn nudge_resumes_an_exited_worker_and_prints_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), &claude_or_agent());
    {
        let _campd = Daemon::spawn(&root, &[("FAKE_AGENT", &fake_agent())]);
        let bead = camp_ok(&root, &["sling", "run and exit"]).trim().to_owned();
        wait_until(&root, "the worker to finish", |e| {
            e.iter()
                .any(|ev| ev["type"] == "bead.closed" && ev["bead"] == bead.as_str())
                && count(e, "session.stopped") >= 1
        });
    } // campd killed: the resume path must not need it

    let events = events_json(&root);
    let woke = woke_of(&events, "gc-1");
    let session = woke["data"]["name"].as_str().unwrap().to_owned();
    let sid = woke["data"]["claude_session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let log = dir.path().join("stub.log");
    let out = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(&root)
        .args(["nudge", &session, "how did it go?"])
        .env("NUDGE_STUB_LOG", &log)
        .env("FAKE_AGENT", fake_agent())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "STUB-REPLY");

    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains(&format!("--resume {sid}")),
        "resume argv must carry the recorded claude session id: {logged}"
    );
    assert!(logged.contains("how did it go?"), "log: {logged}");
    let cwd_line = logged
        .lines()
        .find(|l| l.starts_with("cwd:"))
        .unwrap()
        .trim_start_matches("cwd:");
    assert_eq!(
        std::fs::canonicalize(cwd_line).unwrap(),
        std::fs::canonicalize(&rig).unwrap(),
        "resume runs in the session's recorded rig cwd (F3)"
    );

    let events = events_json(&root);
    let nudged = events
        .iter()
        .find(|e| e["type"] == "session.nudged")
        .expect("session.nudged must be in the ledger (invariant 3)");
    assert_eq!(nudged["data"]["via"], "resume");
    assert_eq!(nudged["actor"], "cli");
}

/// "Any running session — worker or overseer": a live hook-registered
/// attended session is not campd's child (no pipe), so campd answers
/// via="none" and the CLI converses over concurrent resume (A4-4).
#[test]
fn nudge_reaches_a_live_attended_session_via_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), &claude_or_agent());
    let _campd = Daemon::spawn(&root, &[]); // campd up: the via="none" branch
    camp_ok(
        &root,
        &[
            "session",
            "register",
            "--name",
            "attended/abc",
            "--agent",
            "attended",
            "--rig",
            "gc",
            "--session-id",
            "0e0e0e0e-1111-4222-8333-444444444444",
        ],
    );

    let log = dir.path().join("stub.log");
    let out = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(&root)
        .args(["nudge", "attended/abc", "overseer ping"])
        .env("NUDGE_STUB_LOG", &log)
        .env("FAKE_AGENT", fake_agent())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "STUB-REPLY");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("--resume 0e0e0e0e"),
        "the attended session resumes by its registered claude session id"
    );
}

#[test]
fn nudge_unknown_session_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), &fake_agent());
    let out = camp(&root, &["nudge", "no/such/session", "hello"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no session named"), "stderr: {err}");
}

#[test]
fn nudge_without_a_claude_session_id_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), &fake_agent());
    camp_ok(
        &root,
        &[
            "session",
            "register",
            "--name",
            "attended/nosid",
            "--agent",
            "attended",
            "--rig",
            "gc",
        ],
    );
    let out = camp(&root, &["nudge", "attended/nosid", "hello"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("claude session id"), "stderr: {err}");
}

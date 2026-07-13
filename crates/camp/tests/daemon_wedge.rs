#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Issue #55 integration: campd wedge observability. A REAL campd is
//! genuinely wedged — a PATH-shimmed `git` that hangs, hit by dispatch on
//! the single-threaded event loop; no test hooks in production code — and
//! the contract is asserted end to end:
//!
//!   1. a CLI verb against the wedged daemon fails LOUDLY within its
//!      bound (never hangs), naming the campd pid and the kill -9 remedy;
//!   2. the CLI (a pure client — design §4.3) never spawns a daemon, and it
//!      never mistakes the wedge for a DOWN campd: something owns the socket,
//!      so the remedy is kill -9, not "start campd";
//!   3. the hung subprocess is killed at `[dispatch] exec_timeout` and
//!      the failure lands in the ledger as dispatch.failed with the
//!      timeout as its reason (invariants 3/5);
//!   4. the daemon RECOVERS: after the bound, the event loop serves a
//!      status round-trip again.
//!
//! Test-side waiting polls the ledger/filesystem — sanctioned for
//! harnesses only (camp never polls).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

fn fake_agent() -> String {
    format!("{}/tests/fake-agent.sh", env!("CARGO_MANIFEST_DIR"))
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

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    camp_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn count(events: &[serde_json::Value], kind: &str) -> usize {
    events.iter().filter(|e| e["type"] == kind).count()
}

/// Test-harness wait (camp never polls; tests may).
fn wait_until(root: &Path, what: &str, pred: impl Fn(&[serde_json::Value]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
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

/// A camp with one rig, a dev agent, and a SHORT `[dispatch]
/// exec_timeout` — the visible-config knob (the fake-agent precedent)
/// that keeps the wedge window test-sized.
fn scaffold(dir: &Path, exec_timeout: &str) -> PathBuf {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
             [dispatch]\ncommand = \"{}\"\ndefault_agent = \"dev\"\nexec_timeout = \"{exec_timeout}\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    let dev = root.join("agents/dev");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("agent.toml"), "isolation = \"none\"\n").unwrap();
    std::fs::write(dev.join("prompt.md"), "Do the work.\n").unwrap();
    camp_ok(&root, &["events", "--json"]); // create the ledger
    root
}

/// A `git` that hangs: touches a marker (so the test knows the wedge has
/// started) then sleeps far past every bound. `exec` keeps the sleep AS
/// the git process, so campd's SIGKILL at the deadline lands on it.
fn hung_git_shim(dir: &Path) -> (PathBuf, PathBuf) {
    let shim_dir = dir.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let marker = dir.join("git-started");
    let script = shim_dir.join("git");
    std::fs::write(
        &script,
        "#!/bin/sh\ntouch \"$WEDGE_MARKER\"\nexec sleep 120\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (shim_dir, marker)
}

/// campd as a real child, its PATH poisoned with the hung git shim.
struct Daemon {
    child: Child,
}

impl Daemon {
    fn spawn(root: &Path, shim_dir: &Path, marker: &Path) -> Daemon {
        let path = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap());
        let mut child = Command::new(BIN)
            .env_remove("CAMP_DIR")
            .env("PATH", path)
            .env("WEDGE_MARKER", marker)
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
        Daemon { child }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One status round-trip on the raw socket (the §5 liveness definition).
fn status(sock: &Path) -> serde_json::Value {
    let mut stream = UnixStream::connect(sock).expect("connect to campd");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(b"{\"op\":\"status\"}\n").unwrap();
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp).unwrap();
    serde_json::from_str(resp.trim_end()).expect("campd response is JSON")
}

#[test]
fn a_wedged_event_loop_fails_the_cli_loudly_within_its_bound_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), "10s");
    let (shim_dir, marker) = hung_git_shim(dir.path());
    let daemon = Daemon::spawn(&root, &shim_dir, &marker);
    let daemon_pid = daemon.child.id();

    // Sling work: the poke is acked before settle, then converge runs the
    // dispatch inline on the event loop and its rig_base `git` hangs —
    // the loop is now GENUINELY wedged (no accept, no reap) until the
    // exec_timeout bound kills the shim.
    camp_ok(&root, &["sling", "wedge me"]);
    let wedge_started = Instant::now();
    while !marker.exists() {
        assert!(
            wedge_started.elapsed() < Duration::from_secs(10),
            "the hung git shim never started"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // (1) A CLI verb against the wedged daemon: loud, actionable, bounded —
    // never a hang. `camp top` needs the daemon, so this also proves (2): the
    // wedge is reported AS a wedge, and no second campd is started.
    let start = Instant::now();
    let top = camp(&root, &["top"]);
    let elapsed = start.elapsed();
    let stderr = String::from_utf8_lossy(&top.stderr);
    assert!(
        !top.status.success(),
        "camp top must fail against a wedged campd; stdout: {}",
        String::from_utf8_lossy(&top.stdout)
    );
    assert!(
        elapsed < Duration::from_secs(9),
        "the verb must fail within its bound (5s request timeout), never hang: took {elapsed:?}"
    );
    assert!(
        stderr.contains(&daemon_pid.to_string()),
        "the error must name the campd pid {daemon_pid}: {stderr}"
    );
    assert!(
        stderr.contains("kill -9"),
        "the error must name the supported remedy: {stderr}"
    );

    // (3) The hung git is killed at the bound and the failure is ledger
    // truth with its cause (invariants 3/5): dispatch.failed, reason =
    // the timeout.
    wait_until(&root, "the bounded dispatch failure", |e| {
        e.iter().any(|ev| {
            ev["type"] == "dispatch.failed"
                && ev["data"]["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("did not finish within"))
        })
    });

    // (2) continued: the wedge started no second campd, and it was never
    // reported as a down one — the remedies differ, so the errors must too.
    assert!(
        !stderr.contains("campd is not running"),
        "a WEDGED campd owns its socket: reporting it as 'not running' would send \
         the operator to `camp service status` instead of `kill -9`: {stderr}"
    );
    let events = events_json(&root);
    assert_eq!(
        count(&events, "campd.started"),
        1,
        "exactly one campd ever started — the CLI never starts one: {events:#?}"
    );

    // (4) Recovery: the SAME daemon serves an event-loop round-trip again
    // (§5 liveness as amended: alive means it answers).
    let response = status(&root.join("campd.sock"));
    assert_eq!(response["ok"], true);
    assert_eq!(
        response["campd_pid"],
        serde_json::json!(daemon_pid),
        "the original daemon, recovered — not a replacement"
    );

    // And the operator's next verb works end to end.
    let top = camp(&root, &["top"]);
    assert!(
        top.status.success(),
        "camp top must succeed after recovery: {}",
        String::from_utf8_lossy(&top.stderr)
    );
    assert!(
        String::from_utf8_lossy(&top.stdout).contains(&format!("campd pid: {daemon_pid}")),
        "top renders the recovered daemon"
    );
}

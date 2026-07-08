#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Plugin hook scripts driven by recorded fixture stdin payloads
//! (Phase 12; spec §16 "throttling and fire-and-forget append behavior
//! verified"). The scripts are trivial shell that ALWAYS exit 0 and never
//! block the session; JSON parsing + idempotency live in tested Rust via
//! `camp session ... --hook-stdin`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

fn plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(plugin().join("tests/fixtures").join(name)).unwrap()
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
    let out = camp(&root, &["rig", "add", rig.to_str().unwrap(), "--prefix", "gc", "--name", "gc"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    root
}

fn events(root: &Path) -> Vec<serde_json::Value> {
    let out = camp(root, &["events", "--json"]);
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Run a plugin hook script with `camp` on PATH and CAMP_DIR pointed at the
/// camp. `extra_env` sets additional variables (e.g. the throttle window).
fn run_hook(
    script: &str,
    stdin: &str,
    camp_dir: &Path,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let camp_bin_dir = PathBuf::from(BIN).parent().unwrap().to_path_buf();
    let path = format!(
        "{}:{}",
        camp_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("sh");
    cmd.arg(plugin().join("hooks").join(script))
        .env("CAMP_DIR", camp_dir)
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// A real campd child; stopped on drop. SessionStart runs `camp adopt`,
/// which connects to a running campd (rather than auto-starting a
/// detached one in the test).
struct Daemon {
    child: Child,
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
        assert!(line.starts_with(READY_PREFIX), "unexpected campd first line: {line:?}");
        Daemon { child }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn count(root: &Path, ty: &str, name: &str) -> usize {
    events(root)
        .iter()
        .filter(|e| e["type"] == ty && e["data"]["name"] == name)
        .count()
}

#[test]
fn session_start_hook_registers_once_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _daemon = Daemon::spawn(&root);
    let payload = fixture("session-start.json"); // session_id "S-1"

    for _ in 0..2 {
        let out = run_hook("session-start.sh", &payload, &root, &[]);
        assert!(out.status.success(), "hook must exit 0: {:?}", out.status);
    }
    assert_eq!(
        count(&root, "session.woke", "attended/S-1"),
        1,
        "SessionStart must register exactly once (idempotent)"
    );
}

#[test]
fn session_end_hook_stops_the_registered_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _daemon = Daemon::spawn(&root);

    let out = run_hook("session-start.sh", &fixture("session-start.json"), &root, &[]);
    assert!(out.status.success());
    let out = run_hook("session-end.sh", &fixture("session-end.json"), &root, &[]);
    assert!(out.status.success(), "session-end hook must exit 0");
    assert_eq!(count(&root, "session.stopped", "attended/S-1"), 1);
}

#[test]
fn breadcrumb_hook_throttles_repeats() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let payload = fixture("post-tool-use.json");

    // large window: two rapid fires → exactly one milestone (2nd throttled)
    for _ in 0..2 {
        let out = run_hook("post-tool-use.sh", &payload, &root, &[("CAMP_BREADCRUMB_THROTTLE", "3600")]);
        assert!(out.status.success());
    }
    let milestones = || {
        events(&root)
            .iter()
            .filter(|e| e["type"] == "worker.milestone")
            .count()
    };
    assert_eq!(milestones(), 1, "the second breadcrumb within the window is throttled");

    // window 0 disables throttling → the next fire emits again
    let out = run_hook("post-tool-use.sh", &payload, &root, &[("CAMP_BREADCRUMB_THROTTLE", "0")]);
    assert!(out.status.success());
    assert_eq!(milestones(), 2, "window 0 bypasses the throttle");
}

#[test]
fn hooks_exit_zero_even_when_camp_is_unavailable() {
    // CAMP_DIR points at a dir with no camp — every `camp` call fails, but
    // the fire-and-forget wrapper notes to stderr and still exits 0.
    let dir = tempfile::tempdir().unwrap();
    let no_camp = dir.path();
    for (script, fix) in [
        ("session-start.sh", "session-start.json"),
        ("session-end.sh", "session-end.json"),
    ] {
        let out = run_hook(script, &fixture(fix), no_camp, &[]);
        assert!(out.status.success(), "{script} must exit 0 even with no camp");
        assert!(
            !out.stderr.is_empty(),
            "{script} must emit a visible stderr note on failure"
        );
    }
}

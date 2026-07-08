#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! camp top --statusline (Phase 12): the fleet badge `▲live ●ready ✖red`.
//! A read-only socket query that does NOT auto-start campd; when campd is
//! down it degrades to empty stdout + a visible stderr note (exit 0) —
//! visible degradation, not silence (spec §11).

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

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
    dir.join(".camp")
}

/// A real campd child; stopped on drop.
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
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected campd first line: {line:?}"
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

#[test]
fn statusline_degrades_visibly_when_campd_is_down() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());

    // campd is NOT running; --statusline must NOT auto-start it.
    let out = camp(&root, &["top", "--statusline"]);
    assert!(out.status.success(), "must exit 0 (fire-and-forget)");
    assert!(
        out.stdout.is_empty(),
        "stdout must be empty when campd is down"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("campd"),
        "must emit a visible stderr note, got: {stderr:?}"
    );

    // and it must not have started a daemon
    let sock = root.join("campd.sock");
    assert!(
        !sock.exists() || UnixStream::connect(&sock).is_err(),
        "--statusline must never auto-start campd"
    );
}

#[test]
fn statusline_renders_the_badge_when_campd_is_up() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _daemon = Daemon::spawn(&root);

    let out = camp(&root, &["top", "--statusline"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let badge = String::from_utf8(out.stdout).unwrap();
    let badge = badge.trim();
    // fresh camp: no live sessions, no ready beads, none stalled
    assert_eq!(badge, "▲0 ●0 ✖0", "unexpected badge: {badge:?}");
}

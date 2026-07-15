//! THE §14 unskippable gate (hermetic Rust half).
//!
//! A fixture camp (real ledger, real shims installed by `launch`, a fake claude
//! in place of the model) drives the worker fragment claim → close → drain-ack
//! → REAP under a wall-clock deadline. A hang is the failing signal.
//!
//! Two properties:
//!  1. The byte-projection (§6.1): the hook JSON, `bd show --json`, and the
//!     worker env all project the ONE bead row. The genuinely independent leg
//!     is env `GC_AGENT` vs the cooked route — pinned here by driving the REAL
//!     shim binaries with `GC_AGENT` set to a DIFFERENT value than the bead's
//!     route and proving the shims still read the route from the BEAD (B1/NB1).
//!  2. The lifecycle (§6.2): the worker LINGERS after drain-ack (a real
//!     `claude -p` does not exit on EOF, P3), so campd's drain-ack →
//!     KillReleased is what reaps it. If that wiring regressed, the worker
//!     sleeps past the deadline and the watchdog fails RED (R2-B2).

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";
/// Deadline < the DEFAULT release_grace (30s) ON PURPOSE (fold-in NB2): the
/// grace backstop must NOT be able to mask a drain-ack→KillReleased regression.
/// A green reap inside 20s can therefore only be the drain-ack prompt kill.
const DEADLINE: Duration = Duration::from_secs(20);

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

/// A camp whose `[dispatch].command` is `fake_claude`, with a `dev` agent and a
/// `gc` rig. Uses the DEFAULT release_grace (30s) — deliberately > DEADLINE.
fn scaffold(dir: &Path, fake_claude: &Path) -> PathBuf {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
             [dispatch]\nmax_workers = 4\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_claude.display(),
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

/// Write the fake claude: run the fragment against the real shims, then LINGER
/// (`exec sleep 600`) — the wrapper stays alive exactly as `claude -p` does, so
/// campd's KillReleased is the only thing that can reap it inside the deadline.
fn write_fake_claude(dir: &Path, fragment: &Path, mode: &str) -> PathBuf {
    let path = dir.join("fake-claude.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n# claude-style argv is ignored; the contract arrives in CAMP_* env.\n\
             GC_FRAGMENT_MODE={mode} sh {} 1>&2\nexec sleep 600\n",
            fragment.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn fragment_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gc-fragment.sh")
}

/// campd in its OWN process group, so cleanup can reap the whole tree — campd
/// AND the lingering `sleep 600` worker (which inherits the group). Fold-in
/// NB3: a failing test must not orphan a 600s sleep.
struct Daemon {
    child: Child,
    pgid: i32,
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
            .stderr(Stdio::inherit())
            .process_group(0); // new group; pgid == campd pid
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let pgid = child.id() as i32;
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon { child, pgid }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Kill the WHOLE group: campd and any worker it spawned (the lingering
        // sleep 600). `kill -KILL -<pgid>` needs no libc/nix dependency.
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{}", self.pgid))
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Step 1 — the byte projection, with a real env≠bead mismatch (B1/NB1).
// ---------------------------------------------------------------------------

#[test]
fn hook_bd_show_and_env_project_the_same_bead_row() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Path::new("/bin/true"));
    // A routed bead: `sling --agent dev` stamps beads.assignee = "dev" (the
    // route → gc.routed_to). No campd runs here, so sling exits nonzero ("not
    // dispatched") AFTER durably creating the bead — read its id from the row.
    let _ = camp(&root, &["sling", "do a thing", "--agent", "dev"]);
    let bead = events_json(&root)
        .into_iter()
        .find(|e| e["type"] == "bead.created")
        .expect("sling creates the bead durably even without campd")["bead"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(bead, "gc-1");

    // Drive the REAL hook shim with GC_AGENT set to a WRONG route on purpose.
    let hook = Command::new(BIN)
        .arg("--camp")
        .arg(&root)
        .args(["gc-shim", "hook", "--claim", "--json"])
        .env("CAMP_BEAD", "gc-1")
        .env("CAMP_SESSION", "t/dev/1")
        .env("GC_AGENT", "gc.WRONG") // <-- ≠ the cooked route "dev"
        .env("GC_TEMPLATE", "gc.WRONG")
        .output()
        .unwrap();
    assert!(hook.status.success(), "hook: {}", String::from_utf8_lossy(&hook.stderr));
    let hook_json: serde_json::Value =
        serde_json::from_slice(&hook.stdout).expect("hook prints one JSON object");
    assert_eq!(hook_json["action"], "work");
    assert_eq!(hook_json["assignee"], "t/dev/1"); // the session
    assert_eq!(hook_json["route"], "dev"); // the BEAD's route — NOT gc.WRONG

    // And bd show projects the SAME row (top-level assignee = the session;
    // metadata.gc.routed_to = the route), both from readiness::bead_metadata.
    let show = Command::new(BIN)
        .arg("--camp")
        .arg(&root)
        .args(["bd-shim", "show", "gc-1", "--json"])
        .env("GC_AGENT", "gc.WRONG")
        .output()
        .unwrap();
    assert!(show.status.success(), "bd show: {}", String::from_utf8_lossy(&show.stderr));
    let bd_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(bd_json["assignee"], hook_json["assignee"]); // same session
    assert_eq!(bd_json["metadata"]["gc.routed_to"], hook_json["route"]); // same route
    assert_eq!(bd_json["metadata"]["gc.routed_to"], "dev"); // NOT env gc.WRONG
}

// ---------------------------------------------------------------------------
// Step 2 — the lingering-worker loop with a real watchdog.
// ---------------------------------------------------------------------------

/// Drive real campd + a fake claude running the fragment in `mode`; assert the
/// bead closes, the worker drain-acks, AND campd reaps the LINGERING worker —
/// all inside DEADLINE. Because the worker `exec sleep 600`s, a reap inside 20s
/// can only be campd's KillReleased; a drain-ack→KillReleased regression sleeps
/// past the deadline → the watchdog panics RED.
fn run_fragment_and_expect_reap(mode: &str) {
    let dir = tempfile::tempdir().unwrap();
    let fake = write_fake_claude(dir.path(), &fragment_fixture(), mode);
    let root = scaffold(dir.path(), &fake);
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "do a thing", "--agent", "dev"])
        .trim()
        .to_owned();

    let start = Instant::now();
    loop {
        let events = events_json(&root);
        let closed = events
            .iter()
            .any(|e| e["type"] == "bead.closed" && e["bead"] == bead.as_str());
        let drain_acked = events.iter().any(|e| e["type"] == "worker.drain_acked");
        let reaped = events.iter().any(|e| e["type"] == "session.stopped");
        if closed && drain_acked && reaped {
            // Exactly the §6.2 chain: the bead closed, the worker acked, and
            // campd's KillReleased reaped the still-sleeping worker in time.
            return;
        }
        if start.elapsed() > DEADLINE {
            panic!(
                "campd did not reap the drained worker within {DEADLINE:?} \
                 (mode={mode}): the drain-ack→KillReleased wiring regressed, or the \
                 fragment hung. closed={closed} drain_acked={drain_acked} reaped={reaped}\n\
                 events: {events:#?}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn campd_reaps_a_lingering_gc_worker_via_drain_ack_after_it_closes_the_bead() {
    run_fragment_and_expect_reap("happy");
}

#[test]
fn the_fail_close_branch_also_closes_and_is_reaped_without_a_hang() {
    run_fragment_and_expect_reap("fail");
}

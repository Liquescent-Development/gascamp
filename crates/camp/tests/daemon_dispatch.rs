#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 8 integration (master plan test obligations; spec §8.1, §8.4,
//! §12, §13.3): sling → dispatch → claim → milestone → close with the
//! full event-with-cause trail; crash → SIGCHLD → release; the
//! concurrency cap; worktree lifecycle; registry-before-exec — all driven
//! by fake-agent.sh, no Claude anywhere.
//!
//! campd is always spawned as a real child process here: SIGCHLD is
//! per-process, so in-thread daemons cannot exercise the reap path.
//! Test-side waiting polls the ledger — sanctioned for harnesses only.

use std::io::{BufRead, BufReader};
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

/// A camp with one rig and full dispatch config. Returns (root, rig).
fn scaffold(dir: &Path, max_workers: usize, rig_extra: &str) -> (PathBuf, PathBuf) {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n{rig_extra}\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
             [dispatch]\nmax_workers = {max_workers}\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    // Post-flip (spec §12) the scaffold's dev agent PINS the live-tree
    // opt-out: these tests exercise worker mechanics (crash, cap, routing,
    // canonicalization) on the rig cwd, not the isolation contract — which
    // has its own tests below. Tests about the DEFAULT overwrite the dev
    // agent dir with write_agent(&root, "dev", "").
    write_agent(&root, "dev", "isolation: none\n");
    // create the ledger so every verb (and campd) finds it
    camp_ok(&root, &["events", "--json"]);
    (root, rig)
}

/// Write an agent DIRECTORY (compat §5.1): `agents/<name>/agent.toml` carries
/// the `isolation` opt-out (the only frontmatter key these tests use); model
/// and tools come from `[agent_defaults]` in camp.toml. Each call resets the
/// agent dir so a flip (none↔worktree, or default) is clean.
fn write_agent(root: &Path, name: &str, front_extra: &str) {
    let dir = root.join("agents").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let agent_toml = if front_extra.contains("isolation: none") {
        Some("isolation = \"none\"\n")
    } else if front_extra.contains("isolation: worktree") {
        Some("isolation = \"worktree\"\n")
    } else {
        None
    };
    if let Some(t) = agent_toml {
        std::fs::write(dir.join("agent.toml"), t).unwrap();
    }
    std::fs::write(dir.join("prompt.md"), "Do the work.\n").unwrap();
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    camp_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Test-harness wait (camp never polls; tests may). Panics with the event
/// dump on timeout so failures are diagnosable.
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

fn seq_of(events: &[serde_json::Value], pred: impl Fn(&serde_json::Value) -> bool) -> i64 {
    events
        .iter()
        .find(|e| pred(e))
        .unwrap_or_else(|| panic!("event not found in {events:#?}"))["seq"]
        .as_i64()
        .unwrap()
}

/// campd as a real child process with fake-agent behavior env. Drop kills
/// and reaps it (workers it spawned exit on their own — fake agents are
/// bounded).
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

/// Master plan: "sling → dispatch → claim → milestone → close pass with
/// the full event-with-cause trail (spec §13.3 asserted literally)".
#[test]
fn tier0_sling_runs_the_whole_contract_with_a_causal_trail() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_MILESTONE", "halfway there")]);

    let bead = camp_ok(&root, &["sling", "add a --json flag"])
        .trim()
        .to_owned();
    assert_eq!(bead, "gc-1");

    wait_until(&root, "the full Tier-0 trail", |e| {
        count(e, "session.stopped") == 1
    });
    let events = events_json(&root);

    // The exact causal order for this bead (spec §13.3): created →
    // dispatched (session.woke, bead linked) → claimed → milestone →
    // closed pass → stopped.
    let created = seq_of(&events, |e| {
        e["type"] == "bead.created" && e["bead"] == bead.as_str()
    });
    let woke = seq_of(&events, |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str()
    });
    let claimed = seq_of(&events, |e| {
        e["type"] == "bead.claimed" && e["bead"] == bead.as_str()
    });
    let milestone = seq_of(&events, |e| {
        e["type"] == "worker.milestone" && e["bead"] == bead.as_str()
    });
    let closed = seq_of(&events, |e| {
        e["type"] == "bead.closed" && e["bead"] == bead.as_str()
    });
    let stopped = seq_of(&events, |e| e["type"] == "session.stopped");
    assert!(
        created < woke
            && woke < claimed
            && claimed < milestone
            && milestone < closed
            && closed < stopped,
        "causal order violated: {events:#?}"
    );

    // Registry facts (spec §7.4): name, agent, claude session id (uuid),
    // transcript path computed from the WORKER cwd (the rig, F3).
    let woke_ev = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(woke_ev["data"]["name"], "t/dev/1");
    assert_eq!(woke_ev["data"]["agent"], "dev");
    let sid = woke_ev["data"]["claude_session_id"].as_str().unwrap();
    assert_eq!(sid.len(), 36, "claude_session_id must be a uuid: {sid}");
    let transcript = woke_ev["data"]["transcript_path"].as_str().unwrap();
    // campd canonicalizes the worker cwd before computing the transcript path
    // (Phase 15 finding: real claude realpath-resolves its cwd). Assert against
    // the CANONICAL rig so this genuinely exercises canonicalization — a loose
    // raw-path substring also matched the buggy raw path on macOS (canonical =
    // "/private" + raw), which is exactly the trap to avoid.
    let canon_rig = std::fs::canonicalize(&rig).unwrap();
    let munged_rig: String = canon_rig
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    assert!(
        transcript.contains(&munged_rig) && transcript.ends_with(&format!("{sid}.jsonl")),
        "transcript {transcript} must be under the canonical munged rig dir {munged_rig}"
    );

    // The milestone's actor is the session name (worker attribution).
    let ms = events
        .iter()
        .find(|e| e["type"] == "worker.milestone")
        .unwrap();
    assert_eq!(ms["actor"], "t/dev/1");
    // stopped records exit 0 (F4)
    let st = events
        .iter()
        .find(|e| e["type"] == "session.stopped")
        .unwrap();
    assert_eq!(st["data"]["exit_code"], 0);

    // The stdout capture existed during the run (phase-8 decision G) and is
    // DISPOSED AT REAP, so it is gone once the session stops.
    //
    // This assertion was inverted deliberately, under an operator ruling
    // (2026-07-13, cp-0 review finding 4): decision G promised the capture
    // was "kept for forensics", but the control-plane spec §2.3/§9 makes that
    // same file campd's live read channel and mandates reap-time disposal,
    // and its phase-0 roadmap assigns that disposal to cp-0 by name. The
    // control-plane spec GOVERNS. The supersession is recorded in the v1
    // design spec §7.1 (`docs/design/2026-07-05-gas-camp-design.md`,
    // amendment 2026-07-13) — spec and code do not diverge silently.
    //
    // Decision G's forensics intent is preserved by a different mechanism:
    // campd drains a reaped session's stream to EOF BEFORE disposing it, so
    // the worker's final bytes survive as durable ledger events. The raw file
    // is what goes, not the record. The capture-during-run is proven by the
    // milestone + transcript checks above; the drain-before-disposal ordering
    // is proven by `read_channel.rs`'s worker-lifecycle test.
    //
    // POLLED, not asserted instantaneously: `session.stopped` is appended by
    // the reap, which runs EARLIER IN THE SAME WAKE than the drain block that
    // disposes the file. So a test that observes the event in the ledger can
    // legitimately look at the filesystem before campd has reached disposal.
    // The bare `exists()` check was racy by construction and passed only on
    // timing luck. The assertion is unchanged in meaning — the file MUST be
    // gone — it just gives campd the moment it is entitled to.
    let stream_file = root.join("sessions").join("t-dev-1.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while stream_file.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the stream file was never disposed at reap — control-plane §2.3, and \
             design spec §7.1 as amended 2026-07-13"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    // The state fold agrees with the whole story.
    let out = camp(&root, &["doctor", "--refold"]);
    assert!(out.status.success(), "refold drift after a Tier-0 run");
}

/// compat §6.3 (B12) — a real dispatch installs the gc/bd shims into
/// `.camp/bin`, absolute-path and executable. Mutation caught: deleting the
/// `write_shims` call in `launch()` (then `.camp/bin/gc` never appears).
#[test]
fn a_dispatch_installs_the_absolute_path_shims_into_camp_bin() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let _campd = Daemon::spawn(&root, &[]);
    camp_ok(&root, &["sling", "do a thing"]);
    wait_until(&root, "a dispatch (session.woke)", |e| {
        count(e, "session.woke") >= 1
    });

    let gc = root.join("bin/gc");
    let bd = root.join("bin/bd");
    assert!(gc.exists() && bd.exists(), "launch must install .camp/bin/{{gc,bd}}");
    let body = std::fs::read_to_string(&gc).unwrap();
    // Absolute path (an installed campd binary lives at an absolute path), and
    // NOT a bare `exec camp` (§6.3): a bare name would find the shim itself.
    assert!(
        body.starts_with("#!/bin/sh\nexec /") && body.contains(" gc-shim \"$@\""),
        "shim must exec camp's absolute path: {body:?}"
    );
    assert!(!body.contains("exec camp "), "no bare-name exec (§6.3): {body:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&gc).unwrap().permissions().mode() & 0o111,
            0o111,
            "shims must be executable"
        );
    }
}

/// spec §13.3's literal example shape: "gc-1 closed → gc-2 ready →
/// dispatched (session)". A dependent bead's dispatch must trail its
/// blocker's close in the ledger.
#[test]
fn a_close_unblocks_and_dispatches_the_dependent() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);

    let a = camp_ok(&root, &["sling", "A"]).trim().to_owned();
    wait_until(&root, "A's worker to wake", |e| {
        count(e, "session.woke") == 1
    });
    // B depends on A; created while A is held mid-work.
    let out = camp_ok(&root, &["create", "B", "--needs", &a]);
    let b = out.trim().to_owned();

    // release A: it closes pass, its worker exits, B dispatches
    std::fs::write(hold.join(&a), "go").unwrap();
    std::fs::write(hold.join(&b), "go").unwrap(); // B may run to completion too
    wait_until(&root, "B's worker to wake", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == b.as_str())
    });

    let events = events_json(&root);
    let a_closed = seq_of(&events, |e| {
        e["type"] == "bead.closed" && e["bead"] == a.as_str()
    });
    let b_woke = seq_of(&events, |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == b.as_str()
    });
    assert!(
        a_closed < b_woke,
        "the trail must read: {a} closed → {b} dispatched; events: {events:#?}"
    );
}

/// Master plan: "crash mid-work → SIGCHLD → session.crashed → bead back
/// to open" — nonzero-exit variant.
#[test]
fn a_crash_mid_work_releases_the_bead() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let _campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CRASH", "7"),
            ("FAKE_AGENT_MILESTONE", "about to die"),
        ],
    );

    let bead = camp_ok(&root, &["sling", "doomed"]).trim().to_owned();
    wait_until(&root, "the crash to be recorded", |e| {
        count(e, "session.crashed") == 1
    });

    let events = events_json(&root);
    let crashed = events
        .iter()
        .find(|e| e["type"] == "session.crashed")
        .unwrap();
    assert_eq!(
        crashed["data"]["exit_code"], 7,
        "F4: nonzero exit is a crash"
    );
    // the milestone proves the crash was mid-work (after claim)
    let claimed = seq_of(&events, |e| e["type"] == "bead.claimed");
    let crashed_seq = seq_of(&events, |e| e["type"] == "session.crashed");
    assert!(claimed < crashed_seq);
    // fold released the bead: open again, unclaimed, visible as ready
    let ls = camp_ok(&root, &["ls", "--ready", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&ls).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == bead.as_str())
        .expect("crashed bead must be open and ready again");
    assert_eq!(row["status"], "open");
    assert!(row["claimed_by"].is_null());
    // and Phase 8 deliberately does NOT respawn it (decision C):
    assert_eq!(count(&events, "session.woke"), 1);
}

/// F4's signal row, observed for real: SIGKILL ⇒ session.crashed with
/// signal 9.
#[test]
fn a_sigkilled_worker_is_a_crash_with_the_signal_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_CRASH", "kill")]);
    camp_ok(&root, &["sling", "shot"]);
    wait_until(&root, "the kill to be recorded", |e| {
        count(e, "session.crashed") == 1
    });
    let events = events_json(&root);
    let crashed = events
        .iter()
        .find(|e| e["type"] == "session.crashed")
        .unwrap();
    assert_eq!(crashed["data"]["signal"], 9);
    assert!(crashed["data"].get("exit_code").is_none());
}

/// Master plan: "concurrency cap honored under a burst of ready beads
/// (11 ready, 10 spawned, 11th dispatched on first close)".
#[test]
fn the_cap_holds_at_ten_and_the_eleventh_dispatches_on_first_close() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);

    let beads: Vec<String> = (0..11)
        .map(|i| {
            camp_ok(&root, &["sling", &format!("job {i}")])
                .trim()
                .to_owned()
        })
        .collect();

    // exactly 10 workers wake and claim; the 11th bead stays undispatched
    wait_until(&root, "ten claims", |e| count(e, "bead.claimed") == 10);
    let events = events_json(&root);
    assert_eq!(count(&events, "session.woke"), 10, "the cap is 10");
    let dispatched: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "session.woke")
        .map(|e| e["data"]["bead"].as_str().unwrap())
        .collect();
    let waiting: Vec<&String> = beads
        .iter()
        .filter(|b| !dispatched.contains(&b.as_str()))
        .collect();
    assert_eq!(waiting.len(), 1, "exactly one bead must wait for capacity");
    let eleventh = waiting[0].clone();

    // first close frees capacity; the 11th dispatches
    std::fs::write(hold.join(dispatched[0]), "go").unwrap();
    wait_until(&root, "the 11th dispatch", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == eleventh.as_str())
    });

    // drain everyone; the ledger-reconstructed concurrency never exceeded 10
    for bead in &beads {
        let _ = std::fs::write(hold.join(bead), "go");
    }
    wait_until(&root, "all workers to finish", |e| {
        count(e, "session.stopped") == 11
    });
    let events = events_json(&root);
    let mut live = 0i64;
    let mut max_live = 0i64;
    for e in &events {
        match e["type"].as_str().unwrap() {
            "session.woke" => {
                live += 1;
                max_live = max_live.max(live);
            }
            "session.stopped" | "session.crashed" => live -= 1,
            _ => {}
        }
    }
    assert_eq!(
        max_live, 10,
        "the ledger must show the cap was never exceeded"
    );
}

fn git_rig(rig: &Path) {
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        // hermetic against operator gitconfig: a global
        // commit.gpgsign=true would stall the fixture (CI never signs)
        vec!["config", "commit.gpgsign", "false"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(rig)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Run git in `repo` with hermetic identity/signing (a global
/// commit.gpgsign=true must not stall tests — spawn.rs::git_rig precedent),
/// returning trimmed stdout.
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Master plan: "worktree created/removed on pass". The worker runs in the
/// worktree (proven by FAKE_AGENT_TOUCH landing there), and a clean pass
/// reaps it with the gc-mirrored event.
#[test]
fn worktree_isolation_creates_then_reaps_on_pass() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", "isolation: worktree\n");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap()),
            ("FAKE_AGENT_TOUCH", "proof.txt"),
        ],
    );

    let bead = camp_ok(&root, &["sling", "isolated work"])
        .trim()
        .to_owned();
    wait_until(&root, "the isolated worker to claim", |e| {
        count(e, "bead.claimed") == 1
    });

    let wt = root.join("worktrees").join(&bead);
    assert!(
        wt.join(".git").exists(),
        "worktree must exist mid-run at {}",
        wt.display()
    );
    assert!(
        wt.join("proof.txt").exists(),
        "the worker's cwd must be the worktree"
    );
    // registry records it (decision E)
    let events = events_json(&root);
    let woke = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(woke["data"]["worktree"], wt.to_str().unwrap());

    std::fs::write(hold.join(&bead), "go").unwrap();
    wait_until(&root, "the worktree reap", |e| {
        count(e, "bead.worktree.reaped") == 1
    });
    assert!(
        !wt.exists(),
        "a passed bead's worktree is removed (spec §12)"
    );
    let events = events_json(&root);
    let reaped = events
        .iter()
        .find(|e| e["type"] == "bead.worktree.reaped")
        .unwrap();
    assert_eq!(reaped["bead"], bead.as_str());
    assert_eq!(reaped["data"]["path"], wt.to_str().unwrap());
}

/// Master plan: "worktree kept on fail" — with the reason in the event.
#[test]
fn worktree_is_kept_with_an_event_on_fail() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", "isolation: worktree\n");
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_OUTCOME", "fail")]);

    let bead = camp_ok(&root, &["sling", "will fail"]).trim().to_owned();
    wait_until(&root, "the kept worktree", |e| {
        count(e, "worktree.kept") == 1
    });

    let wt = root.join("worktrees").join(&bead);
    assert!(
        wt.exists(),
        "a failed bead's worktree is kept for forensics (spec §12)"
    );
    let events = events_json(&root);
    let kept = events
        .iter()
        .find(|e| e["type"] == "worktree.kept")
        .unwrap();
    assert_eq!(kept["bead"], bead.as_str());
    assert!(
        kept["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("did not close pass"),
        "kept: {kept}"
    );
}

/// Master plan: "registry row precedes process start" — observed via a
/// spawn that cannot succeed: the woke row (with claude session id and
/// transcript path) commits, then the failure lands as session.crashed
/// with the reason. Nothing dangles.
#[test]
fn the_registry_row_precedes_the_process_and_spawn_failures_land_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    // break the worker command AFTER scaffold wrote it
    let toml = std::fs::read_to_string(root.join("camp.toml")).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        toml.replace(&fake_agent(), "/nonexistent/no-such-worker"),
    )
    .unwrap();
    let _campd = Daemon::spawn(&root, &[]);

    camp_ok(&root, &["sling", "never runs"]);
    wait_until(&root, "the spawn failure", |e| {
        count(e, "session.crashed") == 1
    });

    let events = events_json(&root);
    let woke = seq_of(&events, |e| e["type"] == "session.woke");
    let crashed = seq_of(&events, |e| e["type"] == "session.crashed");
    assert!(
        woke < crashed,
        "registry at birth: woke commits before the exec attempt"
    );
    let woke_ev = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(
        woke_ev["data"]["claude_session_id"].as_str().unwrap().len(),
        36
    );
    assert!(
        woke_ev["data"]["transcript_path"]
            .as_str()
            .unwrap()
            .ends_with(".jsonl")
    );
    let crashed_ev = events
        .iter()
        .find(|e| e["type"] == "session.crashed")
        .unwrap();
    assert!(
        crashed_ev["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("spawn failed"),
        "crashed: {crashed_ev}"
    );
}

/// PR #14 review finding 2 (operator-approved ACK-BEFORE-SETTLE): a slow
/// settle — here a git worktree checkout throttled by a 6 s post-checkout
/// hook — must not starve the poke ack past the client's 5 s read timeout.
/// The ack means "campd is awake and will process this wake"; the bead's
/// durability carries the Tier-0 promise. The settle still completes in
/// the same wake: the worker's trail appears with no further pokes.
#[test]
fn slow_settle_does_not_starve_the_poke_ack() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", "isolation: worktree\n");
    let hooks = rig.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nsleep 6\n").unwrap();
    #[allow(clippy::unwrap_used)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _campd = Daemon::spawn(&root, &[]);

    let started = Instant::now();
    let bead = camp_ok(&root, &["sling", "slow checkout"])
        .trim()
        .to_owned();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "the ack must not wait for the settle; sling took {elapsed:?}"
    );
    // the settle completes in the same wake: no further pokes are issued,
    // yet the worker's whole trail lands (the ≥6 s hook proves the ack
    // outran the worktree creation)
    wait_until(&root, "the slow worker's close", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["bead"] == bead.as_str())
    });
}

/// PR #14 review finding 7 (integration half): a spawn failure AFTER the
/// worktree was created keeps the worktree with the spawn-failure reason —
/// forensics survive even when the worker never ran.
#[test]
fn a_spawn_failure_with_isolation_keeps_the_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", "isolation: worktree\n");
    let toml = std::fs::read_to_string(root.join("camp.toml")).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        toml.replace(&fake_agent(), "/nonexistent/no-such-worker"),
    )
    .unwrap();
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "never runs, isolated"])
        .trim()
        .to_owned();
    wait_until(&root, "the kept worktree after spawn failure", |e| {
        count(e, "worktree.kept") == 1
    });

    let wt = root.join("worktrees").join(&bead);
    assert!(wt.exists(), "the worktree must survive the spawn failure");
    let events = events_json(&root);
    let kept = events
        .iter()
        .find(|e| e["type"] == "worktree.kept")
        .unwrap();
    assert_eq!(kept["bead"], bead.as_str());
    assert_eq!(
        kept["data"]["reason"], "spawn failed before the worker ran",
        "kept: {kept}"
    );
    // and the causal pair is complete: woke then crashed with the reason
    assert_eq!(count(&events, "session.crashed"), 1);
    let crashed = events
        .iter()
        .find(|e| e["type"] == "session.crashed")
        .unwrap();
    assert!(
        crashed["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("spawn failed"),
    );
}

/// Phase 2 (dispatch-lifecycle Q1, spec §12): running on the rig's live
/// tree is an explicit opt-out and it is LOUD — every isolation="none"
/// dispatch appends dispatch.live_tree naming the path and agent, before
/// the worker's registry row. Never silent.
#[test]
fn an_isolation_none_dispatch_is_loud_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    write_agent(&root, "dev", "isolation: none\n");
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "live tree work"])
        .trim()
        .to_owned();
    wait_until(&root, "the live-tree worker to stop", |e| {
        count(e, "session.stopped") == 1
    });

    let events = events_json(&root);
    let live = events
        .iter()
        .find(|e| e["type"] == "dispatch.live_tree")
        .expect("isolation=none dispatch must event dispatch.live_tree");
    assert_eq!(live["bead"], bead.as_str());
    assert_eq!(live["actor"], "campd");
    assert_eq!(live["data"]["agent"], "dev");
    // the recorded path is the worker's cwd — the CANONICAL rig path
    let canon_rig = std::fs::canonicalize(&rig).unwrap();
    assert_eq!(live["data"]["path"], canon_rig.to_str().unwrap());
    // loud BEFORE the worker exists: live_tree precedes the registry row
    let live_seq = seq_of(&events, |e| e["type"] == "dispatch.live_tree");
    let woke_seq = seq_of(&events, |e| e["type"] == "session.woke");
    assert!(
        live_seq < woke_seq,
        "dispatch.live_tree must precede session.woke: {events:#?}"
    );
    // and no worktree machinery ran
    assert_eq!(count(&events, "bead.worktree.reaped"), 0);
    assert!(!root.join("worktrees").join(&bead).exists());
}

/// Phase 2 test obligation (i) (dispatch-lifecycle §9): an autonomous
/// worker's cwd is a camp worktree on camp/<bead> BY DEFAULT — the agent
/// declares no isolation key — and never the rig's live branch. The
/// branch evidence is recorded by the WORKER from inside its own cwd
/// (`git branch --show-current`, FAKE_AGENT_RECORD_BRANCH).
#[test]
fn default_isolation_puts_the_worker_on_a_worktree_branch_never_the_rigs() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", ""); // NO isolation key: the DEFAULT under test
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap()),
            ("FAKE_AGENT_RECORD_BRANCH", "branch.txt"),
        ],
    );

    let bead = camp_ok(&root, &["sling", "default isolation"])
        .trim()
        .to_owned();
    wait_until(&root, "the default-isolated worker to claim", |e| {
        count(e, "bead.claimed") == 1
    });

    // The worker recorded its own branch from inside its own cwd BEFORE
    // claiming (fake-agent ordering contract, issue #44): a ledger-observed
    // claim implies the proof file already exists.
    let wt = root.join("worktrees").join(&bead);
    assert!(
        wt.join("branch.txt").exists(),
        "the worker must record its branch before claiming; worktree dir: {}",
        wt.display()
    );
    let worker_branch = std::fs::read_to_string(wt.join("branch.txt"))
        .unwrap()
        .trim()
        .to_owned();
    let out = Command::new("git")
        .arg("-C")
        .arg(&rig)
        .args(["branch", "--show-current"])
        .output()
        .unwrap();
    let rig_branch = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(worker_branch, format!("camp/{bead}"));
    assert_eq!(rig_branch, "main");
    assert_ne!(
        worker_branch, rig_branch,
        "obligation (i): the worker's branch must never be the rig's checked-out branch"
    );
    assert!(
        !rig.join("branch.txt").exists(),
        "nothing may leak onto the rig's live tree"
    );
    // the default is not the opt-out: no live-tree event fired
    assert_eq!(count(&events_json(&root), "dispatch.live_tree"), 0);

    // release: a clean pass reaps the worktree (spec §12)
    std::fs::write(hold.join(&bead), "go").unwrap();
    wait_until(&root, "the worktree reap", |e| {
        count(e, "bead.worktree.reaped") == 1
    });
    assert!(!wt.exists());
}

/// Phase 2 test obligation (ii) (dispatch-lifecycle §9, §4.2.2): a rig
/// that cannot host a worktree — git-init-only, NO base commit — fails
/// fast at dispatch: dispatch.failed evented, no worker spawned, no
/// registry row, nothing stranded. The bead stays open and ready for
/// after the operator prepares the rig.
#[test]
fn a_baseless_rig_fails_fast_at_dispatch_with_no_worker_and_nothing_stranded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    // git init but NO commit: no base for a worktree branch
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    write_agent(&root, "dev", ""); // default isolation = worktree
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "cannot isolate here"])
        .trim()
        .to_owned();
    wait_until(&root, "the fail-fast dispatch", |e| {
        count(e, "dispatch.failed") == 1
    });

    let events = events_json(&root);
    let failed = events
        .iter()
        .find(|e| e["type"] == "dispatch.failed")
        .unwrap();
    assert_eq!(failed["bead"], bead.as_str());
    assert!(
        failed["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("cannot host a worktree"),
        "reason must carry the refusal: {failed}"
    );
    // no worker was ever spawned: no registry row, no claim, no session end
    for kind in [
        "session.woke",
        "bead.claimed",
        "session.stopped",
        "session.crashed",
    ] {
        assert_eq!(count(&events, kind), 0, "{kind} must not fire");
    }
    // nothing stranded: no commit, no camp/<bead> branch, no worktree dir
    let revs = Command::new("git")
        .arg("-C")
        .arg(&rig)
        .args(["rev-list", "--all", "--count"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&revs.stdout).trim(), "0");
    let branches = Command::new("git")
        .arg("-C")
        .arg(&rig)
        .args(["branch", "--list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&branches.stdout).trim(), "");
    assert!(!root.join("worktrees").join(&bead).exists());
    // the bead is still open and ready — nothing lost
    let ls = camp_ok(&root, &["ls", "--ready", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&ls).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == bead.as_str()),
        "the bead must remain ready: {rows}"
    );
}

/// Obligation (ii), the emptier case: a rig directory that is not a git
/// repository at all. Same fail-fast contract, same ledger evidence.
#[test]
fn a_non_git_rig_fails_fast_at_dispatch_under_default_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, ""); // plain dir, no git
    write_agent(&root, "dev", ""); // default isolation = worktree
    let _campd = Daemon::spawn(&root, &[]);

    let bead = camp_ok(&root, &["sling", "no repo here"]).trim().to_owned();
    wait_until(&root, "the fail-fast dispatch", |e| {
        count(e, "dispatch.failed") == 1
    });

    let events = events_json(&root);
    let failed = events
        .iter()
        .find(|e| e["type"] == "dispatch.failed")
        .unwrap();
    assert_eq!(failed["bead"], bead.as_str());
    assert!(
        failed["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("cannot host a worktree"),
        "reason: {failed}"
    );
    assert_eq!(count(&events, "session.woke"), 0, "no worker spawned");
    assert!(!rig.join(".git").exists(), "the rig stays untouched");
    assert!(!root.join("worktrees").join(&bead).exists());
}

/// Phase 2 test obligation (iii) (dispatch-lifecycle §9): two concurrent
/// autonomous workers on ONE rig get DISTINCT worktrees on distinct
/// camp/<bead> branches — no shared-tree collision — and the rig's live
/// tree is untouched.
#[test]
fn two_concurrent_default_workers_get_distinct_worktrees() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", ""); // default isolation = worktree
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap()),
            ("FAKE_AGENT_TOUCH", "proof.txt"),
        ],
    );

    let b1 = camp_ok(&root, &["sling", "first"]).trim().to_owned();
    let b2 = camp_ok(&root, &["sling", "second"]).trim().to_owned();
    wait_until(&root, "both workers to claim", |e| {
        count(e, "bead.claimed") == 2
    });

    let wt1 = root.join("worktrees").join(&b1);
    let wt2 = root.join("worktrees").join(&b2);
    assert_ne!(wt1, wt2, "distinct beads must get distinct worktrees");
    // both workers ran in their OWN worktree: proof.txt is written BEFORE
    // the claim (fake-agent ordering contract, issue #44), so two observed
    // claims imply both proof files exist.
    for wt in [&wt1, &wt2] {
        assert!(
            wt.join("proof.txt").exists(),
            "each worker must run in its own worktree ({})",
            wt.display()
        );
    }
    // each worktree sits on its own camp/<bead> branch
    for (bead, wt) in [(&b1, &wt1), (&b2, &wt2)] {
        let out = Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("camp/{bead}")
        );
    }
    assert!(
        !rig.join("proof.txt").exists(),
        "the rig's live tree stays untouched"
    );

    std::fs::write(hold.join(&b1), "go").unwrap();
    std::fs::write(hold.join(&b2), "go").unwrap();
    wait_until(&root, "both reaps", |e| {
        count(e, "bead.worktree.reaped") == 2
    });
}

/// Routing (decision D) through the daemon: the rig's default_agent
/// outranks [dispatch].default_agent; session names carry the agent.
#[test]
fn rig_default_agent_routes_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "default_agent = \"rigger\"\n");
    write_agent(&root, "rigger", "isolation: none\n");
    let _campd = Daemon::spawn(&root, &[]);
    camp_ok(&root, &["sling", "routed"]);
    wait_until(&root, "the routed worker", |e| {
        count(e, "session.stopped") == 1
    });
    let events = events_json(&root);
    let woke = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(woke["data"]["agent"], "rigger");
    assert_eq!(woke["data"]["name"], "t/rigger/1");
}

/// A cooked-formula-shaped bead with no assignee and no routable default
/// lands dispatch.failed in the ledger (decision F) — campd's errors are
/// events, and campd survives.
#[test]
fn an_unroutable_bead_lands_dispatch_failed_and_campd_survives() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    // remove the default agent from [dispatch]
    let toml = std::fs::read_to_string(root.join("camp.toml")).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        toml.replace("default_agent = \"dev\"\n", ""),
    )
    .unwrap();
    let _campd = Daemon::spawn(&root, &[]);

    // create (not sling — sling validates routing client-side)
    let bead = camp_ok(&root, &["create", "orphan work"]).trim().to_owned();
    wait_until(&root, "the dispatch failure", |e| {
        count(e, "dispatch.failed") == 1
    });
    let events = events_json(&root);
    let failed = events
        .iter()
        .find(|e| e["type"] == "dispatch.failed")
        .unwrap();
    assert_eq!(failed["bead"], bead.as_str());
    assert!(
        failed["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("default_agent")
    );
    // exactly once per bead per campd lifetime (decision F): a second
    // unroutable bead fails once; the first does NOT re-fail on its poke
    camp_ok(&root, &["create", "another"]);
    wait_until(&root, "the second bead's dispatch failure", |e| {
        count(e, "dispatch.failed") == 2
    });
    // a further poke re-fails neither bead
    camp_ok(&root, &["event", "emit", "poke"]);
    camp_ok(&root, &["top"]); // campd still answers (and this settles the poke)
    assert_eq!(
        count(&events_json(&root), "dispatch.failed"),
        2,
        "one per unroutable bead, not per poke"
    );
}

/// Phase 15 e2e finding, deterministic on every platform: a rig reached
/// through a SYMLINK. Real claude realpath-resolves its cwd before computing
/// its transcript project dir, so campd must canonicalize the worker cwd too —
/// otherwise the registry records, and patrol watches (spec §10), a path claude
/// never writes. The transcript path must be under the canonical (resolved)
/// rig, NOT the symlink path (the symlink leaf name differs from the real one,
/// so this is a clean red→green — not a `/private`-prefix substring accident).
#[test]
fn worker_cwd_is_canonicalized_so_patrol_watches_the_real_transcript_path() {
    fn munge(p: &str) -> String {
        p.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    }

    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real-rig");
    std::fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("linked-rig");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let root = dir.path().join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
             [dispatch]\nmax_workers = 2\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            link.display(), // the rig path IS a symlink
            fake_agent(),
        ),
    )
    .unwrap();
    // this test asserts the LIVE-TREE (rig cwd) canonicalization branch
    write_agent(&root, "dev", "isolation: none\n");
    camp_ok(&root, &["events", "--json"]);

    let _campd = Daemon::spawn(&root, &[]);
    let bead = camp_ok(&root, &["sling", "canon"]).trim().to_owned();
    wait_until(&root, "worker dispatch", |e| {
        e.iter()
            .any(|x| x["type"] == "session.woke" && x["data"]["bead"] == bead.as_str())
    });
    let events = events_json(&root);
    let transcript = events
        .iter()
        .find(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str())
        .unwrap()["data"]["transcript_path"]
        .as_str()
        .unwrap()
        .to_owned();

    let canon = std::fs::canonicalize(&link).unwrap(); // resolves to `real`
    assert!(
        transcript.contains(&munge(&canon.to_string_lossy())),
        "transcript {transcript} must be under the CANONICAL rig dir {}",
        munge(&canon.to_string_lossy())
    );
    assert!(
        !transcript.contains(&munge(&link.to_string_lossy())),
        "transcript {transcript} must NOT use the un-canonicalized symlink path {}",
        munge(&link.to_string_lossy())
    );
}

/// Phase 15 review MEDIUM: the WORKTREE-isolation branch of the cwd
/// canonicalization must have its own regression coverage. Worktree cwd =
/// canonicalize(camp.root)/worktrees/<bead>; here the camp is reached through a
/// SYMLINKED root, so a revert of that branch to the raw
/// `self.camp.worktrees_path().join(bead)` would make patrol watch a path claude
/// never writes. Deterministic on every platform (the symlink leaf name differs
/// from the real one — not a `/private`-prefix substring accident).
#[test]
fn worktree_worker_cwd_is_canonicalized_on_a_symlinked_camp_root() {
    fn munge(p: &str) -> String {
        p.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    }

    let dir = tempfile::tempdir().unwrap();
    // Real camp under `real/`, reached via the symlink `link/` -> `real/`.
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let rig = real.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    git_rig(&rig); // worktree isolation needs a git repo

    let real_root = real.join(".camp");
    std::fs::create_dir_all(&real_root).unwrap();
    std::fs::write(
        real_root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
             [dispatch]\nmax_workers = 2\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    let dev = real_root.join("agents/dev");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("agent.toml"), "isolation = \"worktree\"\n").unwrap();
    std::fs::write(dev.join("prompt.md"), "Do the work.\n").unwrap();

    // Reach the camp through a symlink: campd's self.camp.root is the symlink
    // spelling; canonicalize must resolve it for the worktree cwd.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let linked_root = link.join(".camp");
    camp_ok(&linked_root, &["events", "--json"]); // create the ledger via the symlink spelling

    let _campd = Daemon::spawn(&linked_root, &[]);
    let bead = camp_ok(&linked_root, &["sling", "isolated canon"])
        .trim()
        .to_owned();
    wait_until(&linked_root, "worktree worker dispatch", |e| {
        e.iter()
            .any(|x| x["type"] == "session.woke" && x["data"]["bead"] == bead.as_str())
    });
    let events = events_json(&linked_root);
    let transcript = events
        .iter()
        .find(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str())
        .unwrap()["data"]["transcript_path"]
        .as_str()
        .unwrap()
        .to_owned();

    // Worker cwd = canonicalize(camp.root)/worktrees/<bead> (resolves link ->
    // real, and /var -> /private/var on macOS).
    let canon_cwd = std::fs::canonicalize(&linked_root)
        .unwrap()
        .join("worktrees")
        .join(&bead);
    assert!(
        transcript.contains(&munge(&canon_cwd.to_string_lossy())),
        "worktree transcript {transcript} must be under the CANONICAL worktree cwd {}",
        munge(&canon_cwd.to_string_lossy())
    );
    let raw_cwd = linked_root.join("worktrees").join(&bead);
    assert!(
        !transcript.contains(&munge(&raw_cwd.to_string_lossy())),
        "worktree transcript {transcript} must NOT use the un-canonicalized symlink spelling {}",
        munge(&raw_cwd.to_string_lossy())
    );
}

// ---- Phase 3 (#34): delivery obligations i/ii/vi through the full path ---

/// Obligation (ii) + the (vi)-complement, dispatch-lifecycle Phase 3
/// (#34, Q4): through the REAL dispatch path — worktree default,
/// camp/<bead> branch — a shipping worker records work_outcome=shipped,
/// the worktree is reaped (clean pass), and the bead branch OUTLIVES the
/// reap: reachable and diffable from the rig. The branch is the
/// deliverable.
#[test]
fn a_worktree_worker_ships_on_the_bead_branch_and_the_branch_outlives_the_reap() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 4, "");
    git_rig(&rig); // the rig must be a COMMITTED git repo
    write_agent(&root, "dev", ""); // default isolation = worktree (Phase 2)
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT_DELIVERY", "ship")]);

    let bead = camp_ok(&root, &["sling", "ship it", "--agent", "dev"])
        .trim()
        .to_owned();
    wait_until(&root, "closed and reaped", |e| {
        count(e, "bead.closed") == 1 && count(e, "bead.worktree.reaped") == 1
    });

    let events = events_json(&root);
    let closed = events.iter().find(|e| e["type"] == "bead.closed").unwrap();
    assert_eq!(closed["data"]["work_outcome"], "shipped");
    assert_eq!(closed["data"]["work_branch"], format!("camp/{bead}"));
    let branch_tip = git(&rig, &["rev-parse", &format!("camp/{bead}")]);
    assert_eq!(
        closed["data"]["work_commit"],
        branch_tip.as_str(),
        "the recorded commit IS the bead branch's tip"
    );

    // the worktree is gone (clean pass), the branch is not:
    let wt = root.join("worktrees").join(&bead);
    assert!(!wt.exists(), "reaped worktree must be removed");
    // reachable + diffable FROM THE RIG (shared object store):
    let diff = Command::new("git")
        .arg("-C")
        .arg(&rig)
        .args(["diff", "--stat", &format!("HEAD...camp/{bead}")])
        .output()
        .unwrap();
    assert!(
        diff.status.success(),
        "the bead branch must be diffable post-reap: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
}

/// Obligation (i), dispatch-lifecycle Phase 3 (#34): the original defect,
/// end to end — a baseless rig (isolation="none" is the only way a worker
/// reaches one; the loud opt-out), a dead-end root commit — and the ledger
/// records blocked. `shipped` appears NOWHERE; the gate held (a gate hole
/// makes the fake agent exit 96, which would surface as a crashed session
/// before the blocked close ever lands).
#[test]
fn a_dead_end_worker_on_a_baseless_rig_records_blocked_never_shipped() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 4, "");
    // make the rig BASELESS: git init -b main, NO commit (the Phase 2
    // dispatch-failed test's shape)
    git(&rig, &["init", "-b", "main"]);
    write_agent(&root, "dev", "isolation: none\n"); // the loud opt-out
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT_DELIVERY", "deadend")]);

    let _bead = camp_ok(
        &root,
        &["sling", "give this repo a README", "--agent", "dev"],
    );
    wait_until(&root, "blocked close", |e| count(e, "bead.closed") == 1);

    let events = events_json(&root);
    let closed = events.iter().find(|e| e["type"] == "bead.closed").unwrap();
    assert_eq!(closed["data"]["outcome"], "fail");
    assert_eq!(closed["data"]["work_outcome"], "blocked");
    let all = serde_json::to_string(&events).unwrap();
    assert!(
        !all.contains(r#""work_outcome":"shipped""#),
        "never shipped: {all}"
    );
    assert_eq!(
        count(&events, "dispatch.live_tree"),
        1,
        "the opt-out was loud"
    );
    let _ = rig; // rig asserted only through the worker's own commits
}

/// Obligation (vi): work that is not shipped loses nothing — a blocked
/// close keeps the worktree (worktree.kept via the existing not-pass rule,
/// which the fold's coherence gate guarantees for blocked/abandoned) AND
/// the camp/<bead> branch with the worker's commit stays reachable.
#[test]
fn a_blocked_close_keeps_the_worktree_and_branch() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 4, "");
    git_rig(&rig); // committed git rig, as in the ship test
    write_agent(&root, "dev", "");
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT_DELIVERY", "blocked")]);

    let bead = camp_ok(&root, &["sling", "doomed work", "--agent", "dev"])
        .trim()
        .to_owned();
    wait_until(&root, "blocked close kept the tree", |e| {
        count(e, "bead.closed") == 1 && count(e, "worktree.kept") == 1
    });

    let events = events_json(&root);
    assert_eq!(count(&events, "bead.worktree.reaped"), 0, "must NOT reap");
    let wt = root.join("worktrees").join(&bead);
    assert!(wt.exists(), "worktree kept for forensics");
    let subject = git(&rig, &["log", "-1", "--format=%s", &format!("camp/{bead}")]);
    assert_eq!(
        subject,
        format!("half-done work for {bead}"),
        "the worker's commit survives on the kept branch"
    );
}

/// Test obligation (i), dispatch-lifecycle Phase 1 (#29): ONE sling → exactly
/// ONE session.woke — including across later converge wakes. converge()
/// re-queries the full dispatchable set on EVERY wake (dispatch.rs), so bead
/// A being held live while bead B's sling pokes campd is precisely the
/// re-dispatch hazard; the sessions-bound exclusion in dispatchable_beads()
/// must keep A invisible. No reservation, no second spawner.
#[test]
fn a_single_sling_dispatches_exactly_once_across_subsequent_wakes() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);

    let bead_a = camp_ok(&root, &["sling", "bead a"]).trim().to_owned();
    // A is dispatched and its worker holds (claimed, alive, not closing).
    wait_until(&root, "bead a claimed", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.claimed" && ev["bead"] == bead_a.as_str())
    });

    // A later, unrelated wake: converge() re-runs the full dispatchable
    // query. Bead A must not be dispatched a second time.
    let bead_b = camp_ok(&root, &["sling", "bead b"]).trim().to_owned();
    wait_until(&root, "bead b claimed", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.claimed" && ev["bead"] == bead_b.as_str())
    });

    // Release both holds; both close pass.
    std::fs::write(hold.join(&bead_a), "go").unwrap();
    std::fs::write(hold.join(&bead_b), "go").unwrap();
    wait_until(&root, "both beads closed", |e| count(e, "bead.closed") == 2);

    let events = events_json(&root);
    let wokes_a = events
        .iter()
        .filter(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead_a.as_str())
        .count();
    let wokes_b = events
        .iter()
        .filter(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead_b.as_str())
        .count();
    assert_eq!(wokes_a, 1, "bead a: exactly one dispatch, ever");
    assert_eq!(wokes_b, 1, "bead b: exactly one dispatch, ever");
    assert_eq!(
        count(&events, "session.woke"),
        2,
        "no third spawn of any kind"
    );
}

/// issue #83: a failed dispatch is recoverable. The rig directory is missing
/// at dispatch time, so dispatch fails; `camp top` stops calling the bead
/// `ready` and names it `stuck`; `camp show` points at `camp retry`; after
/// the cause is fixed, `camp retry` re-dispatches the SAME bead (id and
/// history intact) — never a close + re-sling. Fully evented.
#[test]
fn a_failed_dispatch_is_recoverable_with_camp_retry_keeping_the_bead_id() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    // break the cause: remove the rig directory so prepare() fails is_dir
    std::fs::remove_dir_all(&rig).unwrap();

    let _campd = Daemon::spawn(&root, &[]);
    let bead = camp_ok(&root, &["sling", "recover me"]).trim().to_owned();
    wait_until(&root, "the dispatch failure", |e| {
        count(e, "dispatch.failed") == 1
    });

    // camp top: not counted ready; counted stuck
    let top = camp_ok(&root, &["top"]);
    assert!(top.contains("ready: 0"), "top: {top}");
    assert!(top.contains("stuck: 1"), "top: {top}");

    // camp show: the reason and the recovery verb
    let show = camp_ok(&root, &["show", &bead]);
    assert!(show.contains("dispatch-failed"), "show: {show}");
    assert!(
        show.contains(&format!("camp retry {bead}")),
        "show must name the recovery verb: {show}"
    );

    // fix the cause, then re-arm the EXISTING bead
    std::fs::create_dir_all(&rig).unwrap();
    let retry_out = camp_ok(&root, &["retry", &bead]);
    assert!(retry_out.contains(&bead), "retry out: {retry_out}");

    // it re-dispatches: a session.woke names the same bead
    wait_until(&root, "the re-dispatch", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == bead.as_str())
    });

    // ...and the recovered work runs to completion: the SAME bead closes.
    // The close-fields assertion below sits BEHIND this wait_until on
    // purpose — asserting them without waiting would race the worker.
    wait_until(&root, "the recovered close", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["bead"] == bead.as_str())
    });

    let events = events_json(&root);
    // exactly one re-arm, keyed to the bead
    let rearms: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "dispatch.rearmed" && e["bead"] == bead.as_str())
        .collect();
    assert_eq!(rearms.len(), 1, "one dispatch.rearmed: {events:#?}");
    assert!(
        rearms[0]["data"]["previous_reason"]
            .as_str()
            .unwrap()
            .contains("directory"),
        "the re-arm records the prior reason: {}",
        rearms[0]["data"]["previous_reason"]
    );
    // recovery kept the SAME bead — exactly one bead.created (the original
    // sling), re-dispatched by id above; the bead.closed here is the
    // recovered work completing (pass), the end-to-end proof.
    assert_eq!(
        count(&events, "bead.created"),
        1,
        "recovery must not re-sling under a new id: {events:#?}"
    );
    let closed = events
        .iter()
        .find(|e| e["type"] == "bead.closed" && e["bead"] == bead.as_str())
        .expect("the recovered close was waited for above");
    assert_eq!(
        closed["data"]["outcome"], "pass",
        "the recovered work completes pass: {closed:#?}"
    );
}

/// issue #83: the failed state is ledger-durable — a campd RESTART no longer
/// silently re-attempts the bead (the old in-memory failed set was rebuilt
/// empty on restart). Proven deterministically: bead A fails; after a restart
/// a second bead B is created and also fails (a positive sync that campd has
/// converged at least once) — and A's dispatch.failed count is still exactly
/// one, so the restart did not re-attempt A.
#[test]
fn a_dispatch_failure_survives_a_campd_restart_without_a_silent_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    std::fs::remove_dir_all(&rig).unwrap();

    let bead_a = {
        let _campd = Daemon::spawn(&root, &[]);
        let a = camp_ok(&root, &["sling", "bead A"]).trim().to_owned();
        wait_until(&root, "A's dispatch failure", |e| {
            e.iter()
                .any(|ev| ev["type"] == "dispatch.failed" && ev["bead"] == a.as_str())
        });
        a
        // _campd dropped here: campd is killed and reaped (restart)
    };

    let _campd2 = Daemon::spawn(&root, &[]);
    // positive sync: create B (rig still missing), wait for ITS failure —
    // guarantees campd2 has run a converge that scanned the dispatchable set.
    let bead_b = camp_ok(&root, &["sling", "bead B"]).trim().to_owned();
    wait_until(&root, "B's dispatch failure", |e| {
        e.iter()
            .any(|ev| ev["type"] == "dispatch.failed" && ev["bead"] == bead_b.as_str())
    });

    // A was NOT silently re-attempted across the restart.
    let events = events_json(&root);
    let a_failures = events
        .iter()
        .filter(|e| e["type"] == "dispatch.failed" && e["bead"] == bead_a.as_str())
        .count();
    assert_eq!(
        a_failures, 1,
        "the restart must not silently re-attempt A: {events:#?}"
    );
}

/// issue #83 review F3 — the loop-termination proof: `camp retry` while the
/// cause is STILL broken buys exactly one more attempt. The re-arm clears the
/// marker, converge re-dispatches once, the attempt fails and re-marks the
/// bead in the same transaction, so the very next dispatchable scan excludes
/// it again — back to stuck, no runaway dispatch/failure loop, no timer.
#[test]
fn a_retry_while_the_cause_persists_fails_once_more_and_returns_to_stuck() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    std::fs::remove_dir_all(&rig).unwrap();

    let _campd = Daemon::spawn(&root, &[]);
    let bead = camp_ok(&root, &["sling", "still broken"]).trim().to_owned();
    wait_until(&root, "the first dispatch failure", |e| {
        count(e, "dispatch.failed") == 1
    });

    // retry WITHOUT fixing the cause: the verb succeeds (the re-arm is a
    // durable fact) and campd attempts exactly ONE more dispatch
    let retry_out = camp_ok(&root, &["retry", &bead]);
    assert!(retry_out.contains(&bead), "retry out: {retry_out}");
    wait_until(&root, "the second dispatch failure", |e| {
        e.iter()
            .filter(|ev| ev["type"] == "dispatch.failed" && ev["bead"] == bead.as_str())
            .count()
            == 2
    });

    // back to stuck, not ready — the truth surface recovers too
    let top = camp_ok(&root, &["top"]);
    assert!(top.contains("ready: 0"), "top: {top}");
    assert!(top.contains("stuck: 1"), "top: {top}");

    // positive sync proving no runaway: a second bead's failure is a later
    // converge scan; the first bead's count is still exactly 2 afterwards.
    let bead_b = camp_ok(&root, &["sling", "sync"]).trim().to_owned();
    wait_until(&root, "the sync bead's failure", |e| {
        e.iter()
            .any(|ev| ev["type"] == "dispatch.failed" && ev["bead"] == bead_b.as_str())
    });
    let events = events_json(&root);
    let a_failures = events
        .iter()
        .filter(|e| e["type"] == "dispatch.failed" && e["bead"] == bead.as_str())
        .count();
    assert_eq!(
        a_failures, 2,
        "one re-arm buys exactly one attempt — no loop: {events:#?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "dispatch.rearmed")
            .count(),
        1,
        "exactly the operator's one re-arm, nothing automatic: {events:#?}"
    );
}

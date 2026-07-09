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
             [dispatch]\nmax_workers = {max_workers}\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    write_agent(&root, "dev", "");
    // create the ledger so every verb (and campd) finds it
    camp_ok(&root, &["events", "--json"]);
    (root, rig)
}

fn write_agent(root: &Path, name: &str, front_extra: &str) {
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join(format!("{name}.md")),
        format!("---\nname: {name}\n{front_extra}---\nDo the work.\n"),
    )
    .unwrap();
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

    // Envelope capture exists (decision G)
    assert!(root.join("sessions").join("t-dev-1.json").exists());

    // The state fold agrees with the whole story.
    let out = camp(&root, &["doctor", "--refold"]);
    assert!(out.status.success(), "refold drift after a Tier-0 run");
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

/// Routing (decision D) through the daemon: the rig's default_agent
/// outranks [dispatch].default_agent; session names carry the agent.
#[test]
fn rig_default_agent_routes_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "default_agent = \"rigger\"\n");
    write_agent(&root, "rigger", "");
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
             [dispatch]\nmax_workers = 2\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            link.display(), // the rig path IS a symlink
            fake_agent(),
        ),
    )
    .unwrap();
    write_agent(&root, "dev", "");
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
             [dispatch]\nmax_workers = 2\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    let agents = real_root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("dev.md"),
        "---\nname: dev\nisolation: worktree\n---\nDo the work.\n",
    )
    .unwrap();

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

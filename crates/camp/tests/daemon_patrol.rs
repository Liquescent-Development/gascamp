#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 11 integration (master plan test obligations; spec §8.5, §10):
//! stall → nudge revival; nudge ignored → restart re-hooks the bead;
//! ladder exhaustion emits and stops; kill -9 campd → restart → adopt
//! reconciles exactly; transcript activity keeps a working agent
//! unmolested — all driven by fake-agent.sh, no Claude anywhere.
//!
//! campd is a real child process (SIGCHLD and the patrol pipe are
//! per-process). Test-side waiting polls the ledger — sanctioned for
//! harnesses only. CLAUDE_CONFIG_DIR points into the test tempdir so the
//! computed transcript paths (and the patrol watches on them) stay
//! hermetic.

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

/// A camp with one rig, dispatch config, and a `[patrol]` section.
fn scaffold(dir: &Path, patrol_toml: &str, agents: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
             [dispatch]\nmax_workers = 4\ncommand = \"{}\"\ndefault_agent = \"dev\"\n\n\
             [patrol]\n{patrol_toml}\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    for (name, front_extra) in agents {
        write_agent(&root, name, front_extra);
    }
    // create the ledger so every verb (and campd) finds it
    camp_ok(&root, &["events", "--json"]);
    (root, rig)
}

/// Write an agent DIRECTORY (compat §5.1): `agents/<name>/agent.toml` carries
/// the `isolation` opt-out (the only frontmatter key these tests use); the
/// `stall_after` override also lives in agent.toml. Model + tools come from
/// `[agent_defaults]`. Each call resets the agent dir.
fn write_agent(root: &Path, name: &str, front_extra: &str) {
    let dir = root.join("agents").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut agent_toml = String::new();
    if front_extra.contains("isolation: none") {
        agent_toml.push_str("isolation = \"none\"\n");
    } else if front_extra.contains("isolation: worktree") {
        agent_toml.push_str("isolation = \"worktree\"\n");
    }
    if let Some(idx) = front_extra.find("stall_after:") {
        let rest = &front_extra[idx + "stall_after:".len()..];
        let val = rest.trim_start().split(['\n', ' ']).next().unwrap_or("");
        if !val.is_empty() {
            agent_toml.push_str(&format!("stall_after = {val:?}\n"));
        }
    }
    if !agent_toml.is_empty() {
        std::fs::write(dir.join("agent.toml"), agent_toml).unwrap();
    }
    std::fs::write(dir.join("prompt.md"), "Do the work.\n").unwrap();
}

fn git_rig(rig: &Path) {
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        // hermetic: never depend on the host's signing agent
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

/// Like `wait_until`, but POKES campd (a socket round-trip) each iteration so it
/// wakes and drains tailed stream files — on macOS a worker's stdout append
/// through its inherited fd is notify-suppressed, so an explicit wake surfaces
/// it. Ignores poke failures (campd may be momentarily unavailable).
fn wait_until_poking(root: &Path, what: &str, pred: impl Fn(&[serde_json::Value]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let _ = Command::new(BIN)
            .env_remove("CAMP_DIR")
            .arg("--camp")
            .arg(root)
            .arg("top")
            .output();
        let events = events_json(root);
        if pred(&events) {
            return;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}; events: {events:#?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stalled_actions(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e["type"] == "agent.stalled")
        .map(|e| e["data"]["action"].as_str().unwrap().to_owned())
        .collect()
}

/// campd as a real child process with fake-agent behavior env plus a
/// hermetic CLAUDE_CONFIG_DIR. Drop kills and reaps it.
struct Daemon {
    child: Child,
}

impl Daemon {
    fn spawn(root: &Path, claude_dir: &Path, envs: &[(&str, &str)]) -> Daemon {
        std::fs::create_dir_all(claude_dir).unwrap();
        let mut cmd = Command::new(BIN);
        cmd.env_remove("CAMP_DIR")
            .env("CAMP_BIN", BIN)
            .env("CLAUDE_CONFIG_DIR", claude_dir)
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

    /// The crash the master plan demands: kill -9, no goodbye.
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

/// Master plan: "fake agent goes silent → stall → nudge revives it." The
/// worker blocks reading stdin (silent — fake agents write no transcript
/// unless told to); the stall declares a nudge; the nudge line lands on
/// the held stream stdin, unblocks the worker, and it closes pass. No
/// restart happens: the nudge alone revived it.
#[test]
fn silent_worker_stalls_and_a_nudge_revives_it() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(
        dir.path(),
        "stall_after = \"500ms\"",
        &[("dev", "isolation: none\n")],
    );
    let _campd = Daemon::spawn(
        &root,
        &dir.path().join("claude-home"),
        &[("FAKE_AGENT_NUDGE_CLOSE", "1")],
    );
    camp_ok(&root, &["sling", "revive me"]);

    wait_until(&root, "the nudge declaration", |e| {
        stalled_actions(e).contains(&"nudge".to_owned())
    });
    wait_until(&root, "the revived close", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["data"]["outcome"] == "pass")
    });
    wait_until(&root, "the session end", |e| {
        count(e, "session.stopped") == 1
    });

    let events = events_json(&root);
    assert_eq!(count(&events, "session.woke"), 1, "no restart: one worker");
    assert_eq!(count(&events, "session.crashed"), 0);
    let stalled: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "agent.stalled")
        .collect();
    let first = stalled[0];
    assert_eq!(first["data"]["action"], "nudge");
    assert_eq!(first["data"]["agent"], "dev");
    assert_eq!(first["data"]["restarts"], 0);
    assert!(first["bead"].as_str().unwrap().starts_with("gc-"));
    assert_eq!(first["actor"], "campd");
    assert!(
        !stalled_actions(&events).contains(&"restart".to_owned()),
        "the nudge revived it; no restart may follow: {events:#?}"
    );
}

/// Master plan: "nudge fails → restart re-hooks the bead." The worker
/// ignores its stdin (HOLD gate), so the nudge changes nothing; the next
/// fire restarts: kill (caused session.crashed) → fold releases the bead
/// → converge respawns → the SECOND worker claims the SAME bead and,
/// with the gate now open, closes it pass.
#[test]
fn an_unresponsive_worker_is_restarted_and_the_bead_rehooked() {
    let dir = tempfile::tempdir().unwrap();
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let (root, _rig) = scaffold(
        dir.path(),
        "stall_after = \"500ms\"\nrestart_budget = 1",
        &[("dev", "isolation: none\n")],
    );
    let _campd = Daemon::spawn(
        &root,
        &dir.path().join("claude-home"),
        &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())],
    );
    camp_ok(&root, &["sling", "stubborn work"]);

    // stall #1 → nudge (ignored: the agent holds, reading no stdin)
    wait_until(&root, "the ignored nudge", |e| {
        stalled_actions(e).contains(&"nudge".to_owned())
    });
    // stall #2 → restart with the cause chained to the declaration
    wait_until(&root, "the caused restart crash", |e| {
        e.iter()
            .any(|ev| ev["type"] == "session.crashed" && ev["data"]["reason"] == "patrol restart")
    });
    let events = events_json(&root);
    let restart_decl = events
        .iter()
        .find(|e| e["type"] == "agent.stalled" && e["data"]["action"] == "restart")
        .expect("the restart declaration precedes the kill");
    let crashed = events
        .iter()
        .find(|e| e["type"] == "session.crashed")
        .unwrap();
    assert_eq!(
        crashed["data"]["cause_seq"], restart_decl["seq"],
        "the kill names its cause"
    );

    // the respawned worker re-hooks the bead; open the gate and it passes
    wait_until(&root, "the respawn", |e| count(e, "session.woke") == 2);
    let bead = restart_decl["bead"].as_str().unwrap().to_owned();
    std::fs::write(hold.join(&bead), "go").unwrap();
    wait_until(&root, "the re-hooked close", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["data"]["outcome"] == "pass")
    });
    let events = events_json(&root);
    assert_eq!(count(&events, "bead.claimed"), 2, "re-hooked: two claims");
    let woke_beads: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "session.woke")
        .map(|e| e["data"]["bead"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(woke_beads, vec![bead.clone(), bead], "same bead, twice");
}

/// Master plan ladder table, integration half: budget 0 means nudge then
/// EXHAUSTED — emit and stop. No kill, no further fires; campd keeps
/// serving; escalation is pack content matching event:agent.stalled.
#[test]
fn ladder_exhaustion_emits_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let (root, _rig) = scaffold(
        dir.path(),
        "stall_after = \"400ms\"\nrestart_budget = 0",
        &[("dev", "isolation: none\n")],
    );
    let campd = Daemon::spawn(
        &root,
        &dir.path().join("claude-home"),
        &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())],
    );
    camp_ok(&root, &["sling", "hopeless work"]);

    wait_until(&root, "exhaustion", |e| {
        stalled_actions(e).contains(&"exhausted".to_owned())
    });
    // a bounded quiet window: exhaustion means NO further patrol activity
    std::thread::sleep(Duration::from_millis(1500));
    let events = events_json(&root);
    assert_eq!(
        stalled_actions(&events),
        vec!["nudge".to_owned(), "exhausted".to_owned()],
        "emit and stop"
    );
    assert_eq!(count(&events, "session.crashed"), 0, "never killed");
    assert_eq!(count(&events, "session.woke"), 1, "never respawned");
    // campd is unharmed
    let out = camp_ok(&root, &["top"]);
    assert!(out.contains("live"), "top output: {out}");
    // release the held worker so the daemon teardown is clean
    let bead = events
        .iter()
        .find(|e| e["type"] == "agent.stalled")
        .and_then(|e| e["bead"].as_str())
        .unwrap()
        .to_owned();
    std::fs::write(hold.join(bead), "go").unwrap();
    drop(campd);
}

/// cp-3 §5.3.3 — THE HEART, integration CONFIRM (macOS-genuine; a CONFIRM, not
/// the falsifying guard — that is the platform-independent component test on
/// `stall_step`). A fake worker emits a `can_use_tool` through its OWN long-lived
/// inherited stdout fd — genuinely notify-suppressed on macOS (surfaced only by
/// the ladder's pre-ladder drain), and surfaced via inotify on Linux. Either way
/// it reaches BLOCKED, and past the stall threshold it is NEVER nudged,
/// restarted, or killed. (Do NOT inject the can_use_tool with a test-side
/// open+write+close — that fires notify and defeats the point; the worker
/// self-emits through its inherited fd.)
#[test]
fn a_blocked_worker_is_never_nudged_restarted_or_killed_past_the_stall_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(
        dir.path(),
        "stall_after = \"1s\"",
        &[("dev", "isolation: none\n")],
    );
    let campd = Daemon::spawn(
        &root,
        &dir.path().join("claude-home"),
        &[("FAKE_AGENT_CAN_USE_TOOL", "1")],
    );
    camp_ok(&root, &["sling", "ask permission"]);

    // BLOCKED reached: a permission.pending event carries its cause.
    wait_until(&root, "the permission.pending (BLOCKED)", |e| {
        e.iter()
            .any(|ev| ev["type"] == "permission.pending" && ev["data"]["tool_name"] == "Bash")
    });
    let before = events_json(&root);
    let session = before.iter().find(|e| e["type"] == "session.woke").unwrap()["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    // Deltas measured FROM the BLOCKED point: a pre-BLOCKED nudge (possible if
    // worker startup outran the first 1s fire) is not a violation — the HEART is
    // that a worker that IS blocked takes no further ladder action.
    let stalled_before = count(&before, "agent.stalled");

    // 3× stall_after of real time: a BLOCKED worker takes NO ladder action.
    std::thread::sleep(Duration::from_millis(3000));
    let events = events_json(&root);
    assert_eq!(
        count(&events, "agent.stalled"),
        stalled_before,
        "a BLOCKED worker is never declared stalled past the threshold: {events:#?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "session.crashed" && e["data"]["name"] == session.as_str())
            .count(),
        0,
        "a BLOCKED worker is never killed"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "session.woke" && e["data"]["name"] == session.as_str())
            .count(),
        1,
        "a BLOCKED worker is never respawned"
    );
    assert!(
        !events.iter().any(|e| e["type"] == "permission.decided"),
        "no decision was made, so it is still blocked"
    );
    // campd is unharmed and responsive (not wedged).
    let out = camp_ok(&root, &["top"]);
    assert!(out.contains("live"), "top output: {out}");
    drop(campd);
}

/// cp-3 §5.3.4 (CP3-B4): a permission DISCOVERED after adoption takes the NAMED
/// kill, not the stall ladder. campd1 spawns a worker that delays its
/// can_use_tool; campd1 is kill -9'd BEFORE the request exists; campd2 adopts the
/// still-live worker (non-child, no held stdin); the worker then emits its
/// can_use_tool, campd2 surfaces it, and the steady-state event-loop branch gives
/// it the SAME named kill — with its bead re-hooked, and NO agent.stalled.
/// Mutation caught: routing a discovered pending to the generic ladder.
#[test]
fn a_pending_discovered_after_adoption_takes_the_named_kill_not_the_stall_ladder() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(
        dir.path(),
        "stall_after = \"30s\"", // long: the ladder must not interfere
        &[("dev", "isolation: none\n")],
    );
    let claude_home = dir.path().join("claude-home");
    let campd1 = Daemon::spawn(
        &root,
        &claude_home,
        &[
            ("FAKE_AGENT_CAN_USE_TOOL", "1"),
            ("FAKE_AGENT_CAN_USE_TOOL_DELAY", "6"), // emit only AFTER campd2 adopts
            ("FAKE_AGENT_LINGER_ON_EOF", "30"),     // outlive campd1
        ],
    );
    camp_ok(&root, &["sling", "ask later"]);
    // the worker claimed but has NOT yet emitted its can_use_tool (delayed)
    wait_until(&root, "the claim", |e| count(e, "bead.claimed") == 1);
    let session = events_json(&root)
        .into_iter()
        .find(|e| e["type"] == "session.woke")
        .unwrap()["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned();

    // kill -9 campd1 while the worker lingers with NO pending recorded yet
    campd1.kill9();
    let _campd2 = Daemon::spawn(&root, &claude_home, &[]);

    // campd2 adopts the live worker (non-child); when its delay expires it emits
    // the can_use_tool, campd2 surfaces it, and the steady-state branch kills it
    // with the named cause.
    wait_until_poking(&root, "the named adoption kill", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.crashed"
                && ev["data"]["name"] == session.as_str()
                && ev["data"]["reason"] == "adoption: unanswerable permission request"
        })
    });
    let events = events_json(&root);
    assert_eq!(
        count(&events, "agent.stalled"),
        0,
        "the discovered pending took the NAMED kill, NOT the stall ladder: {events:#?}"
    );
    // the bead re-hooked via the fold crash-reopen
    let bead = events.iter().find(|e| e["type"] == "bead.claimed").unwrap()["bead"]
        .as_str()
        .unwrap()
        .to_owned();
    let show = camp_ok(&root, &["show", &bead, "--json"]);
    let show: serde_json::Value = serde_json::from_str(&show).unwrap();
    assert_eq!(show["status"], "open", "the bead re-hooked: {show}");
}

/// Master plan: "kill -9 campd mid-run → restart → adopt reconciles
/// exactly (crashed marked, live re-armed, orphan worktree swept)."
/// Worker A (worktree-isolated) is SIGKILLed while campd is dead and its
/// bead closed by hand — the interrupted-disposition orphan. Worker B
/// stays alive and held. The restarted campd's automatic adoption marks A
/// crashed, re-arms B, and sweeps A's worktree; a second `camp adopt` is
/// exactly zero.
#[test]
fn kill9_campd_then_adopt_reconciles_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let (root, rig) = scaffold(
        dir.path(),
        "stall_after = \"10s\"", // long: no stalls during the window
        &[
            ("iso", "isolation: worktree\n"),
            ("dev", "isolation: none\n"),
        ],
    );
    git_rig(&rig);
    let claude_home = dir.path().join("claude-home");
    let campd = Daemon::spawn(
        &root,
        &claude_home,
        &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())],
    );
    camp_ok(&root, &["create", "doomed work", "--assignee", "iso"]);
    camp_ok(&root, &["create", "surviving work", "--assignee", "dev"]);
    camp_ok(&root, &["events", "--json"]); // any verb pokes; explicit poke:
    wait_until(&root, "both workers claimed", |e| {
        count(e, "bead.claimed") == 2
    });
    let events = events_json(&root);
    let woke_of = |agent: &str| -> serde_json::Value {
        events
            .iter()
            .find(|e| e["type"] == "session.woke" && e["data"]["agent"] == agent)
            .unwrap_or_else(|| panic!("no woke for {agent}: {events:#?}"))
            .clone()
    };
    let woke_a = woke_of("iso");
    let bead_a = woke_a["data"]["bead"].as_str().unwrap().to_owned();
    let sid_a = woke_a["data"]["claude_session_id"].as_str().unwrap();
    let session_a = woke_a["data"]["name"].as_str().unwrap().to_owned();
    let bead_b = woke_of("dev")["data"]["bead"].as_str().unwrap().to_owned();
    assert!(
        root.join("worktrees").join(&bead_a).is_dir(),
        "A runs isolated"
    );

    // the crash: campd dies ungracefully, then A dies too
    campd.kill9();
    let pkill = Command::new("pkill")
        .args(["-9", "-f", sid_a])
        .status()
        .unwrap();
    assert!(pkill.success(), "worker A must have been alive to kill");
    // A's bead closes pass by hand while campd is down: the disposition
    // (worktree removal) is now interrupted — the orphan.
    camp_ok(
        &root,
        &[
            "close",
            &bead_a,
            "--outcome",
            "pass",
            "--reason",
            "done by hand",
        ],
    );

    // the restart: adoption runs before the ready line
    let _campd2 = Daemon::spawn(
        &root,
        &claude_home,
        &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())],
    );
    wait_until(&root, "A marked crashed by adopt", |e| {
        e.iter().any(|ev| {
            ev["type"] == "session.crashed"
                && ev["data"]["name"] == session_a.as_str()
                && ev["data"]["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("adopt: process not found"))
        })
    });
    wait_until(&root, "A's orphan worktree swept", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.worktree.reaped" && ev["bead"] == bead_a.as_str())
    });
    assert!(
        !root.join("worktrees").join(&bead_a).exists(),
        "the orphan is gone"
    );

    // exactly once: a manual adopt now reconciles NOTHING (B is tracked)
    let second = camp_ok(&root, &["adopt"]);
    assert_eq!(
        second.trim(),
        "adopted: 0 crashed, 0 re-armed, 0 released, 0 worktrees swept, 0 kept",
        "adoption is idempotent"
    );
    let events = events_json(&root);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "session.crashed" && e["data"]["name"] == session_a.as_str())
            .count(),
        1,
        "A crashed exactly once"
    );

    // B was re-armed and stays functional: open its gate, it closes pass
    std::fs::write(hold.join(&bead_b), "go").unwrap();
    wait_until(&root, "B's close", |e| {
        e.iter().any(|ev| {
            ev["type"] == "bead.closed"
                && ev["bead"] == bead_b.as_str()
                && ev["data"]["outcome"] == "pass"
        })
    });
}

/// Master plan: "transcript touch resets." A worker that heartbeats its
/// transcript every 250 ms for ~2 s of work under a 1 s threshold must
/// NEVER look stalled — any missed watch-reset fires a stall before the
/// close and fails this test. (Watch item 3: if platform watch latency
/// makes this flaky in CI, the unit-level reset pins carry the obligation
/// and this test is demoted — noted in the PR.)
#[test]
fn transcript_activity_keeps_a_working_agent_unmolested() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(
        dir.path(),
        "stall_after = \"1s\"",
        &[("dev", "isolation: none\n")],
    );
    let _campd = Daemon::spawn(
        &root,
        &dir.path().join("claude-home"),
        &[("FAKE_AGENT_TOUCH_TRANSCRIPT_LOOP", "8")], // ~2 s of heartbeats
    );
    camp_ok(&root, &["sling", "steady work"]);
    wait_until(&root, "the working close", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["data"]["outcome"] == "pass")
    });
    let events = events_json(&root);
    assert_eq!(
        count(&events, "agent.stalled"),
        0,
        "a heartbeating worker is never stalled: {events:#?}"
    );
    assert_eq!(count(&events, "session.crashed"), 0);
}

/// #81: a hot reload that adds a PACK shipping a new agent must reach
/// PATROL, not just the dispatcher. A worker dispatched to the reloaded
/// pack agent (with NO campd restart) is resolved by patrol: the stall it
/// declares carries the AGENT's own stall_after — not the camp default the
/// stale birth config would fall back to — and no patrol.degraded is
/// emitted. Against main (the CONFIG_WATCH arm never calls
/// patrol.apply_config) patrol keeps its birth config, so it cannot see the
/// pack agent: it falls back to the 5s camp default and logs a
/// patrol.degraded "unknown agent" — both assertions fail.
#[test]
fn a_hot_reloaded_pack_agent_is_resolved_by_patrol_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    // Birth camp: a distinct 5s camp-default stall_after, one local "dev"
    // agent so the base config is runnable; no packs yet.
    let (root, rig) = scaffold(
        dir.path(),
        "stall_after = \"5s\"",
        &[("dev", "isolation: none\n")],
    );

    // A throwaway pack shipping agent "sentry" with a DISTINCT 700ms
    // stall_after override. Compat: a pack is an import bound under
    // `<root>/imports/<binding>/`; the daemon's hot reload only RE-PARSES
    // camp.toml (it does not materialize), so pre-materialize the agent dir.
    let pack = dir.path().join("sentrypack");
    std::fs::create_dir_all(pack.join("agents/sentry")).unwrap();
    std::fs::write(
        pack.join("pack.toml"),
        "[pack]\nname = \"sentrypack\"\nschema = 2\n",
    )
    .unwrap();
    std::fs::write(
        pack.join("agents/sentry/agent.toml"),
        "isolation = \"none\"\nstall_after = \"700ms\"\n",
    )
    .unwrap();
    std::fs::write(pack.join("agents/sentry/prompt.md"), "Work.\n").unwrap();
    // Pre-materialize the import so the reloaded [imports.sentry] resolves.
    let sentry_dir = root.join("imports/sentry/agents/sentry");
    std::fs::create_dir_all(&sentry_dir).unwrap();
    std::fs::copy(
        pack.join("agents/sentry/agent.toml"),
        sentry_dir.join("agent.toml"),
    )
    .unwrap();
    std::fs::copy(
        pack.join("agents/sentry/prompt.md"),
        sentry_dir.join("prompt.md"),
    )
    .unwrap();

    // FAKE_AGENT_NUDGE_CLOSE=1: the worker goes silent (stalls), then closes
    // on the nudge so no fake-agent process outlives the daemon.
    let _campd = Daemon::spawn(
        &root,
        &dir.path().join("claude-home"),
        &[("FAKE_AGENT_NUDGE_CLOSE", "1")],
    );

    // Hot-add the import and route new beads to its agent — NO restart. The
    // reloaded camp.toml carries `[imports.sentry]` (the binding) and a
    // binding-qualified `default_agent = "sentry.sentry"`; `[agent_defaults].tools`
    // is required (compat §5.2).
    let reloaded = format!(
        "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
         [imports.sentry]\nsource = \"{}\"\n\n\
         [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
         [dispatch]\nmax_workers = 4\ncommand = \"{}\"\ndefault_agent = \"sentry.sentry\"\n\n\
         [patrol]\nstall_after = \"5s\"\n",
        rig.display(),
        pack.display(),
        fake_agent(),
    );
    std::fs::write(root.join("camp.toml"), &reloaded).unwrap();
    wait_until(&root, "the applied reload", |e| {
        e.iter()
            .any(|ev| ev["type"] == "config.changed" && ev["data"]["applied"] == true)
    });

    // Dispatch a bead to the freshly added agent.
    camp_ok(&root, &["sling", "watch me"]);

    // Patrol must resolve "sentry.sentry" from the reloaded import: the first
    // stall it declares for this worker carries the agent's 700ms threshold.
    wait_until(&root, "the sentry stall", |e| {
        e.iter()
            .any(|ev| ev["type"] == "agent.stalled" && ev["data"]["agent"] == "sentry.sentry")
    });
    let events = events_json(&root);
    let first_stall = events
        .iter()
        .find(|e| e["type"] == "agent.stalled" && e["data"]["agent"] == "sentry.sentry")
        .unwrap();
    assert_eq!(
        first_stall["data"]["threshold"], "700ms",
        "patrol must arm at the reloaded import agent's stall_after, not the camp default; events: {events:#?}"
    );
    assert_eq!(
        count(&events, "patrol.degraded"),
        0,
        "no unknown-agent degradation once the reload reaches patrol; events: {events:#?}"
    );

    // The nudge revives and closes it: clean shutdown, no lingering worker.
    wait_until(&root, "the revived close", |e| {
        e.iter().any(|ev| ev["type"] == "session.stopped")
    });
}

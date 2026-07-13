#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 13 idle + dispatch-latency suite (spec §14). LOCAL-ONLY: the three
//! measured tests are #[ignore]d and run only by `make perf` in --release.
//! The non-ignored tests exercise the `ps` cputime/rss parsers so CI keeps
//! them correct. The daemon harness below is copied verbatim from
//! `daemon_dispatch.rs` (Phase 8); the perf-specific helpers follow it.

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
    // perf measures the spec §14 dispatch floor; `git worktree add` in the
    // measured path would change what the numbers mean. Pin the live-tree
    // opt-out — post-flip, perf measures the opt-out path (recorded as a
    // non-blocking note in the Phase 2 plan).
    write_agent(&root, "dev", "isolation: none\n");
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

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- Phase 13 perf-specific helpers ----------------------------------------

/// Tight-poll the ledger and return the `Instant` at which `pred` first holds
/// for some event. Poll granularity (5 ms) bounds the measurement error.
fn wait_for_instant(root: &Path, what: &str, pred: impl Fn(&serde_json::Value) -> bool) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if events_json(root).iter().any(&pred) {
            return Instant::now();
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Parse a `ps -o cputime` field into a Duration. Formats:
///  - macOS: `MM:SS.ss` or `HH:MM:SS`
///  - Linux: `[[DD-]HH:]MM:SS`
fn parse_cputime(s: &str) -> Duration {
    let s = s.trim();
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap(), r),
        None => (0, s),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, mins, secs): (u64, u64, f64) = match parts.as_slice() {
        [s] => (0, 0, s.parse().unwrap()),
        [m, s] => (0, m.parse().unwrap(), s.parse().unwrap()),
        [h, m, s] => (h.parse().unwrap(), m.parse().unwrap(), s.parse().unwrap()),
        _ => panic!("unrecognized cputime {s:?}"),
    };
    let total = days as f64 * 86400.0 + hours as f64 * 3600.0 + mins as f64 * 60.0 + secs;
    Duration::from_secs_f64(total)
}

fn parse_rss_kb(s: &str) -> u64 {
    s.trim().parse().unwrap()
}

/// Sample the process's accumulated CPU time and resident set size via `ps`.
fn ps_cputime_rss(pid: u32) -> (Duration, u64) {
    let out = Command::new("ps")
        .args(["-o", "cputime=,rss=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success(), "ps failed for pid {pid}");
    let line = String::from_utf8(out.stdout).unwrap();
    let mut it = line.split_whitespace();
    let cpu = it
        .next()
        .unwrap_or_else(|| panic!("no cputime in ps output {line:?}"));
    let rss = it
        .next()
        .unwrap_or_else(|| panic!("no rss in ps output {line:?}"));
    (parse_cputime(cpu), parse_rss_kb(rss))
}

#[test]
fn parse_cputime_formats() {
    assert_eq!(parse_cputime("00:00.00").as_millis(), 0);
    assert!((parse_cputime("00:00.03").as_millis() as i64 - 30).abs() <= 1);
    assert_eq!(parse_cputime("0:02.50").as_millis(), 2500);
    assert_eq!(parse_cputime("01:02:03").as_secs(), 3723);
    assert_eq!(
        parse_cputime("1-02:03:04").as_secs(),
        86400 + 2 * 3600 + 3 * 60 + 4
    );
}

#[test]
fn parse_rss_kb_parses() {
    assert_eq!(parse_rss_kb("  12345 "), 12345);
}

/// Invariant 1 (idle is free): a campd with no work and no orders blocks in
/// `poll` — over a 30 s idle window its accumulated CPU time does not move
/// (±10 ms) and its RSS stays under 20 MB.
///
/// The ±10 ms tolerance assumes macOS `ps` cputime CENTISECOND resolution
/// (`MM:SS.ss`): 10 ms is one tick, so this detects both a busy-loop and a
/// tick-storm regression. NOTE: on Linux `ps -o cputime` has 1-SECOND
/// resolution, which would make this a coarse busy-loop-only check — a
/// future Linux runner must not read a 1 s-granularity false-green as a
/// pass; tighten the sampling (e.g. `/proc/<pid>/stat` jiffies) there.
#[test]
#[ignore = "idle harness: run via `make perf` (release, local-only)"]
fn idle_campd_cpu_delta_zero_and_rss_under_20mb() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let campd = Daemon::spawn(&root, &[]);
    let pid = campd.pid();

    let (cpu0, _rss0) = ps_cputime_rss(pid);
    std::thread::sleep(Duration::from_secs(30));
    let (cpu1, rss1) = ps_cputime_rss(pid);

    let delta = cpu1.saturating_sub(cpu0);
    eprintln!("[daemon] idle 30s: cpu delta {delta:?}, rss {rss1} KB");
    assert!(
        delta <= Duration::from_millis(10),
        "idle CPU delta {delta:?} exceeds 10 ms (invariant 1: idle is free)"
    );
    assert!(rss1 < 20 * 1024, "idle RSS {rss1} KB exceeds 20 MB");
}

/// cp-0 / control-plane spec §4.3: the EXTENDED idle gate — M quiescent
/// workers with tailed stdout files and the read channel active, zero
/// activity => 0.0% CPU delta / <20 MB RSS (invariant 1). The notify
/// watcher on sessions/ is a live watcher for the whole idle window, AND
/// now M tailed files are open — this is the fleet-scale claim §4.3 makes.
/// N connected subscribers are deferred to phase 2 (the `session.subscribe`
/// verb does not exist in phase 0). The workers hold via FAKE_AGENT_HOLD_DIR
/// (poll the filesystem, not stdin — they outlive campd and stay alive,
/// keeping their stdout files open and their sessions registered for the
/// read channel to tail).
#[test]
#[ignore = "idle harness: run via `make perf` (release, local-only)"]
fn idle_campd_with_tailed_workers_zero_cpu_under_20mb() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 8, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);
    let pid = campd.pid();

    // Dispatch M=4 quiescent workers (each claims then holds; its stdout
    // file stays open and its session stays registered for the read channel).
    for i in 0..4 {
        let _bead = camp_ok(&root, &["sling", &format!("quiescent task {i}")])
            .trim()
            .to_owned();
    }
    wait_until(&root, "4 sessions dispatched", |e| {
        e.iter().filter(|ev| ev["type"] == "session.woke").count() == 4
    });

    // The measurement window: 30 s idle with 4 tailed files open.
    let (cpu0, _rss0) = ps_cputime_rss(pid);
    std::thread::sleep(Duration::from_secs(30));
    let (cpu1, rss1) = ps_cputime_rss(pid);

    let delta = cpu1.saturating_sub(cpu0);
    eprintln!("[daemon] idle 30s with 4 tailed workers: cpu delta {delta:?}, rss {rss1} KB");
    assert!(
        delta <= Duration::from_millis(10),
        "idle CPU delta {delta:?} exceeds 10 ms (invariant 1: idle is free with tailed workers)"
    );
    assert!(
        rss1 < 20 * 1024,
        "idle RSS {rss1} KB exceeds 20 MB with 4 tailed workers"
    );
}

/// Spec §14: sling → worker spawn ≤ 2 s. Measured wall-clock from issuing the
/// sling to observing the worker's dispatch (session.woke for the bead).
#[test]
#[ignore = "dispatch latency: run via `make perf` (release, local-only)"]
fn sling_to_worker_spawn_under_2s() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let _campd = Daemon::spawn(&root, &[]);

    let t0 = Instant::now();
    let bead = camp_ok(&root, &["sling", "add a --json flag"])
        .trim()
        .to_owned();
    let woke = wait_for_instant(&root, "worker spawn", |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str()
    });
    let elapsed = woke.duration_since(t0);
    eprintln!("[daemon] sling -> worker spawn: {elapsed:?}");
    assert!(
        elapsed <= Duration::from_secs(2),
        "sling->spawn {elapsed:?} (>2s)"
    );
}

/// Spec §14: step close → dependent dispatched ≤ 1 s. A is held mid-work; B
/// needs A. Releasing A's gate closes A (pass), which unblocks and dispatches
/// B. Measured wall-clock from observing A's close to observing B's dispatch.
#[test]
#[ignore = "dispatch latency: run via `make perf` (release, local-only)"]
fn close_to_dependent_dispatch_under_1s() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);

    let a = camp_ok(&root, &["sling", "A"]).trim().to_owned();
    wait_until(&root, "A claimed", |e| count(e, "bead.claimed") == 1);
    let b = camp_ok(&root, &["create", "B", "--needs", &a])
        .trim()
        .to_owned();

    // Release A: it closes pass, which unblocks and dispatches B.
    std::fs::write(hold.join(&a), "go").unwrap();
    let t_close = wait_for_instant(&root, "A closed", |e| {
        e["type"] == "bead.closed" && e["bead"] == a.as_str()
    });
    let t_woke = wait_for_instant(&root, "B dispatched", |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == b.as_str()
    });
    let elapsed = t_woke.duration_since(t_close);
    eprintln!("[daemon] close {a} -> dispatch {b}: {elapsed:?}");
    assert!(
        elapsed <= Duration::from_secs(1),
        "close->dispatch {elapsed:?} (>1s)"
    );

    // Let B finish so Drop is clean.
    std::fs::write(hold.join(&b), "go").unwrap();
}

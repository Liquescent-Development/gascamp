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
             [dispatch]\nmax_workers = {max_workers}\ncommand = \"{}\"\ndefault_agent = \"dev\"\n\n\
             [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    // perf measures the spec §14 dispatch floor; `git worktree add` in the
    // measured path would change what the numbers mean. Pin the live-tree
    // opt-out — post-flip, perf measures the opt-out path (recorded as a
    // non-blocking note in the Phase 2 plan).
    write_agent(&root, "dev", "isolation = \"none\"\n");
    // create the ledger so every verb (and campd) finds it
    camp_ok(&root, &["events", "--json"]);
    (root, rig)
}

/// A DIRECTORY agent (compat §5.1): identity is the directory NAME, and
/// model/tools/permission are operator-owned.
///
/// **This was `agents/<name>.md` with YAML front-matter until compat-1 (#94) moved
/// agent resolution to the directory form.** `make perf` is LOCAL-ONLY and
/// `#[ignore]`d, so CI never compiled the mismatch into a failure — the whole perf
/// suite has simply been RED since that merge, with `camp: unknown agent "dev"`.
/// That is precisely the risk AGENTS.md names when it says the perf suite must be
/// run before merging a perf-relevant PR.
fn write_agent(root: &Path, name: &str, agent_toml: &str) {
    let agent = root.join("agents").join(name);
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("agent.toml"), agent_toml).unwrap();
    std::fs::write(agent.join("prompt.md"), "Do the work.\n").unwrap();
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

/// cp-0 + cp-1 / control-plane spec §4.3: the EXTENDED idle gate — M quiescent
/// workers with tailed stdout files, the read channel active, AND **N CONNECTED
/// SUBSCRIBERS**, all idle => 0.0% CPU delta / <20 MB RSS (invariant 1).
///
/// cp-0 built the M-workers half and deferred the N-subscribers half to the phase
/// that builds `subscribe`. This is that phase, and this is that half.
///
/// **WHAT THIS MEASURES, AND WHAT IT DOES NOT.** It measures the WAKEUP PROFILE —
/// the property §4.3 actually asks for: a subscription must cost ZERO WAKEUPS when
/// its session is quiet. campd sleeps on the read-channel self-pipe; a quiet worker
/// writes nothing, so no notify fires, no `pump` runs, and `poll_timeout` returns
/// None. RED on CPU here means something in the subscriber path wakes campd with
/// nothing to do — a REAL invariant-1 bug, to be FIXED, never accommodated.
///
/// It does NOT measure the MEMORY CEILING: these subscribers' buffers are EMPTY.
/// The loaded worst case is `MAX_SUBSCRIBERS * (out + partial)` ~= 16 MiB on top of
/// idle RSS, which can approach the spec's <20 MB figure — so **<20 MB is an IDLE
/// bound**, and this test is what it is measured against. That is stated plainly in
/// the PR body rather than implied away.
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

    // cp-1: N = 4 CONNECTED SUBSCRIBERS, held open across the whole idle window.
    // They join at the TAIL (cursor: null), so there is nothing to stream and every
    // buffer stays empty — which is exactly the idle profile §4.3 asks about.
    let sessions: Vec<String> = events_json(&root)
        .into_iter()
        .filter(|e| e["type"] == "session.woke")
        .map(|e| e["data"]["name"].as_str().unwrap().to_owned())
        .collect();
    let mut subs: Vec<std::os::unix::net::UnixStream> = Vec::new();
    for session in sessions.iter().take(4) {
        let mut s = std::os::unix::net::UnixStream::connect(root.join("campd.sock")).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let req =
            format!("{{\"op\":\"session.subscribe\",\"session\":\"{session}\",\"cursor\":null}}\n");
        std::io::Write::write_all(&mut s, req.as_bytes()).unwrap();
        let mut hello = String::new();
        BufReader::new(s.try_clone().unwrap())
            .read_line(&mut hello)
            .unwrap();
        assert!(
            hello.contains("\"ok\":true"),
            "the subscribe hello must succeed: {hello:?}"
        );
        // §4.4: timeout-exempt after the hello.
        s.set_read_timeout(None).unwrap();
        subs.push(s);
    }
    assert_eq!(subs.len(), 4, "N=4 subscribers held open");

    // The measurement window: 30 s idle with 4 tailed files AND 4 subscribers.
    let (cpu0, _rss0) = ps_cputime_rss(pid);
    std::thread::sleep(Duration::from_secs(30));
    let (cpu1, rss1) = ps_cputime_rss(pid);

    let delta = cpu1.saturating_sub(cpu0);
    eprintln!(
        "[daemon] idle 30s with 4 tailed workers + 4 subscribers: cpu delta {delta:?}, rss {rss1} KB"
    );
    assert!(
        delta <= Duration::from_millis(10),
        "idle CPU delta {delta:?} exceeds 10 ms — a SUBSCRIPTION IS WAKING CAMPD WITH \
         NOTHING TO DO (invariant 1, §4.3). Fix it; never accommodate it."
    );
    assert!(
        rss1 < 20 * 1024,
        "idle RSS {rss1} KB exceeds 20 MB with 4 tailed workers + 4 subscribers"
    );
    drop(subs);
}

/// §4.3 (cp-2): a FLEET subscriber on quiescent workers costs ZERO wakeups. The
/// model does not change while workers are silent, so no diff, no frame, no
/// write — the same idle property session.subscribe proved, for the aggregate.
///
/// Same construction as the tailed-workers arm, but the N held-open connections
/// are `fleet.subscribe`: each reads its hello + snapshot to `synced`, then STOPS
/// reading and holds idle across the same 30 s window. RED on CPU here means the
/// fleet fanout wakes campd with an unchanged model — a real invariant-1 bug.
#[test]
#[ignore = "idle harness: run via `make perf` (release, local-only)"]
fn idle_campd_with_fleet_subscribers_zero_cpu_under_20mb() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 8, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);
    let pid = campd.pid();

    // Dispatch M=4 quiescent workers (each claims then holds; its stdout file
    // stays open and its session stays registered for the read channel).
    for i in 0..4 {
        let _bead = camp_ok(&root, &["sling", &format!("quiescent task {i}")])
            .trim()
            .to_owned();
    }
    wait_until(&root, "4 sessions dispatched", |e| {
        e.iter().filter(|ev| ev["type"] == "session.woke").count() == 4
    });

    // K = 4 FLEET subscribers, held open across the whole idle window. Each reads
    // its hello + full snapshot to `synced`, then stops reading — the quiescent
    // profile §4.3 asks about (an unchanged model produces no further frames).
    let mut subs: Vec<std::os::unix::net::UnixStream> = Vec::new();
    for _ in 0..4 {
        let mut s = std::os::unix::net::UnixStream::connect(root.join("campd.sock")).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        std::io::Write::write_all(&mut s, b"{\"op\":\"fleet.subscribe\"}\n").unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        assert!(
            hello.contains("\"ok\":true"),
            "the fleet.subscribe hello must succeed: {hello:?}"
        );
        // Read frames until the snapshot terminator `synced`, then STOP reading.
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.contains("\"frame\":\"synced\"") {
                break;
            }
        }
        // §4.4: timeout-exempt after the hello + snapshot.
        s.set_read_timeout(None).unwrap();
        subs.push(s);
    }
    assert_eq!(subs.len(), 4, "K=4 fleet subscribers held open");

    // The measurement window: 30 s idle with 4 tailed workers AND 4 fleet subs.
    let (cpu0, _rss0) = ps_cputime_rss(pid);
    std::thread::sleep(Duration::from_secs(30));
    let (cpu1, rss1) = ps_cputime_rss(pid);

    let delta = cpu1.saturating_sub(cpu0);
    eprintln!(
        "[daemon] idle 30s with 4 tailed workers + 4 fleet subscribers: cpu delta {delta:?}, rss {rss1} KB"
    );
    assert!(
        delta <= Duration::from_millis(10),
        "idle CPU delta {delta:?} exceeds 10 ms — a FLEET SUBSCRIPTION IS WAKING CAMPD \
         WITH AN UNCHANGED MODEL (invariant 1, §4.3). Fix it; never accommodate it."
    );
    assert!(
        rss1 < 20 * 1024,
        "idle RSS {rss1} KB exceeds 20 MB with 4 tailed workers + 4 fleet subscribers"
    );
    drop(subs);
}

/// cp-1 (G1/G2) — **THE LOADED ARM**, and it is the one that matters.
///
/// The idle gate is CONSTRUCTED to be blind to the two worst bugs this phase can
/// ship: a spin requires a NON-EMPTY buffer, and a livelock requires a LONG LINE.
/// The idle arm holds subscribers with EMPTY buffers on QUIESCENT sessions — it can
/// observe NEITHER. A gate that cannot see the failure mode its own phase
/// introduces is not a gate.
///
/// So: N subscribers, one session ACTIVELY STREAMING (including a line far larger
/// than one chunk), each subscriber READING normally. Assert:
///   (a) campd's CPU over the streaming window is BOUNDED — well under a busy
///       loop's 100%. A `poll(0) -> pump -> WouldBlock -> poll(0)` spin pegs a core
///       for the whole stream, because macOS's ~8 KiB socket buffer means every
///       healthy subscriber WouldBlocks on essentially every chunk.
///   (b) the stream COMPLETES within a hard deadline. A livelock hangs forever on
///       the first line > 64 KiB, so the deadline is what turns "the suite hangs"
///       into "the gate fails".
///
/// ⚠ **AND SAY THIS PLAINLY: `make perf` is `#[ignore]`d and LOCAL-ONLY, so NO CI
/// GATE CATCHES A SPIN.** This arm is a real gate only as far as an operator runs
/// it before merging a perf-relevant PR. The CPU-bounded property of `pump` is
/// therefore defended primarily by the UNIT tests
/// (`poll_timeout_never_arms_on_a_wouldblock_alone`,
/// `pump_lexes_a_line_that_spans_many_chunks`), which DO run in CI. This arm is the
/// belt, not the braces — do not let its existence imply a protection CI does not
/// provide.
#[test]
#[ignore = "loaded subscriber harness: run via `make perf` (release, local-only)"]
fn loaded_subscribers_do_not_spin_or_livelock_campd() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    // ONE huge line (2 MiB — bigger than a chunk AND bigger than the cap) followed
    // by an ordinary line: the ordinary case on any session that reads a file.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HUGE_LINE", "2097152"),
            ("FAKE_AGENT_HUGE_LINE_LINGER", "120"),
        ],
    );
    let pid = campd.pid();
    camp_ok(&root, &["sling", "stream a monster"]);
    wait_until(&root, "session.woke", |e| {
        e.iter().any(|ev| ev["type"] == "session.woke")
    });
    let session = events_json(&root)
        .into_iter()
        .find(|e| e["type"] == "session.woke")
        .unwrap()["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned();

    // N = 4 subscribers, all at cursor 0, all READING.
    let mut readers = Vec::new();
    for _ in 0..4 {
        let mut s = std::os::unix::net::UnixStream::connect(root.join("campd.sock")).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let req =
            format!("{{\"op\":\"session.subscribe\",\"session\":\"{session}\",\"cursor\":0}}\n");
        std::io::Write::write_all(&mut s, req.as_bytes()).unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        assert!(hello.contains("\"ok\":true"), "hello: {hello:?}");
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        readers.push((s, reader));
    }

    let (cpu0, _rss0) = ps_cputime_rss(pid);
    let started = Instant::now();

    // A POKER: campd only pumps on a WAKE, and the stream watch is latency-only
    // (§2.3 — on macOS it does not fire for a worker's appends through its
    // inherited stdout fd). Without a wake source this measures nothing. The poker
    // is deliberately CHEAP and its cost is inside the CPU budget we assert.
    let poke_sock = root.join("campd.sock");
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poker_done = done.clone();
    let poker = std::thread::spawn(move || {
        while !poker_done.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(mut p) = std::os::unix::net::UnixStream::connect(&poke_sock) {
                let _ = std::io::Write::write_all(&mut p, b"{\"op\":\"poke\",\"seq\":1}\n");
                let mut line = String::new();
                let _ = BufReader::new(p).read_line(&mut line);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });

    // Every subscriber reads until it sees the line AFTER the monster — i.e. the
    // whole stream was delivered and the cursor advanced past a line campd refused
    // to buffer.
    let handles: Vec<_> = readers
        .into_iter()
        .map(|(s, mut reader)| {
            std::thread::spawn(move || {
                let _keep_fd_open = s;
                let deadline = Instant::now() + Duration::from_secs(90);
                let mut saw_skipped = false;
                loop {
                    assert!(
                        Instant::now() < deadline,
                        "campd LIVELOCKED on a line bigger than one chunk — the stream \
                         never completed"
                    );
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => panic!("the subscription closed before the stream finished"),
                        Ok(_) => {
                            let v: serde_json::Value =
                                serde_json::from_str(line.trim_end()).unwrap();
                            if v["frame"] == "skipped" && v["reason"] == "over_cap" {
                                saw_skipped = true;
                            }
                            if v["frame"] == "event"
                                && v["event"]["message"]["content"] == "after the monster"
                            {
                                assert!(saw_skipped, "the monster must be SKIPPED, loudly");
                                return;
                            }
                        }
                        Err(e) => panic!("read failed: {e}"),
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("every subscriber must complete");
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    poker.join().unwrap();

    let elapsed = started.elapsed();
    let (cpu1, rss1) = ps_cputime_rss(pid);
    let delta = cpu1.saturating_sub(cpu0);
    eprintln!(
        "[daemon] loaded: 4 subscribers x 2 MiB monster in {elapsed:?}; cpu delta {delta:?}, rss {rss1} KB"
    );

    // (a) BOUNDED CPU. A spin pegs a core for the whole window; anything near
    // `elapsed` means campd was busy-looping rather than sleeping.
    //
    // THE BAR IS 30%, not 50%. At 50% it sat only slightly above the measured value
    // (90 ms / 205 ms = 44%) — close enough that a modest regression could slip
    // under it, or a modest slowdown could trip it. Fixing the O(n²) drain (B1) took
    // the measurement to ~20 ms / 135 ms = 15%, which is what makes a tighter bar
    // honest: it now has real headroom AND real teeth.
    assert!(
        delta < elapsed.mul_f64(0.30),
        "campd burned {delta:?} of CPU over a {elapsed:?} window (>30% of a core) — \
         that is a SPIN (poll(0) -> pump -> WouldBlock -> poll(0)), not a sleep \
         (invariant 1, §4.3). Measured healthy: ~15%"
    );
    // (b) and its memory stayed bounded while doing it. NOTE the bound is 64 MB,
    // NOT §14's 20 MB: that figure is an IDLE bound (see the idle gate above), and a
    // campd with saturated subscriber buffers is outside it BY DESIGN. Measured
    // loaded: ~24-27 MB.
    assert!(
        rss1 < 64 * 1024,
        "loaded RSS {rss1} KB — the per-subscriber buffers are not bounded"
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

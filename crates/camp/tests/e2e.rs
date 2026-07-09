#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 15 opt-in E2E suite with REAL `claude` (spec §16 e2e bullet, §14
//! numbers). LOCAL-ONLY and OPERATOR-GATED: the measured test `e2e_full` is
//! #[ignore]d and additionally requires `CAMP_E2E=1` + an authenticated
//! `claude` on PATH; it spends real Anthropic API money and runs only via
//! `make e2e` after operator authorization. CI compiles this file and runs
//! ONLY the non-ignored fixture/parser tests below.
//!
//! This suite is the canary for the F1–F7 fixture facts
//! (docs/design/2026-07-06-assumption-findings.md) against installed claude
//! 2.1.205 (pinned at 2.1.201; drift expected). A divergence STOPS the run
//! and updates the findings doc + spec in the same PR.
//!
//! Test-side waiting polls the ledger — sanctioned for harnesses only; campd
//! itself never polls.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";
/// Real claude workers do real model work — waits are generous vs the
/// fake-agent harnesses. Bounds a runaway (Drop then kills the group).
const REAL_CLAUDE_TIMEOUT: Duration = Duration::from_secs(300);

// ---- pure helpers (exercised by the non-ignored tests below) ---------------

/// Every non-ASCII-alphanumeric CHARACTER → one '-'. Mirror of
/// `crates/camp/src/daemon/spawn.rs::munge` (F3). Recomputed here (the camp
/// binary exposes no lib) so the F3 assertion is an independent cross-check.
fn munge(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The last JSONL line whose `type == "result"` (F2 result envelope). In
/// stream-json output mode the capture file is JSONL; the result event is
/// last. Returns None if absent (worker never produced a result).
fn find_result_event(capture: &str) -> Option<serde_json::Value> {
    capture
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .rfind(|v| v["type"] == "result")
}

/// True once the worker's transcript file exists and holds at least one
/// non-empty line — the session has begun emitting (spec §14 "first token":
/// dispatch/startup latency, transcript-schema-agnostic).
fn transcript_has_output(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => s.lines().any(|l| !l.trim().is_empty()),
        Err(_) => false,
    }
}

/// Parse a `ps -o cputime` field into a Duration. macOS `MM:SS.ss`/`HH:MM:SS`;
/// Linux `[[DD-]HH:]MM:SS`. (Verbatim from perf_daemon.rs.)
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

/// Sample a process's accumulated CPU time and RSS (KB) via `ps`.
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
        .unwrap_or_else(|| panic!("no cputime in ps {line:?}"));
    let rss = it.next().unwrap_or_else(|| panic!("no rss in ps {line:?}"));
    (parse_cputime(cpu), parse_rss_kb(rss))
}

#[test]
fn munge_matches_spawn_scheme() {
    assert_eq!(munge("/Users/kie/x-1"), "-Users-kie-x-1");
    assert_eq!(munge("abc123"), "abc123");
    assert_eq!(munge("a.b/c"), "a-b-c");
}

#[test]
fn find_result_event_picks_the_result_line() {
    let capture = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\"}\n",
        "{\"type\":\"assistant\",\"message\":{}}\n",
        "{\"type\":\"result\",\"is_error\":false,\"session_id\":\"abc\",\"total_cost_usd\":0.01,\"ttft_ms\":842,\"num_turns\":3}\n"
    );
    let r = find_result_event(capture).unwrap();
    assert_eq!(r["is_error"], false);
    assert_eq!(r["session_id"], "abc");
    assert_eq!(r["total_cost_usd"], 0.01);
    assert_eq!(r["ttft_ms"], 842);
    assert!(find_result_event("{\"type\":\"assistant\"}\n").is_none());
}

#[test]
fn toy_project_fixture_is_present_and_shaped() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/toy-project");
    let toy = std::fs::read_to_string(format!("{root}/toy")).unwrap();
    assert!(toy.contains("def cmd_ls"), "toy must have an ls command");
    let tests = std::fs::read_to_string(format!("{root}/test_toy.py")).unwrap();
    assert!(
        tests.contains("test_ls_lists_items_one_per_line"),
        "test suite must exist for the worker to extend"
    );
    let verify = std::fs::read_to_string(format!("{root}/scripts/verify.sh")).unwrap();
    assert!(verify.contains("unittest"), "verify.sh must run the suite");
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

// ---- ledger + process helpers (used by the #[ignore]d e2e_full) ------------

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

fn seq_of(events: &[serde_json::Value], pred: impl Fn(&serde_json::Value) -> bool) -> i64 {
    events
        .iter()
        .find(|e| pred(e))
        .unwrap_or_else(|| panic!("event not found in {events:#?}"))["seq"]
        .as_i64()
        .unwrap()
}

/// Poll the ledger until `pred` holds or `REAL_CLAUDE_TIMEOUT` elapses.
fn wait_until(root: &Path, what: &str, pred: impl Fn(&[serde_json::Value]) -> bool) {
    let deadline = Instant::now() + REAL_CLAUDE_TIMEOUT;
    loop {
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

/// Tight-poll (5 ms) and return the Instant a predicate first holds — bounds
/// the latency-measurement error. Call it BEFORE the awaited event can occur.
fn wait_for_instant(root: &Path, what: &str, pred: impl Fn(&serde_json::Value) -> bool) -> Instant {
    let deadline = Instant::now() + REAL_CLAUDE_TIMEOUT;
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

/// Fail-loud env gate (no silent skip). All four are required for a real run.
fn require_e2e_env() -> String {
    assert_eq!(
        std::env::var("CAMP_E2E").as_deref(),
        Ok("1"),
        "e2e is opt-in: set CAMP_E2E=1 (use `make e2e`) with an authenticated claude"
    );
    for tool in ["python3", "git"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "e2e requires `{tool}` on PATH");
    }
    resolve_claude()
}

/// Absolute path to the real claude binary (fail fast if absent). Using an
/// absolute path pins the worker binary regardless of PATH edits.
fn resolve_claude() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v claude"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "e2e requires an authenticated `claude` on PATH"
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Recursively copy `src` into `dst` (preserving exec bits) via `cp -R`.
fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let ok = Command::new("cp")
        .arg("-R")
        .arg(src)
        .arg(dst)
        .status()
        .unwrap()
        .success();
    assert!(ok, "cp -R {} {} failed", src.display(), dst.display());
}

/// `git init` + one commit, so the reviewer step has a real diff baseline.
fn git_init_commit(rig: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.email=e2e@camp",
            "-c",
            "user.name=e2e",
            "commit",
            "-qm",
            "toy baseline",
        ],
    ] {
        let ok = Command::new("git")
            .current_dir(rig)
            .args(&args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed in {}", rig.display());
    }
}

/// A camp with TWO rigs (fresh copies of the toy-project — scenario 1 uses
/// `toy`, scenario 2 uses `toy2` so it is not polluted by scenario 1's change),
/// the real claude as the worker binary, a non-interactive `dev` worker + a
/// read-only `reviewer`, and the e2e-guarded formula. Returns (root, rig1, rig2).
fn scaffold_e2e(dir: &Path, claude: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();

    let fixture = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/toy-project"
    ));
    let rig1 = dir.join("rig1");
    let rig2 = dir.join("rig2");
    copy_dir(&fixture, &rig1);
    copy_dir(&fixture, &rig2);
    git_init_commit(&rig1);
    git_init_commit(&rig2);

    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"e2e\"\n\n\
             [[rigs]]\nname = \"toy\"\npath = \"{}\"\nprefix = \"toy\"\n\n\
             [[rigs]]\nname = \"toy2\"\npath = \"{}\"\nprefix = \"toy2\"\n\n\
             [dispatch]\nmax_workers = 2\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig1.display(),
            rig2.display(),
            claude,
        ),
    )
    .unwrap();

    // Non-interactive worker pinning (F7): explicit model, permission mode,
    // and tool allowlist so worker capability does not silently vary. The
    // agent BODY becomes --append-system-prompt; the task arrives via the
    // held stdin (the WORKER_CONTRACT).
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    // Phase 2 note: e2e Tier-0 asserts the work lands in the RIG's live
    // tree (`toy ls --json` run in the rig) — that is delivery, which is
    // Phase 3's contract. Until Phase 3 defines "landed" for the worktree
    // path, e2e pins the explicit live-tree opt-out (spec §12).
    std::fs::write(
        agents.join("dev.md"),
        "---\nname: dev\nmodel: sonnet\npermissionMode: bypassPermissions\n\
         isolation: none\n\
         tools: Read, Edit, Write, Bash, Grep, Glob\n---\n\
         You are a camp worker. Do the assigned bead with TDD: write the failing \
         test first, then the minimal change to pass it, keeping existing behavior \
         intact. Run the project's tests to confirm green before you close. \
         Fail fast; do not linger after closing.\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\nmodel: sonnet\npermissionMode: bypassPermissions\n\
         isolation: none\n\
         tools: Read, Bash, Grep, Glob\n---\n\
         You are a read-only camp reviewer. Inspect the change (e.g. `git diff`) for \
         the assigned bead, confirm it is sound and existing behavior is intact, then \
         close pass with a one-line reason (or fail with the reason). Do not edit files.\n",
    )
    .unwrap();

    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/formulas/e2e-guarded.toml"
        ),
        root.join("formulas").join("e2e-guarded.toml"),
    )
    .unwrap();

    camp_ok(&root, &["events", "--json"]); // create the ledger
    (root, rig1, rig2)
}

/// campd as a real child in its OWN process group (`process_group(0)`), so the
/// worker processes it spawns (which by design outlive campd — adoption, §8.5)
/// can all be reaped on teardown. Drop kills the whole group — this bounds
/// real-API spend if a worker runs away or a wait times out.
struct Daemon {
    child: Child,
    /// campd's pid == its process-group id (it is the group leader).
    pgid: u32,
}

impl Daemon {
    fn spawn(root: &Path, envs: &[(&str, &str)]) -> Daemon {
        // Put the built `camp` binary's dir first on PATH so the worker's
        // `camp claim/show/event emit/close` (per the WORKER_CONTRACT) resolve
        // to BIN. campd sets no PATH on workers; they inherit campd's.
        let bin_dir = Path::new(BIN)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let existing = std::env::var("PATH").unwrap_or_default();
        let path = format!("{bin_dir}:{existing}");

        let mut cmd = Command::new(BIN);
        cmd.env_remove("CAMP_DIR")
            .env("CAMP_BIN", BIN)
            .env("PATH", &path)
            .args(["daemon", "--camp"])
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0); // campd becomes its own group leader
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let pgid = child.id();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon { child, pgid }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Reap campd AND every worker in its process group (real claude
        // workers outlive campd by design). pkill -g targets the group; then
        // reap campd itself. This is the cost fuse for the paid e2e run.
        let _ = Command::new("pkill")
            .args(["-KILL", "-g", &self.pgid.to_string()])
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- scenarios -------------------------------------------------------------

/// Scenario 1 (spec §16 / §14): `camp sling "add a --json flag to toy ls, TDD
/// it" --rig toy` -> a real worker claims, works, closes pass. Asserts sling ->
/// first transcript output <= 2 s; the Tier-0 envelope is ~3 bead-lifecycle
/// writes (created/claimed/closed) + bounded milestones; `camp show` tells the
/// whole story; the --json FEATURE really works and the suite is green; and the
/// F1/F2/F3/F4 fixture facts hold at the installed claude version.
fn run_tier0(root: &Path, rig: &Path) {
    let t0 = Instant::now();
    let bead = camp_ok(
        root,
        &[
            "sling",
            "add a --json flag to toy ls, TDD it",
            "--rig",
            "toy",
        ],
    )
    .trim()
    .to_owned();
    eprintln!("[tier0] slung bead {bead}");

    // Dispatch: registry-at-birth session.woke carries the pre-assigned sid
    // (F1) and the transcript path (F3).
    wait_until(root, "worker dispatch", |e| {
        e.iter()
            .any(|x| x["type"] == "session.woke" && x["data"]["bead"] == bead.as_str())
    });
    let events = events_json(root);
    let woke = events
        .iter()
        .find(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str())
        .unwrap()["data"]
        .clone();
    let sid = woke["claude_session_id"].as_str().unwrap().to_owned();
    let session_name = woke["name"].as_str().unwrap().to_owned();
    let transcript = PathBuf::from(woke["transcript_path"].as_str().unwrap());

    // §14: sling -> worker's first token (first transcript output) <= 2 s.
    let deadline = Instant::now() + REAL_CLAUDE_TIMEOUT;
    while !transcript_has_output(&transcript) {
        assert!(Instant::now() < deadline, "no transcript output for {bead}");
        std::thread::sleep(Duration::from_millis(5));
    }
    let first_token = t0.elapsed();
    eprintln!("[tier0] sling -> first transcript output: {first_token:?}");
    assert!(
        first_token <= Duration::from_secs(2),
        "sling->first token {first_token:?} exceeds 2 s (§14)"
    );

    // Worker runs to a pass close.
    wait_until(root, "bead closed", |e| {
        e.iter()
            .any(|x| x["type"] == "bead.closed" && x["bead"] == bead.as_str())
    });
    wait_until(root, "session stopped", |e| {
        e.iter()
            .any(|x| x["type"] == "session.stopped" && x["data"]["name"] == session_name.as_str())
    });
    let events = events_json(root);

    // ~3 ledger writes for the Tier-0 envelope: created + claimed + closed,
    // plus a bounded milestone trail; NO formula/run machinery.
    let for_bead = |kind: &str| {
        events
            .iter()
            .filter(|e| e["type"] == kind && e["bead"] == bead.as_str())
            .count()
    };
    let created = for_bead("bead.created");
    let claimed = for_bead("bead.claimed");
    let closed = for_bead("bead.closed");
    let milestones = for_bead("worker.milestone");
    eprintln!(
        "[tier0] envelope writes: created={created} claimed={claimed} closed={closed} milestones={milestones}"
    );
    assert_eq!(created, 1, "exactly one bead created (Tier-0)");
    assert_eq!(claimed, 1, "the bead was claimed once");
    assert_eq!(closed, 1, "the bead was closed once");
    assert!(
        milestones <= 8,
        "milestone trail should be a bounded heartbeat, got {milestones}"
    );
    assert_eq!(count(&events, "run.cooked"), 0, "Tier-0 has no formula/run");
    assert_eq!(
        count(&events, "check.passed") + count(&events, "check.failed"),
        0
    );

    // Closed pass.
    let close = events
        .iter()
        .find(|e| e["type"] == "bead.closed" && e["bead"] == bead.as_str())
        .unwrap();
    assert_eq!(
        close["data"]["outcome"], "pass",
        "Tier-0 worker must close pass"
    );

    // F4: clean exit (0) maps to session.stopped with exit_code 0.
    let stopped = events
        .iter()
        .find(|e| e["type"] == "session.stopped" && e["data"]["name"] == session_name.as_str())
        .unwrap();
    assert_eq!(
        stopped["data"]["exit_code"], 0,
        "F4: clean exit -> session.stopped exit 0"
    );

    // F1 + F3: the transcript is at the campd-computed munged path under the
    // real claude root, named by the pre-assigned sid. campd canonicalizes the
    // worker cwd (matching claude's realpath), so compute `expected` from the
    // CANONICAL rig — otherwise the macOS /var -> /private/var symlink makes the
    // raw-path expected diverge from where claude actually writes.
    assert_eq!(sid.len(), 36, "F1: claude_session_id is a uuid");
    let claude_root = std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".claude"));
    let canon_rig = std::fs::canonicalize(rig).unwrap();
    let expected = claude_root
        .join("projects")
        .join(munge(&canon_rig.to_string_lossy()))
        .join(format!("{sid}.jsonl"));
    assert_eq!(
        transcript, expected,
        "F3: transcript path munge/canonicalization drifted"
    );
    assert!(
        transcript.exists(),
        "F1/F3: transcript file must exist at the munged path"
    );

    // F2: the stream-json capture holds a result envelope; assert the
    // load-bearing keys are PRESENT (absence == drift -> record in findings)
    // and record cost + ttft.
    let capture = std::fs::read_to_string(
        root.join("sessions")
            .join(format!("{}.json", munge(&session_name))),
    )
    .unwrap();
    let result = find_result_event(&capture).expect("F2: capture must hold a result event");
    assert_eq!(
        result["is_error"], false,
        "F2: worker result is_error must be false"
    );
    assert_eq!(
        result["session_id"],
        sid.as_str(),
        "F2: result session_id must echo F1's id"
    );
    for key in ["total_cost_usd", "ttft_ms", "num_turns"] {
        assert!(
            result.get(key).is_some_and(|v| !v.is_null()),
            "F2 canary: result envelope missing `{key}` at this claude version — record the drift in the findings doc"
        );
    }
    eprintln!(
        "[tier0] result: total_cost_usd={} ttft_ms={} num_turns={}",
        result["total_cost_usd"], result["ttft_ms"], result["num_turns"]
    );

    // `camp show` tells the whole story.
    let show = camp_ok(root, &["show", &bead]);
    assert!(
        show.contains("outcome  pass"),
        "camp show must show the pass outcome:\n{show}"
    );
    assert!(
        show.contains("--json"),
        "camp show must carry the task title:\n{show}"
    );
    assert!(
        show.contains("bead.claimed"),
        "camp show history must include the claim"
    );
    assert!(
        show.contains("bead.closed"),
        "camp show history must include the close"
    );

    // The work is REAL: verify the FEATURE (not source text) and the suite.
    let json_out = Command::new("python3")
        .arg(rig.join("toy"))
        .args(["ls", "--json"])
        .current_dir(rig)
        .output()
        .unwrap();
    assert!(
        json_out.status.success(),
        "`toy ls --json` must exit 0:\n{}",
        String::from_utf8_lossy(&json_out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap_or_else(|e| {
        panic!(
            "`toy ls --json` stdout must be valid JSON ({e}): {:?}",
            String::from_utf8_lossy(&json_out.stdout)
        )
    });
    assert!(
        parsed.is_array() || parsed.is_object(),
        "toy ls --json should emit a JSON array/object, got {parsed}"
    );
    let suite = Command::new("python3")
        .args(["-m", "unittest", "discover", "-s", ".", "-p", "test_*.py"])
        .current_dir(rig)
        .output()
        .unwrap();
    assert!(
        suite.status.success(),
        "toy suite must be green after the flag-add:\n{}",
        String::from_utf8_lossy(&suite.stderr)
    );
    eprintln!(
        "[tier0] DONE: --json feature works, suite green, story visible ('hours for a flag' is dead)"
    );
}

/// Scenario 2 (spec §16 / §14): one guarded-change formula run with a real
/// verification script, against a SECOND fresh rig (`toy2`). A real worker
/// implements the `--count` change on the `implement` step; campd's check runs
/// `scripts/verify.sh` (the toy suite); on pass the anchor closes and the
/// `review` step dispatches. Measures step close -> dependent dispatch <= 1 s
/// LIVE (5 ms poll, started before the close), and asserts the check passed and
/// the run finalized pass.
fn run_formula(root: &Path) {
    let out = camp_ok(
        root,
        &["sling", "--formula", "e2e-guarded", "--rig", "toy2"],
    );
    let mut w = out.split_whitespace();
    let run_id = w.next().unwrap().to_owned();
    assert_eq!(w.next(), Some("root"));
    let root_bead = w.next().unwrap().to_owned();
    eprintln!("[formula] cooked run {run_id} root {root_bead}");

    // run.cooked is committed synchronously by the sling; read the anchor beads.
    wait_until(root, "run cooked", |e| {
        e.iter()
            .any(|x| x["type"] == "run.cooked" && x["data"]["run_id"] == run_id.as_str())
    });
    let cooked = events_json(root)
        .into_iter()
        .find(|e| e["type"] == "run.cooked" && e["data"]["run_id"] == run_id.as_str())
        .unwrap();
    let implement = cooked["data"]["steps"]["implement"]
        .as_str()
        .unwrap()
        .to_owned();
    let review = cooked["data"]["steps"]["review"]
        .as_str()
        .unwrap()
        .to_owned();

    // §14 WALL-CLOCK, measured LIVE (poll begins now, before the implement
    // worker finishes): the instant the implement anchor closes vs the instant
    // review dispatches.
    let t_close = wait_for_instant(root, "implement closed", |e| {
        e["type"] == "bead.closed" && e["bead"] == implement.as_str()
    });
    let t_woke = wait_for_instant(root, "review dispatched", |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == review.as_str()
    });
    let gap = t_woke.duration_since(t_close);
    eprintln!("[formula] step close -> dependent dispatch: {gap:?}");
    assert!(
        gap <= Duration::from_secs(1),
        "close->dispatch {gap:?} exceeds 1 s (§14)"
    );

    // Run to finalization (bounded by REAL_CLAUDE_TIMEOUT per wait).
    wait_until(root, "the run to finalize", |e| {
        e.iter()
            .any(|x| x["type"] == "run.finalized" && x["data"]["root"] == root_bead.as_str())
    });
    let events = events_json(root);

    // The real verification script ran and passed exactly once.
    let passed = count(&events, "check.passed");
    let failed = count(&events, "check.failed");
    eprintln!("[formula] check.passed={passed} check.failed={failed}");
    assert_eq!(
        passed, 1,
        "verify.sh must pass exactly once for the implement step"
    );

    // The implement anchor closed pass; the run finalized pass.
    let impl_close = events
        .iter()
        .find(|e| e["type"] == "bead.closed" && e["bead"] == implement.as_str())
        .expect("implement anchor must close");
    assert_eq!(
        impl_close["data"]["outcome"], "pass",
        "implement step must close pass"
    );
    let finalized = events
        .iter()
        .find(|e| e["type"] == "run.finalized" && e["data"]["root"] == root_bead.as_str())
        .unwrap();
    assert_eq!(
        finalized["data"]["outcome"], "pass",
        "the guarded-change run must finalize pass"
    );

    // §14 FUNCTIONAL invariant: review woke only AFTER implement closed.
    let close_seq = seq_of(&events, |e| {
        e["type"] == "bead.closed" && e["bead"] == implement.as_str()
    });
    let woke_seq = seq_of(&events, |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == review.as_str()
    });
    assert!(
        woke_seq > close_seq,
        "review must dispatch after implement closed (§7.3/§8.3)"
    );
    eprintln!(
        "[formula] DONE: real verify.sh passed, run finalized pass, dependent dispatched in order"
    );
}

/// Scenario 3 (spec §14 invariant 1, re-asserted with REAL artifacts on disk):
/// after both scenarios drain, the SAME campd blocks on poll. Over a 30 s idle
/// window its accumulated CPU does not move (<= 10 ms, macOS `ps` centisecond
/// grain) and RSS stays < 20 MB — proven with real transcripts under
/// ~/.claude/projects, real sessions/ captures, and real git rigs present.
fn assert_idle(campd_pid: u32, root: &Path) {
    // Drain: every session that woke must have stopped/crashed (no lingering
    // stream worker in release-grace).
    wait_until(root, "all sessions to drain", |e| {
        let woke: Vec<&str> = e
            .iter()
            .filter(|x| x["type"] == "session.woke")
            .filter_map(|x| x["data"]["name"].as_str())
            .collect();
        let done: Vec<&str> = e
            .iter()
            .filter(|x| x["type"] == "session.stopped" || x["type"] == "session.crashed")
            .filter_map(|x| x["data"]["name"].as_str())
            .collect();
        !woke.is_empty() && woke.iter().all(|n| done.contains(n))
    });

    let (cpu0, _rss0) = ps_cputime_rss(campd_pid);
    std::thread::sleep(Duration::from_secs(30));
    let (cpu1, rss1) = ps_cputime_rss(campd_pid);
    let delta = cpu1.saturating_sub(cpu0);
    eprintln!(
        "[idle] campd idle 30 s (real artifacts on disk): cpu delta {delta:?}, rss {rss1} KB"
    );
    assert!(
        delta <= Duration::from_millis(10),
        "idle CPU delta {delta:?} exceeds 10 ms (invariant 1: idle is free)"
    );
    assert!(rss1 < 20 * 1024, "idle RSS {rss1} KB exceeds 20 MB");
}

/// Post-teardown insurance (paid-suite): after Drop group-kills the tree, no
/// worker process for any session id may survive. `pgrep -f <sid>` matches the
/// worker's `--session-id <uuid>` argv — attributable to THIS run even if
/// claude `setsid`-detached out of the process group.
fn assert_no_orphans(sids: &[String]) {
    assert!(!sids.is_empty(), "expected at least one worker session id");
    let deadline = Instant::now() + Duration::from_secs(15);
    for sid in sids {
        loop {
            let found = Command::new("pgrep").arg("-f").arg(sid).output().unwrap();
            let pids: Vec<String> = String::from_utf8_lossy(&found.stdout)
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            if pids.is_empty() {
                break;
            }
            // The process-group kill missed this worker (claude can setsid-
            // detach out of campd's group). REAP it directly so the fuse
            // actually stops the spend, not merely reports it — then loop until
            // pgrep confirms it is gone.
            for pid in &pids {
                let _ = Command::new("kill").args(["-KILL", pid]).status();
            }
            eprintln!(
                "[e2e] WARNING: reaped detached worker(s) {pids:?} for session {sid} (escaped the process group)"
            );
            assert!(
                Instant::now() < deadline,
                "worker(s) for session {sid} survived teardown and could not be reaped within 15 s (orphaned spend risk)"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    eprintln!(
        "[e2e] teardown reaped all {} worker session(s); no orphans",
        sids.len()
    );
}

/// The whole spec §16 e2e bullet as ONE orchestrated, operator-gated run
/// against real claude: (1) Tier-0 flag-add, (2) guarded-change formula run,
/// (3) idle-daemon re-assertion with real transcripts on disk — reusing one
/// campd + two rigs so scenario 3 sees real artifacts. Prints every measured
/// number for the PR record. Run via `make e2e` (CAMP_E2E=1) ONLY after
/// operator authorization for API spend.
#[test]
#[ignore = "real-claude e2e: run via `make e2e` (CAMP_E2E=1) — OPERATOR-GATED API spend"]
fn e2e_full() {
    let claude = require_e2e_env();
    eprintln!("[e2e] claude binary: {claude}");
    let version = String::from_utf8(
        Command::new(&claude)
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    eprintln!("[e2e] {}", version.trim());

    let dir = tempfile::tempdir().unwrap();
    let (root, rig1, _rig2) = scaffold_e2e(dir.path(), &claude);
    let campd = Daemon::spawn(&root, &[]);

    run_tier0(&root, &rig1);
    run_formula(&root);
    assert_idle(campd.pid(), &root);

    // The ledger is a faithful fold of its event log even after a real run.
    let refold = camp_ok(&root, &["doctor", "--refold"]);
    assert!(refold.contains("0 drift rows"), "doctor --refold: {refold}");
    eprintln!("[e2e] all scenarios green; ledger refold clean");

    // Cost fuse verification: collect every worker session id, tear campd down
    // (Drop group-kills the tree), then assert none survived.
    let sids: Vec<String> = events_json(&root)
        .iter()
        .filter(|e| e["type"] == "session.woke")
        .filter_map(|e| e["data"]["claude_session_id"].as_str().map(String::from))
        .collect();
    drop(campd);
    assert_no_orphans(&sids);
}

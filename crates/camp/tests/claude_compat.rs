#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The $0 tier of the real-`claude` compatibility gate (control-plane spec
//! §8 "the $0 tier", phase 0). A `#!/bin/sh` fake worker ignores argv and can
//! never reject a flag the real CLI rejects — that is exactly how #86 shipped
//! green. This gate spawns the REAL `claude` and asserts the contract camp
//! does not control: the HeldStream argv is accepted, the `initialize`
//! handshake round-trips, and a pre-turn `interrupt` is acknowledged.
//!
//! It costs $0 and needs no auth: every assertion happens BEFORE any turn is
//! sent (argv validation and the control protocol are CLI-local — verified:
//! the `initialize` response carries `account.tokenSource:"none"`). The gate
//! runs under a FRESH throwaway `CLAUDE_CONFIG_DIR`, which (a) keeps it
//! hermetic and (b) makes `verbose` default to `false`, so the negative
//! control honestly proves `--verbose` is required rather than being masked
//! by an operator's `"verbose": true` setting (the #86 contamination).
//!
//! LOCAL-ONLY and OPERATOR-GATED: the measured test `claude_compat_zero_cost`
//! is `#[ignore]`d AND requires `CAMP_COMPAT=1` (run via `make compat`); CI
//! compiles this file and runs ONLY the non-ignored pure tests below.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// The pinned `claude` version (Task 2). A build without the pin file fails to
/// compile — the pin is not optional.
const PINNED_VERSION: &str = include_str!("../../../ci/claude-compat/CLAUDE_VERSION");

/// cp-1 (B14): CAMP'S OWN INTERRUPT BYTES. Task 1 pins `ParentMessage::Interrupt`
/// against THIS file (byte-equal), and this gate sends THIS file to the REAL CLI.
/// Transitively: **the bytes camp produces are the bytes the CLI accepts.**
///
/// Precisely: this does NOT make the fixture "recorded" — camp AUTHORED it. What
/// the gate proves is ACCEPTANCE, and PROVENANCE.md says exactly that and no
/// more. An integration test cannot call `ParentMessage::to_line` (`camp` is a
/// binary-only crate), so the FIXTURE is the shared truth between the two.
const INTERRUPT_REQUEST: &str = include_str!("fixtures/control/interrupt_request.json");

/// The fixture with its `request_id` retargeted — the only field a caller varies.
fn interrupt_line(request_id: &str) -> String {
    INTERRUPT_REQUEST.replace("camp-fixture-1", request_id)
}

/// The HeldStream worker flag list, mirroring `spawn.rs::build_spec`'s
/// `StdinMode::HeldStream` arm (its exact output is pinned by that module's
/// `stream_argv_matches_probe_p2_and_the_fixture_facts` unit test — keep the
/// two in sync). `with_verbose=false` reproduces the PRE-FIX argv for the
/// negative control that reproduces #86.
fn held_stream_flags(session_id: &str, with_verbose: bool) -> Vec<String> {
    let mut v = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    if with_verbose {
        v.push("--verbose".to_string());
    }
    v.push("--input-format".to_string());
    v.push("stream-json".to_string());
    v.push("--session-id".to_string());
    v.push(session_id.to_string());
    v
}

/// True iff `line` is a `control_response` with `subtype=="success"` and the
/// given `request_id`. Pins the wire shape (spec §2.1). A malformed or
/// non-matching line is simply `false` (the gate keeps reading until it finds
/// the match or times out).
fn control_response_is_success(line: &str, request_id: &str) -> bool {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v["type"] == "control_response"
        && v["response"]["subtype"] == "success"
        && v["response"]["request_id"] == request_id
}

#[test]
fn pinned_version_file_is_present_and_shaped() {
    let pin = PINNED_VERSION.trim();
    assert!(!pin.is_empty(), "CLAUDE_VERSION pin must not be empty");
    assert!(
        !pin.contains(char::is_whitespace),
        "pin must be a bare version token (no `(Claude Code)` suffix, no spaces): {pin:?}"
    );
    // Shape: dotted numeric, e.g. 2.1.208.
    assert!(
        pin.split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "pin must be a dotted numeric version: {pin:?}"
    );
}

#[test]
fn held_stream_flags_include_verbose_in_sdk_order() {
    // The fixed argv (cross-check of the Task 1 fix): --verbose sits between
    // the stream-json output value and --input-format, matching the SDK.
    assert_eq!(
        held_stream_flags("sid-1", true),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--session-id",
            "sid-1",
        ]
    );
    // The pre-fix argv (negative control): identical but for --verbose.
    assert_eq!(
        held_stream_flags("sid-1", false),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--session-id",
            "sid-1",
        ]
    );
}

#[test]
fn control_response_parser_pins_the_wire_shape() {
    // Recorded verbatim from claude 2.1.208 — the PINNED version — on a $0 run:
    // a pre-turn interrupt with NO `initialize`, which is the configuration camp
    // actually ships. Byte-for-byte identical on 2.1.207, so the pin bump moved
    // nothing on the wire.
    let ok = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_int","response":{"still_queued":[]}}}"#;
    assert!(control_response_is_success(ok, "req_int"));
    // Wrong request_id must not match.
    assert!(!control_response_is_success(ok, "other"));
    // An error subtype is not success.
    let err = r#"{"type":"control_response","response":{"subtype":"error","request_id":"req_int","response":{}}}"#;
    assert!(!control_response_is_success(err, "req_int"));
    // A non-control line is not a match.
    let other = r#"{"type":"system","subtype":"init"}"#;
    assert!(!control_response_is_success(other, "req_int"));
    // Garbage is not a match (parser must not panic).
    assert!(!control_response_is_success("not json", "req_int"));
}

#[test]
fn gate_core_flags_match_build_spec_held_stream_arm() {
    // `held_stream_flags` is deliberately NOT argv-for-argv parity with
    // `build_spec` (plan note N4: the gate argv is a minimal validation argv).
    // But its CORE flags — the HeldStream format/verbose block — must never
    // drift from the builder's. `camp` is a binary crate (no lib target), so
    // the gate cannot call `build_spec` directly; instead the builder's
    // HeldStream arm is parsed out of the source, making `spawn.rs` the
    // single source of truth. If the arm's `arg("...")` sequence changes,
    // this test fails and forces the gate back into sync.
    const SPAWN_RS: &str = include_str!("../src/daemon/spawn.rs");
    const ARM: &str = "StdinMode::HeldStream => {";
    assert_eq!(
        SPAWN_RS.matches(ARM).count(),
        1,
        "expected exactly one HeldStream match arm in spawn.rs"
    );
    let start = SPAWN_RS.find(ARM).unwrap() + ARM.len();
    let after = &SPAWN_RS[start..];
    // Brace-depth scan (the arm is no longer brace-free — cp-4 added a
    // conditional `if include_partial_messages { arg("--include-partial-messages"); }`
    // block). We collect the arm's UNCONDITIONAL CORE flags — `arg(..)` calls at
    // depth 0 — and stop at the arm's matching close brace. A flag nested in a
    // conditional block is spawn-time-optional (attach-only, §2.2), NOT part of
    // the always-on core this differential gate validates against real claude, so
    // depth-> 0 arg() calls are intentionally excluded.
    let mut depth: i32 = 0;
    let mut builder_flags: Vec<&str> = Vec::new();
    for line in after.lines() {
        let trimmed = line.trim();
        if depth == 0
            && let Some(flag) = trimmed
                .strip_prefix("arg(\"")
                .and_then(|s| s.strip_suffix("\");"))
        {
            builder_flags.push(flag);
        }
        let mut closed = false;
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth < 0 {
                        closed = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if closed {
            break; // the arm's matching close brace — stop the scan
        }
    }
    assert!(
        !builder_flags.is_empty(),
        "parsed no arg(..) calls from the HeldStream arm — parser drifted from the source shape"
    );
    // The gate's core = everything between the leading "-p" and the trailing
    // "--session-id <sid>" pair.
    let gate = held_stream_flags("sid-1", true);
    assert_eq!(
        gate[1..gate.len() - 2].to_vec(),
        builder_flags,
        "the gate's core HeldStream flags drifted from build_spec's HeldStream arm"
    );
}

// ---- real-claude harness (used only by the #[ignore]d gate) ----------------

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const SESSION_ID: &str = "7bd2befc-b018-4080-8738-429d541b3646";

/// Absolute path to the real `claude` (fail loud if absent).
fn resolve_claude() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v claude"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the compat gate requires a `claude` binary on PATH"
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// The installed `claude` version's first whitespace token (e.g. `2.1.207`
/// from `2.1.207 (Claude Code)`).
fn claude_version(claude: &str) -> String {
    let out = Command::new(claude).arg("--version").output().unwrap();
    assert!(out.status.success(), "`claude --version` failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .expect("`claude --version` produced no output")
        .to_owned()
}

/// Build a `claude` Command under a FRESH throwaway config dir (hermetic +
/// `verbose` defaults to false). Returns the Command and the tempdir guard
/// (dropping it removes the config dir — keep it alive for the child's life).
fn claude_command(claude: &str, flags: &[String]) -> (Command, tempfile::TempDir) {
    let cfg = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(claude);
    cmd.args(flags)
        .env("CLAUDE_CONFIG_DIR", cfg.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (cmd, cfg)
}

/// Kills the child on drop so a hung worker never outlives the test.
struct Worker {
    child: Child,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Drain the child's stdout lines onto a channel from a reader thread, so the
/// gate can wait for a specific control_response with a timeout (std has no
/// per-read deadline on ChildStdout).
fn stdout_lines(child: &mut Child) -> mpsc::Receiver<String> {
    let stdout = child.stdout.take().expect("child stdout piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Wait until a `control_response{success, request_id}` arrives, or fail loud.
fn await_success(rx: &mpsc::Receiver<String>, request_id: &str) {
    let deadline = std::time::Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or_default();
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                if control_response_is_success(&line, request_id) {
                    return;
                }
                // Other lines (e.g. a system/init event) are skipped.
            }
            Err(_) => panic!(
                "no control_response success for request_id {request_id:?} within {RESPONSE_TIMEOUT:?}"
            ),
        }
    }
}

fn send(child: &mut Child, json: &str) {
    let stdin = child.stdin.as_mut().expect("child stdin piped");
    stdin.write_all(json.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

/// Drain the child's stderr on a reader thread (mirrors `stdout_lines`): a
/// child writing more than the OS pipe buffer to stderr would otherwise
/// deadlock against a parent that only reads stderr after `wait()` (child
/// blocked on the stderr write, parent blocked in wait). The captured text
/// comes back over a channel so the caller can `recv_timeout` — a `claude`
/// descendant inheriting the write end could hold the pipe open past the
/// child's exit, and an unbounded join would hang the gate on it (the drain
/// thread may then outlive the test; it dies with the process).
fn stderr_capture(child: &mut Child) -> mpsc::Receiver<String> {
    let mut stderr = child.stderr.take().expect("child stderr piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    rx
}

/// Bounded wait: the child must exit within RESPONSE_TIMEOUT or the gate
/// fails loud (a CLI that never exits on stdin EOF is a hang, not a pass —
/// the panic trips `Worker::drop`, which kills the child).
fn wait_within_timeout(child: &mut Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + RESPONSE_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not exit within {RESPONSE_TIMEOUT:?} of stdin EOF — a CLI \
             that never exits on EOF is a hang, not a pass"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// The $0 real-`claude` compatibility gate (control-plane spec §8 "the $0
/// tier", phase 0). Opt-in and local-only: `#[ignore]`d AND gated on
/// CAMP_COMPAT=1. Spends $0 (no turn is ever sent) and needs no auth.
///
/// Three assertions, all pre-turn:
///  1. NEGATIVE CONTROL — the pre-fix argv (no --verbose) is REJECTED by the
///     real CLI with the #86 error. This proves the gate catches #86's class.
///  2. The FIXED argv is ACCEPTED (no `requires --verbose`, argv validation
///     passes) and the `initialize` handshake round-trips.
///  3. A pre-turn `interrupt` is acknowledged — with CAMP'S OWN BYTES.
#[test]
#[ignore = "real-claude $0 gate: run via `make compat` (CAMP_COMPAT=1)"]
fn claude_compat_zero_cost() {
    assert_eq!(
        std::env::var("CAMP_COMPAT").as_deref(),
        Ok("1"),
        "the compat gate is opt-in: set CAMP_COMPAT=1 (use `make compat`)"
    );

    let claude = resolve_claude();
    let version = claude_version(&claude);
    let pinned = PINNED_VERSION.trim();
    assert_eq!(
        version, pinned,
        "installed claude {version:?} != pinned {pinned:?}. This gate pins the \
         tested CLI version (like ci/gc-compat/GASCITY_REF). Re-validate the \
         HeldStream contract against the new version, then bump \
         ci/claude-compat/CLAUDE_VERSION — never widen the pin silently."
    );
    eprintln!("[compat] claude {version} (pinned)");

    // (1) NEGATIVE CONTROL: pre-fix argv is rejected — #86 reproduced.
    {
        let (mut cmd, _cfg) = claude_command(&claude, &held_stream_flags(SESSION_ID, false));
        let out = cmd.stdin(Stdio::null()).output().unwrap();
        assert!(
            !out.status.success(),
            "PRE-FIX argv must be REJECTED by claude {version}; it exited 0 — the \
             gate cannot prove it catches #86"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("requires --verbose"),
            "PRE-FIX argv must fail with the #86 error; stderr was: {stderr:?}"
        );
        eprintln!(
            "[compat] negative control OK: pre-fix argv rejected ({})",
            stderr.trim()
        );
    }

    // (2)+(3) FIXED argv: accepted, initialize round-trips, interrupt acked.
    {
        let (mut cmd, _cfg) = claude_command(&claude, &held_stream_flags(SESSION_ID, true));
        let mut worker = Worker {
            child: cmd.spawn().unwrap(),
        };
        let rx = stdout_lines(&mut worker.child);
        let stderr_rx = stderr_capture(&mut worker.child);

        // initialize handshake (the SDK sends this first).
        send(
            &mut worker.child,
            r#"{"type":"control_request","request_id":"camp-compat-init","request":{"subtype":"initialize","hooks":null}}"#,
        );
        await_success(&rx, "camp-compat-init");
        eprintln!("[compat] initialize round-tripped");

        // pre-turn interrupt — CAMP'S OWN BYTES, not a hand-written literal.
        send(&mut worker.child, &interrupt_line("camp-compat-interrupt"));
        await_success(&rx, "camp-compat-interrupt");
        eprintln!("[compat] pre-turn interrupt acknowledged (camp's own bytes)");

        // Clean shutdown: close stdin (EOF) and confirm the fixed argv exits 0
        // with no `requires --verbose` on stderr — argv was accepted.
        worker.child.stdin.take(); // drop -> EOF
        let status = wait_within_timeout(&mut worker.child);
        assert!(
            status.success(),
            "fixed argv worker must exit 0 on stdin EOF; got {status:?}"
        );
        let stderr = stderr_rx.recv_timeout(RESPONSE_TIMEOUT).expect(
            "stderr not drained within the response timeout — a descendant \
             is holding the worker's stderr write end open past exit",
        );
        assert!(
            !stderr.contains("requires --verbose"),
            "fixed argv must NOT trip the #86 error; stderr was: {stderr:?}"
        );
        eprintln!("[compat] fixed argv accepted; worker exited cleanly ($0, no turn sent)");
    }
}

/// The interrupt fixture is a well-formed control request.
///
/// This one RUNS IN CI (it is not `#[ignore]`d). It cannot prove the real CLI
/// accepts the bytes — only the $0 gate can — but it stops them rotting between
/// compat runs.
#[test]
fn the_interrupt_fixture_is_a_well_formed_control_request() {
    let v: serde_json::Value = serde_json::from_str(INTERRUPT_REQUEST).unwrap();
    assert_eq!(v["type"], "control_request");
    assert_eq!(v["request"]["subtype"], "interrupt");
    assert!(v["request_id"].as_str().unwrap().starts_with("camp-"));
    // The retarget is a pure substitution — it must not disturb the shape.
    let v: serde_json::Value = serde_json::from_str(&interrupt_line("camp-x")).unwrap();
    assert_eq!(v["request_id"], "camp-x");
    assert_eq!(v["request"]["subtype"], "interrupt");
}

/// B15 — THE CONFIGURATION CAMP ACTUALLY SHIPS: **no `initialize`, ever.**
///
/// Every recorded ack in this repo is POST-initialize, and the fake worker acks
/// anything — so nothing here proves camp's REAL configuration works. camp sends an
/// interrupt into a worker it spawned with NO handshake at all. This gate sends
/// camp's own interrupt bytes straight at the real pinned CLI, before any turn. $0,
/// no auth, no API spend.
///
/// If this ever goes RED, cp-1's interrupt path is BROKEN against the real CLI and
/// camp MUST start sending `initialize` (§9's "camp sends it anyway"). Do NOT paper
/// over it by adding the handshake to this test.
#[test]
#[ignore = "real-claude $0 gate: run via `make compat` (CAMP_COMPAT=1)"]
fn no_initialize_pre_turn_interrupt_is_acked() {
    assert_eq!(
        std::env::var("CAMP_COMPAT").as_deref(),
        Ok("1"),
        "the compat gate is opt-in: set CAMP_COMPAT=1 (use `make compat`)"
    );
    let claude = resolve_claude();
    let version = claude_version(&claude);
    let pinned = PINNED_VERSION.trim();
    assert_eq!(
        version, pinned,
        "installed claude {version:?} != pinned {pinned:?} — never widen the pin silently."
    );

    let (mut cmd, _cfg) = claude_command(&claude, &held_stream_flags(SESSION_ID, true));
    let mut worker = Worker {
        child: cmd.spawn().unwrap(),
    };
    let rx = stdout_lines(&mut worker.child);

    // NO initialize. Just camp's interrupt, exactly as campd sends it.
    send(&mut worker.child, &interrupt_line("camp-b15"));
    await_success(&rx, "camp-b15");
    eprintln!("[compat] pre-turn interrupt acked with NO initialize");

    worker.child.stdin.take(); // EOF
    let status = wait_within_timeout(&mut worker.child);
    assert!(status.success(), "the worker must exit 0 on stdin EOF");
}

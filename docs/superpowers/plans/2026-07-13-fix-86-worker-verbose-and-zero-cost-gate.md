# Fix #86 (`--verbose` in HeldStream argv) + the $0 real-claude gate — Implementation Plan

> **Plan review: APPROVE, 2026-07-13 (Opus 4.8 plan gate).** Non-blocking notes: N1 spawn.rs co-tenancy with fix-82 (stay in the HeldStream arm; expect a trivial rebase after sibling merges); N2 Makefile compat target may need a one-line rebase; N3 TDD-red is carried by Task 1 plus the gate's permanent negative-control assertion (the gate test itself is green-on-write, honestly labeled); N4 the gate's held_stream_flags is a minimal validation argv, not argv-for-argv parity with build_spec — do not 'fix' it toward parity. No deviations accepted.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (this stream is planning-only; a fresh implementer session executes). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pass `--verbose` unconditionally in campd's `HeldStream` worker argv so dispatch works on any machine (not just one whose `~/.claude/settings.json` sets `"verbose": true`), and stand up the **$0 tier** of the real-`claude` compatibility gate as an opt-in, local-only, no-API-spend target that catches this bug class before a release.

**Architecture:** Two deliverables. (1) A one-flag fix to the pure argv builder `build_spec` in `crates/camp/src/daemon/spawn.rs`, driven by updating the existing argv-pinning unit test (TDD: watch it fail, fix, watch it pass). (2) A new opt-in integration test `crates/camp/tests/claude_compat.rs` plus a `make compat` target and a version-pin file `ci/claude-compat/CLAUDE_VERSION`. The gate spawns a real `claude` under a **fresh, throwaway `CLAUDE_CONFIG_DIR`** (this both keeps it hermetic and makes `verbose` default to `false`, so it honestly proves the flag is required), asserts the fixed argv is accepted, the `initialize` handshake round-trips, and a pre-turn `interrupt` is acknowledged — all **before any turn**, so it spends **$0** and needs **no auth**. A negative control spawns the pre-fix argv and asserts the CLI rejects it, proving the gate catches #86's class.

**Tech Stack:** Rust (edition per workspace), `std::process`, `serde_json` (already a dev/normal dep), `tempfile` (already a dev-dep), GNU Make. No new dependencies.

---

## Root-cause analysis (systematic-debugging — confirmed, not assumed)

The issue's analysis was independently verified on this machine against the installed `claude 2.1.207` (the version this plan pins). Evidence:

1. **The omission is real.** `crates/camp/src/daemon/spawn.rs` `build_spec`, `StdinMode::HeldStream` arm (lines 187–194) emits `--output-format stream-json --input-format stream-json` and **never** `--verbose`. The string `--verbose` does not appear anywhere in `spawn.rs`. The `StdinMode::Null` arm uses `--output-format json` and is correctly unaffected.

2. **The CLI hard-rejects the pre-fix argv, pre-auth, at $0.** Reproduced directly with a fresh (unauthenticated) config dir:
   ```
   $ D=$(mktemp -d); CLAUDE_CONFIG_DIR="$D" claude -p --output-format stream-json \
       --input-format stream-json --session-id <uuid> </dev/null; echo exit=$?
   Error: When using --print, --output-format=stream-json requires --verbose
   exit=1
   ```
   This is argv validation — it happens before authentication and before any network call. `verbose` resolves **flag → settings → false**, so the only reason dispatch works on the maintainer's box is `"verbose": true` in `~/.claude/settings.json`, a file camp does not own.

3. **The fixed argv passes, pre-auth, at $0.** With `--verbose` added and stdin at EOF, the same fresh-config invocation exits `0` with no error and no output (no turn sent → no spend):
   ```
   $ CLAUDE_CONFIG_DIR="$D" claude -p --verbose --output-format stream-json \
       --input-format stream-json --session-id <uuid> </dev/null; echo exit=$?
   exit=0
   ```

4. **The full $0 handshake works pre-auth.** Sending the SDK's `initialize` control_request then a pre-turn `interrupt` over the held stdin (fresh unauthenticated config) each returned a `control_response{subtype:"success"}` with the matching `request_id`. The `initialize` response even carried `account.tokenSource:"none"` — proving no auth and no spend. Exact captured success line (used as a pinned fixture in Task 4):
   ```
   {"type":"control_response","response":{"subtype":"success","request_id":"req_int","response":{"still_queued":[]}}}
   ```

**Root cause:** a missing `--verbose` in one argv arm. **Fix:** add it, in the SDK's canonical position. **Why tests never caught it:** every worker in the suite is a `#!/bin/sh` fake that ignores argv, so no fake can reject a flag the real CLI rejects — hence the second deliverable, a real-`claude` gate.

## Global Constraints

Copied verbatim from AGENTS.md and the kickoff — every task's requirements implicitly include these:

- **TDD, strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything.
- **Never commit to main.** All work on branch `fix-86-worker-verbose`; land via one PR.
- **Gates green before push:** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`.
- **No panics in library code** (clippy `unwrap_used`/`expect_used`/`panic` denied; `unsafe_code` forbidden). Test files opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` at the top — this is the existing convention (see `crates/camp/tests/e2e.rs:1`).
- **Fail fast, no fallbacks, no silenced errors.** The gate must **fail loudly** on an unpinned/mismatched `claude` version and on a missing binary — never skip silently.
- **No test may spawn a real `claude` in CI or spend API money.** The measured gate is `#[ignore]`d **and** env-gated (`CAMP_COMPAT=1`); CI compiles the file and runs only the non-ignored pure tests. The $0 gate spends nothing even when run (no turn is ever sent).
- **No co-author lines in commits; never mention the assistant.**
- **Owned files for this stream:** `crates/camp/src/daemon/spawn.rs` (HeldStream argv only) + the new gate scaffolding (new test file, new pin file, Makefile target). Do **not** touch a sibling's owned files (patrol.rs, dispatch.rs, event_loop.rs, import machinery). `docs/design/2026-07-06-assumption-findings.md` is a doc, not a sibling's owned code — the additive note in Task 6 is in scope. **A spec edit is an escalation — do not edit any file under `docs/superpowers/specs/`.**
- **Pinned `claude` version:** `2.1.207` (the installed version this plan was validated against; the issue verified the bug in 2.1.205, 2.1.206, and 2.1.207).

---

## File Structure

- `crates/camp/src/daemon/spawn.rs` — **modify.** Add `--verbose` to the `HeldStream` arm of `build_spec`; update the existing argv-pinning unit test `stream_argv_matches_probe_p2_and_the_fixture_facts`.
- `ci/claude-compat/CLAUDE_VERSION` — **create.** One line: the pinned `claude` version string. Mirrors `ci/gc-compat/GASCITY_REF`'s role (a version the gate pins and fails loudly against).
- `crates/camp/tests/claude_compat.rs` — **create.** The $0 gate: non-ignored pure tests (parser fixture, pin-file shape, flag-order cross-check) that run in CI, plus the `#[ignore]`d + `CAMP_COMPAT=1`-gated real-`claude` test.
- `Makefile` — **modify.** Add the `compat` target and list it in `.PHONY`.
- `docs/design/2026-07-06-assumption-findings.md` — **modify.** Additive doc-only note recording the F5/F7 config-contamination and what the $0 gate now re-validates.

No shared files (`main.rs`, `event.rs`, `vocab.rs`, `fold.rs`, `Cargo.toml`, `Cargo.lock`) are touched — no new dependencies are needed.

---

## Task 1: Fix the HeldStream argv (add `--verbose`)

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs` — the `build_spec` `HeldStream` arm (around lines 187–194) and the unit test `stream_argv_matches_probe_p2_and_the_fixture_facts` (around lines 797–844).

**Interfaces:**
- Consumes: nothing new.
- Produces: `build_spec(..., StdinMode::HeldStream)` now emits, in order: `--output-format stream-json --verbose --input-format stream-json --session-id <sid> [--model …] [--permission-mode …] [--allowedTools …] [--append-system-prompt …] -p`. Task 4's gate mirrors this flag list; keep them consistent.

- [ ] **Step 1: Update the failing test to require `--verbose` (TDD red).**

In `crates/camp/src/daemon/spawn.rs`, in `stream_argv_matches_probe_p2_and_the_fixture_facts`, the expected argv vector currently reads (excerpt):
```rust
                "claude",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--session-id",
```
Insert `"--verbose",` between `"stream-json"` (the output-format value) and `"--input-format"`, so it reads:
```rust
                "claude",
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--session-id",
```
This mirrors the Agent SDK's canonical order (`--output-format stream-json --verbose --input-format stream-json`, control-plane spec §2.2 / §2 table).

- [ ] **Step 2: Run the test, watch it fail.**

Run: `cargo test -p camp --lib stream_argv_matches_probe_p2_and_the_fixture_facts`
Expected: FAIL — `assert_eq!` panics; the `left` (actual) argv lacks `--verbose`, the `right` (expected) has it.

- [ ] **Step 3: Add `--verbose` to the builder (TDD green).**

In `build_spec`, the `StdinMode::HeldStream` arm currently reads:
```rust
            StdinMode::HeldStream => {
                // P2: stream in requires stream out; both accepted with
                // the pinned flags at 2.1.204.
                arg("--output-format");
                arg("stream-json");
                arg("--input-format");
                arg("stream-json");
            }
```
Change it to (insert the two `arg("--verbose")`… lines and refresh the comment):
```rust
            StdinMode::HeldStream => {
                // P2: stream in requires stream out. The shipped CLI
                // (2.1.205–2.1.207) hard-rejects `--print` + stream-json
                // output UNLESS `--verbose` is passed (#86): `verbose`
                // resolves flag -> settings -> false, so without the flag
                // dispatch dies at argv validation on every machine whose
                // ~/.claude/settings.json does not set it. The Agent SDK
                // hardcodes `--verbose` here for exactly this reason;
                // camp does too. Order mirrors the SDK:
                // --output-format stream-json --verbose --input-format stream-json.
                arg("--output-format");
                arg("stream-json");
                arg("--verbose");
                arg("--input-format");
                arg("stream-json");
            }
```

- [ ] **Step 4: Run the test, watch it pass.**

Run: `cargo test -p camp --lib stream_argv_matches_probe_p2_and_the_fixture_facts`
Expected: PASS.

- [ ] **Step 5: Run the whole spawn/daemon suite to confirm nothing else pinned this argv.**

Run: `cargo test -p camp`
Expected: PASS. (A repo-wide grep confirmed only this one test and the builder reference the HeldStream flag list; no fixture or integration test asserts it. If any other test fails on the new flag, it is a legitimately pinned expectation — update it to include `--verbose` in the same SDK position; do not remove the flag.)

- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/daemon/spawn.rs
git commit -m "fix(spawn): pass --verbose in the HeldStream worker argv (#86)"
```

---

## Task 2: Pin the tested `claude` version

**Files:**
- Create: `ci/claude-compat/CLAUDE_VERSION`

**Interfaces:**
- Produces: a repo-root pin file the gate reads via a manifest-relative path. Content is a single line: the exact version string `claude --version` prints as its first whitespace token.

- [ ] **Step 1: Create the pin file.**

Create `ci/claude-compat/CLAUDE_VERSION` with exactly this content (one line, trailing newline):
```
2.1.207
```
Rationale (mirrors `ci/gc-compat/GASCITY_REF` pinning the gc compiler for invariant 6): the gate reads this, compares it to the installed `claude --version`, and **fails loudly** on any mismatch, so a `claude` upgrade is a red gate that forces re-validation, not a silent drift.

- [ ] **Step 2: Commit.**

```bash
git add ci/claude-compat/CLAUDE_VERSION
git commit -m "test(compat): pin the tested claude version at 2.1.207 (#86)"
```

---

## Task 3: The $0 gate — CI-safe pure tests (parser fixture, pin shape, flag-order cross-check)

These tests run in CI (`cargo test --workspace`) with no `claude` binary. They pin the control-protocol wire shape as a recorded fixture (spec §2.1: "wire shapes are pinned by tests against recorded fixtures"), verify the pin file's shape, and cross-check the gate's flag list against the fix.

**Files:**
- Create: `crates/camp/tests/claude_compat.rs` (this task writes the header + pure helpers + non-ignored tests; Task 4 appends the `#[ignore]`d gate to the same file).

**Interfaces:**
- Produces, for Task 4 to consume:
  - `const PINNED_VERSION: &str` — `include_str!` of the pin file.
  - `fn held_stream_flags(session_id: &str, with_verbose: bool) -> Vec<String>` — the worker flag list mirroring `build_spec`'s HeldStream arm; `with_verbose=false` reproduces the pre-fix argv for the negative control.
  - `fn control_response_is_success(line: &str, request_id: &str) -> bool` — parses one stdout line; true iff it is a `control_response` with `subtype=="success"` and the given `request_id`.

- [ ] **Step 1: Write the file header and the three helpers.**

Create `crates/camp/tests/claude_compat.rs`:
```rust
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
```

- [ ] **Step 2: Write the non-ignored CI tests.**

Append to `crates/camp/tests/claude_compat.rs`:
```rust
#[test]
fn pinned_version_file_is_present_and_shaped() {
    let pin = PINNED_VERSION.trim();
    assert!(!pin.is_empty(), "CLAUDE_VERSION pin must not be empty");
    assert!(
        !pin.contains(char::is_whitespace),
        "pin must be a bare version token (no `(Claude Code)` suffix, no spaces): {pin:?}"
    );
    // Shape: dotted numeric, e.g. 2.1.207.
    assert!(
        pin.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
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
    // Recorded verbatim from claude 2.1.207 (pre-turn interrupt ack, $0 run).
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
```

- [ ] **Step 3: Run the CI tests, watch them pass.**

Run: `cargo test -p camp --test claude_compat`
Expected: PASS for all three (`pinned_version_file_is_present_and_shaped`, `held_stream_flags_include_verbose_in_sdk_order`, `control_response_parser_pins_the_wire_shape`); the `#[ignore]`d gate (added in Task 4) is not yet present.

- [ ] **Step 4: Commit.**

```bash
git add crates/camp/tests/claude_compat.rs
git commit -m "test(compat): CI-safe pins for the \$0 real-claude gate (#86)"
```

---

## Task 4: The $0 gate — the real-`claude` test (ignored, `CAMP_COMPAT=1`)

**Files:**
- Modify: `crates/camp/tests/claude_compat.rs` (append the harness helpers and the `#[ignore]`d test).

**Interfaces:**
- Consumes: `PINNED_VERSION`, `held_stream_flags`, `control_response_is_success` from Task 3.
- Produces: `make compat` runs `claude_compat_zero_cost`, the release-blocking $0 gate.

- [ ] **Step 1: Append the process harness (spawn under fresh config, read-with-timeout, kill-on-drop).**

Append to `crates/camp/tests/claude_compat.rs`:
```rust
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
```

- [ ] **Step 2: Append the `#[ignore]`d $0 gate.**

Append to `crates/camp/tests/claude_compat.rs`:
```rust
/// The $0 real-`claude` compatibility gate (control-plane spec §8 "the $0
/// tier", phase 0). Opt-in and local-only: `#[ignore]`d AND gated on
/// CAMP_COMPAT=1. Spends $0 (no turn is ever sent) and needs no auth.
///
/// Three assertions, all pre-turn:
///  1. NEGATIVE CONTROL — the pre-fix argv (no --verbose) is REJECTED by the
///     real CLI with the #86 error. This proves the gate catches #86's class.
///  2. The FIXED argv is ACCEPTED (no `requires --verbose`, argv validation
///     passes) and the `initialize` handshake round-trips.
///  3. A pre-turn `interrupt` is acknowledged.
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
        eprintln!("[compat] negative control OK: pre-fix argv rejected ({})", stderr.trim());
    }

    // (2)+(3) FIXED argv: accepted, initialize round-trips, interrupt acked.
    {
        let (mut cmd, _cfg) = claude_command(&claude, &held_stream_flags(SESSION_ID, true));
        let mut worker = Worker { child: cmd.spawn().unwrap() };
        let rx = stdout_lines(&mut worker.child);

        // initialize handshake (the SDK sends this first).
        send(
            &mut worker.child,
            r#"{"type":"control_request","request_id":"camp-compat-init","request":{"subtype":"initialize","hooks":null}}"#,
        );
        await_success(&rx, "camp-compat-init");
        eprintln!("[compat] initialize round-tripped");

        // pre-turn interrupt.
        send(
            &mut worker.child,
            r#"{"type":"control_request","request_id":"camp-compat-interrupt","request":{"subtype":"interrupt"}}"#,
        );
        await_success(&rx, "camp-compat-interrupt");
        eprintln!("[compat] pre-turn interrupt acknowledged");

        // Clean shutdown: close stdin (EOF) and confirm the fixed argv exits 0
        // with no `requires --verbose` on stderr — argv was accepted.
        worker.child.stdin.take(); // drop -> EOF
        let status = worker.child.wait().unwrap();
        assert!(
            status.success(),
            "fixed argv worker must exit 0 on stdin EOF; got {status:?}"
        );
        let mut stderr = String::new();
        if let Some(mut e) = worker.child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut stderr);
        }
        assert!(
            !stderr.contains("requires --verbose"),
            "fixed argv must NOT trip the #86 error; stderr was: {stderr:?}"
        );
        eprintln!("[compat] fixed argv accepted; worker exited cleanly ($0, no turn sent)");
    }
}
```

Note on the clean-shutdown read: `worker.child.stderr` is `Some` because `stderr(Stdio::piped())` was set; reading it after `wait()` is a best-effort assertion that the #86 error is absent. The core assertions are the two `await_success` calls and the negative control.

- [ ] **Step 3: Confirm CI-safe tests still compile and pass (the gate is skipped by default).**

Run: `cargo test -p camp --test claude_compat`
Expected: the three pure tests PASS; `claude_compat_zero_cost` shows as `ignored`.

- [ ] **Step 4: Run the real gate locally and watch it pass.**

Run: `CAMP_COMPAT=1 cargo test -p camp --test claude_compat -- --ignored --nocapture --test-threads=1`
Expected: PASS, with the `[compat]` trace showing: pinned version, negative control rejected with `requires --verbose`, initialize round-tripped, interrupt acknowledged, clean exit. **Requires `claude 2.1.207` on PATH.**

- [ ] **Step 5: Prove the gate catches the bug (run it against the PRE-FIX code).**

This is the acceptance proof that the gate is meaningful. Temporarily (do NOT commit) point the gate's FIXED-argv build at the pre-fix argv to confirm the positive path would fail without the fix — but the **negative control already does this permanently** by asserting the pre-fix argv is rejected. Verify the negative-control assertion is load-bearing:
```bash
# Sanity: confirm the negative-control branch actually reaches the CLI and the
# error string is current for the pinned version (already asserted in the test).
D=$(mktemp -d); CLAUDE_CONFIG_DIR="$D" claude -p --output-format stream-json \
  --input-format stream-json --session-id 7bd2befc-b018-4080-8738-429d541b3646 \
  </dev/null; echo "exit=$?"; rm -rf "$D"
```
Expected: `Error: When using --print, --output-format=stream-json requires --verbose` / `exit=1`. If this string ever changes, the gate fails loud and the fixture + assertion are updated in the same PR (spec §8 canary discipline).

- [ ] **Step 6: Commit.**

```bash
git add crates/camp/tests/claude_compat.rs
git commit -m "test(compat): \$0 real-claude gate — argv accept + initialize + interrupt (#86)"
```

---

## Task 5: The `make compat` target

**Files:**
- Modify: `Makefile` — add `compat` to `.PHONY` and add the target.

**Interfaces:**
- Produces: `make compat`, the operator entry point for the $0 gate.

- [ ] **Step 1: Add `compat` to the `.PHONY` line.**

In `Makefile`, change:
```make
.PHONY: install uninstall perf e2e service-e2e container-smoke
```
to:
```make
.PHONY: install uninstall perf e2e compat service-e2e container-smoke
```

- [ ] **Step 2: Add the target (place it after the `e2e:` target block).**

```make
# Opt-in $0 real-`claude` compatibility gate (control-plane design §8 "the $0
# tier"). LOCAL-ONLY: CI never runs it (the test is #[ignore]d AND gated on
# CAMP_COMPAT=1). Unlike `make e2e`, it spends NO API money and needs NO auth —
# every assertion is pre-turn (argv acceptance, the `initialize` handshake, a
# pre-turn `interrupt`), run under a fresh throwaway CLAUDE_CONFIG_DIR. It PINS
# the tested claude version (ci/claude-compat/CLAUDE_VERSION) and fails loudly
# on a mismatch. Requires a `claude` binary on PATH at the pinned version.
# Single-threaded: it spawns a real worker and speaks the control protocol.
compat:
	CAMP_COMPAT=1 cargo test -p camp --test claude_compat -- --ignored --nocapture --test-threads=1
```

- [ ] **Step 3: Verify the target runs.**

Run: `make compat`
Expected: same PASS as Task 4 Step 4 (the `[compat]` trace, all assertions green). **Requires `claude 2.1.207`.**

- [ ] **Step 4: Commit.**

```bash
git add Makefile
git commit -m "build(compat): add \`make compat\` for the \$0 real-claude gate (#86)"
```

---

## Task 6: Record the F5/F7 re-validation note (doc-only)

The kickoff requires this stream to state whether the config-contaminated assumption-findings (F5 held-stream worker, F7 config inheritance — validated on a machine with `"verbose": true` set) are re-validated here or deferred. A doc-only note is acceptable; a spec edit is an escalation. This is a doc-only note.

**Files:**
- Modify: `docs/design/2026-07-06-assumption-findings.md` — append a note after the "Net: F1–F5 and F7 HOLD at claude 2.1.205" paragraph (around line 72–74).

**Interfaces:** none (documentation).

- [ ] **Step 1: Append the note.**

After the paragraph ending "No pinned fact drifted beyond the F3 computation refinement above." add:
```markdown

### #86 config-contamination and the $0 gate (2026-07-13)

The Phase 15 e2e re-verification above ran on a machine whose
`~/.claude/settings.json` set `"verbose": true`. That silently satisfied the
CLI's hard requirement that `--print` + `--output-format stream-json` be
accompanied by `--verbose` (`verbose` resolves flag → settings → false). So
**F5's "HOLDS" for the HeldStream argv was not portable**: on any machine
without that setting, the pre-fix argv is rejected at argv validation (exit 1,
`Error: When using --print, --output-format=stream-json requires --verbose`)
before any worker contract runs. Filed and fixed as #86 — camp now passes
`--verbose` unconditionally in the HeldStream argv, so the operator's setting
no longer participates.

**Re-validation:** the argv-acceptance portion of F5 is now re-validated
portably by the **$0 real-`claude` compatibility gate** (`make compat`,
control-plane design §8), which spawns the real CLI under a **fresh
`CLAUDE_CONFIG_DIR`** (so `verbose` defaults to false) and asserts the fixed
argv is accepted, the pre-fix argv is rejected (#86 reproduced), the
`initialize` handshake round-trips, and a pre-turn `interrupt` is acknowledged
— all at $0, no auth. The task-delivery portion of F5 and F7's capability
pinning (`--model`/`--permission-mode`/`--allowedTools` behavior over a real
turn) still ride the paid `make e2e` tier; those runs are no longer
argv-contaminated now that camp passes `--verbose` explicitly, but re-running
`make e2e` with `verbose` unset to confirm the turn-level facts remains a
follow-up, deferred out of this stream's scope.
```

- [ ] **Step 2: Commit.**

```bash
git add docs/design/2026-07-06-assumption-findings.md
git commit -m "docs(findings): note #86 config-contamination and \$0-gate re-validation of F5/F7"
```

---

## Task 7: Full gates + push

**Files:** none (verification + push).

- [ ] **Step 1: Format check.**

Run: `cargo fmt --all --check`
Expected: clean (no diff). If it complains, run `cargo fmt --all` and re-commit into the relevant task's commit / a fixup.

- [ ] **Step 2: Clippy (deny warnings).**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings. (The new test file's `#![allow(...)]` header covers the `unwrap`/`expect`/`panic` used in test harness code.)

- [ ] **Step 3: Full workspace test suite.**

Run: `cargo test --workspace`
Expected: PASS. The three `claude_compat` pure tests run; `claude_compat_zero_cost` is ignored (as it must be — CI has no `claude`). No existing test regressed.

- [ ] **Step 4: Run the $0 gate one more time (the acceptance evidence).**

Run: `make compat`
Expected: PASS with the full `[compat]` trace. Capture this output for the PR description — it is the direct evidence for the acceptance criteria.

- [ ] **Step 5: Push the branch.**

```bash
git push -u origin fix-86-worker-verbose
```

- [ ] **Step 6: Open the PR and watch CI to a settled green.**

```bash
gh pr create --fill --base main --head fix-86-worker-verbose
gh pr checks --watch
```
Expected: CI settles green. Do not report completion until CI is green (foreground-watch to the settled result — never report "CI is running").

---

## Acceptance criteria → evidence map

Quote each acceptance line from the kickoff and record the evidence:

1. **"the $0 gate passes locally against the pinned real CLI with the fixed argv"** — `make compat` PASSES against `claude 2.1.207`; trace shows initialize round-trip + interrupt ack + clean exit (Task 7 Step 4). Test: `claude_compat_zero_cost`.
2. **"and demonstrably fails against the pre-fix argv (prove the gate catches #86's class)"** — the gate's negative-control branch asserts the pre-fix argv (`held_stream_flags(.., false)`) is rejected with `requires --verbose` and non-zero exit; it is a permanent assertion inside `claude_compat_zero_cost`, so a regression that dropped `--verbose` would fail the gate. Corroborated by the direct repro in Task 4 Step 5.
3. **"all existing tests green"** — `cargo test --workspace` PASSES (Task 7 Step 3); the only pre-existing test touched is `stream_argv_matches_probe_p2_and_the_fixture_facts`, updated in lockstep with the fix (Task 1).
4. **"CI green"** — `gh pr checks --watch` settles green (Task 7 Step 6); the real gate is `#[ignore]`d + `CAMP_COMPAT=1`-gated so CI never needs a `claude` binary or spends money.

## Self-review notes

- **Spec coverage:** §2.2 (`--verbose` mandatory, `--include-partial-messages` explicitly out of scope — not added) → Task 1. §8 "$0 tier" (argv accepted + initialize round-trip + pre-turn interrupt ack, no spend) → Tasks 3–4. §8 "pin the version, fail loudly on unpinned" (like GASCITY_REF) → Task 2 + the version assert in Task 4. §8 "CI must not run it" → `#[ignore]` + `CAMP_COMPAT=1` gate. Read channel / other phase-0 items → explicitly OUT of scope (not planned). F5/F7 re-validation statement → Task 6.
- **No placeholders:** every code and command step is concrete.
- **Type/name consistency:** `held_stream_flags`, `control_response_is_success`, `PINNED_VERSION`, `await_success`, `claude_compat_zero_cost`, `CAMP_COMPAT`, `ci/claude-compat/CLAUDE_VERSION`, `make compat` are used identically across Tasks 3–7.
- **Out of scope (do not build):** the campd read channel (stdout tailing, byte offsets, `notify` wiring), socket verbs, `camp watch`/`attach`, the permission/BLOCKED flow, `--permission-prompt-tool stdio`, `--include-partial-messages`, the paid gate tier. Any spec edit is an escalation to the lead.

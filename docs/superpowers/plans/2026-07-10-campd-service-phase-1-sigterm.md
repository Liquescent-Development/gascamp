# campd Service Management — Phase 1: SIGTERM/SIGINT Graceful Shutdown — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make campd shut down gracefully on SIGTERM/SIGINT — the same clean stop as the socket `Request::Stop` — so it is a well-behaved supervised process under launchd, systemd, and the container runtime (all of which stop a service with SIGTERM).

**Architecture:** campd's event loop is a mio poll over a fixed set of self-pipe tokens (listener, config watch, SIGCHLD, patrol watch) plus connections. We add one reserved token for a SIGTERM/SIGINT self-pipe (the exact SIGCHLD pattern: `signal_hook::low_level::pipe::register` → a `UnixStream` pair → registered in the loop). When the signal pipe becomes readable, the loop runs the existing `stop(ledger, socket_path)` (append `campd.stopped`, unlink the socket) and returns `Ok(())` — byte-identical to the `ConnState::Stop` path. No new event type; crash-only (SIGKILL) stays safe.

**Tech Stack:** Rust; `signal-hook` (already a dep, used for SIGCHLD); `mio` (already a dep); integration test via `assert_cmd` (binary path) + `std::process` + `kill(1)`.

**Phasing:** This is Phase 1 of the campd-service-management design (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`). Phases 2–4 (`camp service` + unit generation + env-aware init; remove CLI auto-start → pure client; container reference + docs + spec amendments) are separate plans authored after this merges. SIGTERM handling is independent and shippable on its own.

## Global Constraints

Copied from the design spec and `AGENTS.md` — every task implicitly includes these:

- **No new event types** — a graceful SIGTERM stop reuses `campd.stopped` (invariant 7, vocabulary mirror, untouched).
- **Crash-only preserved** — SIGKILL remains a supported shutdown; this only makes the *SIGTERM* path clean.
- **Invariant 5 fail-fast** — no fallbacks, no silenced errors, no placeholders. NO `unwrap`/`expect`/`panic` in non-test code (clippy `unwrap_used`/`expect_used`/`panic` denied; `unsafe` forbidden). Test files opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- **Invariant 1** — no ticks/polling added; the signal wakes the existing poll via a self-pipe (an OS event).
- **TDD strictly** — failing test first, watch it fail, implement, watch it pass.
- Branch `campd-service-management` (already created off `main`). Never commit to main. No co-author lines / no self-attribution.
- **Token layout is coordinated** (event_loop.rs comment): today 0=listener, 1=config, 2=SIGCHLD, 3=patrol, 4+=connections. This plan adds `4=SIGTERM/SIGINT` and moves connections to `5+`. Update the layout comment to match.
- Gates green before push: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`.

## File Structure

- `crates/camp/src/daemon/mod.rs` — **modify**: register a SIGTERM/SIGINT self-pipe (mirror of the SIGCHLD block), pass its read end into `event_loop::run(...)`.
- `crates/camp/src/daemon/event_loop.rs` — **modify**: add the `SIGTERM_SIG = Token(4)` reserved token (connections → `5+`), add the `sigterm` param to `run(...)`, register it, and add the dispatch arm that runs `stop(...)` and returns.
- `crates/camp/tests/cli_daemon_signal.rs` — **create**: integration test spawning a real `camp daemon`, sending SIGTERM, asserting a clean exit + `campd.stopped`.

---

## Task 1: SIGTERM/SIGINT → graceful shutdown

**Files:**
- Create: `crates/camp/tests/cli_daemon_signal.rs`
- Modify: `crates/camp/src/daemon/mod.rs:67-82` (add the signal block after the SIGCHLD block) and `crates/camp/src/daemon/mod.rs:199-212` (the `run(...)` call)
- Modify: `crates/camp/src/daemon/event_loop.rs:29-41` (token layout), `:84-123` (`run` signature + registration + `next_token`), and `:288-289` (add the dispatch arm before the `token =>` catch-all)

**Interfaces:**
- Consumes: `signal_hook::low_level::pipe::register`, `signal_hook::consts::{SIGTERM, SIGINT}`; `std::os::unix::net::UnixStream`; the existing `event_loop::stop(ledger: &mut Ledger, socket_path: &Path) -> Result<()>` helper (already emits `campd.stopped` + unlinks — called today from the `ConnState::Stop` arm at `event_loop.rs:313`); `camp_core::event::EventType::CampdStopped` (test); `assert_cmd::cargo::cargo_bin`.
- Produces: no new public API; `event_loop::run` gains one leading `UnixStream` parameter (`sigterm`) after `sigchld`.

- [ ] **Step 1: Write the failing test**

Create `crates/camp/tests/cli_daemon_signal.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 1 (campd service management): campd shuts down gracefully on
//! SIGTERM/SIGINT — the same clean stop as the socket `Request::Stop` — so
//! it is a well-behaved supervised process (launchd/systemd/container all
//! stop a service with SIGTERM).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn camp_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("camp")
}

#[test]
fn campd_stops_gracefully_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();

    // A minimal camp: `camp init` writes ./.camp/{camp.toml,camp.db,...}.
    assert_cmd::Command::cargo_bin("camp")
        .unwrap()
        .current_dir(dir.path())
        .env_remove("CAMP_DIR")
        .arg("init")
        .assert()
        .success();
    let camp_root = dir.path().join(".camp");

    // Spawn the long-lived daemon; capture stdout for the readiness line.
    let mut child = Command::new(camp_bin())
        .args(["daemon", "--camp"])
        .arg(&camp_root)
        .env_remove("CAMP_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Block until campd announces readiness (proves it is up before we signal).
    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    let n = BufReader::new(stdout).read_line(&mut line).unwrap();
    assert!(n > 0, "campd must print a readiness line before we signal it");

    // Send SIGTERM via kill(1) (std::process::Child::kill is SIGKILL-only).
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success(), "kill -TERM failed to send");

    // It must exit cleanly (0), not be terminated by the signal's default action.
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("campd did not exit within 10s of SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        exit.success(),
        "SIGTERM must cause a clean exit(0), got {exit:?}"
    );

    // The graceful stop is recorded (same durable event as `camp stop`).
    let ledger =
        camp_core::ledger::Ledger::open_read_only(&camp_root.join("camp.db")).unwrap();
    let stopped = ledger
        .events_of_type(camp_core::event::EventType::CampdStopped)
        .unwrap();
    assert!(
        !stopped.is_empty(),
        "a graceful SIGTERM stop must record campd.stopped"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p camp --test cli_daemon_signal`
Expected: FAIL. Without a handler, SIGTERM triggers the kernel default action (terminate), so `child` is killed by the signal — `exit.success()` is false (terminated by signal 15, not exit 0), and no `campd.stopped` event is written. One or both assertions fail.

- [ ] **Step 3: Register the signal self-pipe in `daemon/mod.rs`**

Immediately after the existing SIGCHLD registration block (the `sigchld_read`/`sigchld_write` pair + `signal_hook::low_level::pipe::register(SIGCHLD, ...)` + `set_nonblocking`, ending ~`daemon/mod.rs:82`), add:

```rust
    // SIGTERM/SIGINT self-pipe: a supervisor (launchd, systemd, the container
    // runtime) stops a service with SIGTERM; SIGINT is Ctrl-C on a foreground
    // `camp daemon`. Both wake the event loop to run the SAME graceful stop as
    // the socket Request::Stop (append campd.stopped, unlink, exit 0).
    // Registered before the loop, exactly like SIGCHLD. Crash-only is
    // unchanged: SIGKILL remains a supported shutdown.
    let (sigterm_read, sigterm_write) =
        std::os::unix::net::UnixStream::pair().context("creating the SIGTERM pipe")?;
    signal_hook::low_level::pipe::register(
        signal_hook::consts::SIGTERM,
        sigterm_write.try_clone().context("cloning the SIGTERM pipe")?,
    )
    .context("registering the SIGTERM handler")?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGINT, sigterm_write)
        .context("registering the SIGINT handler")?;
    sigterm_read
        .set_nonblocking(true)
        .context("setting the SIGTERM pipe non-blocking")?;
```

Then pass `sigterm_read` into the `event_loop::run(...)` call (`daemon/mod.rs:199-212`) as the second argument, immediately after `sigchld_read`:

```rust
    let result = event_loop::run(
        listener,
        sigchld_read,
        sigterm_read,
        &socket_path,
        &mut ledger,
        &mut processor,
        &mut runtime,
        &clock,
        &mut receiver,
        &mut dispatcher,
        &mut graph,
        &mut patrol,
        &mut patrol_receiver,
    );
```

- [ ] **Step 4: Wire the token + handler in `event_loop.rs`**

(a) Token layout (`event_loop.rs:29-41`) — add the reserved token and update the layout comment. After the `PATROL_WATCH` constant add:

```rust
/// SIGTERM/SIGINT self-pipe (Phase 1, campd service management): a
/// supervisor stops campd with SIGTERM; SIGINT is Ctrl-C on a foreground
/// `camp daemon`. Both run the same graceful stop as Request::Stop.
const SIGTERM_SIG: Token = Token(4);
```

Update the layout comment on `CONFIG_WATCH` (`:30-34`) to read `… 3 = patrol watch, 4 = SIGTERM/SIGINT self-pipe, 5+ = connections.`

(b) `run` signature (`event_loop.rs:85-98`) — add the parameter after `sigchld`:

```rust
pub fn run(
    mut listener: UnixListener,
    sigchld: std::os::unix::net::UnixStream,
    sigterm: std::os::unix::net::UnixStream,
    socket_path: &Path,
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    config_rx: &mut mio::unix::pipe::Receiver,
    dispatcher: &mut Dispatcher,
    graph: &mut GraphRuntime,
    patrol: &mut PatrolRuntime,
    patrol_rx: &mut mio::unix::pipe::Receiver,
) -> Result<()> {
```

(c) Registration (after the SIGCHLD registration at `:107-110`) add:

```rust
    let mut sigterm = UnixStream::from_std(sigterm);
    poll.registry()
        .register(&mut sigterm, SIGTERM_SIG, Interest::READABLE)
        .context("registering the SIGTERM pipe")?;
```

(d) Connections start after the new reserved token (`:122`): change `let mut next_token = 4usize;` to `let mut next_token = 5usize;`, and update its comment to `// Tokens 2–4 are RESERVED (SIGCHLD, patrol watch, SIGTERM); connections start at 5.`

(e) Dispatch arm — in the per-event `match` on the token, add a `SIGTERM_SIG` arm immediately before the `token =>` catch-all (`event_loop.rs:289`):

```rust
                SIGTERM_SIG => {
                    // A supervisor (or Ctrl-C) asked us to stop. Run the SAME
                    // graceful path as Request::Stop: durable event, unlink,
                    // exit 0. Draining the self-pipe is moot — we are exiting.
                    stop(ledger, socket_path)?;
                    return Ok(());
                }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p camp --test cli_daemon_signal`
Expected: PASS. campd catches SIGTERM, runs `stop(...)` (writes `campd.stopped`, unlinks the socket), and returns `Ok(())` → process exits 0. Also run the existing daemon tests to confirm no regression: `cargo test -p camp` (the socket `Request::Stop` path, campd alias, and lifecycle tests are unchanged).

- [ ] **Step 6: Gates**

Run: `cargo fmt --all` then `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean (no `unwrap`/`expect`/`panic` added to non-test code; the signal block uses `?`/`context`).

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/daemon/mod.rs crates/camp/src/daemon/event_loop.rs crates/camp/tests/cli_daemon_signal.rs
git commit -m "feat(campd): SIGTERM/SIGINT -> graceful shutdown (supervised-process readiness)"
```

## Self-Review

**1. Spec coverage:** Design §4.6 / §7 (SIGTERM/SIGINT → graceful shutdown, reusing `campd.stopped`, the one campd core change) → Task 1. Design §9 test ("SIGTERM graceful shutdown: send SIGTERM, assert exit 0 + campd.stopped") → Task 1 Step 1. The container-reference and `camp service`/init/autostart items are explicitly out of scope for Phase 1 (later plans).

**2. Placeholder scan:** No TBD/vague steps; every code step is complete. `<camp-root>` etc. do not appear — paths are constructed in the test code.

**3. Type consistency:** `event_loop::run` gains exactly one `UnixStream` param (`sigterm`) in both the signature (Step 4b) and the call site (Step 3); the token constant `SIGTERM_SIG = Token(4)` and `next_token = 5` agree; the dispatch arm reuses the existing `stop(ledger, socket_path)` with the same argument types the `ConnState::Stop` arm uses.

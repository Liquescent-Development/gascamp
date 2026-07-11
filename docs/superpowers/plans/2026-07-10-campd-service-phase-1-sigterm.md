# campd Service Management — Phase 1: SIGTERM/SIGINT Graceful Shutdown — Implementation Plan

> **Plan review: APPROVED 2026-07-10** (independent Opus reviewer, round 2).
> Round 1 rejected on three blocking findings, all fixed in the revision:
> B1 — spec §9's SIGINT test obligation was unmet (SIGINT handler shipped untested);
> B2 — the tests did not assert the socket is unlinked, though the contract is
> "append campd.stopped, drop the socket, exit 0" and the Request::Stop test asserts it;
> B3 — a stale line anchor would have spliced the signal block between the campd.started
> comment and its own ledger.append call.
> Round 2 re-verified every line anchor and claim against the code and approved with
> non-blocking notes only (recorded below).
>
> Non-blocking notes accepted at approval:
> N1 — after this change, the three in-process `daemon::run` unit tests install process-wide
> SIGTERM/SIGINT handlers in the `camp` unit-test binary, so that binary stops dying on Ctrl-C
> once one has run. Unavoidable with in-process run(); not a bug — do not chase it.
> N2 — the Self-Review's citation should be `daemon/mod.rs:299-300` **plus :306** (the
> campd.stopped assertion lives at :306).
> N3 — `daemon/mod.rs:258` is the `#[test]` attribute; the fn is at :259.
> N4 — Step 3 has a markdown typo: the bold markers straddle the code span in
> "**ending at `daemon/mod.rs:76**`". The line number is correct.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make campd shut down gracefully on SIGTERM **and SIGINT** — the same clean stop as the socket `Request::Stop` — so it is a well-behaved supervised process under launchd, systemd, and the container runtime (all of which stop a service with SIGTERM).

**Architecture:** campd's event loop is a mio poll over a fixed set of self-pipe tokens (listener, config watch, SIGCHLD, patrol watch) plus connections. We add one reserved token for a SIGTERM/SIGINT self-pipe (the exact SIGCHLD pattern: `signal_hook::low_level::pipe::register` → a `UnixStream` pair → registered in the loop). When the signal pipe becomes readable, the loop runs the existing `stop(ledger, socket_path)` (append `campd.stopped`, unlink the socket) and returns `Ok(())` — **the same durable outcome as the `ConnState::Stop` path**. (Not *byte-identical*: the `ConnState::Stop` arm also `respond()`s to the client that asked (`event_loop.rs:314`); a signal has nobody to answer. The durable truth — event + unlink + exit 0 — is identical, and that is what spec §9 contracts.) No new event type; crash-only (SIGKILL) stays safe.

Three properties of this design worth stating up front, because a future reader will ask:

- **In-flight `claude -p` workers are orphaned, and that is correct.** On SIGTERM campd appends `campd.stopped`, unlinks, and exits; any running worker child keeps running, is reparented, and `patrol::adopt` reconciles it at the next start (crash-only, spec §8.5). This is *exactly* what today's socket `Request::Stop` does — so the "identical outcome to `Request::Stop`" contract holds, and Phase 1 adds no new reconciliation burden.
  - **Forward-flag to Phase 2 (do not act on it here):** systemd's default `KillMode=control-group` SIGTERMs (then SIGKILLs) *every* process in the unit's cgroup — the workers too. That is a **different** shutdown shape from the one Phase 1 implements and tests. Phase 2 must choose `KillMode` deliberately (`mixed`/`process` vs. the default) rather than inheriting it silently.
- **Stop latency is bounded, not immediate.** campd runs bounded blocking syscalls inline on the loop thread (`bounded::output_bounded` for git worktree ops / adoption probes / `/bin/kill`; `bounded::write_bounded` for nudges). A signal arriving mid-call is only *observed* when the loop next returns to `poll`, so worst-case stop latency is the largest inline bound — not zero. This is correct by construction (that is what the issue #55 bounds are for) and sits far inside systemd's 90 s `TimeoutStopSec` and launchd's 20 s SIGKILL escalation. **Do not "fix" this by checking a flag inside the signal handler** — that would break async-signal-safety. The self-pipe is the whole point: the handler does one non-blocking write, and all real work happens on the loop thread.
- **Double-shutdown is already safe.** If one wake's event batch contains both `SIGTERM_SIG` and a connection carrying `Request::Stop`, whichever the `for event in events.iter()` loop reaches first returns from `run` — `stop()` cannot run twice. A second SIGTERM arriving during `stop()` just writes a byte nobody reads.

**Tech Stack:** Rust; `signal-hook` (already a dep, used for SIGCHLD); `mio` (already a dep); integration test driving the real binary via `env!("CARGO_BIN_EXE_camp")` + `std::process::Command` + `kill(1)` — the existing precedent in `crates/camp/tests/daemon_lifecycle.rs:11`. (No `assert_cmd` needed: `camp` is a **bin-only crate** — there is no `crates/camp/src/lib.rs` — so a test cannot import from it either; `READY_PREFIX` is re-declared in the test file exactly as `daemon_lifecycle.rs:12` does.)

**Phasing:** This is Phase 1 of the campd-service-management design (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`). Phases 2–4 (`camp service` + unit generation + env-aware init; remove CLI auto-start → pure client; container reference + docs + spec amendments) are separate plans authored after this merges. SIGTERM/SIGINT handling is independent and shippable on its own.

## Global Constraints

Copied from the design spec and `AGENTS.md` — every task implicitly includes these:

- **No new event types** — a graceful signal stop reuses `campd.stopped` (invariant 7, vocabulary mirror, untouched).
- **Crash-only preserved** — SIGKILL remains a supported shutdown; this only makes the *SIGTERM/SIGINT* path clean.
- **Invariant 5 fail-fast** — no fallbacks, no silenced errors, no placeholders. NO `unwrap`/`expect`/`panic` in non-test code (clippy `unwrap_used`/`expect_used`/`panic` denied; `unsafe` forbidden). Test files opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- **Invariant 1** — no ticks/polling added; the signal wakes the existing poll via a self-pipe (an OS event).
- **TDD strictly** — failing test first, watch it fail, implement, watch it pass.
- Branch **`phase-1-campd-sigterm`** (already created off `main`; `phase-N-<slug>`, per AGENTS.md). Never commit to main. No co-author lines / no self-attribution.
- **Token layout is coordinated** (event_loop.rs comment): today 0=listener, 1=config, 2=SIGCHLD, 3=patrol, 4+=connections. This plan adds `4=SIGTERM/SIGINT` and moves connections to `5+`. Update the layout comment to match.
- Gates green before push: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`.
- **Nothing is complete until it is pushed, CI is green, and every claim in the PR description is verified** (AGENTS.md). Step 8 owns that.

## Exit criteria

Phase 1 is done — and only done — when all of these hold:

1. `cargo test -p camp --test cli_daemon_signal` passes **both** `campd_stops_gracefully_on_sigterm` and `campd_stops_gracefully_on_sigint`.
2. Each test asserts all **three** parts of the spec §7 contract: exit code 0, a `campd.stopped` event, and the socket file unlinked.
3. `cargo test --workspace` shows no regression (notably `daemon_serves_status_poke_and_stop_over_the_socket` and the `daemon_lifecycle` suite).
4. `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` are clean.
5. No new event type; no `unwrap`/`expect`/`panic`/`unsafe` in non-test code; no tick/poll loop added.
6. Pushed to `phase-1-campd-sigterm`, PR opened against `main`, and `gh pr checks --watch` is green.

## File Structure

- `crates/camp/src/daemon/mod.rs` — **modify**: register a SIGTERM/SIGINT self-pipe (mirror of the SIGCHLD block), pass its read end into `event_loop::run(...)`.
- `crates/camp/src/daemon/event_loop.rs` — **modify**: add the `SIGTERM_SIG = Token(4)` reserved token (connections → `5+`), add the `sigterm` param to `run(...)`, register it, and add the dispatch arm that runs `stop(...)` and returns.
- `crates/camp/tests/cli_daemon_signal.rs` — **create**: integration tests spawning a real `camp daemon` and sending **SIGTERM** and **SIGINT**, each asserting a clean exit 0 + `campd.stopped` + the socket unlinked.

---

## Task 1: SIGTERM/SIGINT → graceful shutdown

**Files:**
- Create: `crates/camp/tests/cli_daemon_signal.rs`
- Modify: `crates/camp/src/daemon/mod.rs:67-76` — the existing SIGCHLD block (its comment at :67-69, its code at :70-76, ending at `.context("setting the SIGCHLD pipe non-blocking")?;` on **:76**). The new signal block goes **immediately after :76**, i.e. before the blank line at :77 and the `campd.started` comment block at **:78-81** whose `ledger.append(EventInput {` is at **:82**. *(Verified against the file at e895b3b. Do not insert "after ~:82" — :82 is the `campd.started` append itself; splicing there would orphan the comment from its code.)*
- Modify: `crates/camp/src/daemon/mod.rs:199-212` (the `event_loop::run(...)` call — the only caller in the workspace)
- Modify: `crates/camp/src/daemon/event_loop.rs:29-41` (token layout), `:84-123` (`run` signature + registration + `next_token`), and `:289` (add the dispatch arm immediately before the `token =>` catch-all)

**Interfaces:**
- Consumes: `signal_hook::low_level::pipe::register`, `signal_hook::consts::{SIGTERM, SIGINT}`; `std::os::unix::net::UnixStream`; the existing private `event_loop::stop(ledger: &mut Ledger, socket_path: &Path) -> Result<()>` helper (`event_loop.rs:712` — already appends `campd.stopped` + unlinks; called today from the `ConnState::Stop` arm at `event_loop.rs:313`, and callable from the new arm because both live inside `run`'s module); `camp_core::ledger::Ledger::open_read_only` + `events_of_type` + `camp_core::event::EventType::CampdStopped` (test); `env!("CARGO_BIN_EXE_camp")` (test).
- Produces: no new public API; `event_loop::run` gains one `UnixStream` parameter (`sigterm`) immediately after `sigchld`. The `#[allow(clippy::too_many_arguments)]` at `event_loop.rs:84` is already present, so a 13th parameter needs no new allow.

- [ ] **Step 1: Write the failing tests (SIGTERM *and* SIGINT)**

Spec §9 obligates **both** signals, verbatim: *"spawn `camp daemon` in a temp camp, send SIGTERM, assert it exits 0 and appends `campd.stopped` — identical outcome to a socket `Request::Stop`. **Same for SIGINT.**"* Both tests therefore run **one shared helper** — so "identical outcome" is a fact about the code path, not a claim in a comment.

Create `crates/camp/tests/cli_daemon_signal.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 1 (campd service management): campd shuts down gracefully on
//! SIGTERM *and* SIGINT — the same clean stop as the socket `Request::Stop`
//! (append `campd.stopped`, unlink the socket, exit 0; spec §7, §9) — so it
//! is a well-behaved supervised process. launchd/systemd/the container
//! runtime all stop a service with SIGTERM; SIGINT is Ctrl-C on a foreground
//! `camp daemon`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// Repo precedent (`daemon_lifecycle.rs:11-12`). `camp` is a bin-only crate
// (no `src/lib.rs`), so `daemon::READY_PREFIX` cannot be imported into an
// integration test — it is re-declared here, exactly as daemon_lifecycle does.
const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

/// The whole phase contract, once. Both signal tests call THIS — so the
/// "identical outcome" in spec §9 is enforced by construction rather than
/// asserted twice and allowed to drift.
fn graceful_stop_on(signal: &str) {
    let dir = tempfile::tempdir().unwrap();

    // A minimal camp: `camp init` writes ./.camp/{camp.toml,camp.db}.
    let init = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir.path())
        .arg("init")
        .status()
        .unwrap();
    assert!(init.success(), "camp init failed");
    let camp_root = dir.path().join(".camp");

    // Spawn the long-lived daemon; capture stdout for the readiness line.
    let mut child = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["daemon", "--camp"])
        .arg(&camp_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    // Block until campd announces readiness — an OS pipe read, not a
    // sleep/retry loop. Assert the PREFIX, not merely that some bytes
    // arrived: that distinguishes "campd is up and listening" from "campd
    // printed something and died".
    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    assert!(
        line.starts_with(READY_PREFIX),
        "unexpected first line from campd: {line:?}"
    );

    // Signal the child's POSITIVE pid. `Command::spawn` does not put the
    // child in a new process group (nothing in this repo sets setsid /
    // process_group for it), so it shares the test runner's pgroup — a
    // negative-pgid form would signal the test harness itself.
    // kill(1) rather than a libc dep: `Child::kill` is SIGKILL-only.
    let sent = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(sent.success(), "kill -{signal} failed to send");

    // (1 of 3) It exits CLEANLY — not terminated by the signal's default action.
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("campd did not exit within 10s of SIG{signal}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        exit.success(),
        "SIG{signal} must cause a clean exit(0), got {exit:?}"
    );

    // (2 of 3) The graceful stop is DURABLE — the same event as `camp stop`.
    let ledger =
        camp_core::ledger::Ledger::open_read_only(&camp_root.join("camp.db")).unwrap();
    let stopped = ledger
        .events_of_type(camp_core::event::EventType::CampdStopped)
        .unwrap();
    assert!(
        !stopped.is_empty(),
        "a graceful SIG{signal} stop must record campd.stopped"
    );

    // (3 of 3) The socket is DROPPED. This is the part that bites under a
    // KeepAlive/Restart=always supervisor: the restart hits
    // `socket::bind_or_replace` against a stale socket. The Request::Stop
    // test asserts exactly this (`daemon/mod.rs:300`), so the signal path is
    // held to the same standard as the path it claims to be identical to.
    assert!(
        !camp_root.join("campd.sock").exists(),
        "a graceful signal stop must unlink the socket, exactly like Request::Stop"
    );
}

#[test]
fn campd_stops_gracefully_on_sigterm() {
    graceful_stop_on("TERM");
}

#[test]
fn campd_stops_gracefully_on_sigint() {
    graceful_stop_on("INT");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp --test cli_daemon_signal`

Expected: **BOTH tests FAIL** (red), for the same reason:

- `campd_stops_gracefully_on_sigterm` — with no handler installed, SIGTERM takes the kernel default action (terminate). The child is *killed by the signal*, so `exit.success()` is false (terminated by signal 15, not exit 0). The later assertions never run; if you reorder them, note that no `campd.stopped` is written and the socket is left behind too — all three parts of the contract are unmet.
- `campd_stops_gracefully_on_sigint` — **identical red state**: SIGINT's kernel default is *also* terminate (signal 2). It fails on the same `exit.success()` assertion, and for the same reason. Seeing both fail this way is the proof that neither signal is handled today.

Do not proceed until you have watched both fail.

- [ ] **Step 3: Register the signal self-pipe in `daemon/mod.rs`**

**Where, and why there.** Insert immediately after the existing SIGCHLD registration block — the `sigchld_read`/`sigchld_write` pair + `signal_hook::low_level::pipe::register(SIGCHLD, …)` + `set_nonblocking`, **ending at `daemon/mod.rs:76`** — and therefore **before** the `campd.started` comment block at `:78-81` and its `ledger.append(EventInput { … })` at `:82`. (Do **not** insert after :82; that is the append itself.)

The position is deliberate, not incidental: it is after `socket::bind_or_replace` (`:61`, so we only claim the signal once we own the socket) and after SIGCHLD, but before the long startup work (`campd.started`, the settle, adoption, cron catch-up). A signal arriving in the window *before* the register call still takes the kernel default (terminate) — unavoidable, and acceptable, since campd has done nothing durable yet. A signal arriving *after* it, anywhere in that long startup, **queues a byte in the pipe** that the very first `poll` consumes into a clean `campd.stopped`. That is what makes the test's readiness-line handshake race-free rather than lucky.

```rust
    // SIGTERM/SIGINT self-pipe: a supervisor (launchd, systemd, the container
    // runtime) stops a service with SIGTERM; SIGINT is Ctrl-C on a foreground
    // `camp daemon`. Both wake the event loop to run the SAME graceful stop as
    // the socket Request::Stop (append campd.stopped, unlink, exit 0).
    // Registered here — after the bind, before the startup work — so a signal
    // landing anywhere in the long startup queues a byte the first poll
    // consumes, instead of killing us mid-catch-up. Crash-only is unchanged:
    // SIGKILL remains a supported shutdown. The handler itself is signal-hook's
    // one non-blocking write; all real work happens on the loop thread.
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

Two registrations sharing one write end is the documented signal-hook shape (`register` takes the pipe **by value**: `P: Into<OwnedFd>`, hence the `try_clone()` for the first). Either signal writes a byte; the loop only needs to know that *a* stop was requested, so write-collating is exactly right.

Then pass `sigterm_read` into the `event_loop::run(...)` call (`daemon/mod.rs:199-212`) immediately after `sigchld_read`:

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

- [ ] **Step 4: Wire the token + dispatch arm in `event_loop.rs`**

(a) Token layout (`event_loop.rs:29-41`) — add the reserved token after the `PATROL_WATCH` constant (`:41`):

```rust
/// SIGTERM/SIGINT self-pipe (Phase 1, campd service management): a
/// supervisor stops campd with SIGTERM; SIGINT is Ctrl-C on a foreground
/// `camp daemon`. Both run the same graceful stop as Request::Stop.
const SIGTERM_SIG: Token = Token(4);
```

Update the authoritative layout comment on `CONFIG_WATCH` (`:30-34`) to end `… 3 = Phase 11's patrol transcript-watch self-pipe, 4 = Phase 1's SIGTERM/SIGINT self-pipe, 5+ = connections.` (The comment says "coordinate with the lead before renumbering" — this plan *is* that coordination; the renumber is connections 4+ → 5+.)

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

(c) Registration — immediately after the SIGCHLD registration (`:107-110`), mirroring it exactly (`mio::net::UnixStream` is already imported at `:20`):

```rust
    let mut sigterm = UnixStream::from_std(sigterm);
    poll.registry()
        .register(&mut sigterm, SIGTERM_SIG, Interest::READABLE)
        .context("registering the SIGTERM pipe")?;
```

(d) Connections start after the new reserved token (`:120-122`): change `let mut next_token = 4usize;` to `let mut next_token = 5usize;`, and update its comment to `// Tokens 2–4 are RESERVED (SIGCHLD, patrol watch, SIGTERM/SIGINT — the layout above); connections start at 5.`

(e) Dispatch arm — in the per-event `match` on the token, add a `SIGTERM_SIG` arm **immediately before the `token =>` catch-all at `:289`** (a const arm before the binding arm is the same structural-match shape `LISTENER`/`SIGCHLD`/`PATROL_WATCH` already use; put it after them, before the catch-all, or the catch-all swallows it):

```rust
                SIGTERM_SIG => {
                    // A supervisor (or Ctrl-C) asked us to stop. Run the SAME
                    // graceful path as Request::Stop: durable event, unlink,
                    // exit 0. No respond() — a signal has nobody to answer.
                    // Draining the self-pipe is moot: we are exiting, and the
                    // fd dies with us. In-flight connection events later in
                    // this same batch are dropped, exactly as the
                    // ConnState::Stop arm drops them — durable truth is
                    // already appended.
                    stop(ledger, socket_path)?;
                    return Ok(());
                }
```

`stop(...)` is fallible and stays fail-fast: a failed unlink propagates `Err` → exit 1. That is inherited from today's `Request::Stop` path, not a Phase 1 regression. (Phase 2 must decide deliberately whether a generated unit treats exit 1 as restart-worthy — flagged, not solved here.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p camp --test cli_daemon_signal`
Expected: **PASS, both tests.** campd catches SIGTERM (and SIGINT), the self-pipe byte wakes `poll`, the loop runs `stop(...)` (writes `campd.stopped`, unlinks the socket), and returns `Ok(())` → `main.rs`'s `report()` maps `Ok(())` → `ExitCode::SUCCESS` → the process exits 0.

Then confirm no regression across the crate: `cargo test -p camp` — in particular `daemon_serves_status_poke_and_stop_over_the_socket` (the socket `Request::Stop` path, `daemon/mod.rs:259`) and the `daemon_lifecycle` suite, which are unchanged by this work. Then `cargo test --workspace`.

- [ ] **Step 6: Gates**

Run: `cargo fmt --all` then `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean. No `unwrap`/`expect`/`panic` added to non-test code (the signal block is all `?` + `.context(...)`); no `unsafe`; the test file carries the standard `#![allow(...)]` header.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/daemon/mod.rs crates/camp/src/daemon/event_loop.rs crates/camp/tests/cli_daemon_signal.rs
git commit -m "feat(campd): SIGTERM/SIGINT -> graceful shutdown (supervised-process readiness)"
```

- [ ] **Step 8: Push, open the PR, and drive CI green**

AGENTS.md: *"Nothing is complete until it is pushed, CI is green, and every claim in the PR description is verified."* The implementer owns this step.

```bash
git push -u origin phase-1-campd-sigterm
gh pr create --base main --title "feat(campd): SIGTERM/SIGINT -> graceful shutdown (Phase 1)" --body "…"
gh pr checks --watch
```

The PR body states what is verified: both signals exit 0, both append `campd.stopped`, both unlink the socket, no new event type, crash-only preserved. Do not claim the phase is complete until `gh pr checks --watch` reports green.

## Self-Review

**1. Spec coverage.** Design §4.6 / §7 (SIGTERM/SIGINT → graceful shutdown — "append `campd.stopped`, drop the socket, exit 0" — reusing `campd.stopped`; the one campd core change) → Task 1. Design §9's test obligation, quoted in **full**:

> **SIGTERM graceful shutdown:** spawn `camp daemon` in a temp camp, send SIGTERM, assert it exits 0 and appends `campd.stopped` — identical outcome to a socket `Request::Stop`. **Same for SIGINT.**

→ Task 1 Step 1, **both** sentences: `campd_stops_gracefully_on_sigterm` **and** `campd_stops_gracefully_on_sigint`, sharing one `graceful_stop_on(signal)` helper so the "identical outcome" is structural. All **three** observable parts of the contract are asserted in that helper (exit 0, `campd.stopped`, socket unlinked) — the same three the existing `Request::Stop` test asserts (`daemon/mod.rs:299-300` plus `:306`, where the `campd.stopped` assertion lives). The container reference, `camp service`/unit generation, and the auto-start removal are explicitly out of scope for Phase 1 (Phases 2–4).

**2. Placeholder scan.** No TBD/vague steps; every code step is complete and every line anchor was verified against the source at e895b3b. `<camp-root>` etc. do not appear — paths are constructed in the test code.

**3. Type consistency.** `event_loop::run` gains exactly one `UnixStream` param (`sigterm`) in both the signature (Step 4b) and its single call site (Step 3, `daemon/mod.rs:199-212` — the only caller in the workspace); the token constant `SIGTERM_SIG = Token(4)` and `next_token = 5usize` agree; the dispatch arm reuses the existing private `stop(ledger, socket_path)` with the same argument types the `ConnState::Stop` arm passes it (`event_loop.rs:313`).

**4. Scope discipline.** No change to `docs/design/2026-07-05-gas-camp-design.md` (its §5/§9/§12 amendments belong to Phase 4). No new event types. No `libc` dependency (the workspace has none; `kill(1)` is the right tool for a test). Phase 2's `KillMode` decision is flagged in the Architecture section, not acted on.

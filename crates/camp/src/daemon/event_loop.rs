//! The campd event loop (spec §5, §15.1): mio poll over the listener,
//! per-connection reads, the camp.toml watch pipe, the SIGCHLD self-pipe
//! (Phase 8 worker reaping), and the cron heap. The
//! poll timeout is the earliest armed timer deadline
//! (`OrdersRuntime::poll_timeout` — the only timeout expression in campd);
//! an idle heap means `None` and the idle daemon blocks in `poll` with
//! zero wakeups (invariant 1).

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use camp_core::clock::Clock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::orders::cron::Fire;
use jiff::{SignedDuration, Timestamp};
use mio::net::{UnixListener, UnixStream};
use mio::{Events, Interest, Poll, Token};

use super::cursor::{self, ReadinessProcessor};
use super::dispatch::{Dispatcher, GraphRuntime, ReapFailure};
use super::orders::{self, OrdersRuntime};
use super::patrol::PatrolRuntime;
use super::socket::{Request, Response};

const LISTENER: Token = Token(0);
/// The notify→mio self-pipe (camp.toml watch). Authoritative campd token
/// layout (lead ruling, PR #13 review MEDIUM 4; Phase 11 plan Decision I,
/// approved): 0 = listener, 1 = config watch, 2 = Phase 8's SIGCHLD
/// self-pipe, 3 = Phase 11's patrol transcript-watch self-pipe, 4 = Phase
/// 1's SIGTERM/SIGINT self-pipe (campd service management), 5 = cp-0's
/// read-channel stream-watch self-pipe (control-plane spec §2.3), 6+ =
/// connections. Coordinate with the lead before renumbering.
const CONFIG_WATCH: Token = Token(1);
/// Phase 8's SIGCHLD self-pipe (worker death detection, spec §10.1), per
/// the shared token layout above.
const SIGCHLD: Token = Token(2);
/// Phase 11's patrol transcript-watch self-pipe (spec §10.2), per the
/// shared token layout above.
const PATROL_WATCH: Token = Token(3);
/// SIGTERM/SIGINT self-pipe (Phase 1, campd service management): a
/// supervisor stops campd with SIGTERM; SIGINT is Ctrl-C on a foreground
/// `camp daemon`. Both run the same graceful stop as Request::Stop.
const SIGTERM_SIG: Token = Token(4);
/// cp-0's read-channel stream-watch self-pipe (control-plane spec §2.3):
/// the notify watcher on the sessions/ directory signals through this
/// pipe; the drain-all-on-every-wake rule makes it a latency-only wake.
const READ_WATCH: Token = Token(5);

/// Upper bound on a single request line (PR #8 review finding 3). Real
/// requests are tens of bytes; the cap keeps a broken or hostile client
/// from ballooning campd's RSS past the idle budget (invariant 1). A
/// connection whose buffered line fragment exceeds this is answered with a
/// clean error and dropped.
pub(super) const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// How many times a failed reap may self-raise SIGCHLD before degrading to
/// log-and-wait (PR #14 fix-pass NEW MEDIUM). Each retryable failure is
/// SQLite contention already bounded by the 5 s busy_timeout, so the
/// budget bounds total retry work; a persistent failure then waits for the
/// next natural wake instead of hot-spinning (invariant 1).
const SELF_RAISE_BUDGET: u32 = 3;

/// Whether a failed reap earns a SIGCHLD self-raise: only retryable
/// failures, only while the budget lasts. The caller resets the budget on
/// success.
fn should_self_raise(retryable: bool, budget: &mut u32) -> bool {
    if !retryable || *budget == 0 {
        return false;
    }
    *budget -= 1;
    true
}

/// Wall-vs-monotonic divergence beyond this is a wall-clock jump
/// (sleep/wake, NTP step — spec §9): deadlines recompute and missed fires
/// within each order's catch-up window fire once.
const JUMP_TOLERANCE: SignedDuration = SignedDuration::from_secs(30);

/// One accepted connection. `pub(super)` because cp-1's `control.rs` owns the
/// subscriber write path (`pump` is the ONLY place bytes reach a subscriber's
/// socket) and must be handed the stream.
pub struct Conn {
    pub(super) stream: UnixStream,
    pub(super) buf: Vec<u8>,
}

enum ConnState {
    Open,
    Closed,
    Stop,
}

#[allow(clippy::too_many_arguments)]
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
    read_channel: &mut super::read_channel::ReadChannelRuntime,
    read_rx: &mut mio::unix::pipe::Receiver,
    control: &mut super::control::ControlRuntime,
) -> Result<()> {
    let mut poll = Poll::new().context("creating the poller")?;
    let mut events = Events::with_capacity(64);
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)
        .context("registering the listener")?;
    poll.registry()
        .register(config_rx, CONFIG_WATCH, Interest::READABLE)
        .context("registering the config watch pipe")?;
    let mut sigchld = UnixStream::from_std(sigchld);
    poll.registry()
        .register(&mut sigchld, SIGCHLD, Interest::READABLE)
        .context("registering the SIGCHLD pipe")?;
    poll.registry()
        .register(patrol_rx, PATROL_WATCH, Interest::READABLE)
        .context("registering the patrol watch pipe")?;
    poll.registry()
        .register(read_rx, READ_WATCH, Interest::READABLE)
        .context("registering the read-channel watch pipe")?;
    let mut sigterm = UnixStream::from_std(sigterm);
    poll.registry()
        .register(&mut sigterm, SIGTERM_SIG, Interest::READABLE)
        .context("registering the SIGTERM pipe")?;
    // The connection map is bounded by the process fd limit — the natural
    // cap for a single-user local socket. An artificial cap was considered
    // (PR #8 review finding 3) and rejected: it would reject legitimate
    // bursts, and per-connection memory is already bounded by
    // MAX_REQUEST_BYTES.
    let mut conns: HashMap<Token, Conn> = HashMap::new();
    // Tokens 2–5 are RESERVED (SIGCHLD, patrol watch, SIGTERM/SIGINT, cp-0
    // read-channel watch — the layout above); connections start at 6.
    let mut next_token = 6usize;
    let mut self_raise_budget = SELF_RAISE_BUDGET;
    // cp-3 (§5.3.2): dedup the loud `permission.saturated` fault to the crossing
    // edge — emit once when the BLOCKED count crosses `max_blocked`, clear when
    // it drops back, never once per wake.
    let mut saturated = false;

    let mut last_seen = Timestamp::now();
    loop {
        // Decision 11c: THE poll-timeout composition point. Each deadline
        // source converts its own deadline to a Duration-from-now (cron is
        // wall-anchored, checks are monotonic, patrol stall timers are
        // wall-anchored). Phase 11's stall timers join this same
        // combinator (Decision I) — THREE sources, earliest wins.
        let poll_now = Timestamp::now();
        let timeout = min_deadline(
            min_deadline(
                min_deadline(
                    runtime.poll_timeout(poll_now),
                    graph.poll_timeout(Instant::now()),
                ),
                patrol.poll_timeout(poll_now),
            ),
            // cp-1: the control plane's deadlines — a pending control request's
            // silence/ceiling bound, and (Task 8) the subscriber continuation
            // and stall timer. `None` when nothing is pending, so an idle campd
            // still blocks forever (invariant 1).
            control.poll_timeout(poll_now),
        );
        let wall_before = Timestamp::now();
        let mono_before = Instant::now();
        if let Err(e) = poll.poll(&mut events, timeout) {
            // A signal (SIGCHLD) interrupting the wait is not an error:
            // restart the loop — the timeout recomputes, due fires pop,
            // and the self-pipe byte is readable.
            if e.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(e).context("poll");
        }
        // Each wake compares expected vs actual wall time (spec §9): a
        // jump recomputes every deadline; an honest wake pops due fires.
        // Both paths apply the same catch-up window rule, so platforms
        // whose poll timeout ticks through a system sleep behave
        // identically to detected jumps.
        let now = Timestamp::now();
        let wall_delta = now.duration_since(wall_before);
        let mono_delta =
            SignedDuration::try_from(mono_before.elapsed()).unwrap_or(SignedDuration::MAX);
        let fires: Vec<Fire> = if (wall_delta - mono_delta).abs() > JUMP_TOLERANCE {
            // into_fire computes the catch_up flag exactly as fire_due
            // does (PR #13 fix-pass review: one rule, one flag).
            runtime
                .recompute(now, last_seen)
                .into_iter()
                .map(|c| c.into_fire(now))
                .collect()
        } else {
            runtime.fire_due(now)
        };
        last_seen = now;
        // Decision 11d: enforce check deadlines on every wake (a
        // deadline-only wake has no other trigger). Kills convert to
        // SIGCHLD -> reap_checks -> timed-out check.failed verdicts.
        graph.kill_expired(Instant::now());
        // Declare the fires (durable first); the settle below cooks them.
        // A ledger that refuses the declaration is fatal — campd must not
        // run automation it cannot record.
        let mut wake_ledger_work = orders::declare_cron_fires(ledger, &fires)?;
        // Patrol stall fires: same declare-then-act shape (Phase 11).
        // agent.stalled lands durably here; the settle executes the
        // queued ladder actions. §5.3.3: the ladder's FIRST act each due wake is
        // to drain the read channel, so a `can_use_tool` whose notify was lost
        // surfaces as BLOCKED before any stall is declared against a worker that
        // is only waiting on us (`stall_step`).
        wake_ledger_work |= stall_step(
            ledger,
            patrol,
            control,
            dispatcher,
            read_channel,
            &mut conns,
            &mut poll,
            now,
        )?;
        for event in events.iter() {
            match event.token() {
                LISTENER => loop {
                    match listener.accept() {
                        Ok((mut stream, _addr)) => {
                            let token = Token(next_token);
                            next_token += 1;
                            // cp-1: WRITABLE too — a subscriber's socket blocks,
                            // and the WRITABLE edge is what re-arms its pump (G2:
                            // arming a zero poll timeout on a blocked write would
                            // SPIN campd for the duration of every stream).
                            // Edge-triggered poll reports writability ONCE at
                            // registration: an accept-time cost, not an idle one —
                            // and that already-consumed edge is exactly why the
                            // hello's first bytes need an explicit pump (B11).
                            poll.registry()
                                .register(
                                    &mut stream,
                                    token,
                                    Interest::READABLE | Interest::WRITABLE,
                                )
                                .context("registering a connection")?;
                            conns.insert(
                                token,
                                Conn {
                                    stream,
                                    buf: Vec::new(),
                                },
                            );
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e).context("accept"),
                    }
                },
                SIGCHLD => {
                    drain_signal_pipe(&mut sigchld, "SIGCHLD")?;
                    // Reap → record ends → settle (catch up, cook, refill
                    // capacity). Errors are reported, never fatal: a broken
                    // child must not take campd down, and unrecorded exits
                    // are retried next wake (try_wait re-returns the
                    // status).
                    match reap_and_refill(
                        ledger,
                        processor,
                        runtime,
                        clock,
                        dispatcher,
                        graph,
                        patrol,
                        read_channel,
                    ) {
                        Ok(()) => self_raise_budget = SELF_RAISE_BUDGET,
                        Err(failure) => {
                            eprintln!("campd: reap failed: {failure}");
                            // Re-raise SIGCHLD to self (PR #14 review
                            // finding 3): if this was the LAST live child,
                            // no further SIGCHLD ever comes and an idle
                            // camp would never retry. Bounded (fix-pass NEW
                            // MEDIUM): only retryable failures (SQLite
                            // contention, each attempt already bounded by
                            // the 5 s busy_timeout) and only while the
                            // budget lasts — a persistent failure degrades
                            // to log-and-wait for the next natural wake
                            // instead of a hot self-raise loop
                            // (invariant 1). try_wait OS errors never
                            // self-raise.
                            if should_self_raise(failure.retryable, &mut self_raise_budget) {
                                if let Err(raise_err) =
                                    signal_hook::low_level::raise(signal_hook::consts::SIGCHLD)
                                {
                                    eprintln!("campd: SIGCHLD self-raise failed: {raise_err}");
                                }
                            } else {
                                eprintln!(
                                    "campd: not self-raising (retryable: {}, budget: {}); \
                                     the next wake retries",
                                    failure.retryable, self_raise_budget
                                );
                            }
                        }
                    }
                }
                PATROL_WATCH => {
                    drain_pipe(patrol_rx)?;
                    // Transcript activity → timer resets (the watch IS the
                    // heartbeat, spec §10.2); watcher errors → durable
                    // patrol.degraded (the LOW-8 mold).
                    patrol.drain_touched(now);
                    for input in patrol.take_watch_error_events() {
                        ledger.append(input)?;
                        wake_ledger_work = true;
                    }
                }
                READ_WATCH => {
                    drain_pipe(read_rx)?;
                    // The watch is a latency-only wake (§2.3); drain_all
                    // runs in the common path below regardless of token.
                }
                CONFIG_WATCH => {
                    drain_pipe(config_rx)?;
                    // A dead watcher is a durable, rejected config.changed —
                    // hot reload degraded, in the ledger (PR #13 review
                    // LOW 8), never just a stderr line.
                    if let Some(input) = runtime.take_watch_error_event() {
                        ledger.append(input)?;
                        wake_ledger_work = true;
                    }
                    if let Some(input) = runtime.reload_if_changed(now)? {
                        // spec §13.4: the config change is itself an event,
                        // applied or rejected.
                        let applied = input.data["applied"].as_bool() == Some(true);
                        ledger.append(input)?;
                        wake_ledger_work = true;
                        // Issue #28: an APPLIED reload must reach dispatch,
                        // not just the order scheduler. Push the new config
                        // into the dispatcher (routing + max_workers) and
                        // the graph runtime's rig snapshot BEFORE the settle
                        // below converges — so a new pack/agent/rig/
                        // default_agent takes effect with no restart. The
                        // ledger's `applied:true` already means the runtime
                        // swapped its state, so this never runs on a rejected
                        // torn write.
                        if applied {
                            dispatcher.apply_config(runtime.config().clone());
                            graph.apply_config(runtime.config());
                            // Issue #81: patrol resolves agents/rigs/
                            // thresholds against its own cached config too —
                            // an applied reload must reach it, or a worker
                            // dispatched to a freshly added pack agent draws a
                            // spurious patrol.degraded "unknown agent".
                            patrol.apply_config(runtime.config().clone())?;
                        }
                    }
                }
                SIGTERM_SIG => {
                    // Drain FIRST, then decide — signal-hook's prescribed
                    // order for its pipe module, and the one arm in this loop
                    // that cannot shrug off a spurious readable (mio documents
                    // that events MAY be spurious; here acting on one would
                    // EXIT the daemon). A byte in the pipe is the only proof a
                    // signal was actually delivered; no byte, no stop.
                    if drain_signal_pipe(&mut sigterm, "SIGTERM/SIGINT")? == 0 {
                        continue;
                    }
                    // A supervisor (or Ctrl-C) asked us to stop. Run the SAME
                    // graceful path as Request::Stop: durable event, unlink,
                    // exit 0. No respond() — a signal has nobody to answer. A
                    // second signal racing this stop just writes a byte nobody
                    // reads; the fd dies with us. In-flight connection events
                    // later in this same batch are dropped, exactly as the
                    // ConnState::Stop arm drops them — durable truth is
                    // already appended.
                    stop(ledger, socket_path)?;
                    return Ok(());
                }
                token => {
                    let Some(mut conn) = conns.remove(&token) else {
                        continue; // already dropped this cycle
                    };
                    // cp-1 (B7): a subscriber's WRITABLE wake is why we are here —
                    // pump it. But there is NO SHORT-CIRCUIT: we then fall through
                    // into `serve_connection` like any other connection, so cp-0's
                    // `ReadStop::Eof => ConnState::Closed` still detects a hangup.
                    // A subscriber that simply detaches is NOT a fault and appends
                    // NO event (§5.2).
                    if control.is_subscriber(token) {
                        let outcome = control.pump(token, &mut conn, Timestamp::now());
                        for input in control.take_pending_events() {
                            ledger.append(input)?;
                        }
                        match outcome {
                            super::control::PumpOutcome::Ok => {}
                            super::control::PumpOutcome::Gone => {
                                control.forget(token);
                                let _ = poll.registry().deregister(&mut conn.stream);
                                continue;
                            }
                            super::control::PumpOutcome::Drop(event) => {
                                ledger.append(event)?;
                                control.forget(token);
                                let _ = poll.registry().deregister(&mut conn.stream);
                                continue;
                            }
                        }
                    }
                    match serve_connection(
                        &mut conn,
                        ledger,
                        processor,
                        runtime,
                        clock,
                        dispatcher,
                        graph,
                        patrol,
                        read_channel,
                        control,
                        token,
                        MAX_REQUEST_BYTES,
                    ) {
                        Ok(ConnState::Open) => {
                            conns.insert(token, conn);
                        }
                        Ok(ConnState::Closed) => {
                            control.forget(token);
                            poll.registry().deregister(&mut conn.stream)?;
                        }
                        Ok(ConnState::Stop) => {
                            // Durable truth first, then the goodbye:
                            // event → unlink → respond → exit.
                            stop(ledger, socket_path)?;
                            let _ = respond(&mut conn.stream, &Response::Ok { ok: true });
                            return Ok(());
                        }
                        Err(error) => {
                            // A broken client must not take campd down; the
                            // error is reported, the connection dropped.
                            eprintln!("campd: connection error: {error:#}");
                            control.forget(token);
                            let _ = poll.registry().deregister(&mut conn.stream);
                        }
                    }
                }
            }
        }
        if wake_ledger_work {
            // Timer-path settle errors mirror Phase 7 decision H: surface
            // to stderr, keep serving; the cursor holds position and the
            // error re-surfaces on the next poke. The joint settle also
            // dispatches whatever the cooks made ready (Phase 8), in this
            // same wake.
            if let Err(e) = settle(
                ledger,
                processor,
                runtime,
                clock,
                dispatcher,
                graph,
                patrol,
                read_channel,
            ) {
                eprintln!("campd: settle failed: {e:#}");
            }
        }
        // cp-0 (§2.3): on EVERY wake — any poll token — drain every tailed
        // stream file to EOF before going back to sleep. The watch only
        // makes the common case fast; correctness never depends on a
        // delivered event. This runs AFTER settle, so sessions registered
        // by this wake's settle are drained on this same wake (no lag).
        // The `apply_tracking` here is the safety net for the no-settle-
        // ran case (settle already applied this wake's ops when it ran);
        // idempotent via `mem::take`-drained `track_ops`.
        // review fix 7: `apply_tracking` does LEDGER work (register loads the
        // persisted cursor; the deferred unregister clears it). Fix 8's
        // non-fatal carve-out was for per-session FILE I/O (open/seek/read/
        // stat) ONLY — never for ledger writes. Dropping a failed cursor
        // write to stderr leaves campd re-reading from a stale offset on
        // every restart while looking healthy (invariant 5: stderr is
        // neither the caller nor the ledger). Fail fast — this matches the
        // `apply_tracking(ledger)?` in `settle`.
        read_channel.apply_tracking(ledger)?;
        // cp-0 fix 8: drain_all is non-fatal — drain_one captures per-
        // session errors into the drain_errors collector (campd keeps
        // draining the other tailed sessions and stays up; a `?` here would
        // let one bad stream crash the whole wake — invariant 1). The
        // collector is surfaced as durable patrol.degraded events below.
        if let Err(e) = read_channel.drain_all(ledger) {
            eprintln!("campd: read-channel drain_all failed: {e:#}");
        }
        // cp-1 — HARVEST 1: the lines `drain_all` just consumed.
        //
        // Under MERGED LAW this is the harvest that gets an answer-and-exit
        // worker's `control_response`: the reap appends
        // session.stopped/session.crashed BEFORE settle, so the unregister is
        // queued before `drain_all`, and `drain_all` reads the worker's final
        // bytes while the session is still in `tailed` (read_channel.rs's fix-1
        // ordering, and the merged test that pins it).
        let mut appended_control_events = control_step(
            ledger,
            control,
            dispatcher,
            patrol,
            read_channel,
            &mut conns,
            &mut poll,
        )?;
        // §5.3.3: reconcile patrol's timers with the ledger's BLOCKED set every
        // wake — a `can_use_tool` that surfaced in the harvest above disarms its
        // stall timer, and a `permission.decided` from a socket wake re-arms it,
        // both SAME-WAKE. Idempotent with stall_step's own reconcile.
        patrol.reconcile_blocked(ledger, now)?;
        // cp-0 (§2.3): a max_stream_bytes breach is a loud session failure —
        // append the named cause event FIRST (the agent.stalled → kill →
        // session.crashed mold), then kill the worker. The reap appends
        // session.crashed with cause_seq pointing at stream_capped, and the
        // bead re-hooks via the patrol restart path (fix 6: the kill reason
        // starts with "patrol restart" so patrol::observe queues a Respawn).
        // The kill triggers SIGCHLD → the next wake reaps.
        let mut appended_read_channel_events = false;
        for breach in read_channel.take_cap_breaches() {
            let (rig, bead) = dispatcher
                .child_info(&breach.session)
                .map(|(r, b)| (Some(r), Some(b)))
                .unwrap_or((None, breach.bead.clone()));
            let cause_seq = ledger.append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::SessionStreamCapped,
                rig,
                actor: "campd".into(),
                bead: bead.clone(),
                data: serde_json::json!({
                    "session": breach.session,
                    "file": breach.file.to_string_lossy(),
                    "file_size": breach.file_size,
                    "cap_bytes": breach.cap_bytes,
                    "bead": bead,
                }),
            })?;
            // review fix 3: the return value is CHECKED. `false` means no
            // live child holds that session (an adopted worker from a
            // previous campd life, or one already reaped): the SIGKILL never
            // lands, so there is no SIGCHLD, no reap, no `session.crashed`,
            // no `cause_seq`, and no bead re-hook. Discarding it left the
            // bead stranded behind a `session.stream_capped` that had no
            // effect — and because the session is now `capped` (a hard stop)
            // the breach would never re-surface. A campd action must always
            // have a ledger consequence (invariant 3) and a failure must be
            // durable, never silent (invariant 5).
            let killed = dispatcher.kill_worker_with_reason(
                &breach.session,
                cause_seq,
                "patrol restart: stream cap exceeded max_stream_bytes".to_owned(),
            )?;
            if !killed {
                ledger.append(camp_core::event::EventInput {
                    kind: camp_core::event::EventType::PatrolDegraded,
                    rig: None,
                    actor: "campd".into(),
                    bead: bead.clone(),
                    data: serde_json::json!({
                        "session": breach.session,
                        "error": format!(
                            "read_channel: stream cap exceeded max_stream_bytes \
                             ({} bytes > cap {}) but the kill could not be delivered: \
                             no live worker holds this session (adopted from a previous \
                             campd life, or already reaped). The session is no longer \
                             tailed.",
                            breach.file_size, breach.cap_bytes
                        ),
                    }),
                })?;
                // Stop tailing it: a capped session is a hard stop, so
                // leaving it registered would tail a file campd refuses to
                // read, forever.
                read_channel.queue_unregister(&breach.session);
            }
            appended_read_channel_events = true;
        }
        // Fail fast (§2.3): watcher errors, non-JSON lines, and drain
        // (open/seek/read) errors are durable patrol.degraded events, never
        // stderr-only. Appending them is ledger work, so settle this same
        // wake to advance the campd cursor past them (the `wake_ledger_work`
        // local is reassigned at the top of each iteration, so we settle
        // directly here instead).
        for input in read_channel.take_watch_error_events() {
            ledger.append(input)?;
            appended_read_channel_events = true;
        }
        for input in read_channel.take_drain_error_events() {
            ledger.append(input)?;
            appended_read_channel_events = true;
        }
        for input in read_channel.take_parse_error_events() {
            ledger.append(input)?;
            appended_read_channel_events = true;
        }
        if appended_read_channel_events
            && let Err(e) = settle(
                ledger,
                processor,
                runtime,
                clock,
                dispatcher,
                graph,
                patrol,
                read_channel,
            )
        {
            eprintln!("campd: read-channel event settle failed: {e:#}");
        }
        // cp-0 fix 7: persist the in-memory offsets LAST — after the
        // cap-breach kills and the fault-event appends + settle, so the
        // offset commits after the line's ledger effect (phase 0: the
        // drain; phase 1+: the permission.pending event's txn). A cap-
        // killed session is `crashed`, so live_sessions() won't re-tail it
        // on restart and the cap (file-size-based) re-detects if needed.
        // review fix 7: `persist_offsets` is a LEDGER write (set_stream_cursor)
        // — fatal, not eprintln-only. A persistently failing cursor write
        // (disk full, corrupt DB) would otherwise leave campd re-reading from
        // a stale offset on every restart while reporting itself healthy.
        read_channel.persist_offsets(ledger)?;
        // review fix 1 (CRITICAL): ONLY NOW dispose the reaped sessions. By
        // this point their stream files have been drained to EOF by the
        // `drain_all` above (they were still in `tailed` for it), their
        // final lines have become durable parse/drain fault events, and
        // their cursors are persisted. Unregistering earlier — which is what
        // `apply_tracking` used to do inside `settle`, before this whole
        // block — unlinked the file first and deleted the worker's last
        // output unread.
        //
        // Lead ruling (a): this call also ENFORCES the ordering it depends on.
        // A session queued for unregister after `drain_all` (which today
        // cannot happen — the reap appends session.stopped/crashed BEFORE
        // settle — but which a future phase could introduce) is drained before
        // disposal and the violated ordering is recorded as a durable fault
        // event. It returns `true` when it appended anything, so the campd
        // cursor is advanced past those events in this same wake rather than
        // waiting for a wake that an idle campd may never take.
        // cp-1 (C5): the final drain is now SEPARATE from disposal, so the
        // control-plane harvest sits BETWEEN them — BEFORE the unlink. A reaped
        // worker's last line carries the `control_response` to an interrupt, and
        // it must be INGESTED before its file is gone, not merely read.
        if read_channel.final_drain_pending(ledger)?
            && let Err(e) = settle(
                ledger,
                processor,
                runtime,
                clock,
                dispatcher,
                graph,
                patrol,
                read_channel,
            )
        {
            eprintln!("campd: read-channel disposal settle failed: {e:#}");
        }
        // cp-1 — HARVEST 2: the lines the DISPOSAL-TIME final drain consumed.
        //
        // DEFENSE IN DEPTH, honestly labelled. Under merged law the final drain
        // yields ZERO lines (harvest 1 already read them), so this is normally a
        // no-op. It exists because `drain_one` has two callers and a future phase
        // could append session.stopped from INSIDE settle, which would move a
        // worker's last bytes onto this path. It is idempotent (the hand-over is
        // `mem::take`-drained): no double-ingest, no double-append.
        //
        // DO NOT claim that deleting it turns a test red — it does not.
        //
        // (The lines themselves are already SAFE at this point: the final drain
        // read them into memory BEFORE `dispose_pending` unlinked the file. What
        // this harvest does is INGEST them — which must happen before
        // `expire_pending` below, or campd could declare an interrupt unanswered
        // while its answer sat un-ingested in a Vec.)
        appended_control_events |= control_step(
            ledger,
            control,
            dispatcher,
            patrol,
            read_channel,
            &mut conns,
            &mut poll,
        )?;

        // ---- G4/A2: THE DISPOSAL HAND-OFF, IN THE ONLY ORDER THAT WORKS ------
        //
        // Consuming `take_disposed()` INSIDE `control_step` — i.e. BEFORE
        // `dispose_pending` has produced anything — leaves the list EMPTY on the
        // disposal wake: `closing` is never set, and a subscriber that is exactly
        // CAUGHT UP (poll_timeout == None — the steady state of every long-lived
        // watch) gets NO end frame and NO EOF, forever.
        //
        // A2 is why that cannot be papered over: what "rescues" it in practice is
        // that `unregister`'s remove_file fires a notify event and `on_watch_event`
        // always signals — so the end frame's delivery would DEPEND ON A DELIVERED
        // NOTIFY EVENT, which cp-0's law in this very block forbids ("correctness
        // never depends on a delivered event"). It is also why a test would pass
        // while the design was broken.
        //
        // So: DISPOSE FIRST (which is what RECORDS Disposed{session, final_offset}),
        // and only THEN hand the list to the subscriber registry.
        //
        // `close_disposed` is NOT reachable from `control_step`, and it CANNOT be:
        // it needs a `Vec<Disposed>`, and only `dispose_pending` can mint one.
        // The disposed list is the RETURN VALUE of `dispose_pending` — there is no
        // way to obtain one without having disposed. That is the ordering guarantee,
        // made structural rather than conventional (no black-box test can gate it:
        // the stream watch always delivers another wake, so a broken ordering only
        // makes the end frame LATE, never absent).
        let disposed = read_channel.dispose_pending(ledger)?;
        if !disposed.is_empty() {
            let (gone, events) =
                control.close_disposed(&disposed, ledger, &mut conns, Timestamp::now());
            for input in events {
                ledger.append(input)?;
                appended_control_events = true;
            }
            for token in gone {
                if let Some(mut conn) = conns.remove(&token) {
                    let _ = poll.registry().deregister(&mut conn.stream);
                }
                control.forget(token);
            }
        }
        // ----------------------------------------------------------------------

        // cp-1 (B5): ONLY NOW may a control deadline expire — AFTER every
        // ingest this wake. A `control_response` sitting in the stream file
        // because its notify was coalesced must be READ and INGESTED before
        // campd may declare that it never arrived. That is cp-0's law, in the
        // very block above: correctness never depends on a delivered event.
        for input in control.expire_pending(Timestamp::now()) {
            ledger.append(input)?;
            appended_control_events = true;
        }
        // cp-3 (§5.3.4): the steady-state adoption kill. A BLOCKED session that
        // is NOT a live child is a worker campd holds no stdin pipe for — a
        // can_use_tool that arrived via tailing for an adopted worker (surfaced
        // this wake). It can never be answered, so it takes the SAME named kill
        // as the startup adoption path, NOT the generic stall ladder. The
        // `crashed` session leaves `blocked_sessions` next wake (the live-join),
        // so the set-shrink dedups a re-kill.
        if super::patrol::kill_discovered_unanswerable_permissions(ledger, patrol, dispatcher)? > 0
        {
            appended_control_events = true;
        }
        // cp-3 (§5.3.2): a loud saturation fault when the BLOCKED count crosses
        // `max_blocked` — the operator has more unanswered permission questions
        // than campd will let pile up silently. Edge-deduped: emitted only on the
        // <=max → >max transition, cleared on the way back. Audit-only (no fold).
        {
            let n_blocked = ledger.blocked_sessions()?.len();
            let over = n_blocked > dispatcher.max_blocked();
            if over && !saturated {
                ledger.append(camp_core::event::EventInput {
                    kind: EventType::PermissionSaturated,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "blocked": n_blocked as u64,
                        "max_blocked": dispatcher.max_blocked() as u64,
                    }),
                })?;
                appended_control_events = true;
            }
            saturated = over;
        }
        if appended_control_events
            && let Err(e) = settle(
                ledger,
                processor,
                runtime,
                clock,
                dispatcher,
                graph,
                patrol,
                read_channel,
            )
        {
            eprintln!("campd: control settle failed: {e:#}");
        }
    }
}

/// §5.3.3: pop due stall fires, and — the ladder's FIRST act — drain the read
/// channel so a `can_use_tool` whose notify event was lost surfaces as BLOCKED
/// before any stall is declared against a worker that is only waiting on us.
/// Then reconcile patrol's timers (the newly-surfaced BLOCKED session disarms)
/// and declare the remaining fires. A blocked session's fire is swallowed by
/// `declare_stalls`, so it is never nudged/restarted/killed by the ladder.
///
/// Returns whether ledger work was appended (drives `wake_ledger_work`). Guarded
/// on `!is_empty` so the idle path pays nothing (invariant 1): the drain runs
/// only when a stall fire is actually due.
///
/// Acknowledged (non-blocking): on a stall-fire wake the `control_step` here
/// re-runs the full harvest INCLUDING subscriber fanout, so fanout runs twice
/// this wake. It is idempotent — fanout pumps only NEW file bytes and the second
/// call finds none (the byte cursor advanced) — so there is no double-delivery.
#[allow(clippy::too_many_arguments)]
fn stall_step(
    ledger: &mut Ledger,
    patrol: &mut PatrolRuntime,
    control: &mut super::control::ControlRuntime,
    dispatcher: &mut Dispatcher,
    read_channel: &mut super::read_channel::ReadChannelRuntime,
    conns: &mut HashMap<Token, Conn>,
    poll: &mut Poll,
    now: Timestamp,
) -> Result<bool> {
    let stall_fires = patrol.fire_due(now);
    if !stall_fires.is_empty() {
        if let Err(e) = read_channel.drain_all(ledger) {
            eprintln!("campd: pre-ladder drain failed: {e:#}");
        }
        control_step(
            ledger,
            control,
            dispatcher,
            patrol,
            read_channel,
            conns,
            poll,
        )?;
        patrol.reconcile_blocked(ledger, now)?;
    }
    patrol.declare_stalls(ledger, &stall_fires, now)
}

/// cp-1: ingest whatever the read channel just handed over, and append what it
/// produced. ONE helper, called at every harvest point, so a line's control
/// effect can never depend on WHICH drain path read it.
///
/// Returns whether it appended anything (the caller settles to advance the campd
/// cursor past those events).
#[allow(clippy::too_many_arguments)]
fn control_step(
    ledger: &mut Ledger,
    control: &mut super::control::ControlRuntime,
    dispatcher: &mut Dispatcher,
    patrol: &PatrolRuntime,
    read_channel: &mut super::read_channel::ReadChannelRuntime,
    conns: &mut HashMap<Token, Conn>,
    poll: &mut Poll,
) -> Result<bool> {
    let now = Timestamp::now();
    let mut appended = false;

    let lines = read_channel.take_stream_lines();
    if !lines.is_empty() {
        // The immutable borrow for `ingest` (it reads the ledger to dedup a
        // permission.pending) ends when it returns the owned Vec, before the
        // `&mut` append loop below.
        let inputs = control.ingest(&lines, dispatcher, ledger, now);
        for input in inputs {
            ledger.append(input)?;
            appended = true;
        }
    }

    // D6": refresh every subscriber's `tail` and pump. A "live" line is just
    // `tail` advancing — `fanout` never touches `lines` at all, which is what
    // makes truncation and duplication structurally impossible.
    let (gone, events) = control.fanout(ledger, patrol, read_channel, conns, now);
    for input in events {
        ledger.append(input)?;
        appended = true;
    }
    for token in gone {
        if let Some(mut conn) = conns.remove(&token) {
            let _ = poll.registry().deregister(&mut conn.stream);
        }
        control.forget(token);
    }
    Ok(appended)
}

/// Drain the watch pipe (the byte content is meaningless — the signal
/// coalesces; `reload_if_changed` dedupes by file content).
fn drain_pipe(rx: &mut mio::unix::pipe::Receiver) -> Result<()> {
    let mut buf = [0u8; 64];
    loop {
        match rx.read(&mut buf) {
            Ok(0) => return Ok(()), // watcher gone; campd keeps serving
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e).context("draining the config watch pipe"),
        }
    }
}

/// Why the read phase stopped: only WouldBlock means the kernel is drained
/// and it is safe to go back to poll (edge-triggered registration).
enum ReadStop {
    WouldBlock,
    Eof,
    CapReached,
}

/// Read whatever is available (edge-triggered: until WouldBlock or EOF),
/// then answer every complete line in the buffer. `max_request_bytes` is
/// `MAX_REQUEST_BYTES` in production; injectable so unit tests can exercise
/// the cap within real kernel socket-buffer sizes.
///
/// Read and drain alternate in an outer loop (PR #8 re-review finding 1):
/// when the cap pauses reading mid-backlog and the drain brings the buffer
/// back under it, we must read again rather than return to poll — mio is
/// edge-triggered, so data already in the kernel fires no further event and
/// a pipelining client would wedge. Only a WouldBlock read (kernel empty)
/// may end in `Open`; only a single line fragment that exceeds the cap is
/// rejected.
#[allow(clippy::too_many_arguments)]
fn serve_connection(
    conn: &mut Conn,
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    dispatcher: &mut Dispatcher,
    graph: &mut GraphRuntime,
    patrol: &mut PatrolRuntime,
    read_channel: &mut super::read_channel::ReadChannelRuntime,
    control: &mut super::control::ControlRuntime,
    token: Token,
    max_request_bytes: usize,
) -> Result<ConnState> {
    let mut chunk = [0u8; 4096];
    loop {
        // Read phase: until WouldBlock, EOF, or the buffer crosses the cap
        // (bounding memory; the drain below shrinks it again).
        let stop = loop {
            match conn.stream.read(&mut chunk) {
                Ok(0) => break ReadStop::Eof,
                Ok(n) => {
                    conn.buf.extend_from_slice(&chunk[..n]);
                    if conn.buf.len() > max_request_bytes {
                        break ReadStop::CapReached;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break ReadStop::WouldBlock,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("reading a request"),
            }
        };
        let drained = drain_lines(
            conn,
            ledger,
            processor,
            runtime,
            clock,
            dispatcher,
            graph,
            patrol,
            read_channel,
            control,
            token,
        )?;
        if let Some(terminal) = drained {
            return Ok(terminal);
        }
        if conn.buf.len() > max_request_bytes {
            // A single line may not exceed the cap (finding 3): answer, then
            // drop the connection. campd itself is unharmed. The response is
            // best-effort courtesy: a client that still has data in flight
            // may see a connection reset instead of the error line (closing
            // with unread receive data resets on Linux) — the drop is the
            // contract.
            respond(
                &mut conn.stream,
                &Response::Error {
                    ok: false,
                    error: format!("request line exceeds {max_request_bytes} bytes"),
                },
            )?;
            return Ok(ConnState::Closed);
        }
        match stop {
            ReadStop::Eof => return Ok(ConnState::Closed),
            ReadStop::WouldBlock => return Ok(ConnState::Open),
            // The cap paused reading mid-backlog; the kernel may still hold
            // data that will never fire another event — read again.
            ReadStop::CapReached => {}
        }
    }
}

/// Answer every complete line in the buffer. Returns `Some(state)` when a
/// line demands the connection (or the daemon) wind down, `None` to keep
/// serving.
#[allow(clippy::too_many_arguments)]
fn drain_lines(
    conn: &mut Conn,
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    dispatcher: &mut Dispatcher,
    graph: &mut GraphRuntime,
    patrol: &mut PatrolRuntime,
    read_channel: &mut super::read_channel::ReadChannelRuntime,
    control: &mut super::control::ControlRuntime,
    token: Token,
) -> Result<Option<ConnState>> {
    while let Some(newline) = conn.buf.iter().position(|&b| b == b'\n') {
        let line_bytes: Vec<u8> = conn.buf.drain(..=newline).collect();
        let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]).into_owned();
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(&line) {
            Ok(Request::Stop) => return Ok(Some(ConnState::Stop)),
            Ok(Request::Poke { seq: _ }) => {
                // ACK BEFORE SETTLE (PR #14 review finding 2, operator
                // approved 2026-07-07): the ack means "campd is awake and
                // will process this wake" — the poker's write is already
                // durable, so making it wait out a slow settle (worktree
                // checkouts, cooks, up to max_workers spawns) only risks
                // its 5 s client timeout and a retried duplicate. The ack
                // is BEST-EFFORT (fix-pass NEW LOW): a poker that vanished
                // before reading it must not skip the settle — durable
                // work is decoupled from the courtesy ack. The poked seq
                // is advisory; the settle reads past the cursor regardless,
                // in this same wake. A settle error lands on stderr and
                // leaves the cursor before the failing event — surfaced,
                // never skipped; the next wake retries.
                let ack = respond(&mut conn.stream, &Response::Ok { ok: true });
                if let Err(e) = settle(
                    ledger,
                    processor,
                    runtime,
                    clock,
                    dispatcher,
                    graph,
                    patrol,
                    read_channel,
                ) {
                    eprintln!("campd: poke processing failed: {e:#}");
                }
                if let Err(e) = ack {
                    // ANY ack write error closes the connection, not only
                    // broken-pipe (round-2 review note): responses are a
                    // few bytes, so even WouldBlock means the client is
                    // not reading — the documented respond() contract
                    // already drops such connections, and any buffered
                    // lines from a client that cannot hear answers are
                    // discarded with it. The settle above already ran;
                    // only the courtesy ack is lost.
                    eprintln!("campd: poke ack write failed (client gone?): {e:#}");
                    return Ok(Some(ConnState::Closed));
                }
            }
            Ok(Request::Status) => {
                let response = match ledger.status_summary() {
                    Ok(summary) => Response::Status {
                        ok: true,
                        summary,
                        red: patrol.stalled_count(),
                        campd_pid: std::process::id(),
                    },
                    Err(e) => {
                        eprintln!("campd: status failed: {e}");
                        Response::Error {
                            ok: false,
                            error: format!("status failed: {e}"),
                        }
                    }
                };
                respond(&mut conn.stream, &response)?;
            }
            Ok(Request::Adopt) => {
                // The startup routine, on demand (spec §8.5). Its events
                // (session.crashed/stopped, sweep dispositions) settle in
                // this same wake, after the summary is answered.
                let response = match super::patrol::adopt(ledger, patrol, dispatcher) {
                    Ok(s) => Response::Adopt {
                        ok: true,
                        crashed: s.crashed,
                        rearmed: s.rearmed,
                        released: s.released,
                        swept: s.swept,
                        kept: s.kept,
                    },
                    Err(e) => {
                        eprintln!("campd: adopt failed: {e:#}");
                        Response::Error {
                            ok: false,
                            error: format!("adopt failed: {e}"),
                        }
                    }
                };
                respond(&mut conn.stream, &response)?;
                if let Err(e) = settle(
                    ledger,
                    processor,
                    runtime,
                    clock,
                    dispatcher,
                    graph,
                    patrol,
                    read_channel,
                ) {
                    eprintln!("campd: adopt settle failed: {e:#}");
                }
            }
            // cp-1 (§4.1). Every body lives in `control.rs` — the ONE module
            // that owns the control plane — so these arms are delegations.
            Ok(Request::SessionsList) => {
                let response = control.serve_sessions_list(ledger, patrol, read_channel);
                respond(&mut conn.stream, &response)?;
            }
            // cp-1 (§4.4): subscribe turns this connection into a STREAM. The hello
            // goes out FIRST (it must be the first bytes on the socket), and only
            // then does `pump` start writing frames — B11.
            Ok(Request::SessionSubscribe { session, cursor }) => {
                let response = control.serve_subscribe(token, &session, cursor, read_channel);
                let subscribed = matches!(response, Response::Subscribed { .. });
                respond(&mut conn.stream, &response)?;
                if subscribed {
                    // The accept-time WRITABLE edge is already consumed, so the
                    // first frames need an explicit pump — nothing else will fire.
                    match control.pump(token, conn, Timestamp::now()) {
                        super::control::PumpOutcome::Ok => {}
                        super::control::PumpOutcome::Gone => {
                            control.forget(token);
                            return Ok(Some(ConnState::Closed));
                        }
                        super::control::PumpOutcome::Drop(event) => {
                            ledger.append(event)?;
                            control.forget(token);
                            return Ok(Some(ConnState::Closed));
                        }
                    }
                    for input in control.take_pending_events() {
                        ledger.append(input)?;
                    }
                }
            }
            Ok(Request::SessionSendTurn { session, text }) => {
                let response = control.serve_send_turn(&session, &text, ledger, dispatcher);
                respond(&mut conn.stream, &response)?;
            }
            Ok(Request::SessionInterrupt { session }) => {
                let response =
                    control.serve_interrupt(&session, ledger, dispatcher, Timestamp::now());
                respond(&mut conn.stream, &response)?;
            }
            // cp-3 (§5.3.4): answer a worker's can_use_tool. The handler appends
            // `permission.decided` to the ledger FIRST (the serialization point),
            // then writes the control_response. The blocked→working re-arm rides
            // `reconcile_blocked` in the post-harvest path (Task 8), same wake.
            Ok(Request::SessionPermissionDecision {
                session,
                request_id,
                decision,
                message,
            }) => {
                let response = control.serve_permission_decision(
                    &session,
                    &request_id,
                    &decision,
                    message.as_deref(),
                    ledger,
                    dispatcher,
                );
                respond(&mut conn.stream, &response)?;
            }
            // cp-2 (§4.1): fleet.subscribe turns this connection into the aggregate
            // STREAM. The hello goes out FIRST (it must be the first bytes on the
            // socket); the post-hello pump emits the snapshot (B11 — nothing else
            // will fire for it). Mirrors the session.subscribe arm exactly.
            Ok(Request::FleetSubscribe) => {
                let response = control.serve_fleet_subscribe(token, ledger, patrol, read_channel);
                let subscribed = matches!(response, Response::FleetSubscribed { .. });
                respond(&mut conn.stream, &response)?;
                if subscribed {
                    match control.pump(token, conn, Timestamp::now()) {
                        super::control::PumpOutcome::Ok => {}
                        super::control::PumpOutcome::Gone => {
                            control.forget(token);
                            return Ok(Some(ConnState::Closed));
                        }
                        super::control::PumpOutcome::Drop(event) => {
                            ledger.append(event)?;
                            control.forget(token);
                            return Ok(Some(ConnState::Closed));
                        }
                    }
                    for input in control.take_pending_events() {
                        ledger.append(input)?;
                    }
                }
            }
            Err(e) => {
                respond(
                    &mut conn.stream,
                    &Response::Error {
                        ok: false,
                        error: format!("bad request: {e}"),
                    },
                )?;
                return Ok(Some(ConnState::Closed));
            }
        }
    }
    Ok(None)
}

/// Drain a signal self-pipe, returning how many bytes it held (signal
/// deliveries coalesce; one byte or many, one sweep of the service path
/// covers them all).
///
/// The count is what lets a caller distinguish a real delivery from a
/// spurious readable — mio documents that readiness events MAY be spurious.
/// SIGCHLD ignores it (a wasted try_wait sweep reaps nothing and is
/// harmless); SIGTERM/SIGINT must not, because its service path exits the
/// daemon. Drain first, then act, is also signal-hook's prescribed order for
/// its `pipe` module.
fn drain_signal_pipe(stream: &mut UnixStream, which: &str) -> Result<usize> {
    let mut buf = [0u8; 64];
    let mut drained = 0usize;
    loop {
        match stream.read(&mut buf) {
            // the write end lives in the signal handler for the process
            // lifetime; 0 is unreachable-but-safe
            Ok(0) => return Ok(drained),
            Ok(n) => drained += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(drained),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e).with_context(|| format!("draining the {which} pipe")),
        }
    }
}

/// The SIGCHLD service path (Phase 8 plan decision I): reap exited
/// workers, record their session ends, then settle — the
/// 11th-ready-bead-dispatches-on-first-close path.
#[allow(clippy::too_many_arguments)]
fn reap_and_refill(
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    dispatcher: &mut Dispatcher,
    graph: &mut GraphRuntime,
    patrol: &mut PatrolRuntime,
    read_channel: &mut super::read_channel::ReadChannelRuntime,
) -> Result<(), ReapFailure> {
    dispatcher.reap(ledger)?;
    // Decision 11e: check-script children ride the same SIGCHLD pipe;
    // their verdict batches land before the settle that acts on them.
    graph.reap_checks(ledger)?;
    // settle failures are ledger-side: retry-worthy, like the appends
    settle(
        ledger,
        processor,
        runtime,
        clock,
        dispatcher,
        graph,
        patrol,
        read_channel,
    )
    .map_err(|error| ReapFailure {
        retryable: true,
        error,
    })
}

/// One wake's processing, to a joint fixpoint (spec §7.3 append → fold →
/// dispatch): orders::settle catches up past the cursor (patrol observing
/// every event on the same pass — Phase 11) and cooks fired orders to ITS
/// fixpoint, patrol applies tracking (watches + timers) and executes the
/// queued ladder actions (nudge/restart/release — their records land as
/// events), then the dispatcher spawns workers up to the cap, and the
/// loop repeats until nothing appends — so the campd cursor always
/// settles on the ledger head with every dispatch, cook, and patrol
/// event processed, and a patrol-released bead's respawn dispatches in
/// the same wake. Bounded by the shrinking queues: convergence, not
/// polling.
#[allow(clippy::too_many_arguments)]
pub(super) fn settle(
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    dispatcher: &mut Dispatcher,
    graph: &mut GraphRuntime,
    patrol: &mut PatrolRuntime,
    read_channel: &mut super::read_channel::ReadChannelRuntime,
) -> Result<()> {
    // Decision 11f / issue #17: the fire budget spans this WHOLE
    // invocation (every orders::settle / converge round below) — resetting
    // any deeper lets through-converge regeneration escape the budget.
    runtime.reset_fire_budget();
    loop {
        orders::settle(
            ledger,
            processor,
            runtime,
            clock,
            graph,
            patrol,
            read_channel,
        )?;
        // Phase 9: drain the graph work the processor queued — spawn due
        // check scripts, cook due bond children. Cooks append events, so
        // the fixpoint below re-settles them in this same invocation.
        graph.execute(ledger)?;
        // Phase 11: apply queued patrol tracking (watches + timers) and
        // execute the queued ladder actions (nudge/restart/release) — all
        // in this same wake, before converge respawns released beads.
        let now = Timestamp::now();
        patrol.apply_tracking(ledger, now)?;
        // cp-0: apply read-channel tracking (register/unregister + offset
        // load) outside the cursor txn — the patrol apply_tracking mold.
        // Covers the settle path: ops queued by this invocation's
        // CampdProcessor::process (observe) are applied here, so the
        // drain-on-every-wake block after `settle` sees freshly-registered
        // sessions tailed on this same wake.
        read_channel.apply_tracking(ledger)?;
        patrol.execute_pending(ledger, dispatcher, now)?;
        dispatcher.converge(ledger)?;
        let cursor = ledger.cursor(cursor::CAMPD_CURSOR)?;
        if !ledger.has_events_past(cursor)? {
            return Ok(());
        }
    }
}

/// The poll-timeout combinator (Decision 11c, review note 1): None = no
/// deadline from that source; the earliest wins. Deliberately THE single
/// composition point — new deadline sources (phase-11 stall timers) join
/// here.
fn min_deadline(
    a: Option<std::time::Duration>,
    b: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    match (a, b) {
        (None, x) => x,
        (x, None) => x,
        (Some(a), Some(b)) => Some(a.min(b)),
    }
}

fn respond(stream: &mut UnixStream, response: &Response) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    // Responses are a few bytes; a WouldBlock here means the client is not
    // reading — surfacing it drops that connection.
    stream
        .write_all(line.as_bytes())
        .context("writing the response")?;
    Ok(())
}

fn stop(ledger: &mut Ledger, socket_path: &Path) -> Result<()> {
    ledger.append(EventInput {
        kind: EventType::CampdStopped,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({}),
    })?;
    std::fs::remove_file(socket_path)
        .with_context(|| format!("removing {}", socket_path.display()))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::daemon::cursor::ReadinessProcessor;
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::time::Duration;

    /// A patrol runtime for settle threading (Phase 11): unwatched, empty.
    fn test_patrol() -> crate::daemon::patrol::PatrolRuntime {
        let config = camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap();
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        crate::daemon::patrol::PatrolRuntime::new(patrol_config, &config)
    }

    /// A read-channel runtime for settle/serve_connection threading (cp-0):
    /// empty tailed set, sessions dir under the camp root.
    fn test_read_channel(dir: &std::path::Path) -> crate::daemon::read_channel::ReadChannelRuntime {
        crate::daemon::read_channel::ReadChannelRuntime::new(
            dir.join("sessions"),
            256 * 1024 * 1024,
        )
        .unwrap()
    }

    /// Phase 11: the poll timeout composes THREE deadline sources through
    /// `min_deadline` (orders, graph checks, patrol stall timers); both
    /// idle = infinite wait (invariant 1 stays intact).
    #[test]
    fn min_deadline_takes_the_earliest_deadline_and_none_means_idle() {
        let a = Some(Duration::from_secs(5));
        let b = Some(Duration::from_secs(9));
        assert_eq!(min_deadline(None, None), None);
        assert_eq!(min_deadline(a, None), a);
        assert_eq!(min_deadline(None, b), b);
        assert_eq!(min_deadline(a, b), a);
        assert_eq!(min_deadline(b, a), a);
        // the three-source composition: earliest of all three
        assert_eq!(
            min_deadline(min_deadline(a, b), Some(Duration::from_secs(2))),
            Some(Duration::from_secs(2))
        );
    }

    /// Phase 11 wiring pin: a due stall declares agent.stalled on the wake
    /// path, and the settle EXECUTES the queued action — here the nudge
    /// fails loudly (no held child, no resumable session id) and the
    /// evented nudge_failed proves declare → settle → execute end to end.
    #[test]
    fn a_due_stall_declares_and_the_settle_executes_the_action() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        std::fs::write(
            dir.path().join("agents/dev.md"),
            "---\nname: dev\n---\nWork.\n",
        )
        .unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title": "t"}),
            })
            .unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({
                    "name": "t/dev/1", "agent": "dev",
                    "transcript_path": dir.path().join("projects/-p/sid.jsonl"),
                    "bead": "gc-1",
                }),
            })
            .unwrap();
        let mut processor = ReadinessProcessor::default();
        let mut runtime =
            OrdersRuntime::build(dir.path(), Timestamp::now(), jiff::tz::TimeZone::UTC).unwrap();
        let clock = camp_core::clock::SystemClock;
        let config = camp_core::config::CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        let mut dispatcher = Dispatcher::new(
            crate::campdir::CampDir {
                root: dir.path().to_path_buf(),
            },
            config.clone(),
        );
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        let mut patrol = crate::daemon::patrol::PatrolRuntime::new(patrol_config, &config);
        let mut graph = GraphRuntime::new(dir.path().to_path_buf(), &config);
        let mut read_channel = test_read_channel(dir.path());

        // first settle: observe the woke row, arm the timer
        settle(
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut patrol,
            &mut read_channel,
        )
        .unwrap();
        assert!(
            patrol.poll_timeout(Timestamp::now()).is_some(),
            "the wake's poll timeout is now patrol-sourced"
        );

        // the wake path, replayed at a synthetic future instant: fire, declare, settle
        let later = Timestamp::now()
            .checked_add(jiff::SignedDuration::from_mins(11))
            .unwrap();
        let stall_fires = patrol.fire_due(later);
        assert_eq!(stall_fires.len(), 1, "the 10m default threshold fired");
        assert!(
            patrol
                .declare_stalls(&mut ledger, &stall_fires, later)
                .unwrap()
        );
        settle(
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut patrol,
            &mut read_channel,
        )
        .unwrap();

        let events = ledger.events_range(1, None).unwrap();
        let stalled: Vec<_> = events
            .iter()
            .filter(|e| e.kind.as_str() == "agent.stalled")
            .collect();
        assert_eq!(stalled[0].data["action"], "nudge", "the declaration");
        assert_eq!(
            stalled[1].data["action"], "nudge_failed",
            "the settle executed the action and its failure is evented"
        );
        assert_eq!(
            ledger.cursor(cursor::CAMPD_CURSOR).unwrap() as usize,
            events.len(),
            "the settle fixpoint consumed every patrol event"
        );
    }

    /// cp-3 §5.3.3 — THE HEART, platform-independent (CP3-R2-B1): `stall_step`'s
    /// FIRST act drains the read channel, so a `can_use_tool` sitting UNREAD in
    /// the stream file — with NO watcher running here, it is unread BY
    /// CONSTRUCTION on every platform, no dependence on FSEvents vs inotify —
    /// surfaces as BLOCKED before any stall is declared against a worker that is
    /// only waiting on us.
    ///
    /// THE LOAD-BEARING falsifying assertion is `agent.stalled == 0`: removing
    /// the pre-ladder drain from `stall_step` makes `declare_stalls` see a
    /// not-blocked session and append `agent.stalled` — RED on every platform.
    /// (The `!is_armed` assertion is trivially true regardless — `fire_due` pops
    /// the fired timer before the drain — so it is documentation, not the guard.)
    #[test]
    fn stall_step_drains_the_read_channel_before_declaring_a_stall() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        std::fs::write(
            dir.path().join("agents/dev.md"),
            "---\nname: dev\n---\nWork.\n",
        )
        .unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title": "t"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({
                    "name": "s", "agent": "dev",
                    "transcript_path": dir.path().join("projects/-p/sid.jsonl"),
                    "bead": "gc-1",
                }),
            })
            .unwrap();

        let config = camp_core::config::CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        let mut dispatcher = Dispatcher::new(
            crate::campdir::CampDir {
                root: dir.path().to_path_buf(),
            },
            config.clone(),
        );
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        let mut patrol = crate::daemon::patrol::PatrolRuntime::new(patrol_config, &config);
        let mut control = crate::daemon::control::ControlRuntime::new(1024);
        let mut read_channel = test_read_channel(dir.path());
        read_channel.register(&mut ledger, "s").unwrap();

        // Arm the stall timer: observe the woke row + apply_tracking at t0.
        let woke = ledger
            .events_of_type(EventType::SessionWoke)
            .unwrap()
            .remove(0);
        let t0 = Timestamp::now();
        patrol.observe(&woke);
        patrol.apply_tracking(&mut ledger, t0).unwrap();
        assert!(
            patrol.is_armed("s"),
            "precondition: the stall timer is armed"
        );

        // A can_use_tool sits UNREAD in the stream file — no watcher, no drain yet.
        let stdout = dir.path().join("sessions").join("s.json");
        std::fs::write(
            &stdout,
            b"{\"type\":\"control_request\",\"request_id\":\"cli-9\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\"}}\n",
        )
        .unwrap();

        let mut conns: HashMap<Token, Conn> = HashMap::new();
        let mut poll = Poll::new().unwrap();
        // past the armed deadline (10m default threshold)
        let now = t0.checked_add(jiff::SignedDuration::from_mins(11)).unwrap();

        stall_step(
            &mut ledger,
            &mut patrol,
            &mut control,
            &mut dispatcher,
            &mut read_channel,
            &mut conns,
            &mut poll,
            now,
        )
        .unwrap();

        // The ladder's FIRST act drained the channel: the pending surfaced.
        assert!(
            ledger.blocked_sessions().unwrap().contains(&"s".to_owned()),
            "the unread can_use_tool surfaced as BLOCKED"
        );
        // THE LOAD-BEARING falsifying assertion.
        assert_eq!(
            ledger
                .events_of_type(EventType::AgentStalled)
                .unwrap()
                .len(),
            0,
            "no stall was declared against the waiting worker"
        );
        assert!(!patrol.is_armed("s"));
    }

    /// PR #14 fix-pass NEW MEDIUM: the self-raise retry budget must bound
    /// — non-retryable failures never self-raise, and a persistent
    /// retryable failure degrades to log-and-wait instead of a hot loop.
    #[test]
    fn self_raise_budget_bounds_and_resets() {
        let mut budget = SELF_RAISE_BUDGET;
        // non-retryable: never raise, budget untouched
        assert!(!should_self_raise(false, &mut budget));
        assert_eq!(budget, SELF_RAISE_BUDGET);
        // retryable: raise while budget lasts
        for _ in 0..SELF_RAISE_BUDGET {
            assert!(should_self_raise(true, &mut budget));
        }
        // exhausted: degrade to log-and-wait
        assert!(!should_self_raise(true, &mut budget));
        assert!(!should_self_raise(true, &mut budget));
        // success resets the budget (the caller assigns)
        budget = SELF_RAISE_BUDGET;
        assert!(should_self_raise(true, &mut budget));
    }

    /// PR #14 fix-pass NEW LOW: a poke whose ack write fails (client gone)
    /// must STILL settle in the same wake — the poked write is durable and
    /// the ack is courtesy. The dead connection closes cleanly.
    #[test]
    fn a_dead_client_poke_still_settles() {
        // a child forked mid-pair-creation could inherit the client end
        // and keep the "dead" peer alive (see spawn_probe_guard)
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        // one event past the cursor: the settle's catch-up must consume it
        ledger
            .append(camp_core::event::EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title": "t", "type": "memory"}),
            })
            .unwrap();
        let mut processor = ReadinessProcessor::default();
        let mut runtime =
            OrdersRuntime::build(dir.path(), Timestamp::now(), jiff::tz::TimeZone::UTC).unwrap();
        let clock = camp_core::clock::SystemClock;
        let mut dispatcher = Dispatcher::new(
            crate::campdir::CampDir {
                root: dir.path().to_path_buf(),
            },
            camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );

        let (mut client, daemon_end) = StdUnixStream::pair().unwrap();
        daemon_end.set_nonblocking(true).unwrap();
        let mut conn = Conn {
            stream: UnixStream::from_std(daemon_end),
            buf: Vec::new(),
        };
        client.write_all(b"{\"op\":\"poke\",\"seq\":1}\n").unwrap();
        drop(client); // the poker vanishes before reading its ack

        let mut graph = GraphRuntime::new(
            dir.path().to_path_buf(),
            &camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );
        let mut read_channel = test_read_channel(dir.path());
        let state = serve_connection(
            &mut conn,
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut test_patrol(),
            &mut read_channel,
            &mut crate::daemon::control::ControlRuntime::new(1024),
            Token(6),
            1024,
        )
        .expect("a dead poker must not error the connection loop");
        assert!(matches!(state, ConnState::Closed));
        assert_eq!(
            ledger.cursor(cursor::CAMPD_CURSOR).unwrap(),
            1,
            "the settle must run even when the ack write fails"
        );
    }

    /// Issue #17 scenario 2 — the review's Blocker B trace, pinned: an
    /// order on event:dispatch.failed in a camp with a ROUTING HOLE
    /// regenerates one fire per OUTER settle iteration (fire -> cook ->
    /// converge appends dispatch.failed for the fresh step bead -> fire).
    /// The budget only catches this because it resets per event_loop::
    /// settle INVOCATION — a per-orders::settle reset never accumulates
    /// (the rejected scoping; red state: this test never terminates).
    /// Process-free: converge's prepare fails routing before any spawn.
    #[test]
    fn through_converge_regeneration_is_budget_bounded_and_quiesces() {
        let dir = tempfile::tempdir().unwrap();
        let rig = dir.path().join("repo");
        std::fs::create_dir_all(&rig).unwrap();
        // no step assignee, no rig/dispatch default_agent: a routing hole
        std::fs::write(
            dir.path().join("camp.toml"),
            format!(
                "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n[[order]]\nname=\"resurrector\"\non=\"event:dispatch.failed\"\nformula=\"one-step\"\n",
                rig.display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("formulas")).unwrap();
        std::fs::write(
            dir.path().join("formulas/one-step.toml"),
            "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
        )
        .unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        // the seed: one ready task the dispatcher cannot route
        ledger
            .append(camp_core::event::EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title": "seed"}),
            })
            .unwrap();
        let mut processor = ReadinessProcessor::default();
        let mut runtime =
            OrdersRuntime::build(dir.path(), Timestamp::now(), jiff::tz::TimeZone::UTC).unwrap();
        let clock = camp_core::clock::SystemClock;
        let config = camp_core::config::CampConfig::parse(
            &std::fs::read_to_string(dir.path().join("camp.toml")).unwrap(),
        )
        .unwrap();
        let mut graph = GraphRuntime::new(dir.path().to_path_buf(), &config);
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        let mut patrol = crate::daemon::patrol::PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = Dispatcher::new(
            crate::campdir::CampDir {
                root: dir.path().to_path_buf(),
            },
            config,
        );
        let mut read_channel = test_read_channel(dir.path());

        settle(
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut patrol,
            &mut read_channel,
        )
        .expect("settle must return despite the regenerative order");

        let budget_failures: Vec<_> = ledger
            .events_of_type(EventType::OrderFailed)
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.data["error"]
                    .as_str()
                    .is_some_and(|m| m.contains("fire budget"))
            })
            .collect();
        assert_eq!(budget_failures.len(), 1, "exactly one budget failure");
        assert_eq!(budget_failures[0].data["order"], "resurrector");
        let total = ledger.events_range(1, None).unwrap().len();
        assert!(
            total < super::super::orders::FIRE_BUDGET * 8,
            "event growth is bounded, got {total}"
        );
        // true quiescence: the cursor sits on the head
        let cursor = ledger.cursor(cursor::CAMPD_CURSOR).unwrap();
        assert!(!ledger.has_events_past(cursor).unwrap());

        // and the drip STOPS: a second invocation (fresh budget) appends
        // nothing — suppressed matches are behind the cursor, and each
        // cooked step already carries its per-lifetime dispatch.failed
        let before = ledger.events_range(1, None).unwrap().len();
        settle(
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut patrol,
            &mut read_channel,
        )
        .unwrap();
        assert_eq!(
            ledger.events_range(1, None).unwrap().len(),
            before,
            "a second settle invocation regenerates nothing"
        );
    }

    /// PR #8 re-review finding 1, pinned at the unit level: one readable
    /// event must drain everything the kernel already holds. mio is
    /// edge-triggered — if `serve_connection` returns `Open` with data
    /// still queued (the cap-break stopped reading and the line drain
    /// brought the buffer back under the cap), no further event ever fires
    /// (the peer is done writing) and the connection wedges.
    ///
    /// The cap is injected small (1 KB) so a multi-cap burst of VALID
    /// pipelined requests fits inside real kernel socketpair buffers on
    /// every platform; the burst is also bigger than one 4 KB read chunk,
    /// so the cap-break fires with bytes still queued. A reader thread
    /// drains responses concurrently, like a real pipelining client — on
    /// Linux, per-write skb overhead is charged against SO_SNDBUF, so
    /// hundreds of tiny unread responses would exhaust the send buffer far
    /// below its nominal size and WouldBlock the daemon's response write.
    #[test]
    fn one_readable_event_drains_a_pipelined_backlog_beyond_the_cap() {
        const TEST_CAP: usize = 1024;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut processor = ReadinessProcessor::default();
        let mut runtime =
            OrdersRuntime::build(dir.path(), Timestamp::now(), jiff::tz::TimeZone::UTC).unwrap();
        let clock = camp_core::clock::SystemClock;
        let mut dispatcher = Dispatcher::new(
            crate::campdir::CampDir {
                root: dir.path().to_path_buf(),
            },
            camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );

        let (mut client, daemon_end) = StdUnixStream::pair().unwrap();
        daemon_end.set_nonblocking(true).unwrap();
        let mut conn = Conn {
            stream: UnixStream::from_std(daemon_end),
            buf: Vec::new(),
        };

        let n = 280usize; // ~6.4 KB of pokes: > 4 KB chunk, > 6x the cap

        // Response reader, started first so it is always draining.
        let reader_end = client.try_clone().unwrap();
        reader_end
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(reader_end);
            let mut answered = 0usize;
            let mut line = String::new();
            while answered < n {
                line.clear();
                match std::io::BufRead::read_line(&mut reader, &mut line) {
                    Ok(bytes) if bytes > 0 => answered += 1,
                    _ => break, // EOF or timeout: stop counting
                }
            }
            answered
        });

        // Pre-queue the whole burst BEFORE the one and only "readable
        // event" — exactly what a pipelining client produces.
        let mut burst = String::new();
        for i in 0..n {
            burst.push_str(&format!("{{\"op\":\"poke\",\"seq\":{i}}}\n"));
        }
        assert!(burst.len() > TEST_CAP * 4, "burst must dwarf the cap");
        assert!(burst.len() > 4096, "burst must exceed one read chunk");
        client.write_all(burst.as_bytes()).unwrap();

        // One event's worth of serving must answer every request.
        let mut graph = GraphRuntime::new(
            dir.path().to_path_buf(),
            &camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );
        let mut read_channel = test_read_channel(dir.path());
        let state = serve_connection(
            &mut conn,
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut test_patrol(),
            &mut read_channel,
            &mut crate::daemon::control::ControlRuntime::new(1024),
            Token(6),
            TEST_CAP,
        )
        .unwrap();
        assert!(matches!(state, ConnState::Open));
        let answered = reader.join().unwrap();
        assert_eq!(
            answered, n,
            "one readable event must drain the whole queued backlog"
        );
    }
}

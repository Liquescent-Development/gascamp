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
use std::time::{Duration, Instant};

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
/// self-pipe, 3 = Phase 11's patrol transcript-watch self-pipe, 4+ =
/// connections. Coordinate with the lead before renumbering.
const CONFIG_WATCH: Token = Token(1);
/// Phase 8's SIGCHLD self-pipe (worker death detection, spec §10.1), per
/// the shared token layout above.
const SIGCHLD: Token = Token(2);
/// Phase 11's patrol transcript-watch self-pipe (spec §10.2), per the
/// shared token layout above.
const PATROL_WATCH: Token = Token(3);

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

struct Conn {
    stream: UnixStream,
    buf: Vec<u8>,
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
    // The connection map is bounded by the process fd limit — the natural
    // cap for a single-user local socket. An artificial cap was considered
    // (PR #8 review finding 3) and rejected: it would reject legitimate
    // bursts, and per-connection memory is already bounded by
    // MAX_REQUEST_BYTES.
    let mut conns: HashMap<Token, Conn> = HashMap::new();
    // Tokens 2 and 3 are RESERVED (SIGCHLD, patrol watch — the layout
    // above); connections start at 4.
    let mut next_token = 4usize;
    let mut self_raise_budget = SELF_RAISE_BUDGET;

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
                runtime.poll_timeout(poll_now),
                graph.poll_timeout(Instant::now()),
            ),
            patrol.poll_timeout(poll_now),
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
        // queued ladder actions.
        let stall_fires = patrol.fire_due(now);
        wake_ledger_work |= patrol.declare_stalls(ledger, &stall_fires)?;
        for event in events.iter() {
            match event.token() {
                LISTENER => loop {
                    match listener.accept() {
                        Ok((mut stream, _addr)) => {
                            let token = Token(next_token);
                            next_token += 1;
                            poll.registry()
                                .register(&mut stream, token, Interest::READABLE)
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
                    drain_signal_pipe(&mut sigchld)?;
                    // Reap → record ends → settle (catch up, cook, refill
                    // capacity). Errors are reported, never fatal: a broken
                    // child must not take campd down, and unrecorded exits
                    // are retried next wake (try_wait re-returns the
                    // status).
                    match reap_and_refill(
                        ledger, processor, runtime, clock, dispatcher, graph, patrol,
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
                        ledger.append(input)?;
                        wake_ledger_work = true;
                    }
                }
                token => {
                    let Some(mut conn) = conns.remove(&token) else {
                        continue; // already dropped this cycle
                    };
                    match serve_connection(
                        &mut conn,
                        ledger,
                        processor,
                        runtime,
                        clock,
                        dispatcher,
                        graph,
                        patrol,
                        MAX_REQUEST_BYTES,
                    ) {
                        Ok(ConnState::Open) => {
                            conns.insert(token, conn);
                        }
                        Ok(ConnState::Closed) => {
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
            if let Err(e) = settle(ledger, processor, runtime, clock, dispatcher, graph, patrol) {
                eprintln!("campd: settle failed: {e:#}");
            }
        }
    }
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
        let drained =
            drain_lines(conn, ledger, processor, runtime, clock, dispatcher, graph, patrol)?;
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
                if let Err(e) =
                    settle(ledger, processor, runtime, clock, dispatcher, graph, patrol)
                {
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

/// Drain the SIGCHLD self-pipe (signal deliveries coalesce; one byte or
/// many, one sweep of try_wait covers them all).
fn drain_signal_pipe(stream: &mut UnixStream) -> Result<()> {
    let mut buf = [0u8; 64];
    loop {
        match stream.read(&mut buf) {
            // the write end lives in the signal handler for the process
            // lifetime; 0 is unreachable-but-safe
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e).context("draining the SIGCHLD pipe"),
        }
    }
}

/// The SIGCHLD service path (Phase 8 plan decision I): reap exited
/// workers, record their session ends, then settle — the
/// 11th-ready-bead-dispatches-on-first-close path.
fn reap_and_refill(
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    dispatcher: &mut Dispatcher,
    graph: &mut GraphRuntime,
    patrol: &mut PatrolRuntime,
) -> Result<(), ReapFailure> {
    dispatcher.reap(ledger)?;
    // Decision 11e: check-script children ride the same SIGCHLD pipe;
    // their verdict batches land before the settle that acts on them.
    graph.reap_checks(ledger)?;
    // settle failures are ledger-side: retry-worthy, like the appends
    settle(ledger, processor, runtime, clock, dispatcher, graph, patrol).map_err(|error| {
        ReapFailure {
            retryable: true,
            error,
        }
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
pub(super) fn settle(
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    dispatcher: &mut Dispatcher,
    graph: &mut GraphRuntime,
    patrol: &mut PatrolRuntime,
) -> Result<()> {
    // Decision 11f / issue #17: the fire budget spans this WHOLE
    // invocation (every orders::settle / converge round below) — resetting
    // any deeper lets through-converge regeneration escape the budget.
    runtime.reset_fire_budget();
    loop {
        orders::settle(ledger, processor, runtime, clock, graph, patrol)?;
        // Phase 9: drain the graph work the processor queued — spawn due
        // check scripts, cook due bond children. Cooks append events, so
        // the fixpoint below re-settles them in this same invocation.
        graph.execute(ledger)?;
        // Phase 11: apply queued patrol tracking (watches + timers) and
        // execute the queued ladder actions (nudge/restart/release) — all
        // in this same wake, before converge respawns released beads.
        let now = Timestamp::now();
        patrol.apply_tracking(ledger, now)?;
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
        assert_eq!(min_deadline(min_deadline(a, b), Some(Duration::from_secs(2))), Some(Duration::from_secs(2)));
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

        // first settle: observe the woke row, arm the timer
        settle(
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut patrol,
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
        assert!(patrol.declare_stalls(&mut ledger, &stall_fires).unwrap());
        settle(
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut patrol,
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
        let state = serve_connection(
            &mut conn,
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut test_patrol(),
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
        let mut dispatcher = Dispatcher::new(
            crate::campdir::CampDir {
                root: dir.path().to_path_buf(),
            },
            config,
        );

        settle(
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
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
        let state = serve_connection(
            &mut conn,
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
            &mut dispatcher,
            &mut graph,
            &mut test_patrol(),
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

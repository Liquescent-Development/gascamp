//! The campd event loop (spec §5, §15.1): mio poll over the listener,
//! per-connection reads, the camp.toml watch pipe, and the cron heap. The
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
use camp_core::orders::FireCause;
use camp_core::orders::cron::Fire;
use jiff::{SignedDuration, Timestamp};
use mio::net::{UnixListener, UnixStream};
use mio::{Events, Interest, Poll, Token};

use super::cursor::ReadinessProcessor;
use super::orders::{self, OrdersRuntime};
use super::socket::{Request, Response};

const LISTENER: Token = Token(0);
/// The notify→mio self-pipe (camp.toml watch). Phase 8 allocates its
/// SIGCHLD token around this — coordinate before renumbering.
const CONFIG_WATCH: Token = Token(1);

/// Upper bound on a single request line (PR #8 review finding 3). Real
/// requests are tens of bytes; the cap keeps a broken or hostile client
/// from ballooning campd's RSS past the idle budget (invariant 1). A
/// connection whose buffered line fragment exceeds this is answered with a
/// clean error and dropped.
pub(super) const MAX_REQUEST_BYTES: usize = 64 * 1024;

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

pub fn run(
    mut listener: UnixListener,
    socket_path: &Path,
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
    config_rx: &mut mio::unix::pipe::Receiver,
) -> Result<()> {
    let mut poll = Poll::new().context("creating the poller")?;
    let mut events = Events::with_capacity(64);
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)
        .context("registering the listener")?;
    poll.registry()
        .register(config_rx, CONFIG_WATCH, Interest::READABLE)
        .context("registering the config watch pipe")?;
    // The connection map is bounded by the process fd limit — the natural
    // cap for a single-user local socket. An artificial cap was considered
    // (PR #8 review finding 3) and rejected: it would reject legitimate
    // bursts, and per-connection memory is already bounded by
    // MAX_REQUEST_BYTES.
    let mut conns: HashMap<Token, Conn> = HashMap::new();
    let mut next_token = 2usize; // 0 = listener, 1 = config watch

    let mut last_seen = Timestamp::now();
    loop {
        let timeout = runtime.poll_timeout(Timestamp::now());
        let wall_before = Timestamp::now();
        let mono_before = Instant::now();
        poll.poll(&mut events, timeout).context("poll")?;
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
            runtime
                .recompute(now, last_seen)
                .into_iter()
                .map(|c| Fire {
                    order: c.order,
                    scheduled: c.scheduled,
                    catch_up: true,
                })
                .collect()
        } else {
            runtime.fire_due(now)
        };
        last_seen = now;
        // Declare the fires (durable first); the settle below cooks them.
        // A ledger that refuses the declaration is fatal — campd must not
        // run automation it cannot record.
        let mut wake_ledger_work = !fires.is_empty();
        for fire in fires {
            ledger.append(camp_core::orders::fired_input(
                &fire.order,
                &FireCause::Cron {
                    scheduled: fire.scheduled,
                    catch_up: fire.catch_up,
                },
            ))?;
        }
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
                CONFIG_WATCH => {
                    drain_pipe(config_rx)?;
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
            // error re-surfaces on the next poke.
            if let Err(e) = orders::settle(ledger, processor, runtime, clock) {
                eprintln!("campd: settle failed: {e}");
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
fn serve_connection(
    conn: &mut Conn,
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
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
        let drained = drain_lines(conn, ledger, processor, runtime, clock)?;
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
fn drain_lines(
    conn: &mut Conn,
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
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
                // The poked seq is advisory; catch-up reads past the cursor
                // regardless. A processing error answers the poker, lands on
                // stderr, and leaves the cursor before the failing event —
                // surfaced, never skipped. (Phase 10: settle = catch-up +
                // cook-to-fixpoint; readiness pending is drained inside it.)
                let response = match orders::settle(ledger, processor, runtime, clock) {
                    Ok(()) => Response::Ok { ok: true },
                    Err(e) => {
                        eprintln!("campd: catch-up failed: {e}");
                        Response::Error {
                            ok: false,
                            error: format!("catch-up failed: {e}"),
                        }
                    }
                };
                respond(&mut conn.stream, &response)?;
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
        let mut runtime = OrdersRuntime::build(
            dir.path(),
            Timestamp::now(),
            jiff::tz::TimeZone::UTC,
        )
        .unwrap();
        let clock = camp_core::clock::SystemClock;

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
        let state = serve_connection(
            &mut conn,
            &mut ledger,
            &mut processor,
            &mut runtime,
            &clock,
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

//! The campd event loop (spec §5, §15.1): mio poll over the listener and
//! per-connection reads. The poll timeout is the earliest armed timer
//! deadline; Phase 7 arms no timers, so it is always `None` — the idle
//! daemon blocks in `poll` with zero wakeups (invariant 1).

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use mio::net::{UnixListener, UnixStream};
use mio::{Events, Interest, Poll, Token};

use super::cursor::{self, ReadinessProcessor};
use super::socket::{Request, Response};

const LISTENER: Token = Token(0);

/// Upper bound on a single request line (PR #8 review finding 3). Real
/// requests are tens of bytes; the cap keeps a broken or hostile client
/// from ballooning campd's RSS past the idle budget (invariant 1). A
/// connection whose buffered line fragment exceeds this is answered with a
/// clean error and dropped.
pub(super) const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Earliest armed timer deadline → poll timeout. No timer armed = infinite
/// wait. Phase 10 (cron heap) and Phase 11 (stall timers) plug in here;
/// Phase 7 arms nothing. This is the only timeout expression in campd.
fn poll_timeout() -> Option<Duration> {
    None
}

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
) -> Result<()> {
    let mut poll = Poll::new().context("creating the poller")?;
    let mut events = Events::with_capacity(64);
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)
        .context("registering the listener")?;
    // The connection map is bounded by the process fd limit — the natural
    // cap for a single-user local socket. An artificial cap was considered
    // (PR #8 review finding 3) and rejected: it would reject legitimate
    // bursts, and per-connection memory is already bounded by
    // MAX_REQUEST_BYTES.
    let mut conns: HashMap<Token, Conn> = HashMap::new();
    let mut next_token = 1usize;

    loop {
        poll.poll(&mut events, poll_timeout()).context("poll")?;
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
                token => {
                    let Some(mut conn) = conns.remove(&token) else {
                        continue; // already dropped this cycle
                    };
                    match serve_connection(&mut conn, ledger, processor, MAX_REQUEST_BYTES) {
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
        let drained = drain_lines(conn, ledger, processor)?;
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
                // surfaced, never skipped.
                let response = match cursor::catch_up(ledger, processor) {
                    Ok(_) => {
                        // Phase 8 dispatches the newly-ready set; drained
                        // here so the bookkeeping stays bounded in a
                        // long-lived daemon.
                        let _newly_ready = processor.take_pending();
                        Response::Ok { ok: true }
                    }
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
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut processor = ReadinessProcessor::default();

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
        let state = serve_connection(&mut conn, &mut ledger, &mut processor, TEST_CAP).unwrap();
        assert!(matches!(state, ConnState::Open));
        let answered = reader.join().unwrap();
        assert_eq!(
            answered, n,
            "one readable event must drain the whole queued backlog"
        );
    }
}

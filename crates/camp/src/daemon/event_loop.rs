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

use super::cursor::{self, EventProcessor};
use super::socket::{Request, Response};

const LISTENER: Token = Token(0);

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
    processor: &mut dyn EventProcessor,
) -> Result<()> {
    let mut poll = Poll::new().context("creating the poller")?;
    let mut events = Events::with_capacity(64);
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)
        .context("registering the listener")?;
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
                    match serve_connection(&mut conn, ledger, processor) {
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

/// Read whatever is available (edge-triggered: until WouldBlock or EOF),
/// then answer every complete line in the buffer.
fn serve_connection(
    conn: &mut Conn,
    ledger: &mut Ledger,
    processor: &mut dyn EventProcessor,
) -> Result<ConnState> {
    let mut eof = false;
    let mut chunk = [0u8; 4096];
    loop {
        match conn.stream.read(&mut chunk) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(n) => conn.buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("reading a request"),
        }
    }
    while let Some(newline) = conn.buf.iter().position(|&b| b == b'\n') {
        let line_bytes: Vec<u8> = conn.buf.drain(..=newline).collect();
        let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]).into_owned();
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(&line) {
            Ok(Request::Stop) => return Ok(ConnState::Stop),
            Ok(Request::Poke { seq: _ }) => {
                // The poked seq is advisory; catch-up reads past the cursor
                // regardless. A processing error answers the poker, lands on
                // stderr, and leaves the cursor before the failing event —
                // surfaced, never skipped.
                let response = match cursor::catch_up(ledger, processor) {
                    Ok(_) => Response::Ok { ok: true },
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
                return Ok(ConnState::Closed);
            }
        }
    }
    Ok(if eof { ConnState::Closed } else { ConnState::Open })
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

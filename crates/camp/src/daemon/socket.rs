//! The campd socket protocol (master plan Phase 7, pinned): newline-delimited
//! JSON over `<camp>/campd.sock`. Liveness is an ANSWERED REQUEST (spec §5
//! as amended by issue #55): alive means an event-loop round-trip, because
//! the kernel's listen backlog accepts connections even when the loop is
//! wedged. Bind-conflict detection is the narrower accept test: a stale
//! file that refuses connections is unlinked and rebound; a live listener
//! makes a second daemon refuse to start (wedged or not — replacing a
//! wedged daemon is the operator's kill -9, never a silent takeover).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use camp_core::Seq;
use camp_core::ledger::StatusSummary;
use serde::{Deserialize, Serialize};

use crate::campdir::CampDir;

/// One request line. Internally tagged on `op`; an unknown op is a parse
/// error (there is no wildcard arm to hide behind).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Post-commit poke (spec §7.2). `seq` is advisory: catch-up processes
    /// everything past the cursor regardless. The `{"ok":true}` reply is an
    /// ACK — campd is awake and will process this wake — not a completion
    /// signal (PR #14 review finding 2, ack-before-settle).
    Poke {
        seq: Seq,
    },
    Status,
    Stop,
    /// Reconcile the session registry against reality (spec §8.5) — the
    /// same routine campd runs at startup, on demand (Phase 11).
    Adopt,
    /// Deliver one user turn into a live worker's campd-held stdin pipe
    /// (dispatch-lifecycle Phase 1, #29 — the converse verb's live path).
    Nudge {
        session: String,
        text: String,
    },
}

/// One response line. Untagged: variant order matters for deserialization
/// (Status needs its fields, Error needs `error`, Ok is the fallback).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Status {
        ok: bool,
        #[serde(flatten)]
        summary: StatusSummary,
        /// Live sessions currently stalled by patrol — the `✖red` of the
        /// fleet badge (spec §10/§11, Phase 12 Decision D2). Serialized
        /// after the flattened summary, before `campd_pid`.
        red: u64,
        campd_pid: u32,
    },
    Adopt {
        ok: bool,
        crashed: usize,
        rearmed: usize,
        released: usize,
        swept: usize,
        kept: usize,
    },
    /// Nudge disposition (dispatch-lifecycle Phase 1): via="stdin" means
    /// the turn is in the held pipe; via="none" means no held pipe for the
    /// session (released, Null-mode, exited, or not campd's child) — the
    /// caller converses over the resume path instead. Must precede Ok in
    /// this untagged enum so {"ok":..,"via":..} resolves here.
    Nudge {
        ok: bool,
        via: String,
    },
    Error {
        ok: bool,
        error: String,
    },
    Ok {
        ok: bool,
    },
}

/// Bind the daemon socket under spec §5's liveness rules: fresh path →
/// bind; existing file that refuses connections (stale, e.g. after
/// kill -9) → unlink and rebind; existing file that accepts → another
/// campd is alive → hard error.
///
/// The whole negotiation is serialized with an exclusive advisory lock on
/// `<socket>.lock` (PR #8 review finding 1): without it, two daemons racing
/// past a stale socket can both probe-refuse, both unlink, both bind — the
/// loser ends up live but orphaned on an unlinked inode (split brain). The
/// lock releases on drop / process exit, so a crash never wedges startup
/// (crash-only, spec §5). The lock file is a serialization primitive, not
/// status: bind-conflict detection stays "the socket accepts" (a wedged
/// daemon still owns its socket — issue #55 puts liveness-for-service at
/// an answered request, but replacing a stuck daemon is the operator's
/// kill -9, never a silent takeover here) — no pidfiles, no
/// lockfiles-as-status.
pub fn bind_or_replace(path: &Path) -> Result<UnixListener> {
    // Literally `<socket>.lock` (campd.sock.lock) — appended, not
    // with_extension, which would silently rename to campd.lock and
    // contradict every doc that names this file (re-review finding 2).
    let lock_path = {
        let mut os = path.as_os_str().to_owned();
        os.push(".lock");
        std::path::PathBuf::from(os)
    };
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening bind lock {}", lock_path.display()))?;
    lock_file
        .lock()
        .with_context(|| format!("locking {}", lock_path.display()))?;
    // Lock held for the whole probe → unlink → rebind section; released on
    // return (drop). A loser blocks here, then sees the winner's live
    // socket and refuses cleanly.
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                bail!(
                    "campd is already running (socket {} accepts connections)",
                    path.display()
                );
            }
            std::fs::remove_file(path)
                .with_context(|| format!("removing stale socket {}", path.display()))?;
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
        }
        Err(e) => Err(e).with_context(|| format!("binding {}", path.display())),
    }
}

/// How long one CLI request may wait on campd before the CLI declares
/// the daemon wedged (issue #55). A bound on one operation, not a wakeup
/// — the daemon's own poll timeout stays None (invariant 1). Because
/// every daemon-needing verb sends a real request, every operator
/// interaction doubles as an event-loop liveness probe at zero standing
/// cost.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// campd accepted the connection and then went away before answering: it
/// exited or crashed mid-request.
///
/// Typed because it is NOT always a fault, and the third state matters. To a
/// verb that merely wanted an answer, this is an error. To a verb that has just
/// asked the supervisor to STOP campd, it is the shutdown working — campd is
/// tearing down, and a moment later nothing will be listening at all. Telling
/// the two apart is the difference between `camp service stop` reporting the
/// truth and reporting a scary failure for a stop that succeeded (which it did,
/// on macOS, until this type existed: `launchctl bootout` returns while campd is
/// still exiting, so the post-stop probe met exactly this).
#[derive(Debug)]
pub struct CampdWentAway;

impl std::fmt::Display for CampdWentAway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "campd accepted the connection and then closed it without responding — it exited \
             or crashed mid-request (a `camp service restart` or a `camp stop` racing this \
             verb will do exactly this). Check it is back: `camp service status`, or run \
             `camp daemon` where no service manager does — then rerun this verb"
        )
    }
}

impl std::error::Error for CampdWentAway {}

/// The wedge shape (issue #55): the kernel's listen backlog accepted the
/// connection — that happens even when the event loop never runs accept —
/// but no response line arrived within REQUEST_TIMEOUT. Typed so it is never
/// confused with `CampdNotRunning` ("nothing is listening"): something owns
/// THIS socket but does not serve it, and its remedy is `kill -9`, not
/// "start campd". Two faults, two remedies — the CLI must not flatten them.
#[derive(Debug)]
pub struct CampdUnresponsive {
    /// From the ledger's last campd.started event; None when no recorded
    /// start carries one (a pre-#55 ledger, or no ledger at all).
    pub campd_pid: Option<u32>,
    pub socket: std::path::PathBuf,
}

impl std::fmt::Display for CampdUnresponsive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secs = REQUEST_TIMEOUT.as_secs();
        // What comes AFTER the kill. The CLI is a pure client (design §4.3), so
        // rerunning the verb does not bring campd back — say who does. On a
        // supervised camp the unit restarts it on its own; on a --no-service
        // camp, in a container, or in CI there is no supervisor and `camp
        // daemon` is the answer. Naming both is what makes this true in every
        // camp rather than only in the supervised one.
        let after = "the CLI never starts campd: once it is dead your supervisor brings it \
                     back (watch it with `camp service status`), or run `camp daemon` \
                     yourself where no service manager does";
        match self.campd_pid {
            Some(pid) => write!(
                f,
                "campd (pid {pid}, per the ledger's last campd.started) accepted the \
                 connection but sent no response within {secs}s — its event loop is \
                 wedged or stuck in one long operation; `kill -9 {pid}` is a supported \
                 shutdown (crash-only: the ledger keeps the whole story). {after}"
            ),
            None => write!(
                f,
                "campd accepted the connection but sent no response within {secs}s — \
                 its event loop is wedged or stuck in one long operation (pid unknown: \
                 no campd.started event records one; `lsof {}` finds the holder); \
                 kill -9 it — a supported shutdown (crash-only: the ledger keeps the \
                 whole story). {after}",
                self.socket.display()
            ),
        }
    }
}

impl std::error::Error for CampdUnresponsive {}

/// campd is NOT RUNNING: nothing is listening on the socket — it is absent,
/// or it is a stale file a `kill -9` left behind. The CLI is a PURE CLIENT
/// (design §4.3: one path — campd is a supervised foreground process, run by
/// launchd / systemd --user / the container runtime / you), so a
/// daemon-needing verb turns this into a LOUD, actionable fault and stops.
/// It never spawns a daemon: a silent respawn hides the real fault (a broken
/// unit, a crash loop, a camp nobody supervised) and it is exactly the
/// behavior this phase removes.
///
/// Typed so it can be told apart from `CampdUnresponsive` — something owns
/// the socket but does not serve it. Different fault, different remedy
/// (`kill -9`, not "start campd"): flattening them would give wrong advice.
#[derive(Debug)]
pub struct CampdNotRunning {
    /// The camp this verb resolved to — a user has several; say which is dark.
    pub camp_root: std::path::PathBuf,
    pub socket: std::path::PathBuf,
    /// Where a supervised campd's stderr lands: a crash-restart loop is
    /// visible there and nowhere else.
    pub log: std::path::PathBuf,
    /// What the ledger says about the campd that WAS running here (design §3) —
    /// a recorded pid, a ledger that holds none, or a ledger that could not be
    /// read. The three are not interchangeable; see `LastCampd`.
    pub last_campd: LastCampd,
}

impl std::fmt::Display for CampdNotRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let root = self.camp_root.display();
        let history = match &self.last_campd {
            LastCampd::Pid(pid) => format!(
                "the last campd here was pid {pid} (the ledger's last campd.started); \
                 that process is gone"
            ),
            LastCampd::NeverStarted => "no campd has ever started in this camp (no \
                                        campd.started event in the ledger)"
                .to_owned(),
            // Never claim the camp never ran a campd on the strength of a ledger
            // we could not read — that is a positive claim about contents we
            // never saw. State the ledger fault instead; it is a second, real
            // problem the operator needs to know about.
            LastCampd::Unknown(why) => format!(
                "whether a campd ever started here is unknown — {why}; that is a second \
                 fault, and worth fixing on its own"
            ),
        };
        write!(
            f,
            "campd is not running for camp {root} — nothing is listening on {socket}\n  \
             {history}\n  \
             the camp CLI never starts campd: it is a supervised service. Bring it up \
             with one of:\n    \
             camp service status --camp {root}   # the managed unit's state \
             (`camp service restart` cycles it)\n    \
             camp daemon --camp {root}           # run it yourself: container, CI, or a \
             box with no service manager\n  \
             if a supervisor is restarting it in a loop, its stderr is in {log}",
            socket = self.socket.display(),
            log = self.log.display(),
        )
    }
}

impl std::error::Error for CampdNotRunning {}

/// The daemon-needing verb's request path (design §4.3) — the ONLY way `top`,
/// `adopt` and `sling` reach campd. Send the request; a campd that is not
/// running is the loud `CampdNotRunning` error. The CLI never starts one.
///
/// Built on `request_if_up`, so liveness is judged on the SAME connection that
/// carries the request (the PR #51 finding 1 law): exactly one connect, no
/// bare pre-probe — which would both open a second connection and be fooled by
/// a wedged daemon's listen backlog. A campd that accepts and then never
/// answers therefore still surfaces as `CampdUnresponsive`: it owns the
/// socket, and its remedy is different.
pub fn require(camp: &CampDir, request: &Request) -> Result<Response> {
    match request_if_up(camp, request)? {
        Some(response) => Ok(response),
        None => Err(anyhow::Error::new(CampdNotRunning {
            camp_root: camp.root.clone(),
            socket: camp.socket_path(),
            log: camp.log_path(),
            last_campd: last_campd(camp),
        })),
    }
}

/// Send one request, read one response line. REQUEST_TIMEOUT bounds the
/// operation against a wedged daemon — not a wakeup; the daemon's own
/// poll timeout stays None (invariant 1). A timeout surfaces as the
/// actionable `CampdUnresponsive` error naming the campd pid (issue #55).
pub fn request(camp: &CampDir, request: &Request) -> Result<Response> {
    let path = camp.socket_path();
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("connecting to campd at {}", path.display()))?;
    request_on(stream, request).map_err(|e| mark_wedge(camp, e))
}

/// Like `request`, but campd-not-listening is a NORMAL state, not an error
/// (dispatch-lifecycle Phase 1: the converse verb's resume path; the same
/// designed degrade as `camp top --statusline`). Liveness is judged on the
/// SAME connection that carries the request — a separate probe connect
/// would leave a window where campd stops between probe and request and
/// the designed degrade becomes a hard error (PR #51 review finding 1).
/// Only an absent/refusing socket maps to Ok(None); a campd that accepts
/// and then misbehaves (or answers an Error line) still surfaces as Err —
/// fail fast.
pub fn request_if_up(camp: &CampDir, request: &Request) -> Result<Option<Response>> {
    use std::io::ErrorKind;
    let path = camp.socket_path();
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
            return Ok(None);
        }
        Err(e) => {
            return Err(e).with_context(|| format!("connecting to campd at {}", path.display()));
        }
    };
    request_on(stream, request)
        .map(Some)
        .map_err(|e| mark_wedge(camp, e))
}

/// Reclassify a timed-out request as the actionable wedge error (issue
/// #55): the root cause of a read/write deadline is the raw io error
/// (WouldBlock on macOS, TimedOut elsewhere) — cryptic and inactionable
/// ("Resource temporarily unavailable"). Everything else passes through
/// untouched.
fn mark_wedge(camp: &CampDir, err: anyhow::Error) -> anyhow::Error {
    use std::io::ErrorKind;
    let timed_out = err
        .root_cause()
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| matches!(io.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut));
    if !timed_out {
        return err;
    }
    anyhow::Error::new(CampdUnresponsive {
        campd_pid: last_recorded_campd_pid(camp),
        socket: camp.socket_path(),
    })
}

/// What the ledger can say about the campd that last ran in this camp — the
/// only pid source that survives a crash (there are no pidfiles, spec §5).
///
/// THREE answers, never flattened into two. "No campd has ever started in this
/// camp" is a positive claim about ledger CONTENTS; a camp.db that could not be
/// read is not evidence for it, because the contents were never read. Collapsing
/// `Unknown` into `NeverStarted` would tell an operator whose ledger is corrupt
/// that their camp is merely fresh — a confident sentence hiding a real fault
/// (invariant 5: no silenced errors).
#[derive(Debug)]
pub enum LastCampd {
    /// The ledger's last `campd.started` recorded this pid.
    Pid(u32),
    /// The ledger WAS read, and it holds no `campd.started` at all.
    NeverStarted,
    /// The ledger could not be read, or its last `campd.started` carries no
    /// usable pid (a pre-#55 ledger). The reason is carried, never dropped.
    Unknown(String),
}

/// Read the ledger's account of the last campd. Infallible by construction: it
/// decorates an error that is ALREADY being reported (a dead or wedged socket),
/// so a ledger fault is folded into the message as `Unknown(why)` — stated, not
/// silent — rather than replacing the fault the operator actually needs to see.
fn last_campd(camp: &CampDir) -> LastCampd {
    let ledger = match camp_core::ledger::Ledger::open_read_only(&camp.db_path()) {
        Ok(ledger) => ledger,
        Err(e) => return LastCampd::Unknown(format!("this camp's ledger could not be read: {e}")),
    };
    let starts = match ledger.events_of_type(camp_core::event::EventType::CampdStarted) {
        Ok(starts) => starts,
        Err(e) => return LastCampd::Unknown(format!("this camp's ledger could not be read: {e}")),
    };
    let Some(last) = starts.last() else {
        return LastCampd::NeverStarted;
    };
    match last
        .data
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
    {
        Some(pid) => LastCampd::Pid(pid),
        None => LastCampd::Unknown(
            "the ledger's last campd.started records no usable pid (a pre-#55 ledger)".to_owned(),
        ),
    }
}

/// The pid from the ledger's last campd.started event — recorded at every
/// daemon start precisely because a WEDGED campd cannot be asked (the
/// status op is the only other place its pid lives). Option, not Result:
/// this decorates an error that is already being reported; a ledger that
/// cannot yield the pid downgrades the message to "pid unknown" (stated,
/// not silent), never masks the wedge.
fn last_recorded_campd_pid(camp: &CampDir) -> Option<u32> {
    match last_campd(camp) {
        LastCampd::Pid(pid) => Some(pid),
        LastCampd::NeverStarted | LastCampd::Unknown(_) => None,
    }
}

/// The shared request body: one line out, one line back, on an
/// already-open connection.
fn request_on(mut stream: UnixStream, request: &Request) -> Result<Response> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut response_line = String::new();
    BufReader::new(stream).read_line(&mut response_line)?;
    if response_line.is_empty() {
        // EOF instead of a response line: campd accepted us and then went away
        // mid-request — it exited or crashed, and a `camp service restart` or a
        // `camp stop` racing this verb does exactly that. The CLI no longer
        // papers over it by spawning a replacement (design §4.3), so this needs
        // a remedy of its own: every other client-side campd fault names one.
        //
        // TYPED (not a bare bail): a verb that just told the supervisor to stop
        // campd is SUPPOSED to meet this, and for it this is success in
        // progress, not a fault. See CampdWentAway.
        return Err(CampdWentAway.into());
    }
    let response: Response = serde_json::from_str(response_line.trim_end())
        .with_context(|| format!("campd sent a malformed response: {response_line:?}"))?;
    if let Response::Error { error, .. } = &response {
        bail!("campd: {error}");
    }
    Ok(response)
}

/// Post-commit poke: fire-and-forget BY DESIGN (spec §7.2 — "if campd is
/// down, writes still succeed and it catches up from its processed-cursor
/// on start"). This is the one sanctioned ignore-the-error site in camp;
/// a poke never auto-starts the daemon.
pub fn poke_best_effort(camp: &CampDir, seq: Seq) {
    let _ = request(camp, &Request::Poke { seq });
}

/// A test double for a campd that is UP and ANSWERING.
///
/// The service verbs prove a stop actually took effect by asking the socket —
/// so faking that answer means really serving one. This binds the camp's
/// socket and speaks the exact wire format `request_on` speaks: one JSON line
/// in, one JSON line out. A verb that only *believes* campd is gone cannot
/// pass a test built on this.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod fake_campd {
    use super::Response;
    use crate::campdir::CampDir;
    use camp_core::ledger::StatusSummary;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A live campd. `served()` counts the requests it read and undertook to
    /// answer, so a test can prove the verb really talked to it rather than
    /// guessing.
    ///
    /// Precisely: the count is published BEFORE the response is written (it has
    /// to be — see the server loop), so a peer that hangs up mid-write would
    /// still be counted. No test does that, and nothing here turns on it. What
    /// every assertion DOES turn on is the other direction, which is exact: a
    /// verb that never connects leaves this at 0, because the server thread is
    /// still parked in `accept`.
    pub struct FakeCampd {
        served: Arc<AtomicUsize>,
    }

    impl FakeCampd {
        pub fn served(&self) -> usize {
            self.served.load(Ordering::SeqCst)
        }
    }

    /// A campd in the middle of the graceful shutdown you just asked for: it
    /// accepts, reads the request, and closes without answering — then goes
    /// away entirely, so the next connect is refused.
    ///
    /// This is not a hypothetical. `launchctl bootout` returns BEFORE campd has
    /// finished exiting (measured: ~760 ms on macOS), so any verb that stops the
    /// unit and immediately asks the socket "is a campd still serving this camp?"
    /// meets exactly this. `dying_accepts` is how many connections get the
    /// accept-then-close treatment before the listener drops.
    pub fn serve_then_die(camp: &CampDir, dying_accepts: usize) -> FakeCampd {
        let listener =
            UnixListener::bind(camp.socket_path()).expect("binding the fake campd socket");
        let served = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&served);
        std::thread::spawn(move || {
            for _ in 0..dying_accepts {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut request_line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut request_line);
                counter.fetch_add(1, Ordering::SeqCst);
                // Close without answering: campd went away mid-request.
                drop(stream);
            }
            // …and now it is gone: the listener drops, so the socket file is
            // still on disk but nothing is behind it — connect() gets
            // ECONNREFUSED, which is what a stopped campd looks like.
            drop(listener);
        });
        FakeCampd { served }
    }

    /// Bind the camp's socket and answer `responses`, one per connection, in
    /// order. The bind completes BEFORE this returns, so a caller may connect
    /// immediately with no race. The server thread is detached: a verb that
    /// never connects (a refusal that precedes any socket work, say) simply
    /// leaves it parked in `accept`, and the test still ends.
    pub fn serve(camp: &CampDir, responses: Vec<Response>) -> FakeCampd {
        let listener =
            UnixListener::bind(camp.socket_path()).expect("binding the fake campd socket");
        let served = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&served);
        std::thread::spawn(move || {
            for response in responses {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut request_line = String::new();
                if BufReader::new(&stream)
                    .read_line(&mut request_line)
                    .is_err()
                {
                    return;
                }
                let mut line = serde_json::to_string(&response).expect("serializing the response");
                line.push('\n');
                // Publish the witness BEFORE the write that unblocks the client.
                // The client returns the moment it reads this response line, so a
                // counter bumped AFTER the write has no happens-before edge to the
                // test's `served()` load: the verb could finish and the assertion
                // could run while this thread was still descheduled between the two
                // statements (CI, both runners, PR #71). Incrementing first puts the
                // increment strictly before the write, which is strictly before the
                // client's read — so any client that can see the response can also
                // see the count.
                counter.fetch_add(1, Ordering::SeqCst);
                if (&stream).write_all(line.as_bytes()).is_err() {
                    return;
                }
            }
        });
        FakeCampd { served }
    }

    /// What a campd alive at `pid` answers a `Status` with.
    pub fn status(pid: u32) -> Response {
        Response::Status {
            ok: true,
            summary: StatusSummary {
                live_sessions: Vec::new(),
                ready: 0,
                open: 0,
            },
            red: 0,
            campd_pid: pid,
        }
    }

    /// What a campd answers a `Stop` with, just before it exits.
    pub fn stopped() -> Response {
        Response::Ok { ok: true }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::ledger::StatusSummary;
    use std::os::unix::net::UnixListener;

    #[test]
    fn request_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Request::Poke { seq: 412 }).unwrap(),
            r#"{"op":"poke","seq":412}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Status).unwrap(),
            r#"{"op":"status"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Stop).unwrap(),
            r#"{"op":"stop"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Adopt).unwrap(),
            r#"{"op":"adopt"}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"adopt"}"#).unwrap(),
            Request::Adopt
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"poke","seq":412}"#).unwrap(),
            Request::Poke { seq: 412 }
        );
    }

    #[test]
    fn unknown_op_is_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"dance"}"#).is_err());
    }

    /// The converse verb's wire op (dispatch-lifecycle Phase 1, #29).
    #[test]
    fn nudge_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Request::Nudge {
                session: "camp/dev/1".into(),
                text: "status?".into()
            })
            .unwrap(),
            r#"{"op":"nudge","session":"camp/dev/1","text":"status?"}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"nudge","session":"s","text":"t"}"#).unwrap(),
            Request::Nudge {
                session: "s".into(),
                text: "t".into()
            }
        );
        // Response: untagged — the Nudge variant must win for {"ok":..,"via":..}
        assert_eq!(
            serde_json::to_string(&Response::Nudge {
                ok: true,
                via: "stdin".into()
            })
            .unwrap(),
            r#"{"ok":true,"via":"stdin"}"#
        );
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true,"via":"none"}"#).unwrap(),
            Response::Nudge { via, .. } if via == "none"
        ));
    }

    /// Review finding 1 (PR #51): liveness must be judged on the SAME
    /// connection that carries the request. A separate probe connect left
    /// a window where campd stops between probe and request, turning the
    /// designed degrade (Ok(None) → resume path) into a hard error. The
    /// accept queue is FIFO, so by the time the request is answered every
    /// earlier probe connection has been accepted and counted.
    #[test]
    fn request_if_up_uses_a_single_connection() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        let path = camp.socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepts);
        // Serve every connection: count it, answer any request line with a
        // plain ok. The thread parks in accept() after the test's last
        // connection; the test process exits regardless (harness-only).
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut line = String::new();
                let mut reader = BufReader::new(match stream.try_clone() {
                    Ok(clone) => clone,
                    Err(_) => continue,
                });
                if reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let _ = stream.write_all(b"{\"ok\":true}\n");
                }
            }
        });

        let response = request_if_up(&camp, &Request::Status).unwrap();
        assert!(matches!(response, Some(Response::Ok { ok: true })));
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "request_if_up must open exactly ONE connection (no separate liveness probe)"
        );
    }

    /// Issue #55 scope 2: a campd whose event loop never serves the
    /// request fails the CLI verb loudly WITHIN ITS BOUND — naming the
    /// campd pid recorded in the ledger and the kill -9 remedy — never
    /// hangs. The bare bound listener IS the wedge simulator: its kernel
    /// backlog accepts the connect, but nothing ever reads or answers,
    /// exactly like a daemon stuck mid-syscall.
    #[test]
    fn a_wedged_campd_fails_the_request_loudly_within_its_bound() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        // the ledger knows the daemon's pid: campd.started records it
        let mut ledger = camp_core::ledger::Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::CampdStarted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({ "pid": 424242 }),
            })
            .unwrap();
        drop(ledger);
        let _wedged = UnixListener::bind(camp.socket_path()).unwrap();

        let start = std::time::Instant::now();
        let err = request(&camp, &Request::Status).unwrap_err();
        let elapsed = start.elapsed();
        assert!(
            elapsed < REQUEST_TIMEOUT * 2 + Duration::from_secs(2),
            "bounded, never a hang: took {elapsed:?}"
        );
        let msg = format!("{err:#}");
        assert!(msg.contains("424242"), "must name the campd pid: {msg}");
        assert!(msg.contains("kill -9"), "must name the remedy: {msg}");
        assert!(
            err.downcast_ref::<CampdUnresponsive>().is_some(),
            "typed, so a wedge is never reported as a down campd: {msg}"
        );
        assert_wedge_text_promises_no_cli_spawn(&msg);
    }

    /// The gap a token grep cannot see. The CLI never starts campd (design
    /// §4.3), so no error may tell the operator that running a camp verb will
    /// — and the wedge error is where that lie survived the truth sweep, because
    /// it phrased the promise ("rerun the verb to start a fresh campd") in words
    /// carrying none of the sweep's tokens.
    ///
    /// It is not merely stale, it is CONTEXT-DEPENDENT, which is worse: on a
    /// supervised camp KeepAlive/Restart=always happens to bring campd back, so
    /// rerunning appears to work; on a --no-service camp, in a container, or in
    /// CI — exactly the camps `CampdNotRunning`'s own `camp daemon` line exists
    /// for — nothing brings it back and the advice strands the operator.
    /// `daemon_lifecycle::camp_top_after_a_kill_dash_nine_names_the_dead_campd_pid`
    /// does precisely what the old text instructed and asserts the rerun FAILS.
    fn assert_wedge_text_promises_no_cli_spawn(msg: &str) {
        assert!(
            !msg.contains("start a fresh campd"),
            "the wedge text must not promise that rerunning a verb starts a campd — \
             the CLI is a pure client: {msg}"
        );
        assert!(
            msg.contains("camp daemon") || msg.contains("camp service"),
            "having killed the wedged campd, the operator needs the REAL way one comes \
             back (the supervisor, or `camp daemon`): {msg}"
        );
    }

    /// The pid-unknown flavor (no campd.started carries one — e.g. a
    /// pre-#55 ledger): still loud, still bounded, still actionable; the
    /// missing pid is stated, never silently omitted.
    #[test]
    fn a_wedged_campd_without_a_recorded_pid_is_still_actionable() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        drop(camp_core::ledger::Ledger::open(&camp.db_path()).unwrap()); // empty ledger
        let _wedged = UnixListener::bind(camp.socket_path()).unwrap();

        let err = request(&camp, &Request::Status).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("kill -9"), "must name the remedy: {msg}");
        assert!(
            msg.contains("pid unknown"),
            "the missing pid must be stated: {msg}"
        );
        assert_wedge_text_promises_no_cli_spawn(&msg);
    }

    /// campd-not-listening is a NORMAL state for the converse verb (the
    /// resume path) — Ok(None), never an error.
    #[test]
    fn request_if_up_returns_none_when_no_daemon_listens() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        // no listener at all
        assert!(request_if_up(&camp, &Request::Status).unwrap().is_none());
        // a stale file that refuses connections is also "not up"
        drop(UnixListener::bind(camp.socket_path()).unwrap());
        assert!(request_if_up(&camp, &Request::Status).unwrap().is_none());
    }

    /// Design §4.3 + §3: campd DOWN is a loud, actionable fault — never a
    /// silent respawn. The error names the camp, the socket, the pid the
    /// ledger last recorded (the only pid source that survives a crash:
    /// there are no pidfiles), BOTH remedies, and the daemon's stderr log.
    /// A `kill -9` leaves a stale socket FILE behind, so "the file exists"
    /// is not life: a refusing socket is "not running" too.
    #[test]
    fn require_reports_a_down_campd_loudly_and_actionably() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        let mut ledger = camp_core::ledger::Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::CampdStarted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({ "pid": 424242 }),
            })
            .unwrap();
        drop(ledger);
        // the kill -9 shape: the socket file is there and refuses connections
        drop(UnixListener::bind(camp.socket_path()).unwrap());

        let err = require(&camp, &Request::Status).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            err.downcast_ref::<CampdNotRunning>().is_some(),
            "typed, so a down campd is never confused with a wedged one: {msg}"
        );
        assert!(msg.contains("campd is not running"), "{msg}");
        assert!(
            msg.contains("424242"),
            "must name the last recorded campd pid: {msg}"
        );
        assert!(
            msg.contains("camp service status"),
            "must name the supervised remedy: {msg}"
        );
        assert!(
            msg.contains("camp daemon"),
            "must name the run-it-yourself remedy (containers, CI, no service manager): {msg}"
        );
        assert!(
            msg.contains(&camp.root.display().to_string()),
            "must name the camp — a user has several: {msg}"
        );
        assert!(
            msg.contains("campd.log"),
            "must point at the daemon's stderr (a crash loop shows up there): {msg}"
        );
    }

    /// The pid-unknown flavor: campd never started in this camp. The absence
    /// is STATED, never silently omitted (the CampdUnresponsive precedent) —
    /// "never had one" and "yours died" are different situations.
    #[test]
    fn require_states_a_missing_pid_rather_than_omitting_it() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        drop(camp_core::ledger::Ledger::open(&camp.db_path()).unwrap()); // empty ledger
        // no socket file at all

        let err = require(&camp, &Request::Status).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no campd has ever started"),
            "the missing pid must be stated: {msg}"
        );
        assert!(msg.contains("camp service status"), "{msg}");
        assert!(msg.contains("camp daemon"), "{msg}");
    }

    /// Invariant 5, at the level of what the message CLAIMS. "No campd has ever
    /// started in this camp (no campd.started event in the ledger)" is a positive
    /// claim about ledger CONTENTS. A camp.db that cannot be read is not evidence
    /// for it — the code never read the contents at all. Reporting the two as one
    /// would tell an operator whose ledger is corrupt that their camp is merely
    /// fresh, hiding the fault behind a confident sentence.
    #[test]
    fn an_unreadable_ledger_is_never_reported_as_a_camp_that_never_ran_campd() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        // A camp.db that is not a database at all.
        std::fs::write(camp.db_path(), b"this is not a sqlite file").unwrap();

        let err = require(&camp, &Request::Status).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("campd is not running"),
            "the socket fault is still the headline: {msg}"
        );
        assert!(
            !msg.contains("no campd has ever started"),
            "an unreadable ledger must NOT be reported as a camp that never ran a campd — \
             the code never read the events: {msg}"
        );
        assert!(
            msg.contains("could not be read"),
            "the ledger fault must be STATED, not silently downgraded: {msg}"
        );
        // and the remedies still reach the operator
        assert!(msg.contains("camp service status"), "{msg}");
        assert!(msg.contains("camp daemon"), "{msg}");
    }

    /// Two laws in one test, both inherited from the path this phase deletes.
    ///
    /// (1) A WEDGED campd is not a down campd: something owns the socket, and
    /// its remedy is `kill -9`, not "start campd". `require` must not flatten
    /// the two — a second daemon would only mask the wedge, and telling the
    /// operator to start one would be wrong advice.
    ///
    /// (2) The request IS the probe (the PR #51 finding 1 law), asserted here
    /// at the VERB-LEVEL entry point — where the unit test in the module this
    /// phase deletes asserted it. A bare-connect pre-probe would open a second
    /// connection AND be fooled by the wedged daemon's kernel backlog, which
    /// accepts connections its event loop never serves. Counting accepts makes
    /// that a test failure, not a review catch, if anyone later "optimizes"
    /// `require`.
    #[test]
    fn require_tells_a_wedged_campd_apart_from_a_down_one_on_one_connection() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        let mut ledger = camp_core::ledger::Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::CampdStarted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({ "pid": 424242 }),
            })
            .unwrap();
        drop(ledger);
        // The wedge simulator: accept (and COUNT) every connection, then hold
        // it open and serve nothing — exactly a daemon stuck mid-syscall.
        let listener = UnixListener::bind(camp.socket_path()).unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepts);
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                counter.fetch_add(1, Ordering::SeqCst);
                held.push(stream); // keep it open, answer nothing
            }
        });

        let start = std::time::Instant::now();
        let err = require(&camp, &Request::Status).unwrap_err();
        assert!(
            start.elapsed() < REQUEST_TIMEOUT * 2 + Duration::from_secs(2),
            "bounded, never a hang"
        );
        let msg = format!("{err:#}");
        assert!(
            err.downcast_ref::<CampdUnresponsive>().is_some(),
            "a wedge stays a wedge: {msg}"
        );
        assert!(
            err.downcast_ref::<CampdNotRunning>().is_none(),
            "a wedged campd must never be reported as a down one: {msg}"
        );
        assert!(
            msg.contains("kill -9"),
            "the wedge remedy, unchanged: {msg}"
        );
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "exactly ONE connection: the request IS the probe — no bare-connect \
             pre-probe, which a wedged daemon's listen backlog would fool anyway"
        );
    }

    #[test]
    fn response_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Response::Ok { ok: true }).unwrap(),
            r#"{"ok":true}"#
        );
        let status = Response::Status {
            ok: true,
            summary: StatusSummary {
                live_sessions: vec!["camp/dev/1".to_owned()],
                ready: 1,
                open: 2,
            },
            red: 1,
            campd_pid: 4242,
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"ok":true,"live_sessions":["camp/dev/1"],"ready":1,"open":2,"red":1,"campd_pid":4242}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Error {
                ok: false,
                error: "bad request".to_owned()
            })
            .unwrap(),
            r#"{"ok":false,"error":"bad request"}"#
        );
        let adopt = Response::Adopt {
            ok: true,
            crashed: 1,
            rearmed: 2,
            released: 3,
            swept: 4,
            kept: 5,
        };
        assert_eq!(
            serde_json::to_string(&adopt).unwrap(),
            r#"{"ok":true,"crashed":1,"rearmed":2,"released":3,"swept":4,"kept":5}"#
        );
        assert!(matches!(
            serde_json::from_str::<Response>(
                r#"{"ok":true,"crashed":0,"rearmed":0,"released":0,"swept":0,"kept":0}"#
            )
            .unwrap(),
            Response::Adopt { .. }
        ));
        // client-side parse resolves the right variants
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true}"#).unwrap(),
            Response::Ok { ok: true }
        ));
        assert!(matches!(
            serde_json::from_str::<Response>(
                r#"{"ok":true,"live_sessions":[],"ready":0,"open":0,"red":0,"campd_pid":1}"#
            )
            .unwrap(),
            Response::Status { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":false,"error":"x"}"#).unwrap(),
            Response::Error { .. }
        ));
    }

    #[test]
    fn fresh_path_binds() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        let listener = bind_or_replace(&path).unwrap();
        drop(listener);
        assert!(path.exists());
        // The bind lock lives literally at `<socket>.lock` — the path every
        // doc names (PR #8 re-review finding 2: with_extension put it at
        // campd.lock, a file the docs never mention).
        assert!(
            dir.path().join("campd.sock.lock").exists(),
            "bind lock must be at <socket>.lock"
        );
    }

    #[test]
    fn stale_socket_is_unlinked_and_rebound() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        // a dead daemon (kill -9) leaves the file behind, refusing connections
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let listener = bind_or_replace(&path).expect("stale socket must be replaced");
        drop(listener);
    }

    #[test]
    fn live_socket_refuses_a_second_daemon() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        let _keep = bind_or_replace(&path).unwrap();
        let err = bind_or_replace(&path).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "error was: {err:#}"
        );
    }

    /// PR #8 review finding 1: the stale-socket replacement critical
    /// section (probe → unlink → rebind) must be serialized. Without a
    /// guard, two daemons racing past a stale socket can both probe-refuse,
    /// both unlink, both bind — leaving one live but orphaned on an
    /// unlinked inode (split brain: the one-standing-process shape breaks).
    #[test]
    fn concurrent_bind_or_replace_elects_exactly_one_daemon() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        use std::sync::{Arc, Barrier};
        for round in 0..50 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("campd.sock");
            // a stale socket, kill -9 shape: bound, then dropped un-unlinked
            drop(UnixListener::bind(&path).unwrap());

            let barrier = Arc::new(Barrier::new(8));
            let mut handles = Vec::new();
            for _ in 0..8 {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    bind_or_replace(&path)
                }));
            }
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners = results.iter().filter(|r| r.is_ok()).count();
            assert_eq!(
                winners, 1,
                "round {round}: exactly one daemon may own the socket"
            );
            assert!(
                UnixStream::connect(&path).is_ok(),
                "round {round}: the socket path must lead to the winner after the race"
            );
        }
    }
}

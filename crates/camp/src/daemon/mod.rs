//! campd: the only standing process (spec §5). Crash-only: no exclusive
//! state, `kill -9` is a supported shutdown method; on start it opens the
//! ledger, appends campd.started, catches up past its cursor, announces
//! readiness on stdout, and sleeps on the socket.

pub mod autostart;
pub mod bounded;
pub mod cursor;
pub mod dispatch;
pub mod event_loop;
pub mod orders;
pub mod patrol;
pub mod socket;
pub mod spawn;

use std::io::Write;

use anyhow::{Context, Result};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;
use cursor::ReadinessProcessor;

/// The single line campd prints to stdout once the socket accepts.
/// Auto-start (and the tests) block on it — an OS pipe read, not a
/// sleep/retry loop. stdout is never written again after this line.
pub const READY_PREFIX: &str = "campd listening on ";

/// Test-only serialization of child-spawning tests against socket-probe
/// tests. macOS lacks SOCK_CLOEXEC, so std sets FD_CLOEXEC in a second
/// syscall after socket(); a test that forks a child (git, /usr/bin/true)
/// in that window inherits another test's listener fd and keeps a
/// "dropped" socket accepting — the stale-socket probes then flake.
/// Production campd is immune: it binds single-threaded, before any
/// worker exists. Tests that spawn processes and tests that probe dead
/// sockets both hold this lock (a poisoned lock is fine — the dead
/// holder's window is over).
#[cfg(test)]
pub(crate) static SPAWN_PROBE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn spawn_probe_guard() -> std::sync::MutexGuard<'static, ()> {
    SPAWN_PROBE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn run(camp: &CampDir) -> Result<()> {
    // A daemon that cannot read its own config must not pretend to be up.
    let config = camp_core::config::CampConfig::load(&camp.config_path())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut ledger = Ledger::open(&camp.db_path())?;
    // "When did campd last OBSERVE the world" — the cursor-position ts,
    // read BEFORE the startup settle advances the cursor, so cron fires
    // missed while campd was down catch up under the window even when a
    // daemon-less CLI write landed in between (spec §9; plan Decision F as
    // amended per the PR #13 review, MEDIUM 2).
    let last_seen0 = orders::catch_up_anchor(&ledger, jiff::Timestamp::now())?;
    let socket_path = camp.socket_path();
    let std_listener = socket::bind_or_replace(&socket_path)?;
    std_listener
        .set_nonblocking(true)
        .context("setting the listener non-blocking")?;
    let listener = mio::net::UnixListener::from_std(std_listener);

    // SIGCHLD self-pipe (Phase 8 plan decision I), registered before any
    // child can exist so no exit can be missed. signal-hook's handler
    // writes a byte; the poll loop drains it. No unsafe anywhere.
    let (sigchld_read, sigchld_write) =
        std::os::unix::net::UnixStream::pair().context("creating the SIGCHLD pipe")?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGCHLD, sigchld_write)
        .context("registering the SIGCHLD handler")?;
    sigchld_read
        .set_nonblocking(true)
        .context("setting the SIGCHLD pipe non-blocking")?;

    // The pid rides campd.started (issue #55): the ONE pid source that
    // survives a wedge — a wedged campd cannot answer the status op, and
    // there are no pidfiles (spec §5). Recorded after the bind wins, so
    // the last campd.started always names the socket's current holder.
    ledger.append(EventInput {
        kind: EventType::CampdStarted,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({ "pid": std::process::id() }),
    })?;

    // Declared automation must parse or campd refuses to start (fail
    // fast); the lenient-but-evented path is hot reload only.
    let clock = camp_core::clock::SystemClock;
    let tz = jiff::tz::TimeZone::system();
    let mut runtime = orders::OrdersRuntime::build(&camp.root, jiff::Timestamp::now(), tz)?;

    // camp.toml watch: notify's callback (its own thread) signals the mio
    // loop through a self-pipe. The camp ROOT is watched non-recursively —
    // editors rename-replace, and a file watch dies with the inode.
    let (sender, mut receiver) = mio::unix::pipe::new().context("creating the watch pipe")?;
    // Watcher errors land in the ledger, not just stderr (PR #13 review
    // LOW 8): the callback stores them in the runtime's slot and wakes the
    // loop, which appends the rejected config.changed.
    let watch_errors = runtime.watch_error_slot();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        orders::on_watch_event(result, Some(&sender), &watch_errors);
    })
    .context("creating the camp.toml watcher")?;
    notify::Watcher::watch(
        &mut watcher,
        &camp.root,
        notify::RecursiveMode::NonRecursive,
    )
    .context("watching the camp directory")?;

    // The patrol runtime (Phase 11, spec §10): typed config fails fast
    // (already validated at parse — belt-and-braces), transcript watches
    // signal the loop through their own self-pipe (Token 3), and the
    // watcher installs before any settle can track a session.
    let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut patrol = patrol::PatrolRuntime::new(patrol_config, &config);
    let (patrol_sender, mut patrol_receiver) =
        mio::unix::pipe::new().context("creating the patrol watch pipe")?;
    let patrol_filter = patrol.filter_slot();
    let patrol_watcher =
        notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            patrol::on_watch_event(result, Some(&patrol_sender), &patrol_filter);
        })
        .context("creating the transcript watcher")?;
    patrol.set_watcher(patrol_watcher);

    // Startup settle is fatal on error: a daemon that cannot process its
    // backlog must not pretend to be up (fail fast). Settle drains events
    // past the cursor, cooks any order.fired they declare, AND dispatches
    // whatever is ready (the Phase 8 + Phase 10 + Phase 11 joint
    // fixpoint). Per-bead dispatch problems are dispatch.failed events,
    // not errors — only a broken ledger stops the daemon.
    let mut processor = ReadinessProcessor::default();
    // Phase 9: the graph runtime shares the config snapshot the Dispatcher
    // takes (rig paths for check-script cwd). Phase 11's patrol::adopt
    // below also needs the config, so the Dispatcher takes a clone.
    let mut graph = dispatch::GraphRuntime::new(camp.root.clone(), &config);
    let mut dispatcher = dispatch::Dispatcher::new(camp.clone(), config.clone());
    event_loop::settle(
        &mut ledger,
        &mut processor,
        &mut runtime,
        &clock,
        &mut dispatcher,
        &mut graph,
        &mut patrol,
    )?;
    // Adoption (spec §8.5, automatic at start): reconcile the registry
    // against the process table — dead rows crash (beads release), living
    // workers re-arm, finished lingerers release, orphan worktrees sweep.
    // Its events drain in the settle below.
    let adopted = patrol::adopt(&mut ledger, &mut patrol, &mut dispatcher)?;
    if adopted != patrol::AdoptSummary::default() {
        eprintln!(
            "campd: adopted: {} crashed, {} re-armed, {} released, {} worktrees swept, {} kept",
            adopted.crashed, adopted.rearmed, adopted.released, adopted.swept, adopted.kept
        );
    }
    // Fires orphaned by a crash between order.fired and its cook (the
    // cursor is already past them): queue them for the next settle —
    // exactly once, execute_fire dedupes. Observation over state; kill -9
    // self-heals.
    for cook in camp_core::orders::unresponded_fires(&ledger)? {
        runtime.queue_cook(cook);
    }
    // Phase 9: re-derive graph work whose side effects died with the last
    // process — interrupted checks re-queue (re-runnable by contract),
    // incomplete fan-outs re-queue (execute computes what is owed).
    graph.reconcile(&mut ledger)?;
    // Cron fires missed while campd was down, under each order's window.
    let now = jiff::Timestamp::now();
    let fires: Vec<camp_core::orders::cron::Fire> = runtime
        .recompute(now, last_seen0)
        .into_iter()
        .map(|c| c.into_fire(now))
        .collect();
    orders::declare_cron_fires(&mut ledger, &fires)?;
    event_loop::settle(
        &mut ledger,
        &mut processor,
        &mut runtime,
        &clock,
        &mut dispatcher,
        &mut graph,
        &mut patrol,
    )?;

    let mut stdout = std::io::stdout();
    writeln!(stdout, "{READY_PREFIX}{}", socket_path.display()).context("announcing readiness")?;
    stdout.flush().context("flushing the readiness line")?;

    // `watcher` must live until the loop returns — dropping it kills the
    // camp.toml watch (the patrol watcher lives inside `patrol`).
    let result = event_loop::run(
        listener,
        sigchld_read,
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
    drop(watcher);
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    /// Test-harness-only readiness wait (the daemon itself never polls;
    /// out-of-process callers get the stdout readiness line instead).
    fn connect_with_retry(sock: &Path) -> UnixStream {
        for _ in 0..500 {
            if let Ok(stream) = UnixStream::connect(sock) {
                return stream;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("campd socket {} never accepted", sock.display());
    }

    fn request(stream: &mut UnixStream, line: &str) -> serde_json::Value {
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut resp = String::new();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        reader.read_line(&mut resp).unwrap();
        serde_json::from_str(resp.trim_end()).expect("campd response is JSON")
    }

    #[test]
    fn daemon_with_a_broken_config_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\nbogus = 1\n").unwrap();
        let camp = CampDir { root };
        let err = run(&camp).unwrap_err();
        assert!(err.to_string().contains("bogus"), "got {err:#}");
    }

    #[test]
    fn daemon_serves_status_poke_and_stop_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let camp = CampDir { root: root.clone() };
        let handle = std::thread::spawn(move || run(&camp));

        let sock = root.join("campd.sock");
        let mut stream = connect_with_retry(&sock);

        let status = request(&mut stream, r#"{"op":"status"}"#);
        assert_eq!(status["ok"], true);
        assert_eq!(status["campd_pid"], std::process::id());
        assert_eq!(status["ready"], 0);
        assert_eq!(status["open"], 0);
        assert_eq!(status["live_sessions"], serde_json::json!([]));

        let poke = request(&mut stream, r#"{"op":"poke","seq":1}"#);
        assert_eq!(poke, serde_json::json!({"ok": true}));

        // Phase 11: adopt on demand — a fresh camp reconciles to zeros
        // and the daemon keeps serving.
        let adopt = request(&mut stream, r#"{"op":"adopt"}"#);
        assert_eq!(
            adopt,
            serde_json::json!({
                "ok": true, "crashed": 0, "rearmed": 0, "released": 0,
                "swept": 0, "kept": 0
            })
        );

        // an unknown op gets a clean error response on a fresh connection
        let mut bad = UnixStream::connect(&sock).unwrap();
        let err = request(&mut bad, r#"{"op":"dance"}"#);
        assert_eq!(err["ok"], false);
        assert!(err["error"].as_str().unwrap().contains("bad request"));

        let stop = request(&mut stream, r#"{"op":"stop"}"#);
        assert_eq!(stop, serde_json::json!({"ok": true}));
        handle.join().unwrap().unwrap();
        assert!(!sock.exists(), "stop must unlink the socket");

        // the ledger tells the story and the cursor is caught up
        let ledger = Ledger::open(&root.join("camp.db")).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let types: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(types, vec!["campd.started", "campd.stopped"]);
        // Issue #55: campd.started records the daemon's pid — the one pid
        // source that survives a wedge (a wedged campd cannot answer the
        // status op), read back by the CLI's CampdUnresponsive error.
        assert_eq!(
            events[0].data["pid"],
            serde_json::json!(std::process::id()),
            "campd.started must carry the daemon pid: {:?}",
            events[0].data
        );
        assert_eq!(
            ledger.cursor(cursor::CAMPD_CURSOR).unwrap(),
            1,
            "startup catch-up covered campd.started; campd.stopped (seq 2) \
             lands after the final catch-up — the next start covers it"
        );
    }

    /// PR #8 review finding 3: a client streaming a newline-less line must
    /// be cut off with a clean error, not buffered without bound past the
    /// idle-RSS budget (invariant 1 / spec §2.1).
    #[test]
    fn oversized_request_line_is_rejected_and_the_connection_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let camp = CampDir { root: root.clone() };
        let handle = std::thread::spawn(move || run(&camp));

        let sock = root.join("campd.sock");
        let mut stream = connect_with_retry(&sock);
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        // Exactly one byte over the cap, no newline, then stop writing: the
        // daemon consumes everything before rejecting, so its close is a
        // clean FIN and the error line is deterministically deliverable on
        // every platform. (Writing MORE would leave unread bytes in the
        // daemon's receive queue at close time, which resets the connection
        // on Linux and clobbers the response — the in-the-wild firehose
        // case, where the drop is the contract and the response is
        // best-effort.)
        let oversized = vec![b'x'; event_loop::MAX_REQUEST_BYTES + 1];
        stream.write_all(&oversized).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the daemon must answer an oversized line, not buffer it forever");
        let resp: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(resp["ok"], false);
        assert!(
            resp["error"].as_str().unwrap().contains("exceeds"),
            "error was: {resp}"
        );
        // the offending connection is closed…
        line.clear();
        let n = reader.read_line(&mut line).unwrap();
        assert_eq!(n, 0, "daemon must close the oversized connection");

        // …and campd is unharmed for the next client
        let mut fresh = connect_with_retry(&sock);
        let status = request(&mut fresh, r#"{"op":"status"}"#);
        assert_eq!(status["ok"], true);
        let stop = request(&mut fresh, r#"{"op":"stop"}"#);
        assert_eq!(stop, serde_json::json!({"ok": true}));
        handle.join().unwrap().unwrap();
    }

    /// PR #8 re-review finding 1: a client pipelining more than the 64 KB
    /// cap of VALID newline-delimited requests in one burst must get every
    /// one answered. The cap-break interacts with mio's edge-triggered
    /// registration: if the daemon stops reading at the cap, answers the
    /// complete lines, and returns to poll with data still in the kernel
    /// receive buffer, no new readable event ever fires (the client is done
    /// writing) — the connection wedges.
    #[test]
    fn pipelined_valid_requests_beyond_the_cap_are_all_answered() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let camp = CampDir { root: root.clone() };
        let handle = std::thread::spawn(move || run(&camp));

        let sock = root.join("campd.sock");
        let mut stream = connect_with_retry(&sock);
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        // Reader thread drains responses concurrently (a client that never
        // reads would hit the decision-J write-backpressure drop instead —
        // a different, intended behavior).
        const N: usize = 3500;
        let reader_stream = stream.try_clone().unwrap();
        reader_stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(reader_stream);
            let mut answered = 0usize;
            let mut line = String::new();
            for _ in 0..N {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(n) if n > 0 && line.trim_end() == r#"{"ok":true}"# => answered += 1,
                    _ => break, // EOF, timeout, or a wrong answer: stop counting
                }
            }
            answered
        });

        // one burst of valid pokes, comfortably past the cap
        let mut burst = String::new();
        for i in 0..N {
            burst.push_str(&format!("{{\"op\":\"poke\",\"seq\":{i}}}\n"));
        }
        assert!(
            burst.len() > event_loop::MAX_REQUEST_BYTES,
            "the burst must exceed the cap for this test to mean anything"
        );
        stream.write_all(burst.as_bytes()).unwrap();

        let answered = reader.join().unwrap();
        assert_eq!(answered, N, "every pipelined request must be answered");

        // graceful shutdown
        let mut fresh = connect_with_retry(&sock);
        let stop = request(&mut fresh, r#"{"op":"stop"}"#);
        assert_eq!(stop, serde_json::json!({"ok": true}));
        handle.join().unwrap().unwrap();
    }
}

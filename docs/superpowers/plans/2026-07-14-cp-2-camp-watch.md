# cp-2 — `camp watch`, the fleet view — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `camp watch` — a push-driven fleet monitor that renders one line per live session (agent, bead, state, FOR, LAST) from campd's socket alone, driven by a new `fleet.subscribe` connection mode. No client file access, zero polling.

**Architecture:** cp-1 shipped `session.subscribe` as a long-lived, server-push socket MODE backed by a flat `Subscriber` struct that tails one worker's stdout FILE. cp-2 does two things. (1) It refactors that flat struct into the seam cp-1's plan claimed but did not take: `Subscriber { out: OutBuf, source: Source }`, where `OutBuf` owns the buffer cap / backpressure-stall / peer-drop policy EXACTLY ONCE and `Source` is either `File(FileSource)` (cp-1's byte-cursor tailer, moved verbatim) or `Fleet(FleetSource)` (new). (2) It adds `fleet.subscribe`, a LEDGER-sourced aggregate stream whose source is campd's fleet MODEL — the same `Vec<SessionInfo>` `sessions.list` already computes from the ledger registry + patrol + the read channel — diffed per subscriber and pushed as `session` / `gone` / `synced` frames on every campd wake. `camp watch` is a stateless renderer that replaces its rows by name.

**Tech Stack:** Rust, mio (single-threaded event loop), serde/serde_json (newline-delimited JSON wire), SQLite ledger, clap CLI. No new dependencies.

## Why the model-diff source (verified, do not restructure)

The plan-gate completeness-critic confirmed this against the merged code, and it is the load-bearing architecture decision: two fleet transitions are NOT evented, so a ledger-seq-cursor replay cannot reconstruct STATE/LAST.
- **Stall RECOVERY is not evented.** `patrol.rs:508` (worker activity revives) and `patrol.rs:601` (`drain_touched` transcript heartbeat revives) both `self.stalled.remove(&session)` in memory with NO ledger append. Only the TO-stalled transition is evented (`patrol.rs:661` `agent.stalled`).
- **`last_activity` is not evented.** `read_channel.rs:806-807` stamps `last_activity` in memory with no append.

So the fleet source is a per-wake recompute of `fleet_model()` (ledger registry + `patrol.is_stalled` + `read_channel.last_activity`), diffed per subscriber. Eventing recovery would be write-amplification on the hot path — the wrong turn. This is "LEDGER-sourced" in the inheritance-warning sense: the source is campd's ledger-derived TRUTH, cursored by session NAME, not a file byte offset — which is exactly why "hold the line in `partial`" cannot transfer (a fleet row has no file), and the cap-STOP becomes "don't advance `sent`; the model is recomputable next wake."

## Global Constraints

Copied verbatim from AGENTS.md invariants and the kickoff; every task's requirements implicitly include these.

- **Idle is free.** No ticks, no polling loops. Components sleep on OS events. A fleet subscriber on quiescent workers must cost ZERO wakeups and stay inside the idle RSS bound (§4.3).
- **Bounded work per wake.** cp-1's `MAX_PUMP_BYTES_PER_WAKE` (256 KiB, `control.rs:3064`) is a PER-`pump_subscriber`-CALL scan budget: a 256 MiB line is consumed over many wakes, each doing bounded work, and it terminates (invariant 1). The refactor MUST preserve this exactly.
- **Fail fast.** No fallbacks, no silenced errors, no placeholders. No panics in library code — clippy `unwrap_used`/`expect_used`/`panic` are DENIED outside `#[cfg(test)]`; `unsafe_code` forbidden. Every error surfaces to the caller or lands in the ledger as an event.
- **Nothing hidden.** Every campd action is an event with its cause. A dropped subscriber is a loud `subscriber.dropped`; campd never blocks on a slow peer and never silently discards a stream.
- **Sessions are addressed by name, never by pid or file path** (§4.2). No wire field may carry a pid or a path to a client.
- **campd owns the truth; clients are stateless renderers** (§4.2). `camp watch` renders what campd sends; it never tails a file or reads the ledger directly.
- **The transport is swappable; the protocol is not** (§4.2). New verbs and frames are additive; existing wire shapes do not change, and every new wire shape is byte-pinned.
- **cp-2 introduces NO new `EventType`** (fleet frames are transport, not durable truth; the only ledger event cp-2 emits is the existing `subscriber.dropped`). So `crates/camp-core/src/event.rs`, `vocab.rs`, `ledger/fold.rs` are NOT modified by this plan.
- **Guaranteed-contention files must stay ADDITIVE** (compat-3 runs in parallel): `crates/camp/src/main.rs`, `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`, `Cargo.toml`, `Cargo.lock`. cp-2 touches ONLY `main.rs` among these (one additive `Watch` command arm + subcommand variant) and adds NO dependency (so `Cargo.toml`/`Cargo.lock` are untouched). All other work lands in `control.rs`, `socket.rs`, `event_loop.rs`, and a new `cmd/watch.rs` — none contended.
- **Gates green before any push:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`. Perf gate (`make perf`) is LOCAL-ONLY and run for the perf-relevant task (Task 8).
- **Branch:** `cp-2-camp-watch`. Never commit to main. No co-author lines. After any merge to main, rebase onto main and re-run the gates before continuing.

---

## Scoping decisions (read before Task 1)

These are decisions this plan makes where the spec shows an end-state richer than cp-2's slice. Each is documented so the implementer does not "fix" a deliberate boundary.

1. **LAST column is a relative-time indicator, not a tool summary.** §5.1 renders LAST as `Edit(src/lib.rs)` / `? Bash(cargo publish)` — a summary of the worker's last typed stream event. Producing that requires parsing stream-json tool-use lines, which is the AGENT VIEW's machinery (`camp attach`, phase 4). campd today tracks only a last-activity TIMESTAMP (`read_channel.last_activity`), not the last tool. cp-2 renders LAST from that timestamp as a relative age (`12s`, `no output 12m`). The rich tool-call form is a phase-4 enrichment of the SAME fleet row and is out of scope here. `fleet.subscribe`'s wire carries the full `SessionInfo` (including `last_activity`), so the enrichment is additive later.
2. **`fleet.subscribe` is LEDGER/model-sourced, not ledger-event-replay** (see "Why the model-diff source" above — verified against the code).
3. **`fleet.subscribe` takes no cursor and does no delta-replay resume.** §4.1 gives `session.subscribe` a cursor ("history, then follows") but gives `fleet.subscribe` none — its "history" is the current snapshot, which is the truth. A reconnecting client re-subscribes and gets a fresh full snapshot (its `sent` map starts empty). The hello carries a `v` (protocol version) for future-proofing, no cursor.
4. **FOR (time-in-state) is tracked CLIENT-SIDE.** Adding a `state_since` field to the cp-1 `SessionInfo` wire struct would churn the pinned `sessions_list_wire_format_is_pinned` test (`socket.rs:793`) and change `sessions.list`'s wire for no cp-2 requirement. Instead `camp watch` records when it first observed each session's current state string and renders FOR from that. Approximate (resets on reconnect), correct for a live monitor.
5. **BLOCKED is rendered, never produced.** cp-3 owns the `can_use_tool` producer that sets `blocked: true`. cp-2 builds the STATE column so BLOCKED "drops in": the renderer maps `blocked == true` → `BLOCKED` (with a `← needs you` marker), even though cp-2's campd never sets the bit. The field already exists on `SessionInfo` (`socket.rs:109`) and is byte-pinned transitively via `sessions_list_wire_format_is_pinned`.

---

## File structure

- **Modify `crates/camp/src/daemon/control.rs`** — the seam (`OutBuf`, `Source::{File,Fleet}`, `Subscriber` restructure), the fleet source (`FleetSource`, frame builders, diff/fill), `fleet_model()` extracted from `serve_sessions_list`, `serve_fleet_subscribe`, and the fleet arm of the unified fanout/pump. Largest change; already the home of all subscriber machinery.
- **Modify `crates/camp/src/daemon/socket.rs`** — `Request::FleetSubscribe`, `Response::FleetSubscribed`, wire-pin tests; add `Clone` to `SessionInfo`.
- **Modify `crates/camp/src/daemon/event_loop.rs`** — dispatch `fleet.subscribe` (mirror the `session.subscribe` arm), thread `patrol` into `control_step`/`fanout`, run the fleet fanout each wake.
- **Create `crates/camp/src/cmd/watch.rs`** — the `camp watch` client: connect, `fleet.subscribe`, read frames, maintain a by-name row map, render the table (pure, unit-tested).
- **Modify `crates/camp/src/main.rs`** — one additive `Watch` command variant + its dispatch arm (guaranteed-contention file; additive only).
- **Modify `crates/camp/tests/control.rs`** — the end-to-end fleet test against a real campd + fake worker; reuse the existing `Daemon`/`scaffold`/`dispatch_one`/`wait_until` harness.
- **Modify `crates/camp/tests/perf_daemon.rs`** — extend the idle gate to include N fleet subscribers (Task 8, `make perf`).

---

## Task 1: The `OutBuf` seam — factor the flat `Subscriber`

Refactor cp-1's flat `Subscriber` (`control.rs:3261-3352`) into `Subscriber { id, out: OutBuf, source: Source }`. `OutBuf` owns the outbound buffer and the cap/backpressure-stall/peer-drop POLICY exactly once. `Source::File(FileSource)` holds ALL of cp-1's file/byte-cursor fields and its FILL logic, MOVED VERBATIM — **including cp-1's per-`pump_subscriber`-call `MAX_PUMP_BYTES_PER_WAKE` scan budget**, which is threaded through `fill`. This task is behaviour-preserving for `session.subscribe`: its gate is the entire existing subscriber test suite staying green, plus new unit tests that pin the `OutBuf` stall-drop policy AND the per-wake scan budget in isolation.

**Files:**
- Modify: `crates/camp/src/daemon/control.rs` (the `Subscriber` struct + `pump_subscriber` + `try_emit_line` + `serve_subscribe` + `fanout` + `pump` + `close_disposed` + `test_insert_subscriber` + the test helpers `pump_to_completion`/`test_sub`)
- Test: `crates/camp/src/daemon/control.rs` (its `#[cfg(test)]` module)

**Interfaces:**
- Consumes: cp-1's `Conn` (`event_loop::Conn`), `PumpOutcome`, `Disposed`, `SUBSCRIBER_STALL_TIMEOUT_DEFAULT` (`control.rs:3087`), `SUBSCRIBER_BUFFER_BYTES`/`SUBSCRIBER_BUFFER_BYTES_DEFAULT` (`control.rs:3054`/`944`), `MAX_SUBSCRIBERS`, `HISTORY_CHUNK_BYTES` (`control.rs:3058`), `MAX_PUMP_BYTES_PER_WAKE` (`control.rs:3064`).
- Produces (used by Tasks 4/5):
  - `struct OutBuf { out: Vec<u8>, high_water: usize, blocked_since: Option<Timestamp> }` (all fields `pub` — test helpers read `.out`/`.blocked_since`)
  - `impl OutBuf`: `fn new() -> OutBuf`; `fn is_empty(&self) -> bool`; `fn has_room(&self, frame_len: usize, cap: usize) -> bool`; `fn append(&mut self, frame: &[u8])`; `fn flush(&mut self, conn: &mut Conn, now: Timestamp, stall_timeout: Duration) -> FlushStep`
  - `#[derive(Debug)] enum FlushStep { Drained, WouldBlock, Stalled, Gone }`
  - `enum Source { File(FileSource), Fleet(FleetSource) }` (Task 4 adds the `Fleet` variant; Task 1 introduces `Source` with only `File`)
  - `struct FileSource { session, file, cursor, scan, partial, held, oversize, tail, closing, end_sent, frame_overhead }` (every field moved from cp-1's `Subscriber` except `id`, `out`, `high_water`, `blocked_since`)
  - `impl FileSource`: `fn fill(&mut self, out: &mut OutBuf, cap: usize, scanned: &mut usize, pending_events: &mut Vec<EventInput>, degraded_seen: &mut HashSet<(String, u64)>) -> bool` — **`scanned` is the per-pump-call budget accumulator, owned by the driver and passed by `&mut`**; `fn try_emit_line(&mut self, out: &mut OutBuf, cap: usize) -> bool`
  - `struct Subscriber { id: String, out: OutBuf, source: Source }`; test accessor `#[cfg(test)] fn Subscriber::file(&self) -> &FileSource`
  - `fn pump_subscriber(sub: &mut Subscriber, conn: &mut Conn, now: Timestamp, cap: usize, stall_timeout: Duration, pending_events: &mut Vec<EventInput>, degraded_seen: &mut HashSet<(String, u64)>, fleet_model: &[SessionInfo]) -> PumpOutcome` (same signature as cp-1 plus a trailing `fleet_model` arg File ignores)

- [ ] **Step 1: Write the failing unit test for `OutBuf`'s stall-drop policy**

Add to the `#[cfg(test)]` module in `control.rs`. It fills the socket at `t0` until a write WouldBlocks (stamping `blocked_since = t0`), then flushes ONCE past the stall window and asserts `Stalled`. This passes against correct code (a fresh socket's first write returns `Ok(n)`/`Drained`, clearing `blocked_since`) and pins the `>= stall_timeout` mutation (the same `later` timestamp reused every iteration would compute `0 < stall` forever and never stall).

```rust
#[test]
fn outbuf_flush_stalls_a_peer_that_stops_reading_past_the_window() {
    use std::os::unix::net::UnixStream;
    let (client, server) = UnixStream::pair().unwrap();
    server.set_nonblocking(true).unwrap();
    let mut conn = Conn { stream: mio::net::UnixStream::from_std(server), buf: Vec::new() };
    // `client` is never read: the peer has stopped reading.

    let mut out = OutBuf::new();
    out.append(&vec![b'x'; 512 * 1024]); // more than one socket send buffer
    let t0 = jiff::Timestamp::now();
    let stall = std::time::Duration::from_millis(50);

    // Flush at t0 until the socket is full and the write WouldBlocks — THIS is
    // where blocked_since is stamped (a first Ok(n) partial write clears it, so
    // the stamp only survives once no more bytes are accepted).
    loop {
        match out.flush(&mut conn, t0, stall) {
            FlushStep::Drained => continue,
            FlushStep::WouldBlock => break,
            other => panic!("unexpected flush step before the window: {other:?}"),
        }
    }
    assert_eq!(out.blocked_since, Some(t0), "a zero-accept write stamps blocked_since at t0");

    // One flush 60ms later — past the 50ms window — must Stall.
    let later = t0 + jiff::SignedDuration::from_millis(60);
    assert!(
        matches!(out.flush(&mut conn, later, stall), FlushStep::Stalled),
        "a peer that has not read for >= stall_timeout is Stalled"
    );
    drop(client);
}
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p camp --lib outbuf_flush_stalls -- --nocapture`
Expected: FAIL — `OutBuf`, `FlushStep` are not yet defined.

- [ ] **Step 3: Define `OutBuf` and `FlushStep`, extracting cp-1's FLUSH block (C)**

Add above `pump_subscriber`. This is cp-1's `pump_subscriber` block `(C) FLUSH` (`control.rs:3593-3643`) MOVED verbatim into a method, MINUS the `SubscriberDropped` event construction — the driver builds that (it needs the subscriber's `id`/source identity, which `OutBuf` deliberately does not know).

```rust
/// What one `flush` attempt did.
#[derive(Debug)]
pub enum FlushStep {
    /// The socket accepted bytes (out may still hold more) — the driver re-fills.
    Drained,
    /// The socket is full and the peer is still reading — the WRITABLE edge re-arms us.
    WouldBlock,
    /// R1: the peer accepted ZERO bytes for `stall_timeout` with data buffered — DROP it.
    Stalled,
    /// EPIPE / ECONNRESET / a zero-length write — the peer is gone.
    Gone,
}

/// The outbound half of every subscription — file OR fleet. It owns the §4.4
/// buffer-cap policy (a STOP), the R1 backpressure-stall policy (a DROP), and
/// the socket write. The stall rule is the ONLY drop policy, and it lives here
/// exactly once. "Hold the line in `partial`" is NOT here — that is a FILE
/// concept (a fleet row has no file), so it stays in `FileSource`.
pub struct OutBuf {
    /// Bytes queued for this socket. Bounded by the cap (a STOP, never a kill),
    /// plus at most one small over-cap `skipped` frame (see `FileSource`).
    pub out: Vec<u8>,
    /// The largest `out` reached — `buffered_bytes` in `subscriber.dropped`.
    pub high_water: usize,
    /// R1: when the peer last accepted ZERO bytes with data buffered. Stamped on
    /// a zero-accept write, CLEARED the moment ANY byte is accepted.
    pub blocked_since: Option<Timestamp>,
}

impl OutBuf {
    pub fn new() -> OutBuf {
        OutBuf { out: Vec::new(), high_water: 0, blocked_since: None }
    }
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }
    /// §4.4: does one more frame fit under the cap? The cap is a STOP — a source
    /// whose next frame does not fit HOLDS it (file: in `partial`; fleet: by not
    /// advancing `sent`) rather than dropping the peer.
    pub fn has_room(&self, frame_len: usize, cap: usize) -> bool {
        self.out.len() + frame_len <= cap
    }
    pub fn append(&mut self, frame: &[u8]) {
        self.out.extend_from_slice(frame);
        self.high_water = self.high_water.max(self.out.len());
    }
    /// ONE write attempt. cp-1's FLUSH block (C), verbatim minus the drop-event
    /// construction (the caller owns the event shape).
    pub fn flush(&mut self, conn: &mut Conn, now: Timestamp, stall_timeout: Duration) -> FlushStep {
        use std::io::Write as _;
        match conn.stream.write(&self.out) {
            Ok(0) => FlushStep::Gone,
            Ok(n) => {
                self.out.drain(..n);
                self.blocked_since = None; // R1: it IS reading.
                FlushStep::Drained
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => FlushStep::Drained,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // G2: the WRITABLE edge re-arms us; do NOT arm a timeout here.
                if self.blocked_since.is_none() {
                    self.blocked_since = Some(now);
                }
                if let Some(since) = self.blocked_since
                    && now.duration_since(since) >= signed(stall_timeout)
                {
                    self.high_water = self.high_water.max(self.out.len());
                    return FlushStep::Stalled;
                }
                FlushStep::WouldBlock
            }
            Err(_) => FlushStep::Gone,
        }
    }
}
```

- [ ] **Step 4: Run the new test to verify it passes**

Run: `cargo test -p camp --lib outbuf_flush_stalls -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Move cp-1's file fields into `FileSource` and its FILL logic into `FileSource::fill` — INCLUDING the scan budget**

Introduce `Source` and `FileSource`. Move every cp-1 `Subscriber` field EXCEPT `id`, `out`, `high_water`, `blocked_since` into `FileSource`. Move cp-1's `pump_subscriber` blocks `(A) FILL` and `(B) TERMINAL` (`control.rs:3442-3591`) into `FileSource::fill`, and `try_emit_line` becomes a `FileSource` helper. **Preserve every inline invariant comment (B1/B2/B3/R1/R2/R3/G1/G2) with its code.**

**THE SCAN BUDGET IS NOT OPTIONAL AND IS NOT INSIDE THAT RANGE.** cp-1 declares `let mut scanned = 0usize;` at `control.rs:3435` — OUTSIDE the outer `loop {` (3437) — and the FILL while-guard at 3450 is `... && scanned < MAX_PUMP_BYTES_PER_WAKE`, with `scanned += 1` per byte absorbed (~3504). Because `scanned` persists across every FILL→FLUSH→re-FILL iteration of one `pump_subscriber` CALL, the budget is 256 KiB scanned PER CALL (const doc `3060-3064`: "a 256 MiB line is consumed over many wakes, each doing bounded work, and it terminates"). If `scanned` were local to `fill`, it would reset on every re-fill and the budget would degrade from per-call to per-fill — unbounded when a fast reader drains the socket. So `scanned` is OWNED BY THE DRIVER (Step 6) and passed into `fill` by `&mut`; the while-guard and the `+= 1` use the passed-in reference.

```rust
pub enum Source {
    File(FileSource),
    // Fleet(FleetSource) is added in Task 4.
}

/// cp-1's byte-cursor tailer, unchanged. One worker's stdout FILE, delivered
/// `[cursor, tail)` as `event`/`skipped`/`end` frames.
pub struct FileSource {
    pub session: String,
    file: std::fs::File,
    cursor: u64,
    scan: u64,
    partial: Vec<u8>,
    held: bool,
    oversize: Option<u64>,
    tail: u64,
    closing: Option<String>,
    end_sent: bool,
    frame_overhead: usize,
}

impl FileSource {
    /// cp-1 blocks (A) FILL + (B) TERMINAL, verbatim. Emits frames INTO `out`
    /// via `out.has_room` / `out.append`, STOPPING at the cap (R1). `scanned` is
    /// the driver's per-pump-call budget: the FILL while-guard keeps
    /// `*scanned < MAX_PUMP_BYTES_PER_WAKE` and `*scanned += 1` runs per byte
    /// absorbed, EXACTLY as cp-1 did with its function-local `scanned`. Returns
    /// whether it is TERMINAL (the `end` frame appended, nothing left to give).
    fn fill(
        &mut self,
        out: &mut OutBuf,
        cap: usize,
        scanned: &mut usize,
        pending_events: &mut Vec<EventInput>,
        degraded_seen: &mut HashSet<(String, u64)>,
    ) -> bool {
        // MOVED from cp-1 pump_subscriber blocks (A) and (B), with the mechanical
        // renames: the function-local `let mut scanned = 0usize;` (control.rs:3435)
        // is DELETED here (the driver owns it); every `scanned` reference uses the
        // `*scanned` parameter; `sub.out.len() + frame.len() > cap` becomes
        // `!out.has_room(frame.len(), cap)`; `sub.out.extend_from_slice(&frame)` +
        // high_water bookkeeping becomes `out.append(&frame)`; `sub.field` becomes
        // `self.field`; `try_emit_line(sub, cap)` becomes
        // `Self::try_emit_line(self, out, cap)`. Return `self.end_sent` at the tail.
        // (The FLUSH block (C) is REMOVED — the driver owns it.)
        todo!("mechanical move — see cp-1 control.rs:3435-3591, scanned threaded via the parameter")
    }

    /// cp-1 `try_emit_line` (control.rs:3368-3404), verbatim, taking `out`
    /// instead of the whole subscriber. Preserve every B1/R1/R3/G11 comment.
    fn try_emit_line(&mut self, out: &mut OutBuf, cap: usize) -> bool {
        todo!("mechanical move — see cp-1 control.rs:3368-3404")
    }
}
```

The `Subscriber` struct becomes:

```rust
pub struct Subscriber {
    id: String,
    out: OutBuf,
    source: Source,
}

/// Test-only: reach the file source, so cp-1's subscriber tests keep reading
/// `.held`/`.cursor`/`.scan`/`.tail` (now nested under `Source::File`).
#[cfg(test)]
impl Subscriber {
    fn file(&self) -> &FileSource {
        match &self.source {
            Source::File(fs) => fs,
            Source::Fleet(_) => panic!("test_sub used on a non-file subscriber"),
        }
    }
}
```

- [ ] **Step 6: Rewrite `pump_subscriber` as the driver loop, OWNING `scanned`**

```rust
#[allow(clippy::too_many_arguments)]
fn pump_subscriber(
    sub: &mut Subscriber,
    conn: &mut Conn,
    now: Timestamp,
    cap: usize,
    stall_timeout: Duration,
    pending_events: &mut Vec<EventInput>,
    degraded_seen: &mut HashSet<(String, u64)>,
    fleet_model: &[SessionInfo], // Task 4 uses it; File ignores it
) -> PumpOutcome {
    let _ = fleet_model; // Task 1: File-only; Task 4 wires the Fleet arm.
    // G1: the per-CALL scan budget. Reset ONCE here, PERSISTS across every
    // FILL→FLUSH→re-FILL below — this is what bounds work per wake. Making it
    // local to `fill` would reset it per re-fill and break the bound.
    let mut scanned = 0usize;
    loop {
        // FILL (source-specific), then FLUSH (OutBuf). The driver ties them.
        let terminal = match &mut sub.source {
            Source::File(fs) => fs.fill(&mut sub.out, cap, &mut scanned, pending_events, degraded_seen),
            // Source::Fleet(_) arm added in Task 4 (it ignores `scanned`).
        };
        if sub.out.is_empty() {
            // Nothing to write. A TERMINAL file source with an empty out has
            // flushed its `end` frame — it is Gone. Otherwise it simply waits
            // for the next wake (a live line, or a fleet state change).
            return if terminal { PumpOutcome::Gone } else { PumpOutcome::Ok };
        }
        match sub.out.flush(conn, now, stall_timeout) {
            FlushStep::Drained => continue, // room freed — re-fill (scanned persists)
            FlushStep::WouldBlock => return PumpOutcome::Ok, // WRITABLE edge re-arms
            FlushStep::Gone => return PumpOutcome::Gone,
            FlushStep::Stalled => return PumpOutcome::Drop(subscriber_dropped_event(sub, cap)),
        }
    }
}

/// R1/§4.4: the loud drop event. `session` names the source; a fleet drop uses
/// the marker `"(fleet)"` (Task 4). `subscription` + `buffered_bytes` +
/// `cap_bytes` are the high-water forensics §4.4 requires.
fn subscriber_dropped_event(sub: &Subscriber, cap: usize) -> EventInput {
    let session = match &sub.source {
        Source::File(fs) => fs.session.clone(),
        // Source::Fleet(_) arm added in Task 4 -> "(fleet)".to_owned()
    };
    EventInput {
        kind: EventType::SubscriberDropped,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({
            "session": session,
            "subscription": sub.id,
            "buffered_bytes": sub.out.high_water as u64,
            "cap_bytes": cap as u64,
        }),
    }
}
```

- [ ] **Step 7: Update the remaining call sites AND the test helpers for the nested shape**

Production:
- `serve_subscribe` (`control.rs:3654`): build `Subscriber { id, out: OutBuf::new(), source: Source::File(FileSource { session: session.to_owned(), file, cursor: c, scan: c, partial: Vec::new(), held: false, oversize: None, tail, closing: None, end_sent: false, frame_overhead: measure_frame_overhead(session) }) }`.
- `poll_timeout` (`control.rs:501`): the `subscriber_work` predicate reads File fields — match on the source: `Source::File(fs) => sub.out.is_empty() && (fs.held || fs.scan < fs.tail) && !fs.end_sent`, `Source::Fleet(_) => false` (Task 4 confirms fleet needs no zero-arm). `earliest_stall` reads `sub.out.blocked_since` for both kinds.
- `fanout` (`control.rs:3744`) and `close_disposed` (`control.rs:3834`): the tail refresh and `closing`/`tail` pinning read File fields — match `Source::File(fs)`; skip `Source::Fleet(_)`. Pass an extra `fleet_model` arg through to `pump_subscriber` (use `&[]` from `fanout`/`close_disposed` in THIS task; Task 5 supplies the real model).
- `pump` (`control.rs:3804`): pass `&[]` for `fleet_model` in this task.

Test helpers (F2 — these break at compile under the nested shape):
- `test_insert_subscriber` (`control.rs:3941`): build the nested `Subscriber { id, out: OutBuf::new(), source: Source::File(FileSource { ... }) }` shape.
- `pump_to_completion` (`control.rs:2236`): it reads `s.out.is_empty()`, `s.held`, `s.cursor`, `s.tail`, `s.out.len()`. Rewrite via the file accessor:

```rust
fn pump_to_completion(rt: &mut ControlRuntime, token: Token, conn: &mut Conn) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        rt.pump(token, conn, t0());
        let s = rt.test_sub(token);
        let fs = s.file();
        if s.out.is_empty() && !fs.held && fs.cursor == fs.tail {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pump never drained: cursor={} tail={} out={} held={}",
            fs.cursor, fs.tail, s.out.out.len(), fs.held
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
```

- Every other `rt.test_sub(T).<field>` access in the module: `.blocked_since` → `.out.blocked_since` (e.g. cp-1's stall tests at `control.rs:2797`, `:2843`, `:2872`); `.held`/`.cursor`/`.scan`/`.tail` → `.file().held`/`.file().cursor`/`.file().scan`/`.file().tail`; `.out.len()` → `.out.out.len()`; `.out.is_empty()` stays (OutBuf has `is_empty`). Grep the test module for `test_sub(` and update every hit.

Preserve behaviour exactly. No `session.subscribe` test may change its assertions.

- [ ] **Step 8: Write the failing per-wake scan-budget test (CP2-B1 — pins the byte budget so it stops being suite-invisible)**

The regression this pins: if `scanned` resets per fill, one pump scans the whole history. This test needs NO concurrency and NO draining client — the budget bounds FILL independently of the socket, because `scanned` counts bytes ABSORBED, not bytes flushed. A history > `MAX_PUMP_BYTES_PER_WAKE` but < the cap means the SCAN budget (not the cap) is what bounds one pump; with the budget, `scan` advances ≤ ~256 KiB and stays `< tail`; without it, `fill` frames the whole file into `out` and `scan == tail`.

```rust
#[test]
fn one_pump_scans_at_most_the_per_wake_budget() {
    use std::io::Write as _;
    const T: Token = Token(9);
    // ~420 KiB of complete JSON lines: past MAX_PUMP_BYTES_PER_WAKE (256 KiB),
    // under the 1 MiB default cap — so the SCAN budget is the binding limit.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let mut f = std::fs::File::create(&path).unwrap();
    let line = format!("{}\n", serde_json::json!({"type": "assistant", "pad": "x".repeat(60)}));
    let mut tail = 0u64;
    while tail < 420 * 1024 {
        f.write_all(line.as_bytes()).unwrap();
        tail += line.len() as u64;
    }
    f.flush().unwrap();

    let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT); // 1 MiB cap
    let file = std::fs::File::open(&path).unwrap();
    // Client NOT read: the socket fills and flush WouldBlocks — but the SCAN
    // budget still bounds FILL, so ONE pump cannot reach the tail.
    let (client, mut conn) = control.test_insert_subscriber(T, "t/dev/1", file, 0, tail);
    control.pump(T, &mut conn, t0());
    let scan = control.test_sub(T).file().scan;
    assert!(
        scan < tail,
        "one pump must NOT scan the whole history — the per-wake budget bounds it \
         (regression: scan={scan}, tail={tail})"
    );
    assert!(
        scan <= MAX_PUMP_BYTES_PER_WAKE as u64 + line.len() as u64,
        "one pump scans at most the budget + one line: scan={scan}"
    );
    drop(client);
}
```

- [ ] **Step 9: Run the full subscriber suite + the two new tests + gates**

Run: `cargo test -p camp --lib one_pump_scans_at_most_the_per_wake_budget outbuf_flush_stalls`
Expected: PASS.
Run: `cargo test -p camp --lib -- daemon::control` then `cargo test -p camp --test control`
Expected: every cp-1 subscriber test PASSES unchanged (pump lexing, over-cap skip, stall drop, resume-from-offset, quiet-period, end-frame, disposal, catch-up-across-a-live-burst). Then:
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add crates/camp/src/daemon/control.rs
git commit -m "refactor(campd): the OutBuf seam — Subscriber { out, source }, per-call scan budget preserved (cp-2)"
```

---

## Task 2: The `fleet.subscribe` wire

Add the request verb and hello response with pinned wire, and make `SessionInfo` `Clone` (the fleet diff clones rows into `sent`).

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs` (`Request`, `Response`, `SessionInfo` derive)
- Test: `crates/camp/src/daemon/socket.rs` (its `#[cfg(test)]` module)

**Interfaces:**
- Produces:
  - `Request::FleetSubscribe` (op `"fleet.subscribe"`, no fields)
  - `Response::FleetSubscribed { ok: bool, v: u8, subscription: String }`
  - `SessionInfo: Clone`

- [ ] **Step 1: Write the failing wire-pin test**

```rust
/// cp-2 (§4.1/§4.2): `fleet.subscribe`'s wire, pinned in both directions.
/// §4.2: the aggregate stream is addressed by NAME; the verb carries no cursor
/// (a fleet's history is the current snapshot — scoping decision 3).
#[test]
fn fleet_subscribe_wire_format_is_pinned() {
    assert_eq!(
        serde_json::to_string(&Request::FleetSubscribe).unwrap(),
        r#"{"op":"fleet.subscribe"}"#
    );
    assert_eq!(
        serde_json::from_str::<Request>(r#"{"op":"fleet.subscribe"}"#).unwrap(),
        Request::FleetSubscribe
    );
    let line = serde_json::to_string(&Response::FleetSubscribed {
        ok: true, v: 1, subscription: "fleet-1".into(),
    })
    .unwrap();
    assert_eq!(line, r#"{"ok":true,"v":1,"subscription":"fleet-1"}"#);
    assert!(matches!(
        serde_json::from_str::<Response>(r#"{"ok":true,"v":1,"subscription":"fleet-1"}"#).unwrap(),
        Response::FleetSubscribed { subscription, .. } if subscription == "fleet-1"
    ));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib fleet_subscribe_wire_format_is_pinned`
Expected: FAIL — `Request::FleetSubscribe` / `Response::FleetSubscribed` undefined.

- [ ] **Step 3: Add the variants and the `Clone` derive**

In `Request` (after `SessionSubscribe`, `socket.rs:66-71`):

```rust
    /// cp-2 (§4.1): SUBSCRIBE to the fleet — the aggregate stream of session
    /// state transitions, stalls, permission requests, and completions. A
    /// connection MODE like `session.subscribe`, but LEDGER/model-sourced: no
    /// cursor, and the hello is followed by a snapshot then live deltas.
    #[serde(rename = "fleet.subscribe")]
    FleetSubscribe,
```

In `Response` (immediately AFTER `Subscribed`, `socket.rs:160`, and BEFORE `Interrupt`, so untagged resolution stays unambiguous — `{"ok":..,"v":..,"subscription":..}` has no `cursor`, distinguishing it from `Subscribed`):

```rust
    /// cp-2 (§4.1) `fleet.subscribe`'s HELLO. No `cursor`: a fleet has no byte
    /// history to resume from — its history is the snapshot the frames deliver
    /// (scoping decision 3). `v` future-proofs the frame vocabulary.
    FleetSubscribed {
        ok: bool,
        v: u8,
        subscription: String,
    },
```

Change `SessionInfo`'s derive (`socket.rs:92`) to add `Clone`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p camp --lib fleet_subscribe_wire_format_is_pinned`
Expected: PASS.

- [ ] **Step 5: Guard the untagged resolution — run the existing response-pin tests**

Run: `cargo test -p camp --lib -- daemon::socket`
Expected: PASS — `response_wire_format_is_pinned`, `control_plane_verbs_wire_format_is_pinned`, `sessions_list_wire_format_is_pinned` all still green (the new variant did not shadow an existing one).

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/daemon/socket.rs
git commit -m "feat(campd): fleet.subscribe wire — the verb and its hello, pinned (cp-2)"
```

---

## Task 3: The fleet model — extract `fleet_model()` from `sessions.list`

`serve_sessions_list` (`control.rs:1190`) already computes the exact fleet truth: one `SessionInfo` per live session from `ledger.live_sessions()` + `patrol.is_stalled()` + `read_channel.last_activity()`. Extract that into a reusable method so `fleet.subscribe` and `sessions.list` share ONE definition of the fleet (DRY).

**Files:**
- Modify: `crates/camp/src/daemon/control.rs`
- Test: `crates/camp/src/daemon/control.rs` (test module)

**Interfaces:**
- Consumes: `Ledger::live_sessions`, `PatrolRuntime::is_stalled`, `ReadChannelRuntime::last_activity`.
- Produces: `fn fleet_model(&self, ledger: &Ledger, patrol: &PatrolRuntime, read_channel: &ReadChannelRuntime) -> anyhow::Result<Vec<SessionInfo>>`

- [ ] **Step 1: Write the failing test**

```rust
/// cp-2: the fleet model is `sessions.list`'s rows, reusable. A live session
/// woken by campd appears as one `working` row addressed by name.
#[test]
fn fleet_model_returns_one_row_per_live_session() {
    // Build a ledger with one campd-woken live session, a PatrolRuntime, and a
    // ReadChannelRuntime EXACTLY as this module's existing serve_sessions_list
    // test builds them (search the test module for that pattern and reuse it
    // verbatim — do not invent a new harness). Then:
    let model = control.fleet_model(&ledger, &patrol, &read_channel).unwrap();
    assert_eq!(model.len(), 1);
    assert_eq!(model[0].agent, "dev");
    assert_eq!(model[0].state, "working");
    assert!(!model[0].blocked, "cp-2 never sets blocked — cp-3 owns the producer");
}
```

If this module has NO existing `serve_sessions_list` unit test to copy the harness from, model the setup on `read_channel.rs`'s tests: `Ledger::open` a tempdir db, `append` a `SessionWoke { name, agent, bead }` with actor `"campd"`, settle/fold it so `live_sessions()` returns it, and construct `PatrolRuntime`/`ReadChannelRuntime` as `daemon::run` does at startup. Keep the fixture minimal.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib fleet_model_returns_one_row_per_live_session`
Expected: FAIL — `fleet_model` is not defined.

- [ ] **Step 3: Extract `fleet_model` and call it from `serve_sessions_list`**

Add the method (adjacent to `serve_sessions_list`), lifting its `SessionInfo`-building loop verbatim:

```rust
/// §4.1/§4.3: the fleet — one `SessionInfo` per LIVE session, BY NAME, from the
/// ledger registry (not campd's child map: an adopted worker is a live session
/// too). The single definition shared by `sessions.list` and `fleet.subscribe`.
pub fn fleet_model(
    &self,
    ledger: &Ledger,
    patrol: &PatrolRuntime,
    read_channel: &ReadChannelRuntime,
) -> anyhow::Result<Vec<SessionInfo>> {
    let rows = ledger.live_sessions()?;
    Ok(rows
        .into_iter()
        .map(|row| SessionInfo {
            last_activity: read_channel
                .last_activity(&row.name)
                .map(|t| t.to_string())
                .unwrap_or(row.spawned_ts),
            state: if patrol.is_stalled(&row.name) {
                "stalled".into()
            } else {
                "working".into()
            },
            blocked: false, // §5.3: cp-3 owns the producer; a can_use_tool is a loud fault in cp-2.
            name: row.name,
            agent: row.agent,
            rig: row.rig,
            bead: row.bead,
        })
        .collect())
}
```

Rewrite `serve_sessions_list` to delegate (preserving its error-to-`Response::Error` behaviour):

```rust
pub fn serve_sessions_list(
    &self,
    ledger: &Ledger,
    patrol: &PatrolRuntime,
    read_channel: &ReadChannelRuntime,
) -> Response {
    match self.fleet_model(ledger, patrol, read_channel) {
        Ok(sessions) => Response::SessionsList { ok: true, sessions },
        Err(e) => Response::Error { ok: false, error: format!("listing live sessions: {e}") },
    }
}
```

- [ ] **Step 4: Run the new test AND the existing sessions.list tests to verify both pass**

Run: `cargo test -p camp --lib fleet_model_returns_one_row_per_live_session`
Expected: PASS.
Run: `cargo test -p camp -- sessions_list`
Expected: PASS — `serve_sessions_list`'s behaviour is unchanged (unit and any e2e sessions.list test).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/control.rs
git commit -m "refactor(campd): extract fleet_model — one fleet definition shared by sessions.list and fleet.subscribe (cp-2)"
```

---

## Task 4: The fleet source — frames, diff, and `FleetSource::fill`

Add `Source::Fleet(FleetSource)`. The fleet source turns a `&[SessionInfo]` model into `session`/`gone`/`synced` frames by diffing against what this subscriber was last sent (`sent`), emitting into `OutBuf` with the §4.4 cap-STOP. Driven with the same `pump_subscriber` driver from Task 1.

**Files:**
- Modify: `crates/camp/src/daemon/control.rs`
- Test: `crates/camp/src/daemon/control.rs` (test module)

**Interfaces:**
- Consumes: `OutBuf`, `SessionInfo` (now `Clone`), `Source`, `pump_subscriber`, `degraded_event` (`control.rs:3406`), `ControlRuntime::with_stall_timeout` (`control.rs:432`).
- Produces:
  - `struct FleetSource { sent: HashMap<String, SessionInfo>, synced: bool }`
  - `Source::Fleet(FleetSource)` variant
  - `fn fleet_session_frame(s: &SessionInfo) -> Vec<u8>`, `fn fleet_gone_frame(name: &str) -> Vec<u8>`, `fn fleet_synced_frame() -> Vec<u8>`
  - `impl FleetSource { fn fill(&mut self, out: &mut OutBuf, cap: usize, model: &[SessionInfo], pending_events: &mut Vec<EventInput>) }`
  - test helpers `fn test_insert_fleet_subscriber(&mut self, token) -> (UnixStream, Conn)`, `fn pump_with_model(...)`, `fn read_frames(...)`, `fn pump_fleet_to_quiet(...)`

- [ ] **Step 1: Write the failing unit test — snapshot then delta then gone**

```rust
/// cp-2 (§4.1): a fresh fleet subscriber gets the SNAPSHOT (one `session` frame
/// per live row) then `synced`; a later state change pushes ONE delta frame; a
/// departed session pushes a `gone` frame. Driven with no daemon, no timing.
#[test]
fn fleet_source_emits_snapshot_then_deltas_then_gone() {
    const T: Token = Token(1);
    let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
    let (client, mut conn) = control.test_insert_fleet_subscriber(T);

    let row = |name: &str, state: &str| SessionInfo {
        name: name.into(), agent: "dev".into(), rig: Some("gc".into()),
        bead: Some("gc-1".into()), state: state.into(),
        last_activity: "2026-07-14T00:00:00Z".into(), blocked: false,
    };

    let model = vec![row("camp/dev/1", "working"), row("camp/dev/2", "working")];
    pump_fleet_to_quiet(&mut control, T, &mut conn, &model);
    let frames = read_frames(&client);
    assert_eq!(frames.iter().filter(|f| f["frame"] == "session").count(), 2);
    assert!(frames.iter().any(|f| f["frame"] == "synced"), "snapshot ends with synced");
    assert!(frames.iter().any(|f|
        f["frame"] == "session" && f["session"]["name"] == "camp/dev/1"
            && f["session"]["state"] == "working"));

    let model = vec![row("camp/dev/1", "stalled"), row("camp/dev/2", "working")];
    pump_fleet_to_quiet(&mut control, T, &mut conn, &model);
    let frames = read_frames(&client);
    assert_eq!(frames.len(), 1, "only the changed row is pushed");
    assert_eq!(frames[0]["frame"], "session");
    assert_eq!(frames[0]["session"]["name"], "camp/dev/1");
    assert_eq!(frames[0]["session"]["state"], "stalled");

    let model = vec![row("camp/dev/1", "stalled")];
    pump_fleet_to_quiet(&mut control, T, &mut conn, &model);
    let frames = read_frames(&client);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["frame"], "gone");
    assert_eq!(frames[0]["name"], "camp/dev/2");
    drop(client);
}
```

Add these `#[cfg(test)]` helpers to the test module (which already allows `unwrap`):

```rust
/// Drive one fleet subscriber to a quiet point against a fixed model.
fn pump_fleet_to_quiet(rt: &mut ControlRuntime, token: Token, conn: &mut Conn, model: &[SessionInfo]) {
    for _ in 0..64 {
        match rt.pump_with_model(token, conn, jiff::Timestamp::now(), model) {
            PumpOutcome::Ok | PumpOutcome::Gone => break,
            PumpOutcome::Drop(_) => panic!("unexpected drop"),
        }
    }
}
/// Read all currently-available newline JSON frames from a non-blocking client.
fn read_frames(client: &std::os::unix::net::UnixStream) -> Vec<serde_json::Value> {
    use std::io::Read as _;
    let mut c = client.try_clone().unwrap();
    c.set_nonblocking(true).unwrap();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match c.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib fleet_source_emits_snapshot_then_deltas_then_gone`
Expected: FAIL — `test_insert_fleet_subscriber`, `Source::Fleet`, frame builders undefined.

- [ ] **Step 3: Add `FleetSource`, the frame builders, and `fill`**

```rust
#[derive(Serialize)]
struct FleetSessionFrame<'a> { frame: &'static str, session: &'a SessionInfo }
#[derive(Serialize)]
struct FleetGoneFrame<'a> { frame: &'static str, name: &'a str }
#[derive(Serialize)]
struct FleetSyncedFrame { frame: &'static str }

fn fleet_session_frame(s: &SessionInfo) -> Vec<u8> {
    let mut line = serde_json::to_string(&FleetSessionFrame { frame: "session", session: s })
        .unwrap_or_else(|_| String::from("{\"frame\":\"session\"}"));
    line.push('\n');
    line.into_bytes()
}
fn fleet_gone_frame(name: &str) -> Vec<u8> {
    let mut line = serde_json::to_string(&FleetGoneFrame { frame: "gone", name })
        .unwrap_or_else(|_| String::from("{\"frame\":\"gone\"}"));
    line.push('\n');
    line.into_bytes()
}
fn fleet_synced_frame() -> Vec<u8> {
    b"{\"frame\":\"synced\"}\n".to_vec()
}

/// The LEDGER/model-sourced half of a subscription. It holds no file — its
/// "cursor" is the by-name snapshot `sent` it last delivered, which is why the
/// §4.4 cap-STOP here is "leave `sent` unchanged for an un-emitted row" (the
/// model is recomputable next wake) rather than "hold the line in `partial`".
pub struct FleetSource {
    sent: HashMap<String, SessionInfo>,
    synced: bool,
}

impl FleetSource {
    fn new() -> FleetSource {
        FleetSource { sent: HashMap::new(), synced: false }
    }

    /// Diff `model` against `sent`, emitting the delta into `out` and STOPPING at
    /// the cap. NON-TERMINAL always — a fleet subscription only ends on client
    /// detach or campd shutdown.
    fn fill(
        &mut self,
        out: &mut OutBuf,
        cap: usize,
        model: &[SessionInfo],
        pending_events: &mut Vec<EventInput>,
    ) {
        // (1) added / changed rows.
        for s in model {
            if self.sent.get(&s.name) == Some(s) {
                continue;
            }
            let frame = fleet_session_frame(s);
            // Fail LOUD, never silent-livelock: a single frame that cannot fit an
            // EMPTY cap would stall forever (invariant 5). Unreachable in practice
            // (a SessionInfo frame is < 1 KiB, cap default 1 MiB) — HANDLED, not
            // assumed: report it and advance `sent` so the snapshot completes.
            if frame.len() > cap {
                pending_events.push(degraded_event(
                    &s.name,
                    format!(
                        "fleet: a session frame of {} bytes exceeds the subscriber buffer cap \
                         ({cap} bytes) and was SKIPPED for subscriber delivery",
                        frame.len()
                    ),
                ));
                self.sent.insert(s.name.clone(), s.clone());
                continue;
            }
            if !out.has_room(frame.len(), cap) {
                return; // R1 cap-STOP: `sent` unchanged; resumed next fill.
            }
            out.append(&frame);
            self.sent.insert(s.name.clone(), s.clone());
        }
        // (2) departed rows: in `sent` but not in `model`.
        let live: HashSet<&str> = model.iter().map(|s| s.name.as_str()).collect();
        let goners: Vec<String> = self
            .sent
            .keys()
            .filter(|n| !live.contains(n.as_str()))
            .cloned()
            .collect();
        for name in goners {
            let frame = fleet_gone_frame(&name);
            if !out.has_room(frame.len(), cap) {
                return;
            }
            out.append(&frame);
            self.sent.remove(&name);
        }
        // (3) the snapshot terminator, once.
        if !self.synced {
            let frame = fleet_synced_frame();
            if !out.has_room(frame.len(), cap) {
                return;
            }
            out.append(&frame);
            self.synced = true;
        }
    }
}
```

Add the `Fleet` arm to `Source`, and wire the driver + drop-event + `poll_timeout`/`fanout`/`close_disposed` arms that Task 1 stubbed as `Source::File`-only:

```rust
pub enum Source {
    File(FileSource),
    Fleet(FleetSource),
}
```

In `pump_subscriber`'s FILL match (Task 1 Step 6), delete the `let _ = fleet_model;` line and add:

```rust
            Source::Fleet(fleet) => {
                fleet.fill(&mut sub.out, cap, fleet_model, pending_events);
                false // never terminal
            }
```

In `subscriber_dropped_event`'s `session` match, add:

```rust
        Source::Fleet(_) => "(fleet)".to_owned(),
```

- [ ] **Step 4: Confirm `poll_timeout`, `fanout`, `close_disposed` handle the Fleet arm**

Verify the `Source::Fleet(_)` arms:
- `poll_timeout`'s `subscriber_work` → `Source::Fleet(_) => false` (a fleet fill fully drains per pump loop or WouldBlocks — no empty-`out`-with-pending state persists, so no zero-arm is ever needed);
- `earliest_stall` reads `sub.out.blocked_since` for BOTH kinds (the stall drop is source-agnostic, in `OutBuf`);
- `close_disposed` skips `Source::Fleet(_)` (not tied to one disposed session);
- `fanout`'s tail refresh skips `Source::Fleet(_)` (no tail).

- [ ] **Step 5: Add the `#[cfg(test)]` `test_insert_fleet_subscriber` + `pump_with_model` shims**

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
impl ControlRuntime {
    pub fn test_insert_fleet_subscriber(
        &mut self,
        token: Token,
    ) -> (std::os::unix::net::UnixStream, Conn) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        client.set_nonblocking(true).unwrap();
        let conn = Conn { stream: mio::net::UnixStream::from_std(server), buf: Vec::new() };
        self.next_subscription += 1;
        self.subscribers.insert(
            token,
            Subscriber {
                id: format!("fleet-{}", self.next_subscription),
                out: OutBuf::new(),
                source: Source::Fleet(FleetSource::new()),
            },
        );
        (client, conn)
    }

    /// Drive ONE subscriber's pump against an explicit fleet model at an explicit
    /// `now` (production supplies the model through `fanout`; this is the unit
    /// entry, and `now` is explicit so stall-window tests are deterministic).
    pub fn pump_with_model(
        &mut self,
        token: Token,
        conn: &mut Conn,
        now: Timestamp,
        model: &[SessionInfo],
    ) -> PumpOutcome {
        let cap = self.subscriber_buffer_bytes;
        let stall = self.stall_timeout;
        let Some(sub) = self.subscribers.get_mut(&token) else {
            return PumpOutcome::Gone;
        };
        pump_subscriber(sub, conn, now, cap, stall, &mut self.pending_events, &mut self.degraded_seen, model)
    }
}
```

- [ ] **Step 6: Run the fleet source test + the full subscriber suite**

Run: `cargo test -p camp --lib fleet_source_emits_snapshot_then_deltas_then_gone`
Expected: PASS.
Run: `cargo test -p camp --lib -- daemon::control && cargo test -p camp --test control`
Expected: PASS — file subscribers unaffected.
Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 7: Byte-pin the three frame wire shapes (CP2-B8)**

The `session`/`gone`/`synced` frames are the product wire (§4.2 "the protocol is not swappable"). Loose value assertions let a field rename/reorder pass — pin the bytes, like `sessions_list_wire_format_is_pinned`.

```rust
/// cp-2 (§4.2): the fleet frame shapes, byte-exact. A rename/reorder in
/// FleetSessionFrame/FleetGoneFrame is a wire break and must turn this red.
#[test]
fn fleet_frame_shapes_are_pinned() {
    let s = SessionInfo {
        name: "camp/dev/1".into(), agent: "dev".into(), rig: Some("gc".into()),
        bead: Some("gc-1".into()), state: "working".into(),
        last_activity: "2026-07-14T00:00:00Z".into(), blocked: false,
    };
    assert_eq!(
        String::from_utf8(fleet_session_frame(&s)).unwrap(),
        "{\"frame\":\"session\",\"session\":{\"name\":\"camp/dev/1\",\"agent\":\"dev\",\
         \"rig\":\"gc\",\"bead\":\"gc-1\",\"state\":\"working\",\
         \"last_activity\":\"2026-07-14T00:00:00Z\",\"blocked\":false}}\n"
    );
    assert_eq!(
        String::from_utf8(fleet_gone_frame("camp/dev/1")).unwrap(),
        "{\"frame\":\"gone\",\"name\":\"camp/dev/1\"}\n"
    );
    assert_eq!(
        String::from_utf8(fleet_synced_frame()).unwrap(),
        "{\"frame\":\"synced\"}\n"
    );
}
```

Run: `cargo test -p camp --lib fleet_frame_shapes_are_pinned`
Expected: PASS.

- [ ] **Step 8: The cap-STOP test — assert `out` never exceeds the cap while the socket is FULL (CP2-B3)**

Names its mutation: with the client NOT reading, the socket fills and `flush` WouldBlocks; the ONLY thing keeping `out` bounded is the `has_room` STOP. Removing it lets `fill` append the whole (unbounded) model into `out`. Feed a model whose total frame bytes far exceed both the socket buffer and the cap; assert `out` stays ≤ cap and NO Drop fires (pumps are well within `stall_timeout`).

```rust
#[test]
fn fleet_cap_is_a_stop_and_out_never_exceeds_the_cap() {
    const T: Token = Token(1);
    let row = |i: usize| SessionInfo {
        name: format!("camp/dev/{i}"), agent: "dev".into(), rig: Some("gc".into()),
        bead: Some("gc-1".into()), state: "working".into(),
        last_activity: "2026-07-14T00:00:00Z".into(), blocked: false,
    };
    let frame_len = fleet_session_frame(&row(1)).len();
    let cap = frame_len * 2; // room for ~2 frames — far below the model's total
    let mut control = ControlRuntime::new(cap);
    let (client, mut conn) = control.test_insert_fleet_subscriber(T);
    // 300 rows (~60 KiB of frames) >> socket send buffer (~8 KiB) and >> cap, so
    // once the socket fills, `out` growth is bounded ONLY by the has_room STOP.
    let model: Vec<SessionInfo> = (0..300).map(row).collect();
    // DO NOT read the client. A few pumps, all within stall_timeout so no Drop.
    for _ in 0..4 {
        assert!(
            !matches!(
                control.pump_with_model(T, &mut conn, jiff::Timestamp::now(), &model),
                PumpOutcome::Drop(_)
            ),
            "the cap is a STOP, never a Drop"
        );
        assert!(
            control.test_sub(T).out.out.len() <= cap,
            "out must never exceed the cap while the socket is full — the cap is a STOP \
             (out={}, cap={cap})",
            control.test_sub(T).out.out.len()
        );
    }
    drop(client);
}
```

Run: `cargo test -p camp --lib fleet_cap_is_a_stop_and_out_never_exceeds_the_cap`
Expected: PASS.

- [ ] **Step 9: The fleet stall-drop test — the loud `subscriber.dropped("(fleet)")` path (CP2-B6)**

Drives a `Source::Fleet` subscriber all the way to Stalled→Drop→`subscriber.dropped`, asserting the event names `"(fleet)"`. Uses a short `stall_timeout` and an explicit `now` past the window.

```rust
#[test]
fn a_stalled_fleet_subscriber_is_dropped_loudly_naming_fleet() {
    const T: Token = Token(1);
    let stall = std::time::Duration::from_millis(50);
    // cap small so `out` stays non-empty (blocked_since persists) while the
    // socket is full; stall short so the window is crossed deterministically.
    let mut control = ControlRuntime::with_stall_timeout(4096, stall);
    let (client, mut conn) = control.test_insert_fleet_subscriber(T);
    let row = |i: usize| SessionInfo {
        name: format!("camp/dev/{i}"), agent: "dev".into(), rig: Some("gc".into()),
        bead: Some("gc-1".into()), state: "working".into(),
        last_activity: "2026-07-14T00:00:00Z".into(), blocked: false,
    };
    let model: Vec<SessionInfo> = (0..500).map(row).collect();
    let t0 = jiff::Timestamp::now();
    // First pump: fills the socket, stamps blocked_since at t0. Client NOT read.
    let _ = control.pump_with_model(T, &mut conn, t0, &model);
    // A pump 60ms later — past the 50ms window — drops the peer LOUDLY.
    let later = t0 + jiff::SignedDuration::from_millis(60);
    match control.pump_with_model(T, &mut conn, later, &model) {
        PumpOutcome::Drop(ev) => {
            assert_eq!(ev.kind, EventType::SubscriberDropped);
            assert_eq!(ev.data["session"], "(fleet)", "a fleet drop names (fleet), not a worker");
            assert!(ev.data["buffered_bytes"].as_u64().unwrap() > 0, "names the high-water mark");
        }
        _ => panic!("a fleet peer that stopped reading must be dropped loudly"),
    }
    drop(client);
}
```

Run: `cargo test -p camp --lib a_stalled_fleet_subscriber_is_dropped_loudly_naming_fleet`
Expected: PASS.

- [ ] **Step 10: The two-divergent-subscriber test — per-`sent` isolation (CP2-B7)**

The central mechanism is the per-subscriber `FleetSource.sent` diff against one shared model (via the `mem::take` split-borrow). This proves two subscribers at different points get different deltas from the SAME model — no `sent` leakage, no dropped update for the second subscriber.

```rust
#[test]
fn two_fleet_subscribers_diverge_by_their_own_sent_state() {
    const A: Token = Token(1);
    const B: Token = Token(2);
    let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
    let row = |name: &str| SessionInfo {
        name: name.into(), agent: "dev".into(), rig: Some("gc".into()),
        bead: Some("gc-1".into()), state: "working".into(),
        last_activity: "2026-07-14T00:00:00Z".into(), blocked: false,
    };

    // A catches up on a ONE-session model and drains its snapshot.
    let (ca, mut conna) = control.test_insert_fleet_subscriber(A);
    let m1 = vec![row("camp/dev/1")];
    pump_fleet_to_quiet(&mut control, A, &mut conna, &m1);
    let _ = read_frames(&ca);

    // B subscribes now; BOTH pumped against a TWO-session model.
    let (cb, mut connb) = control.test_insert_fleet_subscriber(B);
    let m2 = vec![row("camp/dev/1"), row("camp/dev/2")];
    pump_fleet_to_quiet(&mut control, A, &mut conna, &m2);
    pump_fleet_to_quiet(&mut control, B, &mut connb, &m2);

    let fa = read_frames(&ca);
    let fb = read_frames(&cb);
    // A already had dev/1 -> ONLY dev/2 is new (no re-send of dev/1).
    assert_eq!(fa.iter().filter(|f| f["frame"] == "session").count(), 1);
    assert_eq!(
        fa.iter().find(|f| f["frame"] == "session").unwrap()["session"]["name"],
        "camp/dev/2"
    );
    // B is fresh -> the FULL snapshot: both sessions + synced.
    assert_eq!(fb.iter().filter(|f| f["frame"] == "session").count(), 2);
    assert!(fb.iter().any(|f| f["frame"] == "synced"));
    drop(ca);
    drop(cb);
}
```

Run: `cargo test -p camp --lib two_fleet_subscribers_diverge_by_their_own_sent_state`
Expected: PASS.

- [ ] **Step 11: Run the whole control suite + gates**

Run: `cargo test -p camp --lib -- daemon::control && cargo test -p camp --test control`
Expected: PASS.
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 12: Commit**

```bash
git add crates/camp/src/daemon/control.rs
git commit -m "feat(campd): the fleet source — frames (byte-pinned), model diff, cap-STOP, loud drop, per-sub isolation (cp-2)"
```

---

## Task 5: `serve_fleet_subscribe` + the event-loop wiring

Add the hello handler and wire the fleet fanout into every campd wake: compute the model once per fanout (only when a fleet subscriber exists), store it, and pump every subscriber against it. Thread `patrol` through `control_step`/`fanout`.

**Files:**
- Modify: `crates/camp/src/daemon/control.rs` (`serve_fleet_subscribe`, `fanout` model wiring, a `has_fleet_subscribers` guard, a cached `fleet_model` field)
- Modify: `crates/camp/src/daemon/event_loop.rs` (dispatch arm, `control_step` signature, `pump` call sites)
- Test: `crates/camp/src/daemon/control.rs` (test module) + `crates/camp/tests/control.rs` (Task 7 covers e2e)

**Interfaces:**
- Consumes: Task 3 `fleet_model`, Task 4 `FleetSource`/pump, `Request::FleetSubscribe`, `Response::FleetSubscribed`.
- Produces:
  - `fn serve_fleet_subscribe(&mut self, token: Token, ledger: &Ledger, patrol: &PatrolRuntime, read_channel: &ReadChannelRuntime) -> Response`
  - `fanout` gains `ledger: &Ledger` + `patrol: &PatrolRuntime`; `control_step` gains `patrol: &PatrolRuntime`.

- [ ] **Step 1: Write the failing unit test for `serve_fleet_subscribe`**

```rust
/// cp-2 (§4.1/§4.4): the hello registers a fleet subscriber and answers
/// synchronously with `FleetSubscribed` (bounded by REQUEST_TIMEOUT, like every
/// other verb). MAX_SUBSCRIBERS bounds it.
#[test]
fn serve_fleet_subscribe_registers_and_answers_the_hello() {
    // Build ledger + patrol + read_channel as fleet_model's test does.
    let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
    let response = control.serve_fleet_subscribe(Token(7), &ledger, &patrol, &read_channel);
    assert!(matches!(response, Response::FleetSubscribed { ok: true, v: 1, .. }));
    assert_eq!(control.subscriber_count(), 1, "a fleet subscriber is registered");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib serve_fleet_subscribe_registers_and_answers_the_hello`
Expected: FAIL — `serve_fleet_subscribe` undefined.

- [ ] **Step 3: Add the cached model field + `serve_fleet_subscribe` + fanout model wiring**

Add a field to `ControlRuntime` (near `subscribers`) and initialise it in `with_stall_timeout` (which `new` delegates to):

```rust
    /// The most recently computed fleet model, refreshed by `fanout` and by
    /// `serve_fleet_subscribe`. It is what the WRITABLE-edge `pump` diffs against
    /// when it continues a cap-STOPped fleet delta (that path has no ledger in
    /// scope). Empty when no fleet subscriber exists — computing it then would be
    /// pure waste (invariant 1).
    fleet_model: Vec<SessionInfo>,
```

Add the handler and the guard:

```rust
/// §4.1/§4.4 `fleet.subscribe`: the hello. It REGISTERS and refreshes the cached
/// model so the FIRST post-hello pump (event_loop) emits the full snapshot; it
/// never writes — `respond()` writes the hello, then the loop pumps (B11).
pub fn serve_fleet_subscribe(
    &mut self,
    token: Token,
    ledger: &Ledger,
    patrol: &PatrolRuntime,
    read_channel: &ReadChannelRuntime,
) -> Response {
    if self.subscribers.len() >= MAX_SUBSCRIBERS {
        return Response::Error {
            ok: false,
            error: format!(
                "campd already has {MAX_SUBSCRIBERS} subscriptions open (the MAX_SUBSCRIBERS \
                 cap). Each holds up to {SUBSCRIBER_BUFFER_BYTES} bytes of outbound buffer."
            ),
        };
    }
    match self.fleet_model(ledger, patrol, read_channel) {
        Ok(model) => self.fleet_model = model,
        Err(e) => return Response::Error { ok: false, error: format!("building the fleet model: {e}") },
    }
    self.next_subscription += 1;
    let id = format!("fleet-{}", self.next_subscription);
    self.subscribers.insert(
        token,
        Subscriber { id: id.clone(), out: OutBuf::new(), source: Source::Fleet(FleetSource::new()) },
    );
    Response::FleetSubscribed { ok: true, v: 1, subscription: id }
}

/// True when at least one fleet subscriber is registered — the guard that keeps
/// the model recompute off the hot path when nobody is watching.
fn has_fleet_subscribers(&self) -> bool {
    self.subscribers.values().any(|s| matches!(s.source, Source::Fleet(_)))
}
```

Change `fanout` to take `ledger` + `patrol`, refresh the cached model when a fleet subscriber exists, and pass it to `pump_subscriber` via a take/restore split-borrow (so `pump_subscriber`'s `&mut self.pending_events`/`&mut self.degraded_seen` stay disjoint from the immutable model borrow):

```rust
pub fn fanout(
    &mut self,
    ledger: &Ledger,
    patrol: &PatrolRuntime,
    read_channel: &ReadChannelRuntime,
    conns: &mut HashMap<Token, Conn>,
    now: Timestamp,
) -> (Vec<Token>, Vec<EventInput>) {
    if self.has_fleet_subscribers() {
        match self.fleet_model(ledger, patrol, read_channel) {
            Ok(model) => self.fleet_model = model,
            Err(e) => self
                .pending_events
                .push(degraded_event("(fleet)", format!("fleet model refresh: {e}"))),
        }
    }
    let cap = self.subscriber_buffer_bytes;
    let stall = self.stall_timeout;
    let model = std::mem::take(&mut self.fleet_model); // take so the loop can borrow it
    let mut gone = Vec::new();
    let mut events = Vec::new();
    let tokens: Vec<Token> = self.subscribers.keys().copied().collect();
    for token in tokens {
        let Some(sub) = self.subscribers.get_mut(&token) else { continue };
        if let Source::File(fs) = &mut sub.source {
            match read_channel.tail_state(&fs.session) {
                _ if fs.closing.is_some() => {}
                Some((_, t)) => fs.tail = t,
                None => {}
            }
        }
        let Some(conn) = conns.get_mut(&token) else { gone.push(token); continue };
        match pump_subscriber(
            sub, conn, now, cap, stall, &mut self.pending_events, &mut self.degraded_seen, &model,
        ) {
            PumpOutcome::Ok => {}
            PumpOutcome::Gone => gone.push(token),
            PumpOutcome::Drop(event) => { events.push(event); gone.push(token); }
        }
    }
    self.fleet_model = model; // restore
    events.append(&mut self.pending_events);
    (gone, events)
}
```

`pump` (`control.rs:3804`) likewise passes `&self.fleet_model` via the same take/restore split-borrow instead of `&[]`. `close_disposed` keeps passing `&[]` (a disposal wake targets file subscribers of the disposed session; fleet subscribers are pumped by the ordinary fanout on the same wake).

- [ ] **Step 4: Run the handler test + full subscriber suite**

Run: `cargo test -p camp --lib serve_fleet_subscribe_registers_and_answers_the_hello`
Expected: PASS.
Run: `cargo test -p camp --lib -- daemon::control`
Expected: PASS.

- [ ] **Step 5: Wire the `fleet.subscribe` dispatch arm in `event_loop.rs`**

Thread `patrol` into `control_step` (signature + both call sites at `event_loop.rs:492` and `:660`) and into its `fanout` call (`event_loop.rs:765`) — `fanout` now also needs `ledger`, which `control_step` already holds. Then add the dispatch arm in `serve_connection`, mirroring the `session.subscribe` arm (`event_loop.rs:1016-1040`):

```rust
            // cp-2 (§4.1): fleet.subscribe turns this connection into the
            // aggregate STREAM. The hello is the first bytes; the post-hello
            // pump emits the snapshot (B11 — nothing else will fire for it).
            Ok(Request::FleetSubscribe) => {
                let response = control.serve_fleet_subscribe(token, ledger, patrol, read_channel);
                let subscribed = matches!(response, Response::FleetSubscribed { .. });
                respond(&mut conn.stream, &response)?;
                if subscribed {
                    match control.pump(token, conn, Timestamp::now()) {
                        super::control::PumpOutcome::Ok => {}
                        super::control::PumpOutcome::Gone => return Ok(ConnState::Closed),
                        super::control::PumpOutcome::Drop(event) => {
                            ledger.append(event)?;
                            return Ok(ConnState::Closed);
                        }
                    }
                    for input in control.take_pending_events() {
                        ledger.append(input)?;
                    }
                    return Ok(ConnState::Open);
                }
                return Ok(ConnState::Closed);
            }
```

Match the EXACT error/close structure the neighbouring `session.subscribe` arm uses in this file; copy its shape so the two stay consistent.

- [ ] **Step 6: Build and run the whole workspace test suite**

Run: `cargo build -p camp`
Expected: clean (all `control_step`/`fanout` call sites updated).
Run: `cargo test -p camp`
Expected: PASS.
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/daemon/control.rs crates/camp/src/daemon/event_loop.rs
git commit -m "feat(campd): fleet.subscribe served — hello + snapshot + per-wake model-diff fanout (cp-2)"
```

---

## Task 6: `camp watch` — the client

The stateless renderer: connect, `fleet.subscribe`, read frames, keep a by-name row map, render the table on every update. The RENDER is a pure function, unit-tested; the IO loop is thin.

**Files:**
- Create: `crates/camp/src/cmd/watch.rs`
- Modify: `crates/camp/src/main.rs` (module decl in the `pub mod` block near `top`; the `Watch` command variant; the dispatch arm)
- Test: `crates/camp/src/cmd/watch.rs` (test module — the pure renderer)

**Interfaces:**
- Consumes: `crate::daemon::socket::{self, Request, Response, SessionInfo, REQUEST_TIMEOUT}`, `crate::campdir::CampDir`.
- Produces: `pub fn run(camp: &CampDir) -> anyhow::Result<()>`; `fn render(rows: &BTreeMap<String, SessionInfo>, state_since: &BTreeMap<String, Timestamp>, now: Timestamp) -> String`; `fn fmt_dur(d: Duration) -> String`; `fn state_display(s: &SessionInfo) -> String`.

- [ ] **Step 1: Write the failing renderer test**

Create `crates/camp/src/cmd/watch.rs` with the pure renderer test first:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::daemon::socket::SessionInfo;
    use std::collections::BTreeMap;

    fn row(name: &str, agent: &str, bead: &str, state: &str, blocked: bool, last: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(), agent: agent.into(), rig: Some("gc".into()),
            bead: Some(bead.into()), state: state.into(),
            last_activity: last.into(), blocked,
        }
    }

    #[test]
    fn render_shows_a_header_and_one_line_per_session_with_blocked_and_stalled_columns() {
        let now: Timestamp = "2026-07-14T00:10:00Z".parse().unwrap();
        let mut rows = BTreeMap::new();
        rows.insert("a".to_string(), row("a", "bmad/dev", "campdemo-15", "working", true, "2026-07-14T00:09:29Z"));
        rows.insert("b".to_string(), row("b", "gstack/reviewer", "campdemo-12", "working", false, "2026-07-14T00:03:58Z"));
        rows.insert("c".to_string(), row("c", "bmad/dev", "campdemo-11", "stalled", false, "2026-07-13T23:58:00Z"));
        let mut since = BTreeMap::new();
        since.insert("a".to_string(), "2026-07-14T00:09:29Z".parse().unwrap());
        since.insert("b".to_string(), "2026-07-14T00:03:58Z".parse().unwrap());
        since.insert("c".to_string(), "2026-07-13T23:55:10Z".parse().unwrap());

        let out = render(&rows, &since, now);
        assert!(out.contains("AGENT") && out.contains("BEAD") && out.contains("STATE")
            && out.contains("FOR") && out.contains("LAST"));
        assert!(out.contains("BLOCKED"), "blocked row shows BLOCKED: {out}");
        assert!(out.contains("needs you"), "BLOCKED must be impossible to miss: {out}");
        assert!(out.contains("stalled"), "{out}");
        assert!(out.contains("no output"), "{out}");
        assert!(out.contains("gstack/reviewer") && out.contains("campdemo-12"), "{out}");
    }

    #[test]
    fn fmt_dur_is_minutes_and_zero_padded_seconds() {
        assert_eq!(fmt_dur(std::time::Duration::from_secs(134)), "2m14s");
        assert_eq!(fmt_dur(std::time::Duration::from_secs(31)), "0m31s");
        assert_eq!(fmt_dur(std::time::Duration::from_secs(362)), "6m02s");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib -- cmd::watch`
Expected: FAIL — `render`, `fmt_dur` undefined (and the module is not yet declared in `main.rs` — Step 4 declares it; add the `pub mod watch;` line first if needed so the test compiles).

- [ ] **Step 3: Implement the renderer and the IO loop**

Full `crates/camp/src/cmd/watch.rs` (above the test module):

```rust
//! `camp watch` (control-plane spec §5.1): the fleet view — the thing you leave
//! open on a second monitor. A STATELESS RENDERER (§4.2): it opens a
//! `fleet.subscribe` stream and replaces its rows BY NAME as frames arrive. It
//! never tails a file, never reads the ledger, never learns a pid. Push-driven:
//! it blocks on the socket between updates — zero polling.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Result, bail};
use jiff::Timestamp;
use serde::Deserialize;

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response, SessionInfo};

/// One frame off the `fleet.subscribe` wire. Lenient — the daemon may add frame
/// kinds a future phase understands; an unknown `frame` is ignored, never a
/// crash (the client is a renderer, not a validator of campd's own protocol).
#[derive(Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
enum Frame {
    Session { session: SessionInfo },
    Gone { name: String },
    Synced,
    #[serde(other)]
    Unknown,
}

pub fn run(camp: &CampDir) -> Result<()> {
    // The hello is bounded by REQUEST_TIMEOUT (a wedged campd fails fast, like
    // every verb); after it, the stream is timeout-exempt. A pure client never
    // starts campd — a down campd is the standard loud error.
    let path = camp.socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) => {
            socket::require(camp, &Request::FleetSubscribe)?; // returns Err(CampdNotRunning)
            return Ok(()); // unreachable — require errored — but keeps the type total
        }
    };
    stream.set_read_timeout(Some(socket::REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(socket::REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(&Request::FleetSubscribe)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let mut hello = String::new();
    reader.read_line(&mut hello)?;
    match serde_json::from_str::<Response>(hello.trim_end()) {
        Ok(Response::FleetSubscribed { ok: true, .. }) => {}
        Ok(Response::Error { error, .. }) => bail!("campd refused fleet.subscribe: {error}"),
        other => bail!("unexpected fleet.subscribe hello: {other:?}"),
    }
    // Long-lived now: no read timeout (a quiet fleet is not a wedged daemon — §4.4).
    reader.get_ref().set_read_timeout(None)?;

    let mut rows: BTreeMap<String, SessionInfo> = BTreeMap::new();
    let mut state_since: BTreeMap<String, Timestamp> = BTreeMap::new();
    let mut synced = false;

    loop {
        let mut frame_line = String::new();
        let n = reader.read_line(&mut frame_line)?;
        if n == 0 {
            eprintln!("camp watch: campd closed the stream");
            return Ok(());
        }
        let trimmed = frame_line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Frame>(trimmed) {
            Ok(Frame::Session { session }) => {
                let name = session.name.clone();
                let display = state_display(&session);
                let changed = rows.get(&name).map(state_display).as_deref() != Some(display.as_str());
                if changed || !state_since.contains_key(&name) {
                    state_since.insert(name.clone(), Timestamp::now());
                }
                rows.insert(name, session);
            }
            Ok(Frame::Gone { name }) => {
                rows.remove(&name);
                state_since.remove(&name);
            }
            Ok(Frame::Synced) => synced = true,
            Ok(Frame::Unknown) => {}
            Err(e) => bail!("malformed fleet frame {trimmed:?}: {e}"),
        }
        if synced {
            print!("{}", render(&rows, &state_since, Timestamp::now()));
            std::io::stdout().flush().ok();
        }
    }
}

/// The STATE cell: BLOCKED (§5.3, rendered though cp-2 never produces it) wins;
/// else the working/stalled state verbatim.
fn state_display(s: &SessionInfo) -> String {
    if s.blocked { "BLOCKED".to_owned() } else { s.state.clone() }
}

fn fmt_dur(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    format!("{}m{:02}s", secs / 60, secs % 60)
}

/// Age of an RFC3339 timestamp from `now`, saturating at zero.
fn age(ts_str: &str, now: Timestamp) -> std::time::Duration {
    match ts_str.parse::<Timestamp>() {
        Ok(ts) => {
            let delta = now - ts;
            if delta.is_negative() {
                std::time::Duration::ZERO
            } else {
                std::time::Duration::try_from(delta).unwrap_or(std::time::Duration::ZERO)
            }
        }
        Err(_) => std::time::Duration::ZERO,
    }
}

/// Render the fleet: a header and one line per session, sorted by name (BTreeMap
/// order — a stable frame). Clears the screen first so refresh is in-place.
fn render(
    rows: &BTreeMap<String, SessionInfo>,
    state_since: &BTreeMap<String, Timestamp>,
    now: Timestamp,
) -> String {
    let mut out = String::new();
    out.push_str("\x1b[2J\x1b[H"); // clear + home
    out.push_str(&format!(
        "{:<18} {:<13} {:<10} {:>7}  {}\n",
        "AGENT", "BEAD", "STATE", "FOR", "LAST"
    ));
    for (name, s) in rows {
        let state = state_display(s);
        let for_str = state_since
            .get(name)
            .map(|since| {
                let d = now - *since;
                fmt_dur(if d.is_negative() {
                    std::time::Duration::ZERO
                } else {
                    std::time::Duration::try_from(d).unwrap_or(std::time::Duration::ZERO)
                })
            })
            .unwrap_or_else(|| "0m00s".to_owned());
        let last_age = age(&s.last_activity, now);
        // cp-2's LAST is a relative-time indicator (scoping decision 1): a
        // BLOCKED session says "needs you"; a stalled one "no output <age>";
        // else the age of the last line. The rich tool summary is phase 4.
        let last = if s.blocked {
            format!("? {} — needs you", s.bead.as_deref().unwrap_or(""))
        } else if s.state == "stalled" {
            format!("(no output {})", fmt_dur(last_age))
        } else {
            fmt_dur(last_age)
        };
        out.push_str(&format!(
            "{:<18} {:<13} {:<10} {:>7}  {}\n",
            s.agent,
            s.bead.as_deref().unwrap_or("-"),
            state,
            for_str,
            last
        ));
    }
    out
}
```

- [ ] **Step 4: Wire the module, command variant, and dispatch in `main.rs` (additive)**

In the `pub mod` block that declares `top`/`stop` (near `main.rs:31-32`), add:

```rust
    pub mod watch;
```

Add the command variant to `enum Command` (near `Top`, `main.rs:322`):

```rust
    /// Watch the fleet live: one line per session, push-driven from the socket
    /// (control-plane §5.1). campd must be running.
    Watch,
```

Add the dispatch arm (near the `Top` arm, `main.rs:817`):

```rust
        Command::Watch => cmd::watch::run(&camp),
```

- [ ] **Step 5: Run the renderer tests and build**

Run: `cargo test -p camp --lib -- cmd::watch`
Expected: PASS.
Run: `cargo build -p camp && cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/watch.rs crates/camp/src/main.rs
git commit -m "feat(cli): camp watch — the stateless fleet renderer over fleet.subscribe (cp-2)"
```

---

## Task 7: End-to-end — `camp watch` PROVES push, with NO self-poke

The exit criterion, proven over the REAL socket: the fleet delivers a live session from the socket alone (no file access), and a transition is PUSHED — asserted with NO client-initiated poke, so push-driven and poll-driven are distinguishable (the exit criterion is literally "push-driven, zero polling"). Reuse the existing `tests/control.rs` harness.

**Harness facts (verified against `tests/control.rs`):**
- `dispatch_one(root)` returns `(bead, session)` (`control.rs:237-238`) — bind `let (_bead, session) = dispatch_one(&root);`. Getting this backwards makes every `v["session"]["name"] == session` assertion miss.
- Scaffold idiom: `let dir = tempfile::tempdir().unwrap(); let (root, _rig) = scaffold(dir.path(), 4);` (`scaffold(dir, max_workers) -> (root, rig)`, `control.rs:62`). **`dir` (the `TempDir` guard) MUST stay in scope for the whole test** — dropping it deletes `root` mid-test. There is no `scaffold_root` helper.
- Exit trigger for the transition test: `FAKE_AGENT_EXIT_AFTER_CONTROL` + a `session.interrupt` request, exactly as `a_worker_that_answers_and_exits_immediately_still_yields_control_responded` (`control.rs:345`) does. The interrupt is the CAUSE of the transition; the worker exits, and the resulting `gone` rides the SIGCHLD wake — no poke of the fleet connection.

**Files:**
- Modify: `crates/camp/tests/control.rs`
- Test: same file

- [ ] **Step 1: Write the snapshot e2e test — delivered at hello time, no poke**

The snapshot rides the post-hello pump (synchronous in `serve_connection`), so it arrives immediately after the hello with NO wake and NO poke needed.

```rust
// ===== cp-2: fleet.subscribe / camp watch =================================

/// Open a fleet.subscribe connection and read + assert its hello. Mirrors the
/// SubConn idiom used for session.subscribe.
fn fleet_subscribe(root: &Path) -> std::io::BufReader<UnixStream> {
    let mut stream = connect(root);
    stream.write_all(b"{\"op\":\"fleet.subscribe\"}\n").unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let mut hello = String::new();
    reader.read_line(&mut hello).unwrap();
    let v: serde_json::Value = serde_json::from_str(hello.trim_end()).unwrap();
    assert_eq!(v["ok"], true, "fleet hello: {v}");
    assert!(v["subscription"].as_str().is_some(), "fleet hello has a subscription id: {v}");
    reader
}

/// THE EXIT CRITERION: the fleet renders live sessions from the socket ALONE,
/// delivered at hello time with NO client poke. Subscribe AFTER a worker is
/// live; the snapshot (its `session` frame + `synced`) arrives push-only.
#[test]
fn fleet_subscribe_delivers_a_live_session_and_synced() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root); // dispatch_one returns (bead, session)

    let mut reader = fleet_subscribe(&root);
    reader.get_ref().set_read_timeout(Some(Duration::from_millis(500))).unwrap();

    // NO poke: the snapshot is delivered by the post-hello pump.
    let mut saw_session = false;
    let mut saw_synced = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline && !(saw_session && saw_synced) {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() { continue; }
                let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
                if v["frame"] == "session" && v["session"]["name"] == session.as_str() {
                    assert_eq!(v["session"]["state"], "working");
                    assert!(!line.contains("\"pid\""), "§4.2: no pid on the wire: {line}");
                    saw_session = true;
                }
                if v["frame"] == "synced" { saw_synced = true; }
            }
            Err(ref e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => panic!("read: {e}"),
        }
    }
    assert!(saw_session, "the live session must appear in the fleet snapshot");
    assert!(saw_synced, "the snapshot must end with a synced frame");
    drop(campd);
}
```

- [ ] **Step 2: Run to verify it passes (Tasks 1-6 in the tree)**

Run: `cargo test -p camp --test control fleet_subscribe_delivers_a_live_session_and_synced -- --nocapture`
Expected: PASS. If it fails, debug with systematic-debugging — do NOT weaken the assertion or add a poke.

- [ ] **Step 3: Write the pushed-transition test — a completion PUSHes a `gone`, NO poke**

Subscribe (drain the snapshot), then trigger the worker's exit via interrupt, then read WITHOUT any poke and assert the `gone` arrives — proving fanout runs on the genuine SIGCHLD wake, not because the test poked campd.

```rust
/// A completion is PUSHED, not polled: a session that ends yields a `gone` frame
/// to a live fleet subscriber with NO client-initiated poke — the frame rides
/// the SIGCHLD wake the worker's exit causes.
#[test]
fn fleet_subscribe_pushes_a_gone_on_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_EXIT_AFTER_CONTROL", "1")]);
    let (_bead, session) = dispatch_one(&root);

    let mut reader = fleet_subscribe(&root);
    reader.get_ref().set_read_timeout(Some(Duration::from_millis(500))).unwrap();

    // Drain the snapshot (session + synced) so the next frame we see is the delta.
    for _ in 0..8 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 { break; }
        if serde_json::from_str::<serde_json::Value>(line.trim_end())
            .map(|v| v["frame"] == "synced").unwrap_or(false) { break; }
    }

    // Trigger the worker's exit: interrupt it (CAUSE of the transition), exactly
    // as control.rs:345's test does. The worker answers and exits -> SIGCHLD.
    {
        let mut ctl = connect(&root);
        let _ = request(&mut ctl, &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#));
    }

    // NO poke of the fleet connection: the `gone` must ride the SIGCHLD wake.
    let mut saw_gone = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline && !saw_gone {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() { continue; }
                let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
                if v["frame"] == "gone" && v["name"] == session.as_str() {
                    saw_gone = true;
                }
            }
            Err(ref e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => panic!("read: {e}"),
        }
    }
    assert!(saw_gone, "a session that ends must PUSH a gone frame — no poke");
    drop(campd);
}
```

If `FAKE_AGENT_EXIT_AFTER_CONTROL` + interrupt does not reliably drive the worker to exit in your run, adopt the exact mechanism `a_worker_that_answers_and_exits_immediately_still_yields_control_responded` uses (it is the canonical "worker answers and dies" path). Do not add a poke.

Run: `cargo test -p camp --test control fleet_subscribe_pushes_a_gone_on_session_end -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Run the whole control test file + gates**

Run: `cargo test -p camp --test control`
Expected: PASS (all cp-0/cp-1 e2e tests still green alongside the new ones).
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/tests/control.rs
git commit -m "test(cp-2): fleet.subscribe end to end — snapshot + pushed completion, no self-poke"
```

---

## Task 8: The idle perf gate — N fleet subscribers cost zero wakeups (§4.3)

cp-1's idle gate proved M tailed workers + N `session.subscribe` connections cost 0 CPU / <20 MB idle. cp-2 adds a new subscriber KIND; §4.3's obligation names subscribers, so the gate must hold with fleet subscribers too. LOCAL-ONLY (`make perf`).

**Known gap (state, do not close in cp-2):** cp-1's idle instrument holds the window for 30 s, which is shorter than `stall_after`, so it never triggers a stall-timer wake and never exercises the model-recompute cost WITH subscribers attached during a real stall. This is the same blind spot cp-1's gate has; closing it needs a longer window or an injected stall and is out of scope here.

**Files:**
- Modify: `crates/camp/tests/perf_daemon.rs`
- Test: same file

- [ ] **Step 1: Locate the cp-1 idle gate and add a fleet-subscriber arm**

Find `idle_campd_with_tailed_workers_zero_cpu_under_20mb` (cited in spec §4.3 discharge). Add a sibling test that, in addition to the M tailed workers, opens K `fleet.subscribe` connections, reads each one's hello + snapshot to `synced`, then holds them idle across the same 30 s window and asserts the same bounds.

```rust
/// §4.3 (cp-2): a FLEET subscriber on quiescent workers costs ZERO wakeups. The
/// model does not change while workers are silent, so no diff, no frame, no
/// write — the same idle property session.subscribe proved, for the aggregate.
#[test]
fn idle_campd_with_fleet_subscribers_zero_cpu_under_20mb() {
    // Mirror idle_campd_with_tailed_workers_zero_cpu_under_20mb's setup EXACTLY,
    // but add K fleet.subscribe connections: for each, write
    // {"op":"fleet.subscribe"}, read the hello, then read frames until `synced`,
    // then STOP reading and hold the connection open. Sample CPU delta + RSS
    // across the idle window and assert the SAME bounds the cp-1 idle arm asserts
    // (cpu_delta <= 10ms over 30s; RSS inside the idle band). Reuse the cp-1
    // sampling helpers verbatim; match its #[ignore]/cfg attribute if present.
}
```

Copy the cp-1 idle arm's sampling and assertion helpers exactly; do not invent new bounds.

- [ ] **Step 2: Run the perf gate locally**

Run: `make perf` (or the exact invocation the cp-1 perf arm uses).
Expected: PASS — CPU delta inside the idle bound, RSS inside the idle band.

- [ ] **Step 3: Commit**

```bash
git add crates/camp/tests/perf_daemon.rs
git commit -m "test(perf): idle fleet subscribers cost zero wakeups under the §4.3 bound (cp-2)"
```

---

## Final verification

- [ ] **Step 1: Full workspace gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: all green.

- [ ] **Step 2: Manual smoke of the exit criterion**

Bring up a camp with a dispatched fake worker (`camp daemon` in one shell), then in another: `camp watch`. Confirm one line per live session renders and updates without any polling flag, and that the client opens no session files — verify with `lsof -p "$(pgrep -f 'camp watch')"`: exactly one unix socket, no session `.json`.

- [ ] **Step 3: Run `make perf` once more if any perf-relevant code changed since Task 8.**

---

## Self-review checklist (run before hand-off)

1. **Spec coverage.**
   - §5.1 fleet view (agent/bead/state/FOR/LAST, BLOCKED placeholder) → Task 6 renderer; STATE built so BLOCKED drops in (scoping decision 5). ✅
   - §4.1 `fleet.subscribe` (aggregate stream: state transitions, stalls, completions) → Tasks 2/4/5; completion pushed without poke (Task 7); permission requests = cp-3's producer, carried on the wire via `blocked`. ✅
   - §4.2 addressed by name, stateless renderers, protocol not swappable → Task 6 (by-name map, no file/pid), Task 4 Step 7 (byte-pinned frames), Task 7 (no pid on wire). ✅
   - §4.4 subscribe MODE (bounded hello, timeout-exempt after, cap-STOP, stall-drop, per-wake scan budget) → Tasks 1/4/5; cap-STOP (Task 4 Step 8), fleet stall-drop (Step 9), scan budget (Task 1 Step 8) all pin their mutations. ✅
   - §4.3 idle-free with subscribers → Task 8 (known gap stated). ✅
   - §8 subscriber-backpressure test category → Task 4 Steps 8-9. ✅
   - Exit criterion (renders from socket alone, push-driven, zero polling, CI green) → Task 7 (no self-poke) + Final verification. ✅
2. **Placeholder scan.** The only `todo!`s are Task 1 Step 5's two "mechanical move from cp-1 control.rs:NNNN-NNNN" directives with exact source ranges and an explicit note that the `scanned` budget threads through the parameter. Every other code step carries complete code.
3. **Type consistency.** `OutBuf`/`FlushStep`(+`Debug`)/`Source`/`FileSource`(+`fill(scanned: &mut usize)`)/`FleetSource`/`Subscriber`(+`file()`)/`pump_subscriber`(+trailing `fleet_model`, owns `scanned`)/`fleet_model`/`serve_fleet_subscribe`/`Request::FleetSubscribe`/`Response::FleetSubscribed { ok, v, subscription }`/`SessionInfo: Clone`/`render`/`fmt_dur`/`state_display` are used identically across tasks. `control_step` gains `patrol`; `fanout` gains `ledger`+`patrol`; both updated at every call site. Test helpers `test_sub().file()`, `test_sub().out.out.len()`, `test_sub().out.blocked_since` are consistent with the nested shape.

## Notes for the implementer

- Task 1 is the risk, and it has ONE non-mechanical trap: the `MAX_PUMP_BYTES_PER_WAKE` scan budget (`control.rs:3435` `scanned`, guard at 3450) is a per-`pump_subscriber`-CALL bound. It is NOT in the 3442-3591 move range; it must be OWNED BY THE DRIVER and threaded into `FileSource::fill` by `&mut`, so it persists across FILL→FLUSH→re-FILL. Task 1 Step 8's test fails if it degrades to per-fill. Everything else is a mechanical move; do not "improve" cp-1's B/R/G blocks.
- The seam contract, restated: `OutBuf` owns the cap policy (`has_room`), the stall/drop policy (`flush` → `FlushStep::Stalled`), and the socket write — ONCE, for both source kinds. `FileSource` owns "hold the line in `partial`" (a file concept). `FleetSource` owns "don't advance `sent`" (its equivalent, because a ledger row has no file). That asymmetry IS the inheritance warning made concrete.
- Do not add a poll-timeout arm for fleet subscribers: a fleet fill fully drains per pump loop or WouldBlocks, so an empty-`out`-with-pending state never persists across a pump return. The WRITABLE edge is the only continuation it needs. Task 4 Step 4 verifies this.
- Do not add a self-poke to the Task 7 e2e tests: the snapshot rides the post-hello pump and the `gone` rides the SIGCHLD wake. A poke would make push-driven and poll-driven indistinguishable and defeat the exit-criterion proof.

## Known fixture-dimension gaps (state, not necessarily closed in cp-2)

- Session NAME-REUSE across a fleet subscription (a name reappears after a `gone`): the by-name `sent`/`rows` maps handle it, but no test drives it.
- A worker going gone WHILE its snapshot is cap-STOPped mid-delivery to a slow subscriber: `sent` correctness holds (the `gone` diff fires once the row was delivered), but no test drives the interleaving.
- The perf-gate stall-window blind spot (Task 8 known gap).

# cp-1: the control protocol — one module owns the wire, four verbs on the socket — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (this stream is planning-only; a FRESH implementer session executes after plan-gate APPROVE). Steps use checkbox (`- [ ]`) syntax for tracking. Branch: `cp-1-control-protocol` (already created, pushed with `-u origin`).

**Goal:** Give campd a control plane: ONE module that owns the undocumented `claude` control wire format (pinned by recorded fixtures), and the first four socket verbs — `sessions.list`, `session.send_turn`, `session.interrupt`, `session.subscribe` — with `interrupt`'s `control_response` round-tripping back over cp-0's read channel, and `subscribe` as a bounded, drop-loudly connection MODE.

**Architecture:** A new `crates/camp/src/daemon/control.rs` is the ONLY place in camp that constructs or parses a control message (spec §2.1, mitigation 1). It holds `ControlRuntime`: the pending-request table (with a response deadline that joins the event loop's `min_deadline`), the subscriber registry (per-connection output buffers, 1 MiB cap, drop-loudly), and every socket-verb handler body — so `event_loop.rs`'s new match arms are one-line delegations (deliberately minimal: `event_loop.rs` is shared with the in-flight compat-2 stream). campd writes control requests into the worker's already-held stdin pipe (`dispatch.rs::nudge_via_stdin`'s bounded-write mold); the worker's `control_response` comes back as a line in its stdout file, which cp-0's `ReadChannelRuntime` already tails to EOF on every wake — cp-1 adds a hand-off (`take_stream_lines`) so those complete lines reach `ControlRuntime` instead of only being counted.

**Tech Stack:** Rust (workspace edition), `mio` (event loop, already a dep), `serde`/`serde_json` (already), `uuid` (already — `spawn::new_session_id` uses it), `jiff` (already — `Timestamp`), `rusqlite` via `camp_core::ledger` (already), `tempfile` (dev-dep). **No new dependencies.**

---

## Global Constraints

Copied verbatim from AGENTS.md, the wave-2 kickoff, and the control-plane spec. Every task's requirements implicitly include these.

- **TDD, strictly.** Write the failing test, RUN it, watch it fail, implement, RUN it, watch it pass. Never claim a test passes without running it.
- **Never commit to main.** All work on `cp-1-control-protocol`; one reviewable PR.
- **Gates green before push:** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`. Work is not complete until pushed and `gh pr checks --watch` is green.
- **No panics in library code** (clippy `unwrap_used`/`expect_used`/`panic` denied; `unsafe_code` forbidden). Test modules/files opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (the existing convention — see `crates/camp/tests/read_channel.rs:1`).
- **Invariant 1 (idle is free).** No ticks, no polling loops. A connected but idle subscriber must cost ZERO wakeups; the pending-response deadline is an ARMED timer in `min_deadline`, never a poll interval.
- **Invariant 3 (nothing hidden).** Every campd action is an event with its cause. An interrupt written into a pipe is a ledger event; a dropped subscriber is a ledger event; a control response that never arrived is a ledger event.
- **Invariant 5 (fail fast).** No fallbacks, no silenced errors. Spec §2.1: *"An unrecognized control message, or a control response that never arrives, is an evented, operator-visible fault — never a swallowed timeout."*
- **Extend, don't rework.** cp-0's read channel is the transport; fix-86's `--verbose` argv is what makes worker stdout parseable. Do not rework either.
- **New events use `deny_unknown_fields` payload structs**, keep the one-transaction event+state property, satisfy the vocab-pin partition tests, and keep the refold property test green.
- **No test may spawn a real `claude` or spend API money**, except the `#[ignore]`d, `CAMP_COMPAT=1`-gated $0 tier in `crates/camp/tests/claude_compat.rs` (Task 10), which sends no turn and therefore costs $0.
- **Spec and code never silently diverge.** If implementation reality contradicts `docs/superpowers/specs/2026-07-12-camp-control-plane-design.md`, STOP and escalate to the lead — do not edit `docs/superpowers/specs/` without operator sign-off.
- **No co-author lines in commits. Never mention the assistant in a commit message.**

### Parallel-stream file ownership (wave-2, window W2)

- **cp-1 (this stream) OWNS:** `crates/camp/src/daemon/control.rs` (new), `crates/camp/src/daemon/read_channel.rs`, `crates/camp/src/daemon/socket.rs`, `crates/camp/src/daemon/patrol.rs` (one additive accessor, Task 7), `crates/camp/tests/control.rs` (new), `crates/camp/tests/fixtures/control/*.json` (new), `crates/camp/tests/claude_compat.rs`, `crates/camp/tests/fake-agent.sh` (one additive mode), `crates/camp/tests/perf_daemon.rs`.
- **SHARED — every touch strictly ADDITIVE and MINIMAL, expect a real rebase:** `crates/camp/src/daemon/event_loop.rs`, `crates/camp/src/daemon/dispatch.rs`, `crates/camp/src/daemon/mod.rs`, `crates/camp/src/main.rs`, `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`, `Cargo.toml`/`Cargo.lock`. **Do NOT refactor these files.**
- **compat-2 OWNS — DO NOT TOUCH:** `crates/camp-core/src/formula/**`, `ci/gc-compat/**`.
- After ANY merge to main the lead will instruct a rebase onto main and a full re-run of the gates before continuing.

---

## Root-cause analysis (confirmed against the code on this branch at `f6b248c`)

1. **campd can hear, but cannot speak the control protocol.** cp-0 (#93) merged the read channel: `read_channel.rs::drain_all` tails every live session's stdout file by byte offset on every wake and parses each complete line — but only into a `serde_json::Value` it *counts* (`read_channel.rs:635-650`, `parsed_counts`). Nothing correlates a `control_response`. And nothing in camp ever *writes* a `control_request`: `dispatch.rs::nudge_via_stdin` (dispatch.rs:208-227) is the only writer into the held stdin pipe, and it writes only `spawn::user_message` turns.
2. **The socket has no session verbs.** `socket.rs::Request` (socket.rs:26-45) carries exactly `poke`, `status`, `stop`, `adopt`, `nudge`. §4.1 requires `sessions.list`, `session.subscribe`, `session.send_turn`, `session.interrupt`.
3. **The socket is one-shot.** `event_loop.rs:364-372` returns the connection to the map on `Open` but `respond()` (event_loop.rs:997-1006) documents *"Responses are a few bytes; a WouldBlock here means the client is not reading"* — every write is assumed to fit the kernel buffer, and connections carry no outbound buffer. §4.4: a long-lived, server-push subscription violates all of that and requires per-connection output buffering with a hard cap.
4. **The ground-truth wire shapes are already recorded.** `crates/camp/tests/claude_compat.rs:115-129, 375-397` sends `{"type":"control_request","request_id":"…","request":{"subtype":"interrupt"}}` to the REAL pinned `claude` and reads back `{"type":"control_response","response":{"subtype":"success","request_id":"…","response":{"still_queued":[]}}}`. Note the shape: **`request_id` is nested INSIDE `response`, not at the top level.** Those bytes are cp-1's contract, and Task 1 turns them into shared fixture files that both `control.rs` and the $0 gate read.

**Fix:** one module owning the wire, a pending-request table with a deadline, a hand-off from the read channel, four socket verbs, and a bounded subscriber mode.

---

## Design decisions (each cited; each will be challenged at the plan gate)

**D1 — `interrupt` is ACK-then-ASYNC, not request/response.** `session.interrupt` writes the `control_request` into the worker's stdin and answers the socket immediately with `{"ok":true,"request_id":"…"}`. The worker's `control_response` arrives LATER, as a line in its stdout file, on a later campd wake, and appends `control.responded` carrying the same `request_id`.
*Why:* campd's event loop is single-threaded. Blocking a request handler until a filesystem-latency line appears is precisely the daemon-wide wedge class of issue #55 (`socket.rs:178-190`, `CampdUnresponsive`), and §4.4 makes bounded-answer-within-`REQUEST_TIMEOUT` the law for every verb. §2.3 states the latency budget honestly: *"Latency is filesystem-event latency… Fine for a permission prompt (human-speed) and for an interrupt ack."*
*What proves the round trip end to end:* the ledger — `session.interrupted{request_id}` then `control.responded{request_id, ok:true}` (Task 6's integration test). This is the §7 phase-1 slice: *"`interrupt` is only verifiable once phase 0 lands (its `control_response` arrives on the read channel)."*

**D2 — deliver-then-record for `interrupt` and `send_turn`; ledger-FIRST is scoped to permission decisions.** §5.3 step 5's ledger-before-pipe ordering exists for one stated reason: *"it makes 'the ledger shows an unanswered request' mean 'the response was definitely never sent'"* — the property that lets §5.3.4's adoption kill be safe. That property is about `can_use_tool` answers (phase 3). Interrupt and send_turn therefore follow the MERGED nudge precedent (`event_loop.rs:796-844`: deliver → record → respond), and a post-delivery append failure surfaces to the caller. Stated here so phase 3 does not inherit the wrong ordering.

**D3 — strict on the control surface, transparent on the stream surface.** A line whose `type` is `control_request`, `control_response`, or `control_cancel_request` MUST parse into a known subtype; anything else in that family is a LOUD `control.failed` fault (§2.1: *"An unrecognized control message… is an evented, operator-visible fault"*). Every OTHER `type` (`assistant`, `user`, `system`, `result`, `stream_event`, …) is an opaque stream line, passed through to subscribers verbatim and never faulted — new `claude` releases add stream types routinely, and §2.1's strictness is written about *control messages*, not the stream. This partition is the whole of `WorkerMessage`.

**D4 — `session.send_turn` REPLACES `Request::Nudge`; there is exactly one verb.** §4.1: *"`session.send_turn` — inject a user turn (this is `camp nudge`, promoted to the protocol)."* No back-compat is required (house rule). The existing nudge body moves VERBATIM out of `event_loop.rs` into `control.rs::serve_send_turn` and the new arm is a one-line call — so `event_loop.rs`'s footprint SHRINKS, which is the minimal-touch behaviour the parallel protocol wants. `cmd/nudge.rs:42` switches to the new request in the same commit.

**D5 — `subscriber_buffer_bytes` = 1 MiB as a module constant with a test-only env override**, exactly the cp-0 precedent the lead already approved for `max_stream_bytes` (cp-0 plan-gate ruling (b) + note 1: `MAX_STREAM_BYTES_DEFAULT` + `CAMP_MAX_STREAM_BYTES`). `config.rs` gains no field in cp-1 — operator-configurability is deferred and named below.

**D6 — subscribe's history catch-up is bounded and LOUD, never truncated.** §9: *"`session.subscribe` cursors are byte offsets into the stream file… A cursor into a reaped (disposed) stream is an explicit error, never a silently truncated stream."* So at hello: a cursor past EOF, a cursor into a session that is not tailed (reaped/disposed), or a history span from the cursor to the current tail that exceeds `subscriber_buffer_bytes` all fail the hello with an explicit error. None of them silently truncates.

### Deliberately DEFERRED (named, with the section that owns them)

| Deferred | Owner phase | Why it is not cp-1 |
|---|---|---|
| `--permission-prompt-tool stdio` in the worker argv | phase 3 (§2.0, §5.3.1) | §5.3.1: the flag is per-agent and only for agents whose resolved permission mode can ask. Camp's pinned worker config is `bypassPermissions` + explicit `--allowedTools`, which suppresses the flow entirely. **Consequence: no cp-1 worker can ever emit `can_use_tool`.** cp-1 still PARSES it (fixture-pinned, Task 1) and raises a loud `control.failed` if one ever appears — camp cannot answer it yet, and silence is forbidden. |
| `permission.pending` / BLOCKED / stall-timer disarm / adoption kill | phase 3 (§5.3–§5.3.4) | §7 phase 3 owns them wholesale. cp-1's `sessions.list` carries the `blocked` field (§4.1 requires it in the shape) hard-wired to `false` with a comment naming its phase-3 producer — a protocol field awaiting its producer, not a fallback. |
| The `initialize` handshake from campd | phase 3 (§9) | §9's stated reason for sending it is that its response carries `pending_permission_requests` — which cannot exist before the stdio flag does. §9 also settles that it is *"optional for a parent that only wants `interrupt` + `can_use_tool`… the `stdio` handler is wired from argv at startup, not gated on the handshake."* The $0 gate already proves the handshake round-trips against the real CLI (`claude_compat.rs:381-385`). |
| `--include-partial-messages` | phase 4 (§2.2) | §2.2: *"`camp attach`'s live view needs it; autonomous dispatch does not."* |
| `fleet.subscribe`, `session.permission_decision`, `session.set_model`, `session.set_permission_mode` | phases 2/3/5 (§4.1) | Not in this phase block. |
| `camp watch` / `camp attach` CLI | phases 2/4 (§7) | cp-1 ships the protocol, not an operator TUI. `subscribe` has a client API in `socket.rs` (Task 8) that the tests and phase 2 both consume; it has no CLI verb. |
| `subscriber_buffer_bytes` as `camp.toml` config | a phase that owns `config.rs` | cp-0 precedent (D5). |

---

## File structure

| File | Responsibility |
|---|---|
| `crates/camp/src/daemon/control.rs` **(new, ~700 lines)** | THE wire-format module (§2.1). `ParentMessage` (serialize), `WorkerMessage` (parse), `ControlRuntime` (pending table + response deadline + subscriber registry), and every verb handler body (`serve_sessions_list`, `serve_send_turn`, `serve_interrupt`, `serve_subscribe`). |
| `crates/camp/tests/fixtures/control/*.json` **(new)** | The recorded wire shapes (§2.1: *"pinned by tests against recorded fixtures"*). Read by `control.rs`'s unit tests via `include_str!` AND by the $0 real-claude gate — one source of truth for both sides. |
| `crates/camp/src/daemon/read_channel.rs` | +`take_stream_lines()` (hand complete lines to `ControlRuntime`), +`last_activity()`. |
| `crates/camp/src/daemon/socket.rs` | +4 `Request` variants, +4 `Response` variants, +wire pins, +`subscribe()` client API. |
| `crates/camp/src/daemon/event_loop.rs` **(shared — additive only)** | +`control` param, +4 one-line match arms, +`control.poll_timeout` in `min_deadline`, +the post-drain fanout block, +`pub(super)` on `Conn`. |
| `crates/camp/src/daemon/dispatch.rs` **(shared — additive only)** | +`write_control(session, line) -> ControlWrite` (the `nudge_via_stdin` twin). |
| `crates/camp/src/daemon/patrol.rs` | +`is_stalled(&self, session) -> bool` (one accessor for `sessions.list`'s state column). |
| `crates/camp-core/src/event.rs`, `vocab.rs`, `ledger/fold.rs` **(shared — additive only)** | 4 new camp-specific events. |
| `crates/camp/tests/control.rs` **(new)** | The §8 integration tests: interrupt end-to-end, send_turn, wedged-campd hello, backpressure. |
| `crates/camp/tests/fake-agent.sh` | +`FAKE_AGENT_CONTROL_LOOP` mode: answer a `control_request` on stdin with a `control_response` on stdout. |
| `crates/camp/tests/perf_daemon.rs` | The §4.3 idle gate grows N connected idle subscribers. |

---

## Task 1: `control.rs` — the wire format, and the fixtures that pin it

Spec: §2 (the protocol table), §2.1 (one module owns it; shapes pinned by recorded fixtures; failures loud), §9 (`request_user_dialog` is answered with a deterministic error).

**Files:**
- Create: `crates/camp/src/daemon/control.rs`
- Create: `crates/camp/tests/fixtures/control/interrupt_request.json`
- Create: `crates/camp/tests/fixtures/control/control_response_success.json`
- Create: `crates/camp/tests/fixtures/control/control_response_error.json`
- Create: `crates/camp/tests/fixtures/control/can_use_tool_request.json`
- Create: `crates/camp/tests/fixtures/control/request_user_dialog_request.json`
- Create: `crates/camp/tests/fixtures/control/dialog_refusal_response.json`
- Create: `crates/camp/tests/fixtures/control/user_turn.json`
- Create: `crates/camp/tests/fixtures/control/stream_assistant.json`
- Modify: `crates/camp/src/daemon/mod.rs` (add `pub mod control;` next to `pub mod read_channel;` at mod.rs:12)

**Interfaces produced (later tasks depend on these exact names):**
```rust
pub const REQUEST_ID_PREFIX: &str = "camp-";
pub fn new_request_id() -> String;                       // "camp-<uuid-v4>"
pub enum ParentMessage {                                  // camp -> worker (serialize only)
    Interrupt { request_id: String },
    DialogRefusal { request_id: String },                  // §9's deterministic error
}
impl ParentMessage { pub fn to_line(&self) -> String; }    // NDJSON, '\n'-terminated
pub enum WorkerMessage<'a> {                               // worker -> camp (parse only)
    ControlResponse { request_id: String, ok: bool, detail: String },
    CanUseTool { request_id: String, tool_name: String },
    RequestUserDialog { request_id: String },
    Stream(&'a str),                                       // any non-control stream-json line
}
pub fn parse_worker_line(line: &str) -> Result<WorkerMessage<'_>, ControlWireError>;
pub struct ControlWireError { pub line: String, pub reason: String }
```

- [ ] **Step 1: Write the fixtures.** These are the RECORDED shapes — `interrupt_request.json` and `control_response_success.json` are copied byte-for-byte from what the $0 gate already sends to and receives from the real pinned `claude` (`crates/camp/tests/claude_compat.rs:116, 388-393`). Each file is ONE line, no trailing newline.

`crates/camp/tests/fixtures/control/interrupt_request.json`:
```json
{"type":"control_request","request_id":"camp-fixture-1","request":{"subtype":"interrupt"}}
```
`crates/camp/tests/fixtures/control/control_response_success.json`:
```json
{"type":"control_response","response":{"subtype":"success","request_id":"camp-fixture-1","response":{"still_queued":[]}}}
```
`crates/camp/tests/fixtures/control/control_response_error.json`:
```json
{"type":"control_response","response":{"subtype":"error","request_id":"camp-fixture-1","error":"no turn in progress"}}
```
`crates/camp/tests/fixtures/control/can_use_tool_request.json`:
```json
{"type":"control_request","request_id":"cli-fixture-2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo publish"}}}
```
`crates/camp/tests/fixtures/control/request_user_dialog_request.json`:
```json
{"type":"control_request","request_id":"cli-fixture-3","request":{"subtype":"request_user_dialog","prompt":"pick one"}}
```
`crates/camp/tests/fixtures/control/dialog_refusal_response.json`:
```json
{"type":"control_response","response":{"subtype":"error","request_id":"cli-fixture-3","error":"camp does not support interactive dialogs"}}
```
`crates/camp/tests/fixtures/control/user_turn.json`:
```json
{"type":"user","message":{"role":"user","content":"status?"}}
```
`crates/camp/tests/fixtures/control/stream_assistant.json`:
```json
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}
```

- [ ] **Step 2: Write the failing test.** Create `crates/camp/src/daemon/control.rs` containing ONLY the `#[cfg(test)] mod tests` below plus `use` lines — the module must not compile-error on missing items yet, so write the test first and let it fail to compile, which is the red state for a new module (the codebase's own convention: cp-0's `read_channel.rs` unit tests live in-module).

```rust
//! cp-1 (control-plane spec §2.1): THE module that owns the `claude`
//! control wire format. Nothing else in camp constructs or parses a
//! control message. The shapes are pinned against RECORDED fixtures
//! (crates/camp/tests/fixtures/control/), the same bytes the $0
//! real-claude gate exchanges with the pinned CLI — so a protocol change
//! is a red build, not a silent misbehaviour.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const INTERRUPT: &str = include_str!("../../tests/fixtures/control/interrupt_request.json");
    const OK: &str = include_str!("../../tests/fixtures/control/control_response_success.json");
    const ERR: &str = include_str!("../../tests/fixtures/control/control_response_error.json");
    const CAN_USE_TOOL: &str =
        include_str!("../../tests/fixtures/control/can_use_tool_request.json");
    const DIALOG: &str =
        include_str!("../../tests/fixtures/control/request_user_dialog_request.json");
    const REFUSAL: &str =
        include_str!("../../tests/fixtures/control/dialog_refusal_response.json");
    const USER_TURN: &str = include_str!("../../tests/fixtures/control/user_turn.json");
    const STREAM: &str = include_str!("../../tests/fixtures/control/stream_assistant.json");

    /// §2.1: every shape camp SENDS is byte-pinned by a recorded fixture.
    #[test]
    fn parent_messages_serialize_to_the_recorded_fixtures() {
        let interrupt = ParentMessage::Interrupt {
            request_id: "camp-fixture-1".to_owned(),
        };
        assert_eq!(interrupt.to_line(), format!("{}\n", INTERRUPT.trim_end()));
        let refusal = ParentMessage::DialogRefusal {
            request_id: "cli-fixture-3".to_owned(),
        };
        assert_eq!(refusal.to_line(), format!("{}\n", REFUSAL.trim_end()));
        // The turn envelope camp sends on send_turn is spawn::user_message —
        // pinned here too, because §2.1 says EVERY shape camp sends is pinned,
        // and that constructor is shared with the dispatch launch path.
        assert_eq!(
            crate::daemon::spawn::user_message("status?"),
            format!("{}\n", USER_TURN.trim_end())
        );
    }

    /// §2.1: every shape camp PARSES is byte-pinned by a recorded fixture.
    /// NOTE the nesting the real CLI uses: `request_id` lives INSIDE
    /// `response`, not at the top level (claude_compat.rs:116).
    #[test]
    fn worker_messages_parse_from_the_recorded_fixtures() {
        match parse_worker_line(OK.trim_end()).unwrap() {
            WorkerMessage::ControlResponse { request_id, ok, .. } => {
                assert_eq!(request_id, "camp-fixture-1");
                assert!(ok);
            }
            other => panic!("expected a control_response, got {other:?}"),
        }
        match parse_worker_line(ERR.trim_end()).unwrap() {
            WorkerMessage::ControlResponse {
                request_id,
                ok,
                detail,
            } => {
                assert_eq!(request_id, "camp-fixture-1");
                assert!(!ok);
                assert!(detail.contains("no turn in progress"), "detail: {detail}");
            }
            other => panic!("expected an error control_response, got {other:?}"),
        }
        match parse_worker_line(CAN_USE_TOOL.trim_end()).unwrap() {
            WorkerMessage::CanUseTool {
                request_id,
                tool_name,
            } => {
                assert_eq!(request_id, "cli-fixture-2");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("expected can_use_tool, got {other:?}"),
        }
        match parse_worker_line(DIALOG.trim_end()).unwrap() {
            WorkerMessage::RequestUserDialog { request_id } => {
                assert_eq!(request_id, "cli-fixture-3");
            }
            other => panic!("expected request_user_dialog, got {other:?}"),
        }
    }

    /// D3: the stream surface is TRANSPARENT — a non-control line is passed
    /// through verbatim and is never a fault (new claude releases add stream
    /// types routinely; §2.1's strictness is about CONTROL messages).
    #[test]
    fn non_control_stream_lines_pass_through_verbatim_and_never_fault() {
        let line = STREAM.trim_end();
        match parse_worker_line(line).unwrap() {
            WorkerMessage::Stream(raw) => assert_eq!(raw, line),
            other => panic!("expected a passthrough Stream line, got {other:?}"),
        }
        // A stream type camp has never heard of is STILL a passthrough.
        let future = r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#;
        assert!(matches!(
            parse_worker_line(future).unwrap(),
            WorkerMessage::Stream(_)
        ));
    }

    /// D3 + §2.1: the CONTROL surface is STRICT — an unknown control subtype,
    /// a control envelope missing its request_id, and a non-JSON line are all
    /// LOUD errors. The caller turns each into a durable control.failed event.
    #[test]
    fn an_unrecognized_control_message_is_a_loud_error() {
        let unknown = r#"{"type":"control_request","request_id":"x","request":{"subtype":"teleport"}}"#;
        let err = parse_worker_line(unknown).unwrap_err();
        assert!(err.reason.contains("teleport"), "reason: {}", err.reason);
        let headless = r#"{"type":"control_response","response":{"subtype":"success"}}"#;
        assert!(parse_worker_line(headless).is_err(), "missing request_id");
        let garbage = "not json at all";
        let err = parse_worker_line(garbage).unwrap_err();
        assert_eq!(err.line, garbage);
        // control_cancel_request is a known member of the control family
        // (§2, "other subtypes present in the shipped CLI") that camp does not
        // implement — so it is a loud error, never a silent pass-through.
        let cancel = r#"{"type":"control_cancel_request","request_id":"x"}"#;
        assert!(parse_worker_line(cancel).is_err());
    }

    /// Request ids are globally unique across campd lives (a restarted campd
    /// must never collide with a pending id from its predecessor's ledger).
    #[test]
    fn request_ids_are_unique_and_prefixed() {
        let a = new_request_id();
        let b = new_request_id();
        assert_ne!(a, b);
        assert!(a.starts_with(REQUEST_ID_PREFIX), "{a}");
    }
}
```

- [ ] **Step 3: Run it and watch it fail.**

Run: `cargo test -p camp --lib daemon::control 2>&1 | tail -20`
Expected: FAIL — compile errors (`cannot find type ParentMessage`, `cannot find function parse_worker_line`), because only the test module exists. Also add `pub mod control;` to `crates/camp/src/daemon/mod.rs` (immediately after `pub mod bounded;`/before `pub mod cursor;` — alphabetical, matching the existing order) or the module is not compiled at all and the "failure" is vacuous. Verify the failure names the missing types.

- [ ] **Step 4: Implement the wire format.** Prepend to `control.rs` (above the test module):

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Every request id camp mints carries this prefix — a worker-visible,
/// greppable marker that the parent side is camp (never the SDK).
pub const REQUEST_ID_PREFIX: &str = "camp-";

/// A fresh, globally unique control request id. UUID-backed (the
/// `spawn::new_session_id` mold) because a restarted campd must never
/// collide with a pending id its predecessor left in the ledger.
pub fn new_request_id() -> String {
    format!("{REQUEST_ID_PREFIX}{}", uuid::Uuid::new_v4())
}

/// camp -> worker. Serialize ONLY: camp never parses what it sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentMessage {
    /// §2: the SDK's `interrupt()`.
    Interrupt { request_id: String },
    /// §9 (settled): the CLI genuinely sends `request_user_dialog` under
    /// stdio. Camp answers it with a deterministic error rather than
    /// ignoring it (a §2.1 protocol fault) or hanging the worker forever.
    DialogRefusal { request_id: String },
}

impl ParentMessage {
    /// One NDJSON line, newline-terminated — exactly what the held stdin
    /// pipe expects. Pinned byte-for-byte against the recorded fixtures.
    pub fn to_line(&self) -> String {
        let value = match self {
            ParentMessage::Interrupt { request_id } => serde_json::json!({
                "type": "control_request",
                "request_id": request_id,
                "request": {"subtype": "interrupt"},
            }),
            ParentMessage::DialogRefusal { request_id } => serde_json::json!({
                "type": "control_response",
                "response": {
                    "subtype": "error",
                    "request_id": request_id,
                    "error": "camp does not support interactive dialogs",
                },
            }),
        };
        let mut line = value.to_string();
        line.push('\n');
        line
    }
}

/// worker -> camp. Parse ONLY.
#[derive(Debug)]
pub enum WorkerMessage<'a> {
    /// The answer to a control request camp sent (correlated by request_id).
    ControlResponse {
        request_id: String,
        ok: bool,
        detail: String,
    },
    /// §2/§5.3: the worker asks its parent for a permission decision. cp-1
    /// cannot answer one (the `--permission-prompt-tool stdio` flag is phase
    /// 3, §5.3.1), so its arrival is a LOUD fault, never a silent drop.
    CanUseTool {
        request_id: String,
        tool_name: String,
    },
    /// §9: answered with `ParentMessage::DialogRefusal`.
    RequestUserDialog { request_id: String },
    /// D3: any non-control stream-json line — passed through to subscribers
    /// verbatim. NOT a fault: the stream surface is transparent by design.
    Stream(&'a str),
}

/// A line camp could not make sense of ON THE CONTROL SURFACE (D3). The
/// caller turns this into a durable `control.failed` event (§2.1: never a
/// swallowed error).
#[derive(Debug, Clone)]
pub struct ControlWireError {
    pub line: String,
    pub reason: String,
}

/// The control envelope, as the real CLI writes it. `deny_unknown_fields` is
/// deliberately NOT used on the outer envelope: the CLI is free to add keys
/// to a control message, and camp keys off `type` + `subtype` only. The
/// STRICTNESS lives in the subtype match below — an unknown subtype is a
/// loud error, which is the §2.1 obligation.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    request: Option<serde_json::Value>,
    #[serde(default)]
    response: Option<serde_json::Value>,
}

/// The control family — every `type` camp treats strictly (D3).
const CONTROL_TYPES: [&str; 3] = [
    "control_request",
    "control_response",
    "control_cancel_request",
];

/// Parse ONE complete stdout line from a worker.
///
/// D3, the partition that makes §2.1 implementable:
/// - `type` in CONTROL_TYPES  => strict. An unknown subtype, a missing
///   request_id, or a malformed envelope is a LOUD `ControlWireError`.
/// - any other `type`         => a transparent stream line, returned verbatim.
/// - not JSON at all          => a `ControlWireError` (the read channel
///   already surfaces those as patrol.degraded; the control runtime does not
///   double-report — see ControlRuntime::ingest).
pub fn parse_worker_line(line: &str) -> Result<WorkerMessage<'_>, ControlWireError> {
    let fail = |reason: String| ControlWireError {
        line: line.to_owned(),
        reason,
    };
    let envelope: Envelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => return Err(fail(format!("not a JSON object: {e}"))),
    };
    if !CONTROL_TYPES.contains(&envelope.kind.as_str()) {
        return Ok(WorkerMessage::Stream(line));
    }
    match envelope.kind.as_str() {
        "control_response" => {
            let body = envelope
                .response
                .ok_or_else(|| fail("control_response has no `response` object".to_owned()))?;
            // The real CLI nests request_id INSIDE `response`
            // (claude_compat.rs:116 — recorded from claude 2.1.207).
            let request_id = body["request_id"]
                .as_str()
                .ok_or_else(|| fail("control_response carries no request_id".to_owned()))?
                .to_owned();
            let subtype = body["subtype"].as_str().unwrap_or_default();
            match subtype {
                "success" => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: true,
                    detail: body["response"].to_string(),
                }),
                "error" => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: false,
                    detail: body["error"]
                        .as_str()
                        .unwrap_or("the worker reported an unspecified control error")
                        .to_owned(),
                }),
                other => Err(fail(format!(
                    "unrecognized control_response subtype {other:?}"
                ))),
            }
        }
        "control_request" => {
            let request_id = envelope
                .request_id
                .ok_or_else(|| fail("control_request carries no request_id".to_owned()))?;
            let body = envelope
                .request
                .ok_or_else(|| fail("control_request has no `request` object".to_owned()))?;
            match body["subtype"].as_str().unwrap_or_default() {
                "can_use_tool" => Ok(WorkerMessage::CanUseTool {
                    request_id,
                    tool_name: body["tool_name"].as_str().unwrap_or("<unnamed>").to_owned(),
                }),
                "request_user_dialog" => Ok(WorkerMessage::RequestUserDialog { request_id }),
                other => Err(fail(format!(
                    "unrecognized control_request subtype {other:?} — camp does not know how \
                     to answer it, and a control message it cannot answer is a protocol fault, \
                     never a silent drop (control-plane spec §2.1)"
                ))),
            }
        }
        other => Err(fail(format!(
            "camp does not implement the {other:?} control type"
        ))),
    }
}
```

- [ ] **Step 5: Run the tests and watch them pass.**

Run: `cargo test -p camp --lib daemon::control 2>&1 | tail -20`
Expected: PASS — 4 tests (`parent_messages_serialize_to_the_recorded_fixtures`, `worker_messages_parse_from_the_recorded_fixtures`, `non_control_stream_lines_pass_through_verbatim_and_never_fault`, `an_unrecognized_control_message_is_a_loud_error`, `request_ids_are_unique_and_prefixed` — 5).

- [ ] **Step 6: fmt + clippy, then commit.**

```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/control.rs crates/camp/src/daemon/mod.rs crates/camp/tests/fixtures/control
git commit -m "feat(control): one module owns the control wire format, pinned by recorded fixtures (cp-1 §2.1)"
```

---

## Task 2: the four new events

Spec: §2.1 (failures are loud, evented, operator-visible), §4.4 (`subscriber.dropped`, naming the session and the high-water mark), invariant 3 (every campd action is an event with its cause), invariant 7 (vocabulary mirror).

**Files:**
- Modify: `crates/camp-core/src/event.rs` (the `EventType` enum, `ALL`, `as_str`)
- Modify: `crates/camp-core/src/vocab.rs` (`CAMP_SPECIFIC_EVENTS`)
- Modify: `crates/camp-core/src/ledger/fold.rs` (four fold arms + four payload structs)
- Test: `crates/camp-core/tests/vocab_pin.rs` (existing — must stay green), `crates/camp-core/src/ledger/fold.rs` unit tests

**Interfaces produced:**
```rust
EventType::SessionInterrupted  => "session.interrupted"   // {session, request_id}
EventType::ControlResponded    => "control.responded"     // {session, request_id, verb, ok, detail}
EventType::ControlFailed       => "control.failed"        // {session, request_id, verb, reason}
EventType::SubscriberDropped   => "subscriber.dropped"    // {session, subscription, buffered_bytes, cap_bytes}
```
None of these four names exists in `crates/camp-core/tests/fixtures/gc-vocab.json` (verified: gc has `controller.started`/`controller.stopped`, no `control.*`; no `session.interrupted`; no `subscriber.*`) — so all four are CAMP-SPECIFIC, additive, never redefinitions (invariant 7).

- [ ] **Step 1: Add the variants AND their fold arms together.** (cp-0 plan-gate note 3, learned the hard way: adding the enum variant without the fold arm makes the next step's red an `E0004` compile error rather than a real test failure.)

In `crates/camp-core/src/event.rs`, after `SessionStreamCapped` (event.rs:55) add:
```rust
    /// cp-1 §4.1: campd wrote a `control_request{subtype:"interrupt"}` into
    /// the session's held stdin. The worker's answer arrives later on the
    /// read channel as `control.responded` with the same `request_id`
    /// (audit-only — no state fold).
    SessionInterrupted,
    /// cp-1 §2.1: a `control_response` from a worker, correlated to the
    /// request camp sent (audit-only — no state fold).
    ControlResponded,
    /// cp-1 §2.1: a control message camp could not parse, a control request
    /// camp cannot answer, a control write that failed, or a control response
    /// that never arrived within `CONTROL_RESPONSE_TIMEOUT`. Loud, evented,
    /// operator-visible — never a swallowed timeout (audit-only).
    ControlFailed,
    /// cp-1 §4.4: a subscriber whose per-connection output buffer crossed
    /// `subscriber_buffer_bytes` was dropped, naming the session and the
    /// high-water mark. campd never blocks on a slow subscriber, and it never
    /// silently discards its events (audit-only).
    SubscriberDropped,
```
Add the same four to the `ALL` array (after `EventType::SessionStreamCapped`, before `EventType::ImportAdded`) and to `as_str`:
```rust
            EventType::SessionInterrupted => "session.interrupted",
            EventType::ControlResponded => "control.responded",
            EventType::ControlFailed => "control.failed",
            EventType::SubscriberDropped => "subscriber.dropped",
```
In `crates/camp-core/src/ledger/fold.rs`, extend the audit-only arm (fold.rs:51) — but **parse the payload first**, so a malformed one is a loud fold error (the kickoff's `deny_unknown_fields` requirement; this is strictly stronger than `SessionStreamCapped`'s bare `Ok(())`):
```rust
        EventType::SessionInterrupted => audit::<SessionInterrupted>(event),
        EventType::ControlResponded => audit::<ControlResponded>(event),
        EventType::ControlFailed => audit::<ControlFailed>(event),
        EventType::SubscriberDropped => audit::<SubscriberDropped>(event),
```
and near the other payload structs add:
```rust
/// cp-1: the four control-plane events are AUDIT-ONLY — they mutate no
/// projected state — but their payloads are still validated on the fold, so
/// a malformed shape is a loud error at append time rather than a surprise
/// for a later reader. `deny_unknown_fields` is what makes a typo'd key a
/// red build instead of a silently ignored field.
fn audit<T: serde::de::DeserializeOwned>(event: &Event) -> Result<(), CoreError> {
    let _: T = payload(event)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionInterrupted {
    session: String,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlResponded {
    session: String,
    request_id: String,
    verb: String,
    ok: bool,
    #[serde(default)]
    detail: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlFailed {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    verb: Option<String>,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriberDropped {
    session: String,
    subscription: String,
    buffered_bytes: u64,
    cap_bytes: u64,
}
```

- [ ] **Step 2: Run the vocab-pin test and watch it fail RED (a real assertion failure, not a compile error).**

Run: `cargo test -p camp-core --test vocab_pin 2>&1 | tail -20`
Expected: FAIL in `every_event_type_is_declared_mirrored_or_camp_specific_never_both` — *"vocab.rs must partition exactly the EventType registry"*, listing the four undeclared names.

- [ ] **Step 3: Declare them camp-specific.** In `crates/camp-core/src/vocab.rs`, append to `CAMP_SPECIFIC_EVENTS` (after `"session.nudged"`):
```rust
    "session.interrupted",
    "control.responded",
    "control.failed",
    "subscriber.dropped",
```

- [ ] **Step 4: Run the vocab tests and watch them pass.**

Run: `cargo test -p camp-core --test vocab_pin 2>&1 | tail -20`
Expected: PASS — including `camp_specific_names_do_not_collide_with_gc` (proving none of the four exists in gc's registry).

- [ ] **Step 5: Write the fold round-trip test** (the cp-0 amendment-1 precedent: an audit-only event still has to survive the fold and a refold). Add to the `mod tests` in `crates/camp-core/src/ledger/mod.rs`, next to cp-0's `read_channel_patrol_degraded_shapes_round_trip_through_the_fold`:

```rust
    /// cp-1: the four control-plane events are audit-only, but their payloads
    /// are VALIDATED on the fold (deny_unknown_fields). Each shape appends
    /// cleanly, refolds cleanly, and a typo'd key is a loud error.
    #[test]
    fn control_plane_event_shapes_round_trip_through_the_fold() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let shapes = [
            (
                EventType::SessionInterrupted,
                serde_json::json!({"session": "t/dev/1", "request_id": "camp-1"}),
            ),
            (
                EventType::ControlResponded,
                serde_json::json!({
                    "session": "t/dev/1", "request_id": "camp-1",
                    "verb": "session.interrupt", "ok": true, "detail": "{}"
                }),
            ),
            (
                EventType::ControlFailed,
                serde_json::json!({
                    "session": "t/dev/1", "request_id": "camp-1",
                    "verb": "session.interrupt",
                    "reason": "no control_response within 30s"
                }),
            ),
            (
                EventType::SubscriberDropped,
                serde_json::json!({
                    "session": "t/dev/1", "subscription": "sub-1",
                    "buffered_bytes": 1048577u64, "cap_bytes": 1048576u64
                }),
            ),
        ];
        for (kind, data) in shapes {
            l.append(EventInput {
                kind,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data,
            })
            .unwrap();
        }
        // A typo'd key is REFUSED at append (deny_unknown_fields), never
        // silently ignored.
        let bad = l.append(EventInput {
            kind: EventType::SessionInterrupted,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"session": "t/dev/1", "requestId": "camp-2"}),
        });
        assert!(bad.is_err(), "an unknown payload key must be a loud error");
        // And the whole ledger refolds from scratch.
        l.refold().unwrap();
    }
```
(If the exact `refold()` entry point differs, use whatever `crates/camp-core/tests/refold_prop.rs` calls — match the existing API rather than inventing one.)

- [ ] **Step 6: Run it, then the refold property test.**

Run: `cargo test -p camp-core control_plane_event_shapes_round_trip_through_the_fold && cargo test -p camp-core --test refold_prop`
Expected: PASS both.

- [ ] **Step 7: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/camp-core
git commit -m "feat(events): session.interrupted, control.responded, control.failed, subscriber.dropped (cp-1 §2.1/§4.4)"
```

---

## Task 3: `ControlRuntime` — the pending table and the response deadline

Spec: §2.1 (*"a control response that never arrives… is an evented, operator-visible fault — never a swallowed timeout"*), invariant 1 (the deadline is an ARMED timer, never a tick).

**Files:**
- Modify: `crates/camp/src/daemon/control.rs`

**Interfaces produced:**
```rust
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
pub struct ControlRuntime { /* … */ }
impl ControlRuntime {
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime;
    /// Record a control request camp has just written; arms its deadline.
    pub fn track_pending(&mut self, request_id: String, session: String, verb: &'static str, now: Timestamp);
    /// The earliest pending deadline as a Duration-from-now — joins
    /// event_loop::min_deadline. None when nothing is pending (invariant 1).
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration>;
    /// Pending requests whose deadline has passed: removed from the table and
    /// returned as durable control.failed EventInputs (§2.1).
    pub fn expire_pending(&mut self, now: Timestamp) -> Vec<EventInput>;
    /// Resolve a request_id the worker answered. Returns the control.responded
    /// EventInput, or a control.failed EventInput when the id is UNKNOWN (an
    /// answer to a request camp never sent is itself a protocol fault).
    pub fn resolve(&mut self, request_id: &str, ok: bool, detail: String) -> EventInput;
}
```

- [ ] **Step 1: Write the failing tests.** Append to `control.rs`'s `mod tests`:

```rust
    use camp_core::event::EventType;
    use jiff::{SignedDuration, Timestamp};

    fn later(now: Timestamp, secs: i64) -> Timestamp {
        now.checked_add(SignedDuration::from_secs(secs)).unwrap()
    }

    /// Invariant 1: nothing pending => NO deadline => the idle daemon blocks
    /// in poll forever. A pending request arms exactly one deadline.
    #[test]
    fn a_pending_request_arms_a_deadline_and_an_empty_table_arms_none() {
        let now = Timestamp::now();
        let mut rt = ControlRuntime::new(1024);
        assert_eq!(rt.poll_timeout(now), None, "idle campd must not wake");
        rt.track_pending("camp-1".to_owned(), "t/dev/1".to_owned(), "session.interrupt", now);
        let timeout = rt.poll_timeout(now).expect("a pending request arms a timer");
        assert!(timeout <= CONTROL_RESPONSE_TIMEOUT && !timeout.is_zero());
        // Answered => the deadline disarms.
        rt.resolve("camp-1", true, "{}".to_owned());
        assert_eq!(rt.poll_timeout(now), None, "an answered request disarms");
    }

    /// §2.1: a control response that NEVER arrives is a durable, loud fault —
    /// never a swallowed timeout. Delete `expire_pending` and this dies.
    #[test]
    fn a_control_response_that_never_arrives_becomes_a_durable_fault() {
        let now = Timestamp::now();
        let mut rt = ControlRuntime::new(1024);
        rt.track_pending("camp-1".to_owned(), "t/dev/1".to_owned(), "session.interrupt", now);
        assert!(rt.expire_pending(now).is_empty(), "not due yet");
        let expired = rt.expire_pending(later(now, 31));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].kind, EventType::ControlFailed);
        assert_eq!(expired[0].data["request_id"], "camp-1");
        assert_eq!(expired[0].data["session"], "t/dev/1");
        assert!(
            expired[0].data["reason"].as_str().unwrap().contains("no control_response"),
            "the reason must name the missing response: {:?}", expired[0].data
        );
        // Expiry is idempotent: the pending row is GONE (no duplicate faults).
        assert!(rt.expire_pending(later(now, 60)).is_empty());
        assert_eq!(rt.poll_timeout(later(now, 60)), None);
    }

    /// An answer to a request camp never sent is itself a protocol fault
    /// (§2.1) — it is recorded, never ignored.
    #[test]
    fn a_control_response_for_an_unknown_request_id_is_a_fault() {
        let mut rt = ControlRuntime::new(1024);
        let input = rt.resolve("camp-nobody-sent-this", true, "{}".to_owned());
        assert_eq!(input.kind, EventType::ControlFailed);
        assert!(
            input.data["reason"].as_str().unwrap().contains("never sent"),
            "{:?}", input.data
        );
    }

    /// The happy path: a matching answer resolves to control.responded.
    #[test]
    fn a_matching_control_response_resolves_the_pending_request() {
        let now = Timestamp::now();
        let mut rt = ControlRuntime::new(1024);
        rt.track_pending("camp-1".to_owned(), "t/dev/1".to_owned(), "session.interrupt", now);
        let input = rt.resolve("camp-1", true, "{\"still_queued\":[]}".to_owned());
        assert_eq!(input.kind, EventType::ControlResponded);
        assert_eq!(input.data["request_id"], "camp-1");
        assert_eq!(input.data["session"], "t/dev/1");
        assert_eq!(input.data["verb"], "session.interrupt");
        assert_eq!(input.data["ok"], true);
    }
```

- [ ] **Step 2: Run and watch it fail.**

Run: `cargo test -p camp --lib daemon::control 2>&1 | tail -20`
Expected: FAIL — `cannot find type ControlRuntime in this scope`.

- [ ] **Step 3: Implement.** Add to `control.rs`:

```rust
use std::collections::HashMap;
use std::time::Duration;

use camp_core::event::{EventInput, EventType};
use jiff::{SignedDuration, Timestamp};

/// How long a control request may go unanswered before campd declares the
/// protocol broken (§2.1: "a control response that never arrives … is an
/// evented, operator-visible fault — never a swallowed timeout"). This is a
/// BOUND on one operation, not a wakeup: the deadline joins the event loop's
/// `min_deadline` only while a request is actually pending, so an idle campd
/// with nothing outstanding still blocks in `poll` forever (invariant 1).
///
/// 30s is generous against the transport: the answer travels worker stdout ->
/// file -> notify -> campd wake -> drain (§2.3's "filesystem-event latency"),
/// and §2.3's honest bound is that a LOST notify event delays the read to the
/// next wake. A campd with any live worker wakes far more often than that.
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

struct Pending {
    session: String,
    verb: &'static str,
    deadline: Timestamp,
}

pub struct ControlRuntime {
    pending: HashMap<String, Pending>,
    subscriber_buffer_bytes: usize,
    // (Task 8 adds the subscriber registry here.)
}

impl ControlRuntime {
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime {
        ControlRuntime {
            pending: HashMap::new(),
            subscriber_buffer_bytes,
        }
    }

    pub fn track_pending(
        &mut self,
        request_id: String,
        session: String,
        verb: &'static str,
        now: Timestamp,
    ) {
        let deadline = now
            .checked_add(SignedDuration::try_from(CONTROL_RESPONSE_TIMEOUT).unwrap_or(
                SignedDuration::from_secs(30),
            ))
            .unwrap_or(now);
        self.pending.insert(
            request_id,
            Pending {
                session,
                verb,
                deadline,
            },
        );
    }

    /// The earliest pending deadline, as a Duration-from-now (the
    /// `OrdersRuntime::poll_timeout` / `PatrolRuntime::poll_timeout` mold —
    /// event_loop::min_deadline composes them, earliest wins).
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration> {
        let earliest = self.pending.values().map(|p| p.deadline).min()?;
        let remaining = earliest.duration_since(now);
        Some(
            Duration::try_from(remaining).unwrap_or(Duration::ZERO),
        )
    }

    /// Due pending requests => durable control.failed events. The row is
    /// REMOVED, so a fault is raised exactly once (no duplicate events, no
    /// hot re-fire — invariant 1).
    pub fn expire_pending(&mut self, now: Timestamp) -> Vec<EventInput> {
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        due.into_iter()
            .filter_map(|id| {
                let p = self.pending.remove(&id)?;
                Some(EventInput {
                    kind: EventType::ControlFailed,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "session": p.session,
                        "request_id": id,
                        "verb": p.verb,
                        "reason": format!(
                            "no control_response for {} within {}s — the worker never \
                             answered on its stdout stream (control-plane spec §2.1: a \
                             control response that never arrives is a loud fault, never a \
                             swallowed timeout)",
                            p.verb,
                            CONTROL_RESPONSE_TIMEOUT.as_secs()
                        ),
                    }),
                })
            })
            .collect()
    }

    /// Correlate a worker's control_response with the request camp sent.
    pub fn resolve(&mut self, request_id: &str, ok: bool, detail: String) -> EventInput {
        match self.pending.remove(request_id) {
            Some(p) => EventInput {
                kind: EventType::ControlResponded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": p.session,
                    "request_id": request_id,
                    "verb": p.verb,
                    "ok": ok,
                    "detail": detail,
                }),
            },
            // §2.1: an answer to a request camp never sent means the protocol
            // is not what camp believes it to be. Loud, never ignored.
            None => EventInput {
                kind: EventType::ControlFailed,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "request_id": request_id,
                    "reason": format!(
                        "a worker answered control request {request_id:?}, which camp never \
                         sent (or already resolved) — the control protocol is not what camp \
                         believes it to be (control-plane spec §2.1)"
                    ),
                }),
            },
        }
    }
}
```
Note: `SignedDuration::try_from(Duration)` and `Duration::try_from(SignedDuration)` are the jiff conversions already used in `event_loop.rs:179`. `unwrap_or` here is a saturating conversion of a 30-second constant, not error-silencing — but clippy denies `unwrap()`, so keep the `unwrap_or` form exactly as written.

- [ ] **Step 4: Run and watch pass.**

Run: `cargo test -p camp --lib daemon::control`
Expected: PASS — 9 tests.

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/control.rs
git commit -m "feat(control): pending-request table with an armed response deadline (cp-1 §2.1)"
```

---

## Task 4: the read channel hands its lines to the control runtime

Spec: §2.3 (the read channel is the transport), §4.1 (`sessions.list`'s "last activity").

cp-0's `drain_one` parses each complete line into a `serde_json::Value` and only increments `parsed_counts` (`read_channel.rs:635-650`). cp-1 needs those lines. **This is an EXTENSION of cp-0, not a rework:** the drain loop, the offsets, the cap guards, the partial-line buffering, and the disposal ordering are all untouched.

**Files:**
- Modify: `crates/camp/src/daemon/read_channel.rs`

**Interfaces produced:**
```rust
pub struct StreamLine { pub session: String, pub line: String, pub offset_after: u64 }
impl ReadChannelRuntime {
    /// Complete JSON lines consumed by this wake's drain, in file order, per
    /// session. Drained by the event loop after drain_all.
    pub fn take_stream_lines(&mut self) -> Vec<StreamLine>;
    /// When this session's offset last advanced (None until it has).
    pub fn last_activity(&self, session: &str) -> Option<Timestamp>;
    /// The current tail offset + the stdout path, for subscribe's history
    /// catch-up (Task 8). None when the session is not tailed.
    pub fn tail_state(&self, session: &str) -> Option<(PathBuf, u64)>;
}
```

- [ ] **Step 1: Write the failing test.** Append to `read_channel.rs`'s `mod tests`:

```rust
    /// cp-1: the drain HANDS OVER the complete lines it consumed — in file
    /// order, tagged with the session and the offset after each line — so the
    /// control runtime can correlate a control_response and the subscriber
    /// registry can fan it out. cp-0 only counted them.
    #[test]
    fn drain_all_hands_over_the_complete_lines_it_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        let a = "{\"type\":\"system\",\"subtype\":\"init\"}";
        let b = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"camp-1\"}}";
        std::fs::write(&stdout, format!("{a}\n{b}\n")).unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        assert!(rc.last_activity("t/dev/1").is_none(), "nothing read yet");
        rc.drain_all(&mut ledger).unwrap();

        let lines = rc.take_stream_lines();
        assert_eq!(lines.len(), 2, "both complete lines are handed over");
        assert_eq!(lines[0].session, "t/dev/1");
        assert_eq!(lines[0].line, a);
        assert_eq!(lines[1].line, b, "file order is preserved");
        assert_eq!(
            lines[1].offset_after,
            (a.len() + b.len() + 2) as u64,
            "the offset after the last line is the byte offset a subscriber resumes from"
        );
        assert!(rc.last_activity("t/dev/1").is_some(), "the offset advanced");
        // Drained by mem::take: a second call yields nothing (no re-delivery).
        assert!(rc.take_stream_lines().is_empty());
        // A partial line is NOT handed over until it is complete.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&stdout)
            .unwrap()
            .write_all(b"{\"type\":\"resu")
            .unwrap();
        rc.drain_all(&mut ledger).unwrap();
        assert!(rc.take_stream_lines().is_empty(), "a partial line is never handed over");
    }
```

- [ ] **Step 2: Run and watch it fail.**

Run: `cargo test -p camp --lib daemon::read_channel::tests::drain_all_hands_over 2>&1 | tail -20`
Expected: FAIL — `no method named take_stream_lines`.

- [ ] **Step 3: Implement.** In `read_channel.rs`:

Add the struct and two runtime fields:
```rust
/// cp-1: one complete JSON line the drain consumed, handed to the control
/// runtime (which correlates control messages) and the subscriber registry
/// (which fans it out). `offset_after` is the byte offset PAST this line —
/// the cursor a subscriber resumes from (§9: subscribe cursors are byte
/// offsets into the stream file).
#[derive(Debug, Clone)]
pub struct StreamLine {
    pub session: String,
    pub line: String,
    pub offset_after: u64,
}
```
In `struct ReadChannelRuntime`, after `parsed_counts`:
```rust
    /// cp-1: complete lines consumed this wake, in file order, drained by the
    /// event loop after `drain_all`.
    stream_lines: Vec<StreamLine>,
    /// cp-1: when each session's offset last advanced — `sessions.list`'s
    /// "last activity" column (§4.1). This is the honest source: the last
    /// time the worker actually produced output campd consumed.
    last_activity: HashMap<String, jiff::Timestamp>,
```
Initialize both in `new()` (`stream_lines: Vec::new(), last_activity: HashMap::new(),`).

In `drain_one`, inside the existing `Ok(_v) => { … }` parse arm (read_channel.rs:635-641), keep the `parsed_counts` bump and ADD the hand-off (do not touch the surrounding loop, the offsets, or the cap guards):
```rust
                    Ok(_v) => {
                        self.parsed_counts
                            .entry(session.to_owned())
                            .and_modify(|c| *c += 1)
                            .or_insert(1);
                        // cp-1: hand the complete line over. `new_offset` is
                        // the byte offset PAST it (the subscribe cursor, §9).
                        self.stream_lines.push(StreamLine {
                            session: session.to_owned(),
                            line: line.to_owned(),
                            offset_after: new_offset,
                        });
                        self.last_activity
                            .insert(session.to_owned(), jiff::Timestamp::now());
                    }
```
(The borrow of `self.stream_lines` while `t` is borrowed from `self.tailed` will not compile as written — `t` is a `&mut` from `self.tailed.get_mut`. Fix by collecting into a LOCAL `Vec<StreamLine>` inside `drain_one` and appending it to `self.stream_lines` after the `t` borrow ends, exactly as `parse_errors`/`drain_errors` already work around the same borrow — look at how `self.parse_errors.push` compiles today: it does not, unless the field access is disjoint. **Verify against the compiler**: if `self.parse_errors.push(...)` inside the loop compiles today (it does — disjoint field borrows are accepted within one method body), then `self.stream_lines.push(...)` and `self.parsed_counts` compile identically. Use the same pattern; if the borrow checker objects, collect locally and flush after the loop.)

Add the accessors:
```rust
    /// cp-1: the complete lines this wake's drain consumed (file order,
    /// per session). Drained by `mem::take` — a second call yields nothing,
    /// so no line is ever delivered twice.
    pub fn take_stream_lines(&mut self) -> Vec<StreamLine> {
        std::mem::take(&mut self.stream_lines)
    }

    /// cp-1 §4.1: when this session's offset last advanced — the "last
    /// activity" `sessions.list` reports. None until the first line lands
    /// (the caller falls back to the registry's `spawned_ts`, a DEFINED
    /// semantic, not an error being swallowed).
    pub fn last_activity(&self, session: &str) -> Option<jiff::Timestamp> {
        self.last_activity.get(session).copied()
    }

    /// cp-1 §9: the tailed file and campd's current tail offset — what
    /// `session.subscribe` needs to serve history from a cursor. None when
    /// the session is not tailed (reaped/disposed => an explicit hello error).
    pub fn tail_state(&self, session: &str) -> Option<(PathBuf, u64)> {
        self.tailed
            .get(session)
            .map(|t| (t.stdout_path.clone(), t.offset))
    }
```
Also clear both new maps' per-session entries in `unregister` (next to the `tailed.remove`): `self.last_activity.remove(session);` — a disposed session has no activity to report. `stream_lines` needs no cleanup (it is drained every wake).

- [ ] **Step 4: Run and watch pass, then run the WHOLE cp-0 suite to prove nothing regressed.**

Run: `cargo test -p camp --lib daemon::read_channel && cargo test -p camp --test read_channel`
Expected: PASS — the new test plus every cp-0 test (offsets, cap breach, disposal ordering, restart cursors).

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/read_channel.rs
git commit -m "feat(read-channel): hand the drained complete lines to the control runtime (cp-1)"
```

---

## Task 5: `dispatch::write_control` — the write half

Spec: §2 (*"holds the child's stdin as a live pipe"*), issue #55 (every blocking syscall on the loop carries a deadline).

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` (**shared — ADDITIVE ONLY**: one new public method beside `nudge_via_stdin` at dispatch.rs:208; touch nothing else)

**Interfaces produced:**
```rust
pub enum ControlWrite { Delivered, NoPipe, Failed(String) }
impl Dispatcher {
    pub fn write_control(&mut self, session: &str, line: &str) -> ControlWrite;
}
```

- [ ] **Step 1: Write the failing test.** Append to `dispatch.rs`'s `mod tests` (reuse the existing held-stdin `cat` worker scaffolding — `dispatch.rs:348` `spawn_held_stdin_worker`-style helper; use whichever helper the file already provides, named at dispatch.rs:348 and dispatch.rs:388):

```rust
    /// cp-1: a control_request goes into the SAME held stdin pipe a nudge
    /// uses, with the SAME bounded write (issue #55: an unbounded write into
    /// a full pipe wedges campd's single-threaded loop). A session with no
    /// held pipe is NoPipe, never a panic.
    #[test]
    fn write_control_delivers_into_the_held_stdin_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = test_dispatcher(dir.path()); // the existing helper
        d.register_fake_held_worker("t/dev/1"); // the existing `cat` scaffold
        let line = "{\"type\":\"control_request\",\"request_id\":\"camp-1\",\
                    \"request\":{\"subtype\":\"interrupt\"}}\n";
        assert!(matches!(
            d.write_control("t/dev/1", line),
            ControlWrite::Delivered
        ));
        assert!(matches!(
            d.write_control("t/nobody/9", line),
            ControlWrite::NoPipe
        ));
    }
```
(Use the EXACT names of the two test scaffolds already in `dispatch.rs` — read dispatch.rs:348 and dispatch.rs:388 and call them as they are named. Do not add new scaffolding if one already fits.)

- [ ] **Step 2: Run and watch it fail.**

Run: `cargo test -p camp --lib daemon::dispatch::tests::write_control 2>&1 | tail -20`
Expected: FAIL — `no method named write_control`.

- [ ] **Step 3: Implement.** Add to `dispatch.rs`, immediately AFTER `nudge_via_stdin` (keep the diff local — this file is shared):

```rust
/// cp-1: the disposition of a control-message write into a worker's held
/// stdin. The `NudgeOutcome` twin — separate because a control message has
/// no resume path: `via="none"` is a legitimate answer for a turn (the CLI
/// resumes the session instead), but there is NO way to interrupt a worker
/// campd does not hold a pipe to. NoPipe is therefore a caller-visible
/// failure, not a designed degrade.
#[derive(Debug)]
pub enum ControlWrite {
    Delivered,
    NoPipe,
    Failed(String),
}

impl Dispatcher {
    /// Write one control-protocol line into the session's held stdin pipe
    /// (control-plane spec §2: the pipe camp already holds). BOUNDED, exactly
    /// as `nudge_via_stdin` is (issue #55): an unbounded blocking write into
    /// the full pipe of a worker that stopped reading would wedge campd's
    /// single-threaded event loop. On a bounded failure the pipe may hold a
    /// torn partial line, so it is dropped — no later line can interleave
    /// garbage.
    ///
    /// The LINE must be built by `control::ParentMessage::to_line` — §2.1:
    /// nothing outside `control.rs` constructs a control message.
    pub fn write_control(&mut self, session: &str, line: &str) -> ControlWrite {
        let Some(worker) = self.children.values_mut().find(|w| w.session == session) else {
            return ControlWrite::NoPipe;
        };
        let Some(stdin) = worker.stdin.as_mut() else {
            return ControlWrite::NoPipe;
        };
        match bounded::write_bounded(stdin, line.as_bytes(), STDIN_WRITE_TIMEOUT) {
            Ok(()) => ControlWrite::Delivered,
            Err(e) => {
                worker.stdin = None; // torn pipe: never write after a failed line
                ControlWrite::Failed(format!("control write failed: {e}"))
            }
        }
    }
}
```
(If `impl Dispatcher` is a single block, add the method inside it rather than opening a second `impl` — match the file's existing shape.)

- [ ] **Step 4: Run and watch pass.**

Run: `cargo test -p camp --lib daemon::dispatch`
Expected: PASS — the new test plus every existing dispatch test.

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/dispatch.rs
git commit -m "feat(dispatch): write_control — the bounded control-message write into the held stdin (cp-1 §2)"
```

---

## Task 6: the socket verbs — `session.interrupt` and `session.send_turn`, end to end

Spec: §4.1 (the verbs), §4.2 (*"sessions are addressed by NAME, never by pid or file path"*), §7 phase 1 (*"`interrupt` and `send_turn` first — they are the smallest end-to-end slice through the whole stack"*). Design decisions D1, D2, D4.

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs` (Request/Response variants + wire pins)
- Modify: `crates/camp/src/daemon/control.rs` (the handler bodies)
- Modify: `crates/camp/src/daemon/event_loop.rs` (**shared — additive/minimal**: thread `control`, 2 one-line arms, `min_deadline`, the post-drain block)
- Modify: `crates/camp/src/daemon/mod.rs` (construct `ControlRuntime`, pass it to `event_loop::run`)
- Modify: `crates/camp/src/cmd/nudge.rs:42` (send the new verb)
- Modify: `crates/camp/tests/fake-agent.sh` (the control-loop mode)
- Create: `crates/camp/tests/control.rs` (the integration test)

**Interfaces produced:**
```rust
// socket.rs
Request::SessionSendTurn { session: String, text: String }   // op "session.send_turn"
Request::SessionInterrupt { session: String }                // op "session.interrupt"
Response::SendTurn { ok: bool, via: String }                 // {"ok":true,"via":"stdin"}
Response::Interrupt { ok: bool, request_id: String }         // {"ok":true,"request_id":"camp-…"}
// control.rs
pub fn serve_send_turn(session: &str, text: &str, ledger: &mut Ledger, dispatcher: &mut Dispatcher) -> Response;
impl ControlRuntime {
    pub fn serve_interrupt(&mut self, session: &str, ledger: &mut Ledger, dispatcher: &mut Dispatcher, now: Timestamp) -> Response;
    /// Ingest this wake's drained lines: correlate control responses, answer
    /// dialogs, fault on the unanswerable. Returns durable EventInputs.
    pub fn ingest(&mut self, lines: &[StreamLine], dispatcher: &mut Dispatcher) -> Vec<EventInput>;
}
```

- [ ] **Step 1: Pin the new socket wire (failing test).** Append to `socket.rs`'s `mod tests`:

```rust
    /// cp-1 §4.1: the control-plane verbs. Sessions are addressed by NAME
    /// (§4.2) — never a pid, never a file path: the day a worker lives in a
    /// VM on another host, clients must not notice.
    #[test]
    fn control_plane_verbs_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Request::SessionInterrupt {
                session: "camp/dev/1".into()
            })
            .unwrap(),
            r#"{"op":"session.interrupt","session":"camp/dev/1"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::SessionSendTurn {
                session: "camp/dev/1".into(),
                text: "status?".into()
            })
            .unwrap(),
            r#"{"op":"session.send_turn","session":"camp/dev/1","text":"status?"}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"session.interrupt","session":"s"}"#).unwrap(),
            Request::SessionInterrupt { session: "s".into() }
        );
        // Responses: untagged, so the discriminating field must resolve.
        assert_eq!(
            serde_json::to_string(&Response::Interrupt {
                ok: true,
                request_id: "camp-1".into()
            })
            .unwrap(),
            r#"{"ok":true,"request_id":"camp-1"}"#
        );
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true,"request_id":"camp-1"}"#).unwrap(),
            Response::Interrupt { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true,"via":"stdin"}"#).unwrap(),
            Response::SendTurn { via, .. } if via == "stdin"
        ));
        // D4: `nudge` is GONE — one verb, not two.
        assert!(serde_json::from_str::<Request>(r#"{"op":"nudge","session":"s","text":"t"}"#).is_err());
    }
```
Delete the now-false `nudge_wire_format_is_pinned` test (socket.rs:672) — its verb no longer exists (D4).

- [ ] **Step 2: Run and watch it fail.**

Run: `cargo test -p camp --lib daemon::socket 2>&1 | tail -20`
Expected: FAIL — `no variant named SessionInterrupt`.

- [ ] **Step 3: Implement the socket types.** In `socket.rs`, in `enum Request` REPLACE the `Nudge` variant (D4) with:
```rust
    /// cp-1 §4.1: inject one user turn into a live worker's campd-held stdin
    /// pipe. This is `camp nudge`, promoted to the protocol.
    #[serde(rename = "session.send_turn")]
    SessionSendTurn { session: String, text: String },
    /// cp-1 §4.1: stop the current turn. ACK-then-ASYNC (D1): the answer
    /// carries the request_id; the worker's `control_response` arrives later
    /// on the read channel as a `control.responded` event.
    #[serde(rename = "session.interrupt")]
    SessionInterrupt { session: String },
```
(The enum is `#[serde(tag = "op", rename_all = "snake_case")]`; a per-variant `rename` overrides the snake_case rule and gives the dotted verb the spec names.)

In `enum Response`, REPLACE `Nudge` with (keeping the untagged variant-order rule stated at socket.rs:47 — both must precede `Ok`):
```rust
    /// cp-1: send_turn disposition. via="stdin" => the turn is in the held
    /// pipe; via="none" => no held pipe (the caller converses over the resume
    /// path instead).
    SendTurn { ok: bool, via: String },
    /// cp-1 §4.1 (D1): the interrupt was written into the pipe. The worker's
    /// control_response lands later on the read channel.
    Interrupt { ok: bool, request_id: String },
```
Update `crates/camp/src/cmd/nudge.rs:42` (`&Request::Nudge {` → `&Request::SessionSendTurn {`) and its two `Response::Nudge` matches (nudge.rs:47, 59) → `Response::SendTurn`.

- [ ] **Step 4: Implement the handlers in `control.rs`.**

```rust
use crate::daemon::dispatch::{ControlWrite, Dispatcher, NudgeOutcome};
use crate::daemon::read_channel::StreamLine;
use crate::daemon::socket::Response;
use crate::daemon::spawn;
use camp_core::ledger::Ledger;

/// §4.1 `session.send_turn` — camp nudge, promoted to the protocol (D4).
/// The body is MOVED VERBATIM from event_loop.rs's `Request::Nudge` arm
/// (dispatch-lifecycle Phase 1, #29): deliver -> record -> respond. The
/// injected turn is a ledger fact (invariant 3), and a post-delivery append
/// failure surfaces to the caller (invariant 5) — the ledger must not claim
/// what was not delivered, and the caller must not believe what the ledger
/// does not hold. No held pipe is NOT an error: via="none" routes the caller
/// to the resume path.
pub fn serve_send_turn(
    session: &str,
    text: &str,
    ledger: &mut Ledger,
    dispatcher: &mut Dispatcher,
) -> Response {
    match dispatcher.nudge_via_stdin(session, text) {
        NudgeOutcome::Delivered => {
            let (rig, bead) = dispatcher
                .child_info(session)
                .map(|(r, b)| (Some(r), Some(b)))
                .unwrap_or((None, None));
            match ledger.append(EventInput {
                kind: EventType::SessionNudged,
                rig,
                actor: "campd".into(),
                bead,
                data: serde_json::json!({
                    "session": session, "via": "stdin", "text": text,
                }),
            }) {
                Ok(_) => Response::SendTurn {
                    ok: true,
                    via: "stdin".into(),
                },
                Err(e) => Response::Error {
                    ok: false,
                    error: format!(
                        "turn delivered into {session} but recording session.nudged failed: {e}"
                    ),
                },
            }
        }
        NudgeOutcome::NoPipe => Response::SendTurn {
            ok: true,
            via: "none".into(),
        },
        NudgeOutcome::Failed(e) => Response::Error {
            ok: false,
            error: format!("stdin nudge of {session} failed: {e}"),
        },
    }
}

impl ControlRuntime {
    /// §4.1 `session.interrupt`, D1 (ACK-then-ASYNC) + D2 (deliver -> record
    /// -> respond, the merged nudge ordering; §5.3's ledger-FIRST rule is
    /// scoped to permission DECISIONS, whose whole purpose is to make
    /// "pending in the ledger" prove "never written to the pipe" for §5.3.4's
    /// adoption kill).
    ///
    /// campd does NOT wait for the control_response: its event loop is
    /// single-threaded, and blocking a request handler on a filesystem-latency
    /// line is the daemon-wide wedge class of issue #55. The response arrives
    /// on the read channel and becomes `control.responded` (see `ingest`).
    ///
    /// No held pipe is a LOUD ERROR here, unlike send_turn: there is no resume
    /// path for an interrupt — a worker campd holds no pipe to cannot be
    /// interrupted at all, and pretending otherwise would be a silent no-op.
    pub fn serve_interrupt(
        &mut self,
        session: &str,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        now: Timestamp,
    ) -> Response {
        let request_id = new_request_id();
        let line = ParentMessage::Interrupt {
            request_id: request_id.clone(),
        }
        .to_line();
        match dispatcher.write_control(session, &line) {
            ControlWrite::Delivered => {
                let (rig, bead) = dispatcher
                    .child_info(session)
                    .map(|(r, b)| (Some(r), Some(b)))
                    .unwrap_or((None, None));
                match ledger.append(EventInput {
                    kind: EventType::SessionInterrupted,
                    rig,
                    actor: "campd".into(),
                    bead,
                    data: serde_json::json!({
                        "session": session, "request_id": request_id,
                    }),
                }) {
                    Ok(_) => {
                        self.track_pending(
                            request_id.clone(),
                            session.to_owned(),
                            "session.interrupt",
                            now,
                        );
                        Response::Interrupt {
                            ok: true,
                            request_id,
                        }
                    }
                    Err(e) => Response::Error {
                        ok: false,
                        error: format!(
                            "interrupt delivered into {session} but recording \
                             session.interrupted failed: {e}"
                        ),
                    },
                }
            }
            ControlWrite::NoPipe => Response::Error {
                ok: false,
                error: format!(
                    "campd holds no stdin pipe for {session} — it is not a live campd-spawned \
                     worker (exited, released, attended, or adopted from a previous campd \
                     life), and there is no other way to interrupt a turn (control-plane \
                     spec §2.3)"
                ),
            },
            ControlWrite::Failed(e) => Response::Error {
                ok: false,
                error: format!("writing the interrupt into {session} failed: {e}"),
            },
        }
    }

    /// Consume this wake's drained stream lines (§2.3's read channel is the
    /// transport). Returns the durable events the caller appends:
    ///   - a `control_response`      => `control.responded` (or `control.failed`
    ///                                  for an id camp never sent)
    ///   - a `request_user_dialog`   => answered with a deterministic error
    ///                                  (§9) and recorded
    ///   - a `can_use_tool`          => `control.failed`: camp cannot answer one
    ///                                  until phase 3 wires
    ///                                  `--permission-prompt-tool stdio`
    ///                                  (§5.3.1), and a control message it
    ///                                  cannot answer is never dropped silently
    ///   - an unparsable CONTROL line => `control.failed` (§2.1)
    ///   - any other stream line     => nothing here; Task 8 fans it out to
    ///                                  subscribers
    pub fn ingest(
        &mut self,
        lines: &[StreamLine],
        dispatcher: &mut Dispatcher,
    ) -> Vec<EventInput> {
        let mut out = Vec::new();
        for sl in lines {
            match parse_worker_line(&sl.line) {
                Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok,
                    detail,
                }) => out.push(self.resolve(&request_id, ok, detail)),
                Ok(WorkerMessage::RequestUserDialog { request_id }) => {
                    // §9 (settled): answered with a deterministic error —
                    // neither ignored (a §2.1 protocol fault) nor left to hang
                    // the worker forever.
                    let line = ParentMessage::DialogRefusal {
                        request_id: request_id.clone(),
                    }
                    .to_line();
                    let reason = match dispatcher.write_control(&sl.session, &line) {
                        ControlWrite::Delivered => format!(
                            "the worker asked for an interactive dialog (request {request_id}); \
                             camp answered with the deterministic refusal (control-plane spec §9)"
                        ),
                        ControlWrite::NoPipe => format!(
                            "the worker asked for an interactive dialog (request {request_id}) \
                             but campd holds no stdin pipe for it — the refusal could not be \
                             delivered and the worker will wait forever"
                        ),
                        ControlWrite::Failed(e) => format!(
                            "the worker asked for an interactive dialog (request {request_id}) \
                             and the refusal write failed: {e}"
                        ),
                    };
                    out.push(EventInput {
                        kind: EventType::ControlFailed,
                        rig: None,
                        actor: "campd".into(),
                        bead: None,
                        data: serde_json::json!({
                            "session": sl.session,
                            "request_id": request_id,
                            "verb": "request_user_dialog",
                            "reason": reason,
                        }),
                    });
                }
                Ok(WorkerMessage::CanUseTool {
                    request_id,
                    tool_name,
                }) => out.push(EventInput {
                    kind: EventType::ControlFailed,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "session": sl.session,
                        "request_id": request_id,
                        "verb": "can_use_tool",
                        "reason": format!(
                            "the worker asked for a permission decision on {tool_name} \
                             (request {request_id}), which camp cannot answer yet: the \
                             permission flow is control-plane phase 3 (§5.3), and no camp \
                             worker is spawned with --permission-prompt-tool stdio (§5.3.1), \
                             so this should be UNREACHABLE — the protocol is not what camp \
                             believes it to be. The worker is now blocked forever and must \
                             be killed by the operator."
                        ),
                    }),
                }),
                Ok(WorkerMessage::Stream(_)) => {} // Task 8 fans these out.
                Err(e) => out.push(EventInput {
                    kind: EventType::ControlFailed,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "session": sl.session,
                        "reason": format!(
                            "unparsable control message at stream offset {}: {} — line: {}",
                            sl.offset_after, e.reason, e.line
                        ),
                    }),
                }),
            }
        }
        out
    }
}
```
**Note on double-reporting:** `parse_worker_line` errors on a non-JSON line too — but cp-0's `drain_one` only hands over lines that ALREADY parsed as JSON (`read_channel.rs:635`, the `Ok(_v)` arm), and non-JSON lines are surfaced separately as `patrol.degraded`. So `ingest` never sees a non-JSON line and never double-reports. Do not "fix" this by adding a guard.

- [ ] **Step 5: Wire the event loop (SHARED FILE — additive, minimal).** In `event_loop.rs`:

1. `run()` and the three helpers (`serve_connection`, `drain_lines`, `settle`, `reap_and_refill`) gain one more parameter, threaded exactly like `read_channel: &mut ReadChannelRuntime` already is: `control: &mut super::control::ControlRuntime`. (`settle` does NOT need it — thread it only into `run`, `serve_connection`, and `drain_lines`, the three that serve requests.)
2. The poll-timeout composition (event_loop.rs:153-159) gains a FOURTH source — one nested `min_deadline`:
```rust
        let timeout = min_deadline(
            min_deadline(
                min_deadline(
                    runtime.poll_timeout(poll_now),
                    graph.poll_timeout(Instant::now()),
                ),
                patrol.poll_timeout(poll_now),
            ),
            control.poll_timeout(poll_now),
        );
```
3. Right after the patrol stall-fire declaration (event_loop.rs:203-204), declare expired control requests:
```rust
        // cp-1 §2.1: a control response that never arrived is a durable,
        // operator-visible fault — never a swallowed timeout. Same
        // declare-then-settle shape as the patrol stall fires above.
        for input in control.expire_pending(now) {
            ledger.append(input)?;
            wake_ledger_work = true;
        }
```
4. Two new arms in `drain_lines` (each ONE line of logic — the bodies live in `control.rs`):
```rust
            Ok(Request::SessionSendTurn { session, text }) => {
                let response = super::control::serve_send_turn(&session, &text, ledger, dispatcher);
                respond(&mut conn.stream, &response)?;
            }
            Ok(Request::SessionInterrupt { session }) => {
                let response =
                    control.serve_interrupt(&session, ledger, dispatcher, Timestamp::now());
                respond(&mut conn.stream, &response)?;
            }
```
and DELETE the old `Ok(Request::Nudge { session, text }) => { … }` arm (event_loop.rs:796-844) — its body now lives in `control.rs::serve_send_turn` verbatim. (This makes the shared file SMALLER.)
5. In the post-drain block, immediately AFTER `read_channel.drain_all(ledger)` (event_loop.rs:428-430) and BEFORE the cap-breach loop:
```rust
        // cp-1: the drained complete lines reach the control runtime — this is
        // where an interrupt's control_response is correlated (§2.3 is the
        // transport; §2.1 owns the parse). Its events are durable, and the
        // `appended_read_channel_events` flag below settles them in this same
        // wake.
        let stream_lines = read_channel.take_stream_lines();
        let mut appended_read_channel_events = false;
        for input in control.ingest(&stream_lines, dispatcher) {
            ledger.append(input)?;
            appended_read_channel_events = true;
        }
```
(and DELETE the now-duplicate `let mut appended_read_channel_events = false;` at event_loop.rs:438 — move the declaration up, do not declare it twice.)

**Ordering matters and is load-bearing:** `ingest` runs BEFORE `read_channel.persist_offsets` (event_loop.rs:539), so a line's ledger effect commits before campd persists the offset past it — cp-0 fix 7's rule, now with a real per-line effect. State this in a comment at the `persist_offsets` call site (cp-0 left a TODO there for exactly this phase).

6. In `crates/camp/src/daemon/mod.rs`, construct the runtime beside the read channel (mod.rs:167) and pass it to `event_loop::run`:
```rust
    // cp-1 (§4.4): the control runtime — the pending-request table and the
    // subscriber registry. `subscriber_buffer_bytes` follows the cp-0
    // `max_stream_bytes` precedent: a module constant with a test-only env
    // override; a camp.toml field is deferred to a phase that owns config.rs.
    let mut control = control::ControlRuntime::new(control::subscriber_buffer_bytes_from_env(
        control::SUBSCRIBER_BUFFER_BYTES_DEFAULT,
    )?);
```
(`subscriber_buffer_bytes_from_env` is defined in Task 8; for THIS task define it now as the exact `max_stream_bytes_from_env` twin — `read_channel.rs:34-50` — reading `CAMP_SUBSCRIBER_BUFFER_BYTES`, failing fast on a malformed or zero value.)

- [ ] **Step 6: Teach the fake worker to answer a control request.** In `crates/camp/tests/fake-agent.sh`, add a new mode next to `FAKE_AGENT_NUDGE_CLOSE`. Document it in the header comment block:

```bash
#   FAKE_AGENT_CONTROL_LOOP    cp-1: after the task line, read stdin lines
#                              forever. A control_request line is answered on
#                              stdout with the control_response the real CLI
#                              sends (claude_compat.rs pins the shape); a plain
#                              user turn (a send_turn) ends the loop and the
#                              worker closes its bead. This is the fake's half
#                              of the interrupt round trip: campd -> stdin ->
#                              worker -> stdout file -> campd's read channel.
```
and the body, placed immediately BEFORE the `FAKE_AGENT_NUDGE_CLOSE` block:
```bash
if [[ -n "${FAKE_AGENT_CONTROL_LOOP:-}" ]]; then
  read -r _task_line
  while read -r line; do
    case "$line" in
      *'"control_request"'*)
        # Echo back the request_id in a control_response, exactly as the real
        # CLI does (note the nesting: request_id lives INSIDE `response`).
        rid="$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')"
        if [[ -z "$rid" ]]; then
          echo "fake-agent: control_request with no request_id: $line" >&2
          exit 95
        fi
        emit_stream "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"still_queued\":[]}}}"
        ;;
      *)
        break  # a plain user turn: fall through to the close
        ;;
    esac
  done
fi
```

- [ ] **Step 7: Write the END-TO-END integration test (the phase's exit criterion).** Create `crates/camp/tests/control.rs`. Copy the harness helpers (`munge`, `stdout_path`, `camp`, `camp_ok`, `scaffold`, `fake_agent`, `Daemon`, `connect`, `request`, `events_json`, `wait_until`) VERBATIM from `crates/camp/tests/read_channel.rs:1-180` — they are the established integration harness; do not invent a new one.

```rust
/// cp-1 EXIT CRITERION (§7 phase 1): `interrupt` works END TO END against a
/// fake worker over the REAL socket — and the proof is the round trip, not
/// the ack. campd writes the control_request into the worker's held stdin;
/// the worker answers on its stdout; cp-0's read channel drains that file on
/// the next wake; the control runtime correlates the request_id and appends
/// `control.responded`. Every hop is real.
///
/// D1: the socket answer is the ACK (ok + request_id) — campd's single-
/// threaded loop must never block a request handler on a filesystem-latency
/// line (issue #55's wedge class).
#[test]
fn interrupt_round_trips_through_the_read_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let mut stream = connect(&root);
    let bead = camp_ok(&root, &["sling", "do the thing --json"]).trim().to_owned();
    wait_until(&root, "the worker to wake", |events| {
        events.iter().any(|e| e["kind"] == "session.woke")
    });
    let session = events_json(&root)
        .into_iter()
        .find(|e| e["kind"] == "session.woke")
        .map(|e| e["data"]["name"].as_str().unwrap().to_owned())
        .expect("a session.woke names the session");

    // The verb, over the real socket.
    let response = request(
        &mut stream,
        &serde_json::json!({"op": "session.interrupt", "session": session}).to_string(),
    );
    assert_eq!(response["ok"], true, "response: {response}");
    let request_id = response["request_id"].as_str().expect("the ack carries the request_id");
    assert!(request_id.starts_with("camp-"), "request_id: {request_id}");

    // The durable record of the campd ACTION (invariant 3).
    wait_until(&root, "session.interrupted", |events| {
        events.iter().any(|e| {
            e["kind"] == "session.interrupted" && e["data"]["request_id"] == request_id
        })
    });

    // THE ROUND TRIP: the worker's control_response came back over the read
    // channel and was correlated. This is the whole point of the phase.
    wait_until(&root, "control.responded", |events| {
        events.iter().any(|e| {
            e["kind"] == "control.responded"
                && e["data"]["request_id"] == request_id
                && e["data"]["ok"] == true
                && e["data"]["verb"] == "session.interrupt"
        })
    });
    assert!(!bead.is_empty());
    drop(campd);
}

/// §4.1 `session.send_turn` — camp nudge promoted to the protocol (D4). The
/// turn lands in the held pipe and the delivery is a ledger fact.
#[test]
fn send_turn_delivers_a_user_turn_into_the_held_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let mut stream = connect(&root);
    camp_ok(&root, &["sling", "do the thing --json"]);
    wait_until(&root, "the worker to wake", |events| {
        events.iter().any(|e| e["kind"] == "session.woke")
    });
    let session = events_json(&root)
        .into_iter()
        .find(|e| e["kind"] == "session.woke")
        .map(|e| e["data"]["name"].as_str().unwrap().to_owned())
        .unwrap();

    let response = request(
        &mut stream,
        &serde_json::json!({"op": "session.send_turn", "session": session, "text": "status?"})
            .to_string(),
    );
    assert_eq!(response["ok"], true, "response: {response}");
    assert_eq!(response["via"], "stdin", "the turn went into the held pipe");
    wait_until(&root, "session.nudged", |events| {
        events.iter().any(|e| e["kind"] == "session.nudged" && e["data"]["via"] == "stdin")
    });
    // FAKE_AGENT_NUDGE_CLOSE: the second stdin line unblocks the worker, so
    // the turn REALLY arrived — the worker closes its bead and exits.
    wait_until(&root, "the worker to close its bead", |events| {
        events.iter().any(|e| e["kind"] == "bead.closed")
    });
    drop(campd);
}

/// §4.1/§2.3: interrupting a session campd holds no pipe to is a LOUD error,
/// never a silent no-op. There is no resume path for an interrupt (unlike a
/// turn) — pretending it worked would be the worst possible answer.
#[test]
fn interrupting_a_session_with_no_held_pipe_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[]);
    let mut stream = connect(&root);
    let response = request(
        &mut stream,
        &serde_json::json!({"op": "session.interrupt", "session": "t/dev/404"}).to_string(),
    );
    assert_eq!(response["ok"], false, "response: {response}");
    let error = response["error"].as_str().unwrap();
    assert!(error.contains("no stdin pipe"), "the error must name the cause: {error}");
    drop(campd);
}
```

- [ ] **Step 8: Run the whole thing.**

Run: `cargo test -p camp --test control 2>&1 | tail -30`
Expected: PASS — 3 tests. If `interrupt_round_trips_through_the_read_channel` hangs at `control.responded`, the hand-off (Task 4) or the `ingest` call site (Step 5.5) is wrong — debug with `cargo test -p camp --test control -- --nocapture` and read campd's stderr, which the harness inherits.

- [ ] **Step 9: Full suite (the shared-file blast radius: `cmd/nudge.rs`, `cli_nudge.rs`, `daemon_patrol.rs` all touch the nudge path).**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: PASS. `crates/camp/tests/cli_nudge.rs` exercises `camp nudge` end to end and MUST still pass — if it fails, the D4 rename missed a call site.

- [ ] **Step 10: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A
git commit -m "feat(control): session.interrupt + session.send_turn — the round trip over the read channel (cp-1 §4.1)"
```

---

## Task 7: `sessions.list`

Spec: §4.1 (*"every session: name, agent, rig, bead, state, last activity, and whether it is blocked on a permission decision"*), §4.2 (addressed by name, never by pid), §4.3 (campd owns the truth; clients are stateless renderers).

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs` (`Request::SessionsList`, `Response::SessionsList`, wire pin)
- Modify: `crates/camp/src/daemon/control.rs` (`serve_sessions_list`)
- Modify: `crates/camp/src/daemon/patrol.rs` (one additive accessor)
- Modify: `crates/camp/src/daemon/event_loop.rs` (**shared** — one more one-line arm)
- Test: `crates/camp/tests/control.rs`

**Interfaces produced:**
```rust
// socket.rs
Request::SessionsList                                   // op "sessions.list"
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct SessionInfo {
    pub name: String, pub agent: String, pub rig: Option<String>,
    pub bead: Option<String>, pub state: String,
    pub last_activity: String,   // RFC3339
    pub blocked: bool,           // phase 3's producer (§5.3); false in cp-1
}
Response::SessionsList { ok: bool, sessions: Vec<SessionInfo> }
// patrol.rs
impl PatrolRuntime { pub fn is_stalled(&self, session: &str) -> bool; }
// control.rs
pub fn serve_sessions_list(ledger: &Ledger, patrol: &PatrolRuntime, read_channel: &ReadChannelRuntime) -> Response;
```

- [ ] **Step 1: Write the failing tests.**

Wire pin — append to `socket.rs`'s `mod tests`:
```rust
    /// cp-1 §4.1/§4.2: `sessions.list` — NAMES, never pids, never file paths.
    /// The day a worker lives in a VM on another host, clients must not notice.
    #[test]
    fn sessions_list_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Request::SessionsList).unwrap(),
            r#"{"op":"sessions.list"}"#
        );
        let response = Response::SessionsList {
            ok: true,
            sessions: vec![SessionInfo {
                name: "camp/dev/1".into(),
                agent: "dev".into(),
                rig: Some("gc".into()),
                bead: Some("gc-1".into()),
                state: "working".into(),
                last_activity: "2026-07-13T00:00:00Z".into(),
                blocked: false,
            }],
        };
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"ok":true,"sessions":[{"name":"camp/dev/1","agent":"dev","rig":"gc","bead":"gc-1","state":"working","last_activity":"2026-07-13T00:00:00Z","blocked":false}]}"#
        );
        assert!(
            !serde_json::to_string(&response).unwrap().contains("pid"),
            "§4.2: a protocol that hands out pids cannot cross a machine boundary"
        );
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true,"sessions":[]}"#).unwrap(),
            Response::SessionsList { .. }
        ));
    }
```
Integration — append to `crates/camp/tests/control.rs`:
```rust
/// §4.1: `sessions.list` reports every live session by NAME, with its agent,
/// rig, bead, state, last activity, and whether it is blocked on a permission
/// decision (always false until the phase-3 permission flow exists — §5.3.1
/// makes the flow unreachable in cp-1, and the field is the protocol's, not a
/// guess).
#[test]
fn sessions_list_reports_live_sessions_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let mut stream = connect(&root);
    camp_ok(&root, &["sling", "do the thing --json"]);
    wait_until(&root, "the worker to wake", |events| {
        events.iter().any(|e| e["kind"] == "session.woke")
    });
    let response = request(&mut stream, r#"{"op":"sessions.list"}"#);
    assert_eq!(response["ok"], true, "{response}");
    let sessions = response["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "one live session: {response}");
    let s = &sessions[0];
    assert_eq!(s["agent"], "dev");
    assert_eq!(s["rig"], "gc");
    assert_eq!(s["state"], "working");
    assert_eq!(s["blocked"], false);
    assert!(s["bead"].as_str().unwrap().starts_with("gc-"), "{s}");
    assert!(s["name"].as_str().unwrap().contains("/dev/"), "{s}");
    assert!(s["last_activity"].as_str().unwrap().contains('T'), "RFC3339: {s}");
    assert!(s.get("pid").is_none(), "§4.2: never a pid on the wire");
    drop(campd);
}
```

- [ ] **Step 2: Run and watch both fail.**

Run: `cargo test -p camp --lib daemon::socket 2>&1 | tail -10; cargo test -p camp --test control 2>&1 | tail -10`
Expected: FAIL — `no variant named SessionsList`.

- [ ] **Step 3: Implement.**

`socket.rs` — add `SessionInfo` (above `enum Response`), `Request::SessionsList` (`#[serde(rename = "sessions.list")] SessionsList,`) and `Response::SessionsList { ok: bool, sessions: Vec<SessionInfo> }` (place it FIRST among the untagged variants — its `sessions` key is its discriminator, and untagged resolution is order-sensitive; the existing comment at socket.rs:47-48 states the rule).

`patrol.rs` — one accessor next to `stalled_count` (patrol.rs:233):
```rust
    /// cp-1 §4.1: is this session in patrol's stalled set? `sessions.list`'s
    /// state column. (`stalled_count` already counts them — this names them.)
    pub fn is_stalled(&self, session: &str) -> bool {
        self.stalled.contains(session)
    }
```
(Use the ACTUAL field name behind `stalled_count` — read patrol.rs:233 and match it.)

`control.rs`:
```rust
/// §4.1 `sessions.list`. campd owns the truth; the client is a stateless
/// renderer (§4.3) — so this answers from the LEDGER's session registry
/// (`live_sessions()`), not from campd's in-memory child map: an ADOPTED
/// worker from a previous campd life is a live session too, and a client
/// must see it.
///
/// `state`:
///   "stalled"  — patrol's stall ladder has fired for it
///   "working"  — a live registered session
///   otherwise  — the registry's own status verbatim
/// `blocked` is FALSE in cp-1 and its producer is phase 3 (§5.3): no camp
/// worker is spawned with `--permission-prompt-tool stdio` (§5.3.1), so the
/// `can_use_tool` flow is unreachable. The field is in the shape because §4.1
/// puts it there — a protocol field awaiting its producer, not a guess.
/// `last_activity` is when the read channel last consumed a line from the
/// session's stream (the honest signal: the last output the worker produced),
/// falling back to the registry's `spawned_ts` before the first line lands.
pub fn serve_sessions_list(
    ledger: &Ledger,
    patrol: &crate::daemon::patrol::PatrolRuntime,
    read_channel: &crate::daemon::read_channel::ReadChannelRuntime,
) -> Response {
    let rows = match ledger.live_sessions() {
        Ok(rows) => rows,
        Err(e) => {
            return Response::Error {
                ok: false,
                error: format!("sessions.list failed: {e}"),
            };
        }
    };
    let sessions = rows
        .into_iter()
        .map(|row| {
            let state = if patrol.is_stalled(&row.name) {
                "stalled".to_owned()
            } else {
                "working".to_owned()
            };
            let last_activity = read_channel
                .last_activity(&row.name)
                .map(|ts| ts.to_string())
                .unwrap_or(row.spawned_ts.clone());
            SessionInfo {
                name: row.name,
                agent: row.agent,
                rig: row.rig,
                bead: row.bead,
                state,
                last_activity,
                blocked: false,
            }
        })
        .collect();
    Response::SessionsList { ok: true, sessions }
}
```
`event_loop.rs` — one more arm in `drain_lines`:
```rust
            Ok(Request::SessionsList) => {
                let response =
                    super::control::serve_sessions_list(ledger, patrol, read_channel);
                respond(&mut conn.stream, &response)?;
            }
```

- [ ] **Step 4: Run and watch both pass.**

Run: `cargo test -p camp --lib daemon::socket && cargo test -p camp --test control`
Expected: PASS.

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A && git commit -m "feat(control): sessions.list — every session by name, never by pid (cp-1 §4.1/§4.2)"
```

---

## Task 8: `session.subscribe` — a connection MODE, bounded and drop-loudly

Spec: §4.4 (the whole section), §9 (cursors are byte offsets; a cursor into a reaped stream is an explicit error), §8 (*"Subscriber backpressure: a subscriber that stops reading is dropped at `subscriber_buffer_bytes` with the `subscriber.dropped` event; campd never blocks. The hello arrives within `REQUEST_TIMEOUT` even against a busy daemon."*).

**Files:**
- Modify: `crates/camp/src/daemon/control.rs` (the subscriber registry)
- Modify: `crates/camp/src/daemon/socket.rs` (`Request::SessionSubscribe`, `Response::Subscribed`, the `subscribe()` client API, wire pins)
- Modify: `crates/camp/src/daemon/event_loop.rs` (**shared**: `pub(super)` on `Conn`, one arm, WRITABLE interest, the fanout call)
- Test: `crates/camp/tests/control.rs`

**Interfaces produced:**
```rust
// control.rs
pub const SUBSCRIBER_BUFFER_BYTES_DEFAULT: usize = 1024 * 1024;      // §4.4: 1 MiB
pub fn subscriber_buffer_bytes_from_env(default: usize) -> Result<usize>;  // CAMP_SUBSCRIBER_BUFFER_BYTES
impl ControlRuntime {
    /// The hello (§4.4): validate the cursor, buffer the history, register the
    /// subscriber. Returns the hello Response — ALWAYS within REQUEST_TIMEOUT.
    pub fn serve_subscribe(&mut self, token: Token, session: &str, cursor: Option<u64>,
                           read_channel: &ReadChannelRuntime) -> Response;
    /// Push this wake's lines to every subscriber of their session, then flush
    /// what the sockets will take. Returns (tokens_to_close, durable events).
    pub fn fanout(&mut self, lines: &[StreamLine], conns: &mut HashMap<Token, Conn>)
        -> (Vec<Token>, Vec<EventInput>);
    /// A connection went away: forget its subscription.
    pub fn forget(&mut self, token: Token);
}
// socket.rs
Request::SessionSubscribe { session: String, cursor: Option<u64> }   // op "session.subscribe"
Response::Subscribed { ok: bool, subscription: String, cursor: u64 }
pub struct Subscription { /* holds the stream */ }
impl Subscription { pub fn next_frame(&mut self) -> Result<Option<serde_json::Value>>; }
pub fn subscribe(camp: &CampDir, session: &str, cursor: Option<u64>) -> Result<Subscription>;
```

- [ ] **Step 1: Write the failing tests.** The four §8 obligations, one test each.

`socket.rs` wire pin:
```rust
    /// cp-1 §4.4/§9: subscribe's request and its HELLO frame. The cursor is a
    /// BYTE OFFSET into the stream file (§9) — durable across a campd restart
    /// for free.
    #[test]
    fn subscribe_wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Request::SessionSubscribe {
                session: "camp/dev/1".into(),
                cursor: Some(4096),
            })
            .unwrap(),
            r#"{"op":"session.subscribe","session":"camp/dev/1","cursor":4096}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::SessionSubscribe {
                session: "camp/dev/1".into(),
                cursor: None,
            })
            .unwrap(),
            r#"{"op":"session.subscribe","session":"camp/dev/1","cursor":null}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Subscribed {
                ok: true,
                subscription: "sub-1".into(),
                cursor: 4096,
            })
            .unwrap(),
            r#"{"ok":true,"subscription":"sub-1","cursor":4096}"#
        );
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true,"subscription":"s","cursor":0}"#)
                .unwrap(),
            Response::Subscribed { .. }
        ));
    }
```
`crates/camp/tests/control.rs` — the four §8 tests:
```rust
/// §4.4 EXIT CRITERION: a WEDGED campd fails a subscribe FAST, at the hello.
/// The bare bound listener IS the wedge simulator (socket.rs:751-754): its
/// kernel backlog accepts the connect, but nothing ever reads or answers —
/// exactly a daemon stuck mid-syscall. `camp watch` against it must fail
/// loudly within REQUEST_TIMEOUT, exactly like every other verb; only AFTER
/// the hello is the connection timeout-exempt.
#[test]
fn a_wedged_campd_fails_the_subscribe_hello_fast_and_loudly() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // No campd — just a bound listener that accepts and never answers.
    let _wedged = UnixListener::bind(root.join("campd.sock")).unwrap();
    let start = Instant::now();
    let err = camp_subscribe_err(&root, "t/dev/1");
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "bounded, never a hang: took {:?}", start.elapsed()
    );
    assert!(err.contains("kill -9"), "the wedge remedy, unchanged: {err}");
}

/// §4.4 + §8 BACKPRESSURE: a subscriber that stops reading is DROPPED at
/// `subscriber_buffer_bytes`, loudly, with the `subscriber.dropped` event
/// naming the session and the high-water mark — and campd NEVER BLOCKS on it
/// (its loop is single-threaded; a stalled subscriber campd waits on is a
/// daemon-wide wedge — the bug class of issue #55).
///
/// The proof that campd never blocked: AFTER the drop, a plain `status`
/// request on a SECOND connection is answered promptly. A campd blocked on
/// the dead subscriber could not answer it.
#[test]
fn a_subscriber_that_stops_reading_is_dropped_loudly_and_campd_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    // A tiny cap so a chatty fake worker overruns it in milliseconds.
    let campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CONTROL_LOOP", "1"),
            ("CAMP_SUBSCRIBER_BUFFER_BYTES", "512"),
            ("FAKE_AGENT_STREAM_SPAM", "400"), // 400 stream lines, see Step 4
        ],
    );
    camp_ok(&root, &["sling", "do the thing --json"]);
    wait_until(&root, "the worker to wake", |events| {
        events.iter().any(|e| e["kind"] == "session.woke")
    });
    let session = live_session_name(&root);

    // Subscribe, then NEVER read a frame.
    let mut sub = connect(&root);
    let hello = request(
        &mut sub,
        &serde_json::json!({"op":"session.subscribe","session":session,"cursor":0}).to_string(),
    );
    assert_eq!(hello["ok"], true, "the hello: {hello}");
    assert!(hello["subscription"].is_string(), "{hello}");

    // The worker spams stdout; campd fans it out; our buffer overruns.
    wait_until(&root, "subscriber.dropped", |events| {
        events.iter().any(|e| {
            e["kind"] == "subscriber.dropped"
                && e["data"]["session"] == session
                && e["data"]["cap_bytes"] == 512
                && e["data"]["buffered_bytes"].as_u64().unwrap_or(0) > 512
        })
    });

    // campd NEVER BLOCKED: a fresh connection is served promptly.
    let mut other = connect(&root);
    let start = Instant::now();
    let status = request(&mut other, r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true, "campd must still serve: {status}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "campd blocked on the dead subscriber: took {:?}", start.elapsed()
    );
    drop(sub);
    drop(campd);
}

/// §9: a cursor into a REAPED (disposed) stream is an EXPLICIT ERROR, never a
/// silently truncated stream. Same for a cursor past EOF.
#[test]
fn a_cursor_into_a_reaped_stream_is_an_explicit_error() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[]);
    let mut stream = connect(&root);
    // The session never existed => it is not tailed => the stream is gone.
    let response = request(
        &mut stream,
        &serde_json::json!({"op":"session.subscribe","session":"t/dev/404","cursor":0}).to_string(),
    );
    assert_eq!(response["ok"], false, "{response}");
    let error = response["error"].as_str().unwrap();
    assert!(
        error.contains("not tailed") || error.contains("disposed"),
        "the error must name the cause: {error}"
    );
    drop(campd);
}

/// §4.1/§9: a LATE joiner gets HISTORY from its cursor, then FOLLOWS. The
/// hello's `cursor` is where the stream will start; every complete line from
/// there onward arrives as a frame, in file order.
#[test]
fn a_subscriber_gets_history_from_its_cursor_then_follows_live() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    camp_ok(&root, &["sling", "do the thing --json"]);
    wait_until(&root, "the worker to wake", |events| {
        events.iter().any(|e| e["kind"] == "session.woke")
    });
    let session = live_session_name(&root);
    // The worker has already emitted its system/init line before we subscribe.
    let mut sub = subscribe_from_zero(&root, &session); // helper: real socket, cursor 0
    let first = next_frame(&mut sub).expect("history frame");
    assert_eq!(first["session"], session);
    assert_eq!(first["event"]["type"], "system", "the history line: {first}");
    assert!(first["offset"].as_u64().unwrap() > 0, "the resume cursor: {first}");
    // Now drive a LIVE line: an interrupt makes the worker emit a
    // control_response, which must arrive as the next frame.
    let mut ctl = connect(&root);
    request(
        &mut ctl,
        &serde_json::json!({"op":"session.interrupt","session":session}).to_string(),
    );
    let live = next_frame(&mut sub).expect("a live frame follows the history");
    assert_eq!(live["event"]["type"], "control_response", "live: {live}");
    drop(campd);
}
```
Helpers `camp_subscribe_err`, `subscribe_from_zero`, `next_frame`, `live_session_name` go at the top of `control.rs`; each is a thin wrapper on `socket::subscribe` / the raw socket — write them as the tests need them, using the existing harness (`connect`, `request`).

- [ ] **Step 2: Run and watch all four fail.**

Run: `cargo test -p camp --test control 2>&1 | tail -20`
Expected: FAIL — no `session.subscribe` op (the daemon answers `{"ok":false,"error":"bad request: …"}`).

- [ ] **Step 3: Implement the subscriber registry in `control.rs`.**

```rust
/// §4.4: the per-connection outbound cap. A subscription is otherwise campd's
/// ONLY unbounded allocation — `MAX_REQUEST_BYTES` (event_loop.rs:58, 64 KiB)
/// is its inbound sibling, and "bounded without a number is how #55 happened".
pub const SUBSCRIBER_BUFFER_BYTES_DEFAULT: usize = 1024 * 1024;

/// The cp-0 `max_stream_bytes_from_env` twin (read_channel.rs:34): a test-only
/// override so the backpressure test overruns a 512-byte cap instead of
/// writing a megabyte. Fail fast — a malformed override is an error, never a
/// silent default. A camp.toml field is deferred to a phase that owns config.rs.
pub fn subscriber_buffer_bytes_from_env(default: usize) -> Result<usize> {
    match std::env::var("CAMP_SUBSCRIBER_BUFFER_BYTES") {
        Ok(raw) => {
            let n: usize = raw
                .parse()
                .with_context(|| format!("CAMP_SUBSCRIBER_BUFFER_BYTES={raw:?} is not a usize"))?;
            if n == 0 {
                anyhow::bail!("CAMP_SUBSCRIBER_BUFFER_BYTES must be > 0");
            }
            Ok(n)
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(v)) => {
            anyhow::bail!("CAMP_SUBSCRIBER_BUFFER_BYTES={v:?} is not valid UTF-8")
        }
    }
}

struct Subscriber {
    id: String,
    session: String,
    /// Bytes queued for this connection's socket. NEVER unbounded: crossing
    /// `subscriber_buffer_bytes` drops the subscriber (§4.4).
    out: Vec<u8>,
    high_water: usize,
}

/// One frame on the wire (pinned by `subscribe_frame_is_pinned`):
/// {"session":"…","offset":<byte offset PAST this line>,"event":<the raw
/// stream-json object, verbatim>}. `offset` is the cursor a reconnecting
/// client resumes from (§9) — which is why it is per-frame and not per-hello.
fn frame(session: &str, offset: u64, raw_line: &str) -> Option<Vec<u8>> {
    let event: serde_json::Value = serde_json::from_str(raw_line).ok()?;
    let mut line = serde_json::json!({
        "session": session, "offset": offset, "event": event,
    })
    .to_string();
    line.push('\n');
    Some(line.into_bytes())
}
```
Add `subscribers: HashMap<Token, Subscriber>` and `next_subscription: u64` to `ControlRuntime` (+ `new()`), then:

```rust
impl ControlRuntime {
    /// §4.4 the HELLO: answered within the one-shot REQUEST_TIMEOUT, like every
    /// other verb — so `camp watch` against a WEDGED campd fails fast. Only
    /// AFTER this does the connection become timeout-exempt (a quiet stream is
    /// not a wedged daemon).
    ///
    /// §9: the cursor is a byte offset into the stream file. Three explicit
    /// errors, none of them a silent truncation:
    ///   - the session is not tailed (reaped/disposed, or never existed)
    ///   - the cursor is past the tail campd has consumed
    ///   - the history from the cursor exceeds `subscriber_buffer_bytes`
    pub fn serve_subscribe(
        &mut self,
        token: mio::Token,
        session: &str,
        cursor: Option<u64>,
        read_channel: &crate::daemon::read_channel::ReadChannelRuntime,
    ) -> Response {
        let Some((path, tail)) = read_channel.tail_state(session) else {
            return Response::Error {
                ok: false,
                error: format!(
                    "session {session:?} is not tailed — it was never a campd-spawned worker, \
                     or it ended and its stream file was disposed at reap. A cursor into a \
                     disposed stream is an explicit error, never a silently truncated stream \
                     (control-plane spec §9); the session's transcript is the durable \
                     scrollback"
                ),
            };
        };
        let from = cursor.unwrap_or(tail);
        if from > tail {
            return Response::Error {
                ok: false,
                error: format!(
                    "cursor {from} is past the {tail} bytes campd has consumed of {session:?}'s \
                     stream — never a silent seek to EOF (control-plane spec §9)"
                ),
            };
        }
        let history = match read_history(&path, from, tail) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Response::Error {
                    ok: false,
                    error: format!("reading {session:?}'s stream history from {from}: {e}"),
                };
            }
        };
        if history.len() > self.subscriber_buffer_bytes {
            return Response::Error {
                ok: false,
                error: format!(
                    "the history from cursor {from} is {} bytes, over the \
                     subscriber_buffer_bytes cap of {} — subscribe from a later cursor. camp \
                     never truncates a stream silently (control-plane spec §4.4/§9)",
                    history.len(),
                    self.subscriber_buffer_bytes
                ),
            };
        }
        self.next_subscription += 1;
        let id = format!("sub-{}", self.next_subscription);
        self.subscribers.insert(
            token,
            Subscriber {
                id: id.clone(),
                session: session.to_owned(),
                out: history,
                high_water: 0,
            },
        );
        Response::Subscribed {
            ok: true,
            subscription: id,
            cursor: from,
        }
    }

    /// §4.4: push this wake's lines to every subscriber of their session, then
    /// flush what each socket will take RIGHT NOW. A subscriber whose buffer
    /// crosses the cap is DROPPED, loudly, with a `subscriber.dropped` event —
    /// it is never blocked on, and its events are never silently discarded.
    /// campd's loop is single-threaded: a stalled subscriber campd waits on is
    /// a daemon-wide wedge (the bug class of issue #55).
    ///
    /// Returns (tokens the caller must deregister + drop, durable events).
    pub fn fanout(
        &mut self,
        lines: &[StreamLine],
        conns: &mut std::collections::HashMap<mio::Token, super::event_loop::Conn>,
    ) -> (Vec<mio::Token>, Vec<EventInput>) {
        let mut dropped = Vec::new();
        let mut events = Vec::new();
        for (token, sub) in self.subscribers.iter_mut() {
            for sl in lines.iter().filter(|sl| sl.session == sub.session) {
                if let Some(bytes) = frame(&sl.session, sl.offset_after, &sl.line) {
                    sub.out.extend_from_slice(&bytes);
                }
            }
            // Flush what the socket takes without blocking. A partial write
            // leaves the remainder buffered; WouldBlock leaves it all buffered.
            if let Some(conn) = conns.get_mut(token) {
                flush(&mut conn.stream, &mut sub.out);
            }
            sub.high_water = sub.high_water.max(sub.out.len());
            if sub.out.len() > self.subscriber_buffer_bytes {
                events.push(EventInput {
                    kind: EventType::SubscriberDropped,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "session": sub.session,
                        "subscription": sub.id,
                        "buffered_bytes": sub.out.len() as u64,
                        "cap_bytes": self.subscriber_buffer_bytes as u64,
                    }),
                });
                dropped.push(*token);
            }
        }
        for token in &dropped {
            self.subscribers.remove(token);
        }
        (dropped, events)
    }

    /// A connection went away (EOF, error, or a drop): forget its subscription.
    pub fn forget(&mut self, token: mio::Token) {
        self.subscribers.remove(&token);
    }

    /// A WRITABLE readiness on a subscriber's connection: flush what it takes.
    /// Returns true when this connection is a subscriber (so the event loop
    /// does not also try to read a request from it).
    pub fn flush_subscriber(
        &mut self,
        token: mio::Token,
        conn: &mut super::event_loop::Conn,
    ) -> bool {
        match self.subscribers.get_mut(&token) {
            Some(sub) => {
                flush(&mut conn.stream, &mut sub.out);
                true
            }
            None => false,
        }
    }

    /// Test/`sessions.list` observable.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }
}

/// Write as much of `out` as the socket takes RIGHT NOW; keep the rest
/// buffered. NEVER blocks (the connection is a non-blocking mio stream) —
/// that is the whole §4.4 backpressure contract. A write error leaves the
/// bytes buffered and the connection is dropped by the caller's normal error
/// path on its next event.
fn flush(stream: &mut mio::net::UnixStream, out: &mut Vec<u8>) {
    use std::io::Write as _;
    while !out.is_empty() {
        match stream.write(out) {
            Ok(0) => return,
            Ok(n) => {
                out.drain(..n);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return, // the peer is gone; the cap or the read path drops it
        }
    }
}

/// Read `[from, to)` of the stream file — the history a late joiner gets
/// before it follows (§4.1: "from a cursor, so a late joiner gets history,
/// then follows"). Complete lines only: `to` is campd's tail offset, which is
/// always past a complete line (cp-0 never advances the offset mid-line).
fn read_history(path: &std::path::Path, from: u64, to: u64) -> Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    if to <= from {
        return Ok(Vec::new());
    }
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(from))?;
    let mut raw = vec![0u8; (to - from) as usize];
    file.read_exact(&mut raw)?;
    // Re-frame each complete line so history and live frames are identical on
    // the wire (one shape, §4.4 — a client must not have to tell them apart).
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut out = Vec::new();
    let mut offset = from;
    for line in text.split_inclusive('\n') {
        offset += line.len() as u64;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.trim().is_empty() {
            continue;
        }
        if let Some(bytes) = frame_for_history(trimmed, offset) {
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}
```
(`frame_for_history` needs the session name — pass it in; fold it into `read_history(path, from, to, session)` and call `frame(session, offset, trimmed)`. Fix the signature accordingly; the plan's intent is one framing function, used by both paths.)

- [ ] **Step 4: Wire the event loop and add the client API.**

`event_loop.rs` (**shared — additive**):
- `struct Conn` becomes `pub(super) struct Conn { pub(super) stream: UnixStream, pub(super) buf: Vec<u8> }` (control.rs needs the stream to flush into).
- The accept arm registers `Interest::READABLE | Interest::WRITABLE` (a subscriber must get WRITABLE readiness; a one-shot request connection simply never has anything buffered, so the extra interest costs nothing and wakes nothing — the socket is writable immediately and mio is EDGE-triggered, so it fires once at registration and never again unless the buffer fills and drains).
- The `token =>` arm, FIRST line: `if control.flush_subscriber(token, &mut conn) { conns.insert(token, conn); continue; }` — a subscriber connection is never read as a request stream. (Place it after the `conns.remove` that already happens at event_loop.rs:349.)
- The new `drain_lines` arm:
```rust
            Ok(Request::SessionSubscribe { session, cursor }) => {
                // §4.4: the hello is a NORMAL one-shot response — bounded by
                // REQUEST_TIMEOUT like every other verb. Only after it does the
                // connection become a subscriber (and timeout-exempt).
                let response = control.serve_subscribe(token, &session, cursor, read_channel);
                respond(&mut conn.stream, &response)?;
            }
```
(`drain_lines` needs the connection's `Token` — thread it in as a parameter from `serve_connection`, which gets it from the `token =>` arm. One extra argument on two private functions.)
- The fanout, in the post-drain block right after `control.ingest`:
```rust
        // §4.4: fan the drained lines out to the subscribers, flush what the
        // sockets take, and DROP (loudly) any whose buffer crossed the cap.
        // campd never blocks on a slow subscriber.
        let (dropped, drop_events) = control.fanout(&stream_lines, &mut conns);
        for input in drop_events {
            ledger.append(input)?;
            appended_read_channel_events = true;
        }
        for token in dropped {
            if let Some(mut conn) = conns.remove(&token) {
                let _ = poll.registry().deregister(&mut conn.stream);
            }
        }
```
- Wherever a connection is dropped (`ConnState::Closed`, the error arm), call `control.forget(token);` — one line each, so a closed subscriber's buffer is freed.

`socket.rs` — the client API (used by the tests now and by phase 2's `camp watch`):
```rust
/// §4.4: a live subscription. The HELLO is bounded by `REQUEST_TIMEOUT` (a
/// wedged campd fails fast, exactly like every other verb); AFTER the hello
/// the read deadline is CLEARED — a quiet stream is not a wedged daemon.
pub struct Subscription {
    reader: BufReader<UnixStream>,
    pub subscription: String,
    pub cursor: u64,
}

impl Subscription {
    /// The next frame, or None at EOF (campd dropped us — see the
    /// `subscriber.dropped` event in the ledger for why).
    pub fn next_frame(&mut self) -> Result<Option<serde_json::Value>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(line.trim_end())?))
    }
}

/// Open a subscription. A campd that is not running is the loud
/// `CampdNotRunning` error; a WEDGED one is `CampdUnresponsive` — the hello
/// is judged on the SAME connection that carries the stream (the PR #51
/// finding-1 law: no separate probe).
pub fn subscribe(camp: &CampDir, session: &str, cursor: Option<u64>) -> Result<Subscription> {
    let path = camp.socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) => {
            return Err(anyhow::Error::new(CampdNotRunning {
                camp_root: camp.root.clone(),
                socket: path,
                log: camp.log_path(),
                last_campd: last_campd(camp),
            }));
        }
        Err(e) => return Err(e).with_context(|| format!("connecting to campd at {}", path.display())),
    };
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(&Request::SessionSubscribe {
        session: session.to_owned(),
        cursor,
    })?;
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| mark_wedge(camp, e.into()))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut hello = String::new();
    // The HELLO, within REQUEST_TIMEOUT: a wedged campd fails HERE, loudly.
    reader
        .read_line(&mut hello)
        .map_err(|e| mark_wedge(camp, e.into()))?;
    if hello.is_empty() {
        return Err(CampdWentAway.into());
    }
    match serde_json::from_str::<Response>(hello.trim_end())? {
        Response::Subscribed { subscription, cursor, .. } => {
            // §4.4: timeout-exempt from here on — a quiet stream is not a
            // wedged daemon.
            stream.set_read_timeout(None)?;
            Ok(Subscription { reader, subscription, cursor })
        }
        Response::Error { error, .. } => bail!("campd: {error}"),
        other => bail!("unexpected response to session.subscribe: {other:?}"),
    }
}
```
**`FAKE_AGENT_STREAM_SPAM`** — add to `fake-agent.sh` next to the control loop (the backpressure test needs a chatty worker):
```bash
#   FAKE_AGENT_STREAM_SPAM  cp-1: emit N stream-json lines on stdout before
#                           anything else — the chatty worker a slow subscriber
#                           cannot keep up with (the §4.4 backpressure gate).
if [[ -n "${FAKE_AGENT_STREAM_SPAM:-}" ]]; then
  i=0
  while [ "$i" -lt "$FAKE_AGENT_STREAM_SPAM" ]; do
    emit_stream "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"line $i\"}}"
    i=$((i + 1))
  done
fi
```
(place it immediately after the `system/init` emit).

- [ ] **Step 5: Run the four tests.**

Run: `cargo test -p camp --test control 2>&1 | tail -30`
Expected: PASS — 7 tests total in the file.

- [ ] **Step 6: Full suite + commit.**
```bash
cargo test --workspace 2>&1 | tail -20
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A
git commit -m "feat(control): session.subscribe — a bounded, drop-loudly connection mode (cp-1 §4.4/§9)"
```

---

## Task 9: the §4.3 perf gate grows N idle subscribers

Spec: §4.3 (*"Obligation: extend the `make perf` idle gate to hold M quiescent workers with tailed stdout files and N connected subscribers (fake workers, held open, no output), and assert the same 0.0% CPU / <20 MB RSS numbers. Then §4.3 is a measured property, not an argument."*).

cp-0 built the M-quiescent-workers half and its plan-gate DEFERRED the N-subscribers half to *"the phase that builds subscribe"* — which is this one. Leaving it out is the one gap a §4.3 reader would find.

**Files:**
- Modify: `crates/camp/tests/perf_daemon.rs` (the existing `idle_campd_cpu_delta_zero_and_rss_under_20mb`-family gate cp-0 extended)

- [ ] **Step 1: Extend the existing idle gate.** Find cp-0's M-workers idle gate in `perf_daemon.rs` and add N connected subscribers to the SAME scaffold (do not write a second gate — one measured property, one test):

```rust
    // cp-1 §4.3: N CONNECTED SUBSCRIBERS, held open, reading nothing, with
    // nothing to read. A subscription must cost ZERO wakeups when its session
    // is quiet — campd sleeps on the read-channel self-pipe, and a quiet
    // worker writes nothing, so no notify event fires and no fanout runs. This
    // is the property herdr could not offer (its events.wait is a 100ms sleep
    // loop with no fd to block on).
    const IDLE_SUBSCRIBERS: usize = 4;
    let mut subs = Vec::new();
    for _ in 0..IDLE_SUBSCRIBERS {
        subs.push(
            camp::daemon::socket::subscribe(&camp, &session, Some(0))
                .expect("the hello must land"),
        );
    }
    // ... the existing idle window + the existing 0.0% CPU / <20 MB RSS
    // assertions, unchanged. `subs` is held for the whole window.
    drop(subs);
```
(`perf_daemon.rs` is an integration test and cannot reach `daemon::socket` — the `camp` crate is a BINARY. Follow whatever the existing gate does to talk to campd: it uses the raw `UnixStream` harness. Do the same — send the `session.subscribe` line on N raw connections and hold them open, reading nothing.)

- [ ] **Step 2: Run the perf gate (LOCAL-ONLY, per AGENTS.md).**

Run: `make perf 2>&1 | tail -30`
Expected: PASS — 0.0% CPU delta, < 20 MB RSS, with M tailed workers AND N idle subscribers.
If it FAILS on CPU: something in the subscriber path is waking campd with nothing to do — that is a REAL invariant-1 bug and must be fixed, not accommodated.

- [ ] **Step 3: Commit.**
```bash
git add crates/camp/tests/perf_daemon.rs
git commit -m "test(perf): the idle gate now holds N connected subscribers (cp-1 §4.3)"
```

---

## Task 10: the $0 real-claude gate sends camp's OWN bytes

Spec: §2.1 (*"The wire shapes are pinned by tests against recorded fixtures"*), §8 (*"Without the real layer, §2.1's mitigations are theatre: fixtures pin what camp SENDS and PARSES, never what the CLI ACCEPTS and EMITS"*).

Today `claude_compat.rs:388` sends a HAND-WRITTEN interrupt string to the real CLI. cp-1 has a constructor for that message — so the gate must send what **camp actually produces**, or the two can drift and the gate proves nothing about camp.

**Files:**
- Modify: `crates/camp/tests/claude_compat.rs`

- [ ] **Step 1: Make the gate read the fixture — the same file `control.rs`'s unit test pins its constructor against.** `camp` is a binary crate, so an integration test cannot call `ParentMessage::to_line` directly (the same constraint `gate_core_flags_match_build_spec_held_stream_arm` already works around at claude_compat.rs:132). The fixture is the shared truth: Task 1's unit test asserts `ParentMessage::Interrupt{id}.to_line() == interrupt_request.json`, and this gate sends `interrupt_request.json` to the real CLI. Transitively, **the bytes camp produces are the bytes the real CLI accepts** — which is the entire §2.1 mitigation.

Replace the hand-written literal at claude_compat.rs:387-390:
```rust
/// cp-1: the SHARED fixture — the exact bytes `control::ParentMessage::to_line`
/// produces (pinned by `parent_messages_serialize_to_the_recorded_fixtures` in
/// control.rs). Sending the literal string instead would let camp's constructor
/// drift from what this gate proves the real CLI accepts, and §2.1's mitigation
/// would be theatre.
const INTERRUPT_FIXTURE: &str = include_str!("fixtures/control/interrupt_request.json");

/// The fixture with its request_id swapped for `id` — the one templated field.
fn interrupt_line(id: &str) -> String {
    INTERRUPT_FIXTURE
        .trim_end()
        .replace("camp-fixture-1", id)
}
```
and in `claude_compat_zero_cost`:
```rust
        // pre-turn interrupt — camp's OWN wire bytes (the shared fixture).
        send(&mut worker.child, &interrupt_line("camp-compat-interrupt"));
        await_success(&rx, "camp-compat-interrupt");
        eprintln!("[compat] pre-turn interrupt acknowledged (camp's own bytes)");
```
Add a NON-ignored unit test in the same file (it runs in CI — no `claude` needed):
```rust
/// The fixture really is an interrupt control_request, and templating its
/// request_id yields valid JSON. A CI-runnable guard on the bytes the $0 gate
/// sends.
#[test]
fn the_interrupt_fixture_is_a_well_formed_control_request() {
    let line = interrupt_line("camp-compat-interrupt");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["type"], "control_request");
    assert_eq!(v["request"]["subtype"], "interrupt");
    assert_eq!(v["request_id"], "camp-compat-interrupt");
    assert!(!line.contains("camp-fixture-1"), "the id must be templated out");
}
```

- [ ] **Step 2: Run the CI-runnable half.**

Run: `cargo test -p camp --test claude_compat 2>&1 | tail -20`
Expected: PASS — the non-ignored tests (the $0 gate itself stays `#[ignore]`d).

- [ ] **Step 3: Run the $0 gate LOCALLY if a pinned `claude` is installed** (it costs $0 — no turn is sent). If the installed version does not match `ci/claude-compat/CLAUDE_VERSION`, the gate fails loudly by design: **do NOT widen the pin.** Report the mismatch to the lead instead.

Run: `make compat 2>&1 | tail -30`
Expected: PASS, printing `[compat] pre-turn interrupt acknowledged (camp's own bytes)`. If no pinned `claude` is available, SAY SO in the PR description — do not claim the gate ran.

- [ ] **Step 4: Commit.**
```bash
git add crates/camp/tests/claude_compat.rs
git commit -m "test(compat): the \$0 gate sends camp's own interrupt bytes, not a hand-written literal (cp-1 §8)"
```

---

## Task 11: the gates, the PR, and the honest description

- [ ] **Step 1: Rebase onto main** (the lead will have merged compat-2 or other work; `event_loop.rs` and `dispatch.rs` are shared).
```bash
git fetch origin && git rebase origin/main
```
Resolve conflicts in `event_loop.rs`/`dispatch.rs` by KEEPING BOTH sides — every cp-1 touch there is additive by construction (new match arms, one extra `min_deadline` nesting, one extra parameter, one new method). If a conflict cannot be resolved additively, STOP and ask the lead.

- [ ] **Step 2: The three gates, in order, all green.**
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: all three PASS. Do not proceed on a single failure.

- [ ] **Step 3: The perf gate (local-only, perf-relevant PR — the subscriber registry is new standing state in campd).**
```bash
make perf 2>&1 | tail -20
```

- [ ] **Step 4: Push and open the PR.**
```bash
git push -u origin cp-1-control-protocol
gh pr create --title "control: the wire-format module and the first socket verbs (cp-1)" --body "$(cat <<'EOF'
Control-plane spec §2, §4.1, §4.4, §8, §9. Phase 1 of the control plane.

## What lands

- **`crates/camp/src/daemon/control.rs`** — THE module that owns the `claude` control wire format (§2.1). Nothing else in camp constructs or parses a control message. Shapes pinned against recorded fixtures in `crates/camp/tests/fixtures/control/`, the SAME files the $0 real-claude gate exchanges with the pinned CLI.
- **Verbs (§4.1):** `sessions.list`, `session.send_turn` (camp nudge, promoted — `Request::Nudge` is gone), `session.interrupt`, `session.subscribe`.
- **`interrupt` end to end:** campd writes the `control_request` into the worker's held stdin; the worker answers on stdout; cp-0's read channel drains it; the request_id is correlated; `control.responded` lands in the ledger. ACK-then-async by design (campd's loop is single-threaded — blocking a handler on a filesystem-latency line is issue #55's wedge class).
- **`subscribe` as a connection MODE (§4.4):** per-connection output buffering, a 1 MiB `subscriber_buffer_bytes` cap, drop-loudly with `subscriber.dropped`, a hello bounded by `REQUEST_TIMEOUT` (so a wedged campd fails fast), timeout-exempt thereafter.
- **Four new camp-specific events:** `session.interrupted`, `control.responded`, `control.failed`, `subscriber.dropped` — each with a `deny_unknown_fields` payload validated on the fold.
- **Loud, never silent (§2.1):** a control response that never arrives is a `control.failed` at `CONTROL_RESPONSE_TIMEOUT` (an ARMED deadline in `min_deadline`, never a tick); an unrecognized control message is a `control.failed`; a `can_use_tool` — which cannot happen in cp-1 (§5.3.1 defers the stdio flag to phase 3) — is a `control.failed` rather than a silent drop.

## Exit criteria, and how each is proven

| Criterion | Proof |
|---|---|
| `interrupt` works end to end against a fake worker over the real socket | `tests/control.rs::interrupt_round_trips_through_the_read_channel` — real campd, real socket, real fake worker, asserting `session.interrupted` then `control.responded` with the same request_id |
| `send_turn` works end to end | `tests/control.rs::send_turn_delivers_a_user_turn_into_the_held_pipe` — the worker's blocked `read` unblocks and it closes its bead |
| a wedged-campd subscribe fails fast at the hello | `tests/control.rs::a_wedged_campd_fails_the_subscribe_hello_fast_and_loudly` — a bare bound listener, bounded by `REQUEST_TIMEOUT`, naming `kill -9` |
| fixtures pin every message shape camp sends or parses | `daemon::control::tests::{parent_messages_serialize_to_the_recorded_fixtures, worker_messages_parse_from_the_recorded_fixtures}` + `claude_compat.rs` sends the same fixture bytes to the real CLI |
| backpressure (§8) | `tests/control.rs::a_subscriber_that_stops_reading_is_dropped_loudly_and_campd_keeps_serving` — and campd answers a `status` on another connection right after, proving it never blocked |
| invariant 1 under subscribers (§4.3) | `make perf` idle gate: M tailed workers + N connected subscribers, 0.0% CPU, < 20 MB RSS |

## Deliberately deferred (named)

`--permission-prompt-tool stdio`, `permission.pending`/BLOCKED/the stall-timer disarm/the adoption kill, and the `initialize` handshake → **phase 3** (§5.3, §5.3.1, §9: the handshake's stated purpose is redelivering pending permission requests, which cannot exist before the stdio flag does). `--include-partial-messages` → **phase 4** (§2.2). `fleet.subscribe`, `session.permission_decision`, `set_model`/`set_permission_mode` → later phases (§4.1). `camp watch`/`camp attach` → phases 2/4. `subscriber_buffer_bytes` as a `camp.toml` field → a phase that owns `config.rs` (the cp-0 `max_stream_bytes` precedent).
EOF
)"
```

- [ ] **Step 5: Watch CI to green.**
```bash
gh pr checks --watch
```
Work is NOT complete until this is green. If a check fails, fix it — never re-run hoping for a different result.

- [ ] **Step 6: Report to the lead** — the plan doc path, the branch, the pushed commit SHA, the PR number, and whether `make perf` and `make compat` ran locally (and their results). Do not claim a gate ran that did not.

---

## Self-review against the spec

**Coverage of the phase block's contract:**

| Contract item | Task |
|---|---|
| §2 / §2.1 — one module owns the wire format; shapes pinned by recorded fixtures; failures loud | Task 1 (module + fixtures), Task 3 (the never-arrived response is a durable fault), Task 6 (`ingest`: unrecognized/unanswerable control messages are `control.failed`) |
| §4.1 `sessions.list` | Task 7 |
| §4.1 `session.subscribe` | Task 8 |
| §4.1 `session.send_turn` | Task 6 |
| §4.1 `session.interrupt` | Task 6 |
| §4.4 subscribe as a connection MODE — per-connection buffering, 1 MiB cap, drop-loudly with `subscriber.dropped`, hello within `REQUEST_TIMEOUT` then timeout-exempt | Task 8 (all four, one test each) |
| §8 fixture tests | Task 1 + Task 10 |
| §8 backpressure test | Task 8 |
| §4.3 perf obligation (N subscribers) | Task 9 |
| Exit: interrupt + send_turn end to end over the real socket vs a fake worker | Task 6 |
| Exit: a wedged-campd subscribe fails fast at the hello | Task 8 |
| Exit: fixtures pin every message shape camp sends or parses | Task 1 (interrupt, dialog refusal, user turn out; control_response, can_use_tool, request_user_dialog, stream in) + Task 10 |
| Exit: CI green | Task 11 |

**Anticipated plan-gate challenges, answered in advance:**

1. *"Why doesn't `interrupt` wait for the `control_response`?"* — D1. Single-threaded loop; §4.4 makes bounded-answer the law for every verb; issue #55's wedge class is exactly a handler that waits. The round trip IS proven, via the ledger, in the same test.
2. *"§5.3 says ledger-FIRST; this does deliver-first."* — D2. §5.3's ordering is scoped by its own stated rationale to permission DECISIONS (to make §5.3.4's adoption kill safe). Interrupt and send_turn follow the merged `Request::Nudge` precedent.
3. *"`sessions.list`'s `blocked` is hard-wired false — that is a fallback."* — No: §5.3.1 makes the `can_use_tool` flow structurally unreachable in cp-1 (camp never passes `--permission-prompt-tool stdio`). The field exists because §4.1's shape requires it; its producer is phase 3, and a `can_use_tool` that arrives anyway is a LOUD `control.failed`, not a quietly-flipped bit.
4. *"§9 says camp sends `initialize` anyway."* — §9's stated reason is `pending_permission_requests`, which cannot exist before phase 3's stdio flag. §9 itself settles that the handshake is *optional* for a parent that only wants interrupt. The $0 gate already proves the handshake round-trips against the real CLI. Deferred, named, cited.
5. *"An unknown stream `type` is silently passed through — that violates §2.1."* — D3. §2.1's strictness is written about *control messages*. The control family is strict; the stream surface is transparent, and must be, or every new `claude` stream type breaks camp.
6. *"`event_loop.rs` is shared and you touched it."* — Every touch is additive (new match arms, one `min_deadline` nesting, one parameter, `pub(super)` on `Conn`), and D4 DELETES more than it adds (the nudge body moves to `control.rs`). The rebase protocol is Task 11 Step 1.

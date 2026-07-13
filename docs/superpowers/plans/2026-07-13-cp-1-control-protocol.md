# cp-1: the control protocol — one module owns the wire, four verbs on the socket — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (this stream is planning-only; a FRESH implementer session executes after plan-gate APPROVE). Steps use checkbox (`- [ ]`) syntax for tracking. Branch: `cp-1-control-protocol`.

**Goal:** Give campd a control plane: ONE module that owns the undocumented `claude` control wire format (pinned by fixtures whose provenance is labelled), and the first four socket verbs — `sessions.list`, `session.send_turn`, `session.interrupt`, `session.subscribe` — with `interrupt`'s `control_response` round-tripping back over cp-0's read channel, and `subscribe` as a bounded, drop-loudly, streaming connection MODE.

**Architecture:** A new `crates/camp/src/daemon/control.rs` is the ONLY place in camp that constructs or parses a control message (spec §2.1). It holds `ControlRuntime`: the pending-request table (rebuilt from the ledger at startup, with a response deadline that joins `min_deadline`), the subscriber registry (per-connection output buffers with a HARD cap enforced at append time, plus a per-subscriber history cursor so a late joiner catches up progressively rather than by a slurped `Vec`), and every socket-verb handler body — so `event_loop.rs`'s new match arms are one-line delegations. campd writes control requests into the worker's already-held stdin pipe (the `nudge_via_stdin` bounded-write mold); the worker's `control_response` comes back as a line in its stdout file, which cp-0's `ReadChannelRuntime` tails to EOF on every wake — cp-1 adds a hand-off (`take_stream_lines`) so those lines reach `ControlRuntime`, **ingested on every path that can produce one, including the disposal-time final drain.**

**Tech Stack:** Rust (workspace edition), `mio`, `serde`/`serde_json`, `uuid`, `jiff`, `rusqlite` via `camp_core::ledger`, `tempfile` (dev-dep). **No new dependencies. No new cargo features** (notably NOT `serde_json/preserve_order` — see B1).

---

## What changed in this revision, and why (rev 2 — after plan-gate REJECT, 2026-07-13)

The first panel returned a unanimous BLOCK with 14 blocking defects. All five decisions D1–D5 were RATIFIED and are unchanged. **D6 was REJECTED and is replaced by D6′.** Every defect is answered below with the task that carries the fix. Three of them (B1, B14, B15) were settled by running experiments against the pinned `claude` 2.1.207 that is installed on this machine — results recorded inline, reproducible with the commands given.

| # | Defect | Fix, and where |
|---|---|---|
| **B1** | `serde_json::json!` sorts keys (serde_json 1.0.150 locked, no `preserve_order` ⇒ `Map` is a `BTreeMap`), so every byte-pinning `assert_eq!` in Task 1 was unreachable | **Task 1** — `ParentMessage` is now built from `#[derive(Serialize)]` **structs**, whose field DECLARATION order is preserved. Fixtures rewritten to match. `preserve_order` is explicitly NOT enabled (it would change every `Value` serialization in camp). A second, order-independent `Value`-equality assertion backs up the byte pin. |
| **B2** | `camp` is a binary-only crate ⇒ the `socket::subscribe` client API had no consumer ⇒ `dead_code` ⇒ the clippy gate fails, and Task 8/9's helpers were uncompilable | **RULED: option (c).** The client API is **deleted from cp-1.** Subscribe is driven from tests by a concrete raw-`UnixStream` helper (`SubClient`, specified in Task 8), the idiom every existing harness uses (`tests/read_channel.rs`). The typed `CampdUnresponsive`/`kill -9` mapping arrives with phase 2's `camp watch`, its first real client. The wedged-campd exit criterion is proven directly: the hello read is bounded by `REQUEST_TIMEOUT` and the test asserts it fails inside that bound rather than hanging. |
| **B3** | Every task's own clippy gate would fail on `dead_code` for items introduced before their consumer | **Global Constraints → "The dead_code discipline"** — an exhaustive per-item table: which annotation, which justification, **and at which task it is DELETED**. Task 11 greps to prove none of the temporary ones survived. |
| **B4** | **CRITICAL.** The disposal-time final drain's lines were never ingested ⇒ an ordinary "worker answers the interrupt and exits" **lost the `control_response`**, then manufactured two false `control.failed`s | **Task 6 Step 5** — the post-drain block runs a control step **twice**, through one shared helper: after `drain_all`, and again **after `apply_pending_unregisters`** — the other `drain_one` caller. Named test: `a_worker_that_answers_and_exits_immediately_still_yields_control_responded` (Task 6 Step 8). **The rev-1 claim "the disposal ordering is untouched" is WITHDRAWN — it was false.** |
| **B5** | `expire_pending` ran at the TOP of the wake ⇒ a response sitting unread (coalesced notify) was declared "never arrived" | **Task 6 Step 5** — `expire_pending` now runs in the post-drain block, **after both ingests**. cp-0's law (event_loop.rs:406) holds: correctness never depends on a delivered event. |
| **B6** | The pending table was in-memory only ⇒ a restart both manufactured a false fault AND silently forgot a genuinely unanswered request (the "swallowed timeout" §2.1 forbids) | **Task 3 + Task 6 Step 6** — `ControlRuntime::rehydrate(ledger, now)` rebuilds `pending` from the ledger (a `session.interrupted` with no matching `control.responded`/`control.failed` — the ledger-derived-suppression pattern merged in fix-83/#92), called at startup after `adopt`. Tests: `a_restart_across_an_in_flight_interrupt_neither_lies_nor_forgets` (unit) + `a_campd_restart_across_an_in_flight_interrupt_invents_no_fault` (integration, real kill -9). |
| **B7** | The subscriber short-circuit bypassed `serve_connection` — the ONLY place EOF is detected ⇒ a detached `camp watch` leaked an fd + a buffer forever, then was libeled with a `subscriber.dropped` backpressure event | **Task 8 Step 4** — the short-circuit is GONE. A subscriber token pumps first and then **falls through into `serve_connection` like any other connection**, so cp-0's existing `ReadStop::Eof ⇒ ConnState::Closed` detects the hangup; `control.forget(token)` runs on every close path. A normal detach appends **no event at all** (§5.2 "Detach freely" — the ledger records faults, not client lifecycle). Named test: `a_hung_up_subscriber_is_forgotten_and_is_never_libeled_as_backpressure`. |
| **B8** | The backpressure test could not pass at ANY cursor, and even with timing fixed the kernel socket buffer would absorb everything so `sub.out` never grew | **Task 8 Step 1/4** — new fake-agent mode `FAKE_AGENT_SPAM_ON_TURN` spams only **after** a `send_turn` arrives (the subscriber is registered by then), the subscriber joins at the **tail** (empty history ⇒ a clean hello), and the volume is **8000 lines ≈ 720 KB**, chosen to exceed *kernel socket buffer + app cap* on both platforms (macOS `net.local.stream.sendspace` ≈ 8 KiB; Linux ≈ 200 KiB). Cap: 512 B. Both halves are now deterministic. |
| **B9** | `read_history` allocated `to - from` bytes (up to `max_stream_bytes` = 256 MiB) and read synchronously ON the event loop, checking the cap only afterwards ⇒ a one-line request DoSes campd | **Task 8 Step 3** — history is never slurped (see B10/D6′). Reads are chunked at `HISTORY_CHUNK_BYTES` (64 KiB), refilled only as the socket drains. No unbounded allocation and no unbounded read anywhere on the loop. |
| **B10** | **D6 REJECTED.** `sub.out` was history+live in one `Vec` ⇒ the cap was SOFT (a whole wake's drain was appended before the cap was tested), and any join-from-zero on a session past 1 MiB was REFUSED — redefining §4.1's "a late joiner gets history, then follows" via an error message | **Task 8 Step 3 — D6′ replaces D6.** The `Subscriber` holds an open `File` + a `history_cursor` and streams history in bounded chunks, so **no history size is ever refused**. The cap is enforced **at append time** (a frame that would cross it drops the subscriber; the attempted size is the reported high-water). `high_water` is now READ — it is `buffered_bytes` in the `subscriber.dropped` payload (§4.4: "naming the session and the high-water mark"). §9's explicit-error rule is applied where it belongs: a **reaped/disposed** stream, and a cursor **past the tail**. |
| **B11** | The hello's buffered history was flushed by nothing (the WRITABLE edge was consumed at accept, before the subscription existed) ⇒ the history test would HANG | **Task 8 Step 3/4** — a single `pump()` (refill-from-history → flush → repeat until WouldBlock or exhausted) at **three** sites: right after the hello is written, on every WRITABLE readiness, and after every fanout. The hello-precedes-bytes invariant is stated: `respond()` uses `write_all` on a NON-BLOCKING stream (event_loop.rs:997), so the hello must be the first bytes and nothing may be buffered before it. |
| **B12** | Nothing told a subscriber its session had ended ⇒ `next_frame()` blocks forever, campd holds the entry for life — and the frame shape had no room for a terminal frame, so cp-2/cp-4 would inherit a wire needing a breaking change | **Task 8 Step 3 — decided in cp-1.** The frame is **tagged from birth**: `{"frame":"event",…}` / `{"frame":"end","reason":…}`. On disposal campd emits the `end` frame, pumps, and closes. Pinned by `subscribe_frame_shapes_are_pinned`. Named test: `a_subscriber_gets_an_end_frame_when_its_session_ends`. |
| **B13** | Two shapes on the exit criterion's own list were pinned by nothing: the **subscribe frame** (a dangling reference to a test that did not exist) and the **post-hello timeout exemption** | **Task 8 Step 1** — `subscribe_frame_shapes_are_pinned` (both frame types) now exists, and `a_subscription_survives_a_quiet_period_longer_than_request_timeout` (a 6 s quiet window > the 5 s `REQUEST_TIMEOUT`) proves the exemption. |
| **B14** | **Fixture provenance.** Invented bytes were labelled "RECORDED"; Task 10 closed the loop (camp pinning camp's bytes against camp's bytes); the `error` key was a guess; `can_use_tool`/`request_user_dialog` were invented and cp-3 would inherit them as "the pinned shape" | **Task 1 — every fixture is now provenance-labelled, and the shapes are DERIVED FROM THE SHIPPED CLI at the pinned version.** `sdk.mjs` is not vendored here, but something strictly better is on the machine: the **actual peer**, `~/.local/share/claude/versions/2.1.207`, whose bundle is `strings`-greppable. Extraction commands + the recovered source lines are in Task 1 Step 0; `tests/fixtures/control/PROVENANCE.md` records them per fixture. **Findings: the `error:<string>` key is CORRECT (verified); `request_user_dialog` was WRONG (real keys `dialog_kind`/`payload`/`tool_use_id`, not the invented `prompt`); `can_use_tool` was incomplete (also carries `display_name`, `tool_use_id`).** |
| **B15** | The `initialize` deferral shipped a configuration nothing had ever verified: every recorded ack in the repo is POST-initialize, and the fake acks anything (§8's named trap) | **RULED: option (b) — and it is now EMPIRICALLY SETTLED.** I ran the exact shipped configuration against the pinned CLI at $0 (command + verbatim output in Task 10 Step 1): a pre-turn interrupt with **no `initialize` ever sent** is **acked `subtype:"success"`**, exit 0. Task 10 adds that arm to the $0 gate (`no_initialize_pre_turn_interrupt_is_acked`) so it is a standing gate. The `subtype!=="initialize"` rejection in the binary belongs to the `[bridge:repl]` Remote-Control transport (*"This session is outbound-only…"*), not camp's stdio path. |

**Non-blocking notes — all adopted.** `MAX_SUBSCRIBERS` bounds the connection COUNT, not just the bytes (Task 8) · fault dedupe stops unbounded ledger write-amplification (Task 6) · D3 strictness re-keyed to `type.starts_with("control")` · the real dispatch scaffolds (`test_insert_held_cat`/`test_insert_held_sleeper`) with a torn-pipe test for `ControlWrite::Failed` (Task 5) · `refold_check()` not `refold()` (Task 2) · the borrow-checker hedging paragraph DELETED (it was a non-issue) · the test count and `mod.rs` placement corrected · `serve_sessions_list` no longer promises a third `state` value · `subscriber_buffer_bytes_from_env` is defined in the task that first needs it (Task 6) · D4 now says why `send_turn` keeps emitting `session.nudged` · the WRITABLE-interest cost is described precisely (an accept-time edge, not an idle cost — and that already-consumed edge is exactly why B11 broke) · the "every touch is additive" oversell is **withdrawn** and replaced with an itemized list of the four non-additive `event_loop.rs` touches.

---

## Global Constraints

- **TDD, strictly.** Write the failing test, RUN it, watch it fail, implement, RUN it, watch it pass.
- **Never commit to main.** All work on `cp-1-control-protocol`; one reviewable PR.
- **Gates green at EVERY commit:** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`.
- **No panics in library code** (clippy `unwrap_used`/`expect_used`/`panic` denied; `unsafe_code` forbidden). Test modules opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- **Invariant 1 (idle is free).** No ticks. The response deadline is an ARMED timer in `min_deadline`; an idle subscriber costs zero wakeups.
- **Invariant 3 (nothing hidden).** Every campd action is an event with its cause — and an event must name its TRUE cause (B6's whole point).
- **Invariant 5 (fail fast).** §2.1: *"An unrecognized control message, or a control response that never arrives, is an evented, operator-visible fault — never a swallowed timeout."*
- **Extend, don't rework.** cp-0's read channel is the transport; fix-86's `--verbose` argv makes worker stdout parseable. **But "extend" is not a licence to leave a second caller of a function whose side effects you changed unaccounted for — that was B4.**
- **New events use `deny_unknown_fields` payload structs**, keep the one-transaction event+state property, satisfy the vocab-pin partition tests, keep the refold property test green.
- **No new cargo features.** Specifically NOT `serde_json/preserve_order` (B1).
- **No test may spawn a real `claude` or spend API money**, except the `#[ignore]`d, `CAMP_COMPAT=1`-gated $0 tier (Task 10), which sends no turn.
- **Spec and code never silently diverge.** If reality contradicts `docs/superpowers/specs/2026-07-12-camp-control-plane-design.md`, STOP and escalate — do not edit the spec without operator sign-off.
- **No co-author lines in commits. Never mention the assistant in a commit message.**

### The `dead_code` discipline (B3)

`camp` is a **binary-only** crate: reachability is computed from `main`, and `pub` does NOT exempt an item. Every item introduced before its consumer trips `-D warnings`. The precedent is in the tree (`read_channel.rs:445,451,682`; `dispatch.rs:2193`; `fold.rs:541`). **Every annotation is temporary except where marked PERMANENT; Task 11 greps to prove the temporary ones are gone.**

| Item | Added | Annotation | Deleted at |
|---|---|---|---|
| `control.rs` items: `ParentMessage`, `WorkerMessage`, `parse_worker_line`, `ControlWireError`, `new_request_id`, `REQUEST_ID_PREFIX`, `ControlRuntime` + `track_pending`/`poll_timeout`/`expire_pending`/`resolve`/`rehydrate` | 1, 3 | ONE module-level `#![allow(dead_code)] // cp-1: wired in Task 6 — DELETE this attribute there` | **Task 6 Step 7** (delete it; clippy must pass without it) |
| `read_channel::StreamLine`, `take_stream_lines`, `last_activity`, `tail_state`, `take_disposed` | 4 | per-item `#[allow(dead_code)] // cp-1: consumed in Task N` | the task each comment names (6, 6, 7, 8, 8) |
| `dispatch::write_control`, `ControlWrite` | 5 | per-item `#[allow(dead_code)] // cp-1: consumed in Task 6` | Task 6 |
| `ControlRuntime::subscriber_count` | 8 | `#[allow(dead_code)] // test observable` — **PERMANENT** (the `read_channel.rs:445` precedent) | never |
| `fold.rs` payload structs `SessionInterrupted`, `ControlResponded`, `ControlFailed`, `SubscriberDropped` | 2 | `#[allow(dead_code)] // audit-only: the fields exist to VALIDATE the shape (deny_unknown_fields), never to be read` — **PERMANENT** (the `fold.rs:541` precedent) | never |

### Parallel-stream file ownership (wave-2, window W2)

- **cp-1 OWNS:** `daemon/control.rs` (new), `daemon/read_channel.rs`, `daemon/socket.rs`, `daemon/patrol.rs` (one accessor), `tests/control.rs` (new), `tests/fixtures/control/**` (new), `tests/claude_compat.rs`, `tests/fake-agent.sh`, `tests/perf_daemon.rs`.
- **SHARED — minimal touches, expect a real rebase:** `daemon/event_loop.rs`, `daemon/dispatch.rs`, `daemon/mod.rs`, `main.rs`, `camp-core/src/{event,vocab}.rs`, `camp-core/src/ledger/fold.rs`, `Cargo.toml`/`Cargo.lock`. **Do NOT refactor these files.**
- **compat-2 OWNS — DO NOT TOUCH:** `camp-core/src/formula/**`, `ci/gc-compat/**`.

**Honest accounting of the `event_loop.rs` touches** (rev 1 oversold these as "all additive"; they are not, and compat-2 is in flight in the same file):
1. *Additive:* four new `Request` arms in `drain_lines`; the post-drain control block; `control.forget` on the close paths.
2. *Rewrite (7 lines):* the `min_deadline` composition (event_loop.rs:153-159) gains a fourth nesting.
3. *Signature change:* `run`/`serve_connection`/`drain_lines` gain a `control: &mut ControlRuntime` parameter; the latter two also gain the connection's `Token`.
4. *Visibility change:* `struct Conn` and its two fields become `pub(super)`.
5. *Interest change:* the accept arm registers `READABLE | WRITABLE`.
6. *Deletion:* the `Request::Nudge` arm (event_loop.rs:796-844) moves verbatim into `control.rs` — a net REDUCTION.
7. *Move:* `appended_read_channel_events` is declared once, higher in the block.

---

## Root-cause analysis (verified against this branch at `f6b248c`)

1. **campd can hear but cannot speak.** cp-0 tails every live session's stdout by byte offset on every wake and parses each complete line — into a `Value` it merely *counts* (`read_channel.rs:635-650`). Nothing correlates a `control_response`, and nothing writes a `control_request`: `nudge_via_stdin` (dispatch.rs:208-227) writes only `spawn::user_message` turns.
2. **The socket has no session verbs.** `Request` (socket.rs:26-45) is `poke|status|stop|adopt|nudge`.
3. **The socket is one-shot.** `respond()` (event_loop.rs:997-1006) documents *"Responses are a few bytes; a WouldBlock here means the client is not reading"* — no outbound buffering anywhere. §4.4 requires it.
4. **`drain_one` has TWO callers.** `drain_all` (event_loop.rs:428) and `apply_pending_unregisters` (read_channel.rs:301, called at event_loop.rs:557 — AFTER `persist_offsets`). Any per-line side effect must be harvested on **both**. That is B4, and it is the same bug class cp-0's own review caught.

---

## Design decisions

**D1 — `interrupt` is ACK-then-ASYNC** (RATIFIED). The socket answers `{"ok":true,"request_id":…}` immediately; the `control_response` arrives later on the read channel and appends `control.responded`. campd's loop is single-threaded — a handler that waits on a filesystem-latency line is issue #55's wedge class, and §4.4 makes bounded-answer the law for every verb. **The ledger-mediated round trip is only honest if the async half survives a restart — that is B6, fixed in Tasks 3/6.**

**D2 — deliver-then-record for interrupt and send_turn** (RATIFIED). §5.3's ledger-FIRST ordering exists to make *"pending in the ledger"* prove *"never written to the pipe"* for §5.3.4's adoption kill. No kill hangs off `session.interrupted`, so interrupt/send_turn follow the merged `Request::Nudge` precedent (deliver → record → respond).

**D3 — strict control surface, transparent stream surface** (RATIFIED, hardened). §2.1's loudness is scoped to *control messages*. Strictness now keys on **`type.starts_with("control")`**, not a fixed list — so a future `control_notify` is a loud fault instead of being forwarded to subscribers as content. Every other `type` is opaque stream data camp never claimed to interpret.

**D4 — `session.send_turn` REPLACES `Request::Nudge`** (RATIFIED). One verb (§4.1: *"this is `camp nudge`, promoted to the protocol"*); no back-compat is required. The five dependents: `cmd/nudge.rs:42,47,59`, `event_loop.rs:796`, and the `nudge_wire_format_is_pinned` test. **The `camp nudge` CLI verb survives unchanged** — only the wire op it sends changes. **`send_turn` keeps emitting `session.nudged`**, deliberately: it is the merged vocabulary for "a turn was injected"; renaming it would churn `vocab.rs`, `fold.rs` and `cli_nudge.rs` for nothing.

**D5 — `subscriber_buffer_bytes` = 1 MiB module constant + test-only env override** (RATIFIED) — the cp-0 `max_stream_bytes` precedent (plan-gate ruling (b)). A `camp.toml` field is deferred to a phase owning `config.rs`.

**~~D6~~ REJECTED. D6′ — history is STREAMED, not slurped; the cap is HARD and enforced at append time.**
A `Subscriber` owns an open `File` on the stream and a `history_cursor`. It is *catching up* while `history_cursor < caught_up_at`; during catch-up, live fanout lines are **ignored** — they are already in the append-only file and the history reader will reach them, so nothing is duplicated and nothing is reordered. `pump()` refills from history in 64 KiB chunks only as the socket drains, so **no history size is ever refused, and campd never allocates or reads unboundedly on its event loop** (B9). A frame that would push `out` past `subscriber_buffer_bytes` **drops the subscriber before it is appended** — a hard cap — and the attempted size is reported as the high-water mark (B10). §9's *"explicit error, never a silently truncated stream"* applies where it belongs: a **reaped/disposed** stream and a cursor **past the tail** are errors at the hello; ordinary history is not.

### Deliberately DEFERRED (all nine verified honest by the panel)

`--permission-prompt-tool stdio`, `permission.pending`/BLOCKED/stall-disarm/adoption-kill (phase 3, §5.3–§5.3.4) · the `initialize` handshake (phase 3 — **and cp-1's no-initialize configuration is now PROVEN against the real CLI**, Task 10) · `--include-partial-messages` (phase 4, §2.2) · `fleet.subscribe`, `session.permission_decision`, `set_model`, `set_permission_mode` (§4.1, later phases) · `camp watch`/`camp attach` (phases 2/4) · `subscriber_buffer_bytes` as config (a phase owning `config.rs`).

**Consequence, stated plainly in the PR body:** after cp-1 merges, **an operator still cannot interrupt anything by hand** — no `camp interrupt`, no `camp sessions`, no subscribe CLI. cp-1 ships the protocol and its proofs; phase 2 ships the first human client.

---

## Task 1: `control.rs` — the wire format, and fixtures whose provenance is labelled

Spec: §2 (the protocol table), §2.1 (one module owns it; shapes pinned by fixtures; failures loud), §9 (`request_user_dialog` gets a deterministic error).

**Files:** Create `crates/camp/src/daemon/control.rs`, `crates/camp/tests/fixtures/control/*.json`, `crates/camp/tests/fixtures/control/PROVENANCE.md`. Modify `crates/camp/src/daemon/mod.rs` (add `pub mod control;` alphabetically, after `pub mod bounded;`).

**Interfaces produced:**
```rust
pub const REQUEST_ID_PREFIX: &str = "camp-";
pub fn new_request_id() -> String;                          // "camp-<uuid-v4>"
pub enum ParentMessage { Interrupt { request_id: String }, DialogRefusal { request_id: String } }
impl ParentMessage { pub fn to_line(&self) -> anyhow::Result<String>; }   // NDJSON, '\n'-terminated
pub enum WorkerMessage<'a> {
    ControlResponse { request_id: String, ok: bool, detail: String },
    CanUseTool { request_id: String, tool_name: String },
    RequestUserDialog { request_id: String },
    Stream(&'a str),
}
pub fn parse_worker_line(line: &str) -> Result<WorkerMessage<'_>, ControlWireError>;
pub struct ControlWireError { pub line: String, pub reason: String }
```

- [ ] **Step 0: RECOVER the real shapes from the pinned CLI (B14).** The spec cites `@anthropic-ai/claude-agent-sdk@0.3.207 package/sdk.mjs`; that file is not vendored here. The **actual peer** is on the machine at the pinned version, and its bundle is `strings`-greppable. Run these and paste the output into `PROVENANCE.md`:

```bash
CLI=$(readlink -f "$(command -v claude)")     # must equal ci/claude-compat/CLAUDE_VERSION
strings -a "$CLI" | grep -o 'type:"control_response",response:{subtype:"success".\{0,60\}'
strings -a "$CLI" | grep -o 'type:"control_response",response:{subtype:"error".\{0,60\}'
strings -a "$CLI" | grep -o 'subtype:"can_use_tool".\{0,110\}'
strings -a "$CLI" | grep -o 'subtype:"request_user_dialog".\{0,80\}'
strings -a "$CLI" | grep -o 'type==="control_request"&&.\{0,40\}'
```
These are the lines they returned on 2026-07-13 against 2.1.207 — **the ground truth this task pins**:
```
type:"control_response",response:{subtype:"success",request_id:e.request_id}
type:"control_response",response:{subtype:"error",request_id:e.request_id,error:k.error}
subtype:"can_use_tool",tool_name:n,display_name:s1e(n),input:o,tool_use_id:i,description:s,...a&&{permission_suggestions:a}
subtype:"request_user_dialog",dialog_kind:o,payload:i,...s&&{tool_use_id:s}
type==="control_request"&&"request_id" in e&&"request" in e
```
Four findings, and they change the fixtures:
- **The `error:<string>` key is CORRECT.** Rev 1 guessed it; it is now verified. `body["error"].as_str()` is the right parse.
- **`request_user_dialog` was WRONG.** Real keys: `dialog_kind`, `payload`, optional `tool_use_id` — **not** the invented `prompt`. (`dialog_kind`'s *values* are a minified variable and could not be recovered, so **camp must never key on it**: it refuses every dialog regardless of kind and reads only `request_id`.)
- **`can_use_tool` was incomplete.** It also carries `display_name` and `tool_use_id`. camp reads only `request_id` and `tool_name`, and the parse must tolerate the rest — which is why the envelope is NOT `deny_unknown_fields` (D3's strictness lives in the subtype match).
- The CLI's own inbound validator (`"request_id" in e && "request" in e`) confirms camp's outbound envelope: top-level `type`, `request_id`, `request`.

- [ ] **Step 1: Write the fixtures and `PROVENANCE.md`.** One line each, no trailing newline.

`interrupt_request.json` — *provenance: `camp-authored`; **accepted** by CLI 2.1.207 (Task 10's $0 gate sends exactly these bytes and asserts the ack). The claim is ACCEPTANCE, not recording.*
```json
{"type":"control_request","request_id":"camp-fixture-1","request":{"subtype":"interrupt"}}
```
`control_response_success.json` — *provenance: `recorded-from-CLI-2.1.207` (observed on the wire, live $0 run)*
```json
{"type":"control_response","response":{"subtype":"success","request_id":"camp-fixture-1","response":{"still_queued":[]}}}
```
`control_response_error.json` — *provenance: `derived-from-CLI-2.1.207` (bundle: `subtype:"error",request_id:e.request_id,error:k.error`)*
```json
{"type":"control_response","response":{"subtype":"error","request_id":"camp-fixture-1","error":"no turn in progress"}}
```
`can_use_tool_request.json` — *provenance: `derived-from-CLI-2.1.207` (KEYS from the bundle; VALUES illustrative)*
```json
{"type":"control_request","request_id":"cli-fixture-2","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"cargo publish"},"tool_use_id":"toolu_fixture"}}
```
`request_user_dialog_request.json` — *provenance: KEYS `derived-from-CLI-2.1.207`; `dialog_kind`'s VALUE is `camp-invented` and **camp never reads it***
```json
{"type":"control_request","request_id":"cli-fixture-3","request":{"subtype":"request_user_dialog","dialog_kind":"unknown","payload":{},"tool_use_id":"toolu_fixture"}}
```
`dialog_refusal_response.json` — *provenance: `camp-authored`, shape mirrored from the CLI's OWN error-response construction. **UNVALIDATED against the real CLI**: camp only ever sends this under `--permission-prompt-tool stdio`, which is phase 3, so no $0 gate in cp-1 can exercise it. **PHASE-3 OBLIGATION: validate it.** If the shape is wrong the CLI ignores it and the worker hangs forever — the precise outcome §9 exists to prevent.*
```json
{"type":"control_response","response":{"subtype":"error","request_id":"cli-fixture-3","error":"camp does not support interactive dialogs"}}
```
`user_turn.json` — *provenance: `recorded-from-CLI-2.1.207` via probe P2 (the merged `spawn::user_message`, in production since Phase 8)*
```json
{"type":"user","message":{"role":"user","content":"status?"}}
```
`stream_assistant.json` — *provenance: `camp-authored` (a representative non-control stream line; camp never interprets it — D3)*
```json
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}
```
`PROVENANCE.md` records, per fixture: the label, the extraction command, and — for `dialog_refusal_response.json` — the phase-3 validation obligation. **cp-3 inherits labelled claims, not laundered guesses.**

- [ ] **Step 2: Write the failing test.** Create `control.rs` with the module doc, the module-level `dead_code` allow, and ONLY this test module.

```rust
//! cp-1 (control-plane spec §2.1): THE module that owns the `claude` control
//! wire format. Nothing else in camp constructs or parses a control message.
//! Shapes are pinned against fixtures whose provenance is LABELLED
//! (tests/fixtures/control/PROVENANCE.md) — recorded from, or derived from, the
//! shipped CLI at the version pinned in ci/claude-compat/CLAUDE_VERSION. A
//! protocol change is a red build, not a silent misbehaviour.

// cp-1: wired in Task 6 — DELETE this attribute there (the dead_code
// discipline). `camp` is a binary crate: `pub` does not exempt an item from
// dead_code, and nothing here is reached until the event loop calls it.
#![allow(dead_code)]

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
    const REFUSAL: &str = include_str!("../../tests/fixtures/control/dialog_refusal_response.json");
    const USER_TURN: &str = include_str!("../../tests/fixtures/control/user_turn.json");
    const STREAM: &str = include_str!("../../tests/fixtures/control/stream_assistant.json");

    /// §2.1: every shape camp SENDS is pinned to the byte.
    ///
    /// B1 — why these are STRUCTS and not `serde_json::json!`: serde_json
    /// 1.0.150 is locked WITHOUT `preserve_order`, so its `Map` is a `BTreeMap`
    /// and `Value::to_string()` emits keys ALPHABETICALLY — `request` before
    /// `request_id` before `type`, which is not the CLI's key order. Struct
    /// field DECLARATION order is preserved, so it is. Do NOT "fix" a future
    /// mismatch by enabling `preserve_order`: that would change every `Value`
    /// serialization in camp.
    #[test]
    fn parent_messages_serialize_to_the_pinned_fixture_bytes() {
        let interrupt = ParentMessage::Interrupt {
            request_id: "camp-fixture-1".to_owned(),
        };
        assert_eq!(
            interrupt.to_line().unwrap(),
            format!("{}\n", INTERRUPT.trim_end()),
            "byte-for-byte, in the CLI's key order"
        );
        let refusal = ParentMessage::DialogRefusal {
            request_id: "cli-fixture-3".to_owned(),
        };
        assert_eq!(refusal.to_line().unwrap(), format!("{}\n", REFUSAL.trim_end()));
        // §2.1 pins EVERY shape camp sends — including the turn envelope, whose
        // constructor is shared with the dispatch launch path.
        assert_eq!(
            crate::daemon::spawn::user_message("status?"),
            format!("{}\n", USER_TURN.trim_end())
        );
    }

    /// The order-INDEPENDENT guard: the bytes are also semantically the same
    /// object. If serde ever reorders keys, this still holds and the byte test
    /// above says exactly what moved.
    #[test]
    fn parent_messages_are_semantically_equal_to_their_fixtures() {
        let line = ParentMessage::Interrupt {
            request_id: "camp-fixture-1".to_owned(),
        }
        .to_line()
        .unwrap();
        let produced: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        let pinned: serde_json::Value = serde_json::from_str(INTERRUPT.trim_end()).unwrap();
        assert_eq!(produced, pinned);
    }

    /// §2.1: every shape camp PARSES is pinned. NOTE the nesting the real CLI
    /// uses: `request_id` is INSIDE `response` on a control_response, and at the
    /// TOP level on a control_request (both verified against the 2.1.207
    /// bundle — PROVENANCE.md).
    #[test]
    fn worker_messages_parse_from_the_pinned_fixtures() {
        match parse_worker_line(OK.trim_end()).unwrap() {
            WorkerMessage::ControlResponse { request_id, ok, .. } => {
                assert_eq!(request_id, "camp-fixture-1");
                assert!(ok);
            }
            other => panic!("expected a control_response, got {other:?}"),
        }
        match parse_worker_line(ERR.trim_end()).unwrap() {
            WorkerMessage::ControlResponse { ok, detail, .. } => {
                assert!(!ok);
                // B14: the `error` key is VERIFIED against the CLI bundle. If
                // this ever regresses to the "unspecified control error"
                // placeholder, camp is swallowing the detail on the exact
                // surface §2.1 says must be loud.
                assert_eq!(detail, "no turn in progress");
            }
            other => panic!("expected an error control_response, got {other:?}"),
        }
        match parse_worker_line(CAN_USE_TOOL.trim_end()).unwrap() {
            WorkerMessage::CanUseTool { request_id, tool_name } => {
                assert_eq!(request_id, "cli-fixture-2");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("expected can_use_tool, got {other:?}"),
        }
        // camp reads ONLY the request_id of a dialog: it refuses every dialog
        // regardless of `dialog_kind`, whose values could not be recovered and
        // which camp must therefore never key on.
        match parse_worker_line(DIALOG.trim_end()).unwrap() {
            WorkerMessage::RequestUserDialog { request_id } => {
                assert_eq!(request_id, "cli-fixture-3");
            }
            other => panic!("expected request_user_dialog, got {other:?}"),
        }
    }

    /// D3: the stream surface is TRANSPARENT. A non-control line passes through
    /// verbatim and is never a fault — new claude releases add stream types
    /// routinely, and §2.1's strictness is written about CONTROL messages.
    #[test]
    fn non_control_stream_lines_pass_through_verbatim_and_never_fault() {
        let line = STREAM.trim_end();
        match parse_worker_line(line).unwrap() {
            WorkerMessage::Stream(raw) => assert_eq!(raw, line),
            other => panic!("expected a passthrough Stream line, got {other:?}"),
        }
        let future = r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#;
        assert!(matches!(parse_worker_line(future).unwrap(), WorkerMessage::Stream(_)));
    }

    /// D3 (hardened) + §2.1: the CONTROL surface is STRICT, keyed on the
    /// `control` PREFIX — so a control-family type camp has never heard of is a
    /// LOUD fault, not content forwarded to a subscriber.
    #[test]
    fn an_unrecognized_control_message_is_a_loud_error() {
        let unknown =
            r#"{"type":"control_request","request_id":"x","request":{"subtype":"teleport"}}"#;
        assert!(parse_worker_line(unknown).unwrap_err().reason.contains("teleport"));
        let headless = r#"{"type":"control_response","response":{"subtype":"success"}}"#;
        assert!(parse_worker_line(headless).is_err(), "missing request_id");
        assert_eq!(parse_worker_line("not json at all").unwrap_err().line, "not json at all");
        // control_cancel_request is real in the CLI (it handles it explicitly)
        // and camp does not implement it => loud, never a silent pass-through.
        assert!(parse_worker_line(r#"{"type":"control_cancel_request","request_id":"x"}"#).is_err());
        // A control-family type that DOES NOT EXIST YET must also be loud — the
        // PREFIX rule, not a fixed list (D3's residual hole, closed).
        assert!(parse_worker_line(r#"{"type":"control_notify","payload":{}}"#).is_err());
    }

    #[test]
    fn request_ids_are_unique_and_prefixed() {
        let (a, b) = (new_request_id(), new_request_id());
        assert_ne!(a, b);
        assert!(a.starts_with(REQUEST_ID_PREFIX), "{a}");
    }
}
```

- [ ] **Step 3: Run it and watch it fail.** Add `pub mod control;` to `daemon/mod.rs` FIRST, or the module is never compiled and the failure is vacuous.

Run: `cargo test -p camp --lib daemon::control 2>&1 | tail -20`
Expected: FAIL — compile errors naming `ParentMessage`, `parse_worker_line`, `WorkerMessage`, `new_request_id`.

- [ ] **Step 4: Implement.** Prepend to `control.rs`:

```rust
use serde::{Deserialize, Serialize};

pub const REQUEST_ID_PREFIX: &str = "camp-";

/// A fresh, globally unique control request id. UUID-backed (the
/// `spawn::new_session_id` mold) because a RESTARTED campd rebuilds its pending
/// table from the ledger (B6) and must never collide with an id its predecessor
/// left there.
pub fn new_request_id() -> String {
    format!("{REQUEST_ID_PREFIX}{}", uuid::Uuid::new_v4())
}

// ---- the outbound wire (B1: STRUCTS — field order is declaration order) ----

#[derive(Serialize)]
struct InterruptBody {
    subtype: &'static str,
}

#[derive(Serialize)]
struct InterruptEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: &'a str,
    request: InterruptBody,
}

#[derive(Serialize)]
struct ErrorResponseBody<'a> {
    subtype: &'static str,
    request_id: &'a str,
    error: &'a str,
}

#[derive(Serialize)]
struct ErrorResponseEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    response: ErrorResponseBody<'a>,
}

/// camp -> worker. Serialize ONLY: camp never parses what it sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentMessage {
    /// §2: the SDK's `interrupt()`.
    Interrupt { request_id: String },
    /// §9 (settled): the CLI genuinely sends `request_user_dialog` under stdio.
    /// Camp answers with a deterministic error — neither ignoring it (a §2.1
    /// fault) nor hanging the worker forever.
    DialogRefusal { request_id: String },
}

impl ParentMessage {
    /// One NDJSON line, newline-terminated — what the held stdin pipe expects.
    /// Fallible: serializing these fixed structs cannot fail in practice, but
    /// the error is propagated, never unwrapped (no panics in library code).
    pub fn to_line(&self) -> anyhow::Result<String> {
        let mut line = match self {
            ParentMessage::Interrupt { request_id } => serde_json::to_string(&InterruptEnvelope {
                kind: "control_request",
                request_id,
                request: InterruptBody { subtype: "interrupt" },
            })?,
            ParentMessage::DialogRefusal { request_id } => {
                serde_json::to_string(&ErrorResponseEnvelope {
                    kind: "control_response",
                    response: ErrorResponseBody {
                        subtype: "error",
                        request_id,
                        error: "camp does not support interactive dialogs",
                    },
                })?
            }
        };
        line.push('\n');
        Ok(line)
    }
}

// ---- the inbound wire -----------------------------------------------------

#[derive(Debug)]
pub enum WorkerMessage<'a> {
    ControlResponse { request_id: String, ok: bool, detail: String },
    /// §2/§5.3. cp-1 cannot answer one: `--permission-prompt-tool stdio` is
    /// phase 3 (§5.3.1), so this is structurally unreachable — and if it arrives
    /// anyway it is a LOUD fault, never a silent drop.
    CanUseTool { request_id: String, tool_name: String },
    /// §9: answered with `ParentMessage::DialogRefusal`. camp reads ONLY the
    /// request_id — never `dialog_kind`, whose values are not recoverable from
    /// the shipped bundle, because it refuses every dialog regardless of kind.
    RequestUserDialog { request_id: String },
    /// D3: any non-control stream-json line, verbatim. NOT a fault.
    Stream(&'a str),
}

#[derive(Debug, Clone)]
pub struct ControlWireError {
    pub line: String,
    pub reason: String,
}

/// The control envelope as the real CLI writes it. Deliberately NOT
/// `deny_unknown_fields`: the CLI is free to add keys to a control message
/// (`can_use_tool` already carries `display_name`, `tool_use_id`,
/// `permission_suggestions`), and camp keys off `type` + `subtype` only. The
/// STRICTNESS lives in the subtype match — which is the §2.1 obligation.
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

/// Parse ONE complete stdout line from a worker.
///
/// D3 — the partition that makes §2.1 implementable:
///   `type` starting with "control" => STRICT. An unknown subtype, an unknown
///       control type, or a missing request_id is a LOUD ControlWireError. (The
///       PREFIX rule, not a fixed list: a future `control_notify` must fault,
///       never be forwarded to a subscriber as content.)
///   any other `type`               => a transparent stream line, verbatim.
pub fn parse_worker_line(line: &str) -> Result<WorkerMessage<'_>, ControlWireError> {
    let fail = |reason: String| ControlWireError { line: line.to_owned(), reason };
    let envelope: Envelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => return Err(fail(format!("not a JSON object: {e}"))),
    };
    if !envelope.kind.starts_with("control") {
        return Ok(WorkerMessage::Stream(line));
    }
    match envelope.kind.as_str() {
        "control_response" => {
            let body = envelope
                .response
                .ok_or_else(|| fail("control_response has no `response` object".to_owned()))?;
            // Verified against CLI 2.1.207: request_id is nested INSIDE
            // `response` on a control_response (PROVENANCE.md).
            let request_id = body["request_id"]
                .as_str()
                .ok_or_else(|| fail("control_response carries no request_id".to_owned()))?
                .to_owned();
            match body["subtype"].as_str().unwrap_or_default() {
                "success" => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: true,
                    detail: body["response"].to_string(),
                }),
                "error" => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: false,
                    // The `error` key is VERIFIED (the CLI builds `error:k.error`).
                    // The placeholder is reachable only if the CLI stops sending
                    // it — in which case the protocol changed and the fixture
                    // test is already red.
                    detail: body["error"]
                        .as_str()
                        .unwrap_or("the worker reported an unspecified control error")
                        .to_owned(),
                }),
                other => Err(fail(format!("unrecognized control_response subtype {other:?}"))),
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
                    "unrecognized control_request subtype {other:?} — camp cannot answer it, and a \
                     control message it cannot answer is a protocol fault, never a silent drop \
                     (control-plane spec §2.1)"
                ))),
            }
        }
        other => Err(fail(format!(
            "camp does not implement the {other:?} control type — a control-family message camp \
             does not understand is a LOUD fault, never content forwarded to a subscriber \
             (control-plane spec §2.1)"
        ))),
    }
}
```

- [ ] **Step 5: Run and watch pass.**

Run: `cargo test -p camp --lib daemon::control 2>&1 | tail -20`
Expected: PASS — **6 tests**.

- [ ] **Step 6: fmt + clippy + commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/control.rs crates/camp/src/daemon/mod.rs crates/camp/tests/fixtures/control
git commit -m "feat(control): one module owns the control wire format, pinned by provenance-labelled fixtures (cp-1 §2.1)"
```

---

## Task 2: the four new events

Spec: §2.1 (loud, evented faults), §4.4 (`subscriber.dropped` names the session and the high-water mark), invariants 3 and 7.

**Files:** Modify `camp-core/src/event.rs`, `camp-core/src/vocab.rs`, `camp-core/src/ledger/fold.rs`. Test: `camp-core/tests/vocab_pin.rs` (existing), `camp-core/src/ledger/mod.rs` unit tests.

**Interfaces produced:**
```rust
EventType::SessionInterrupted => "session.interrupted"  // {session, request_id}
EventType::ControlResponded   => "control.responded"    // {session, request_id, verb, ok, detail}
EventType::ControlFailed      => "control.failed"       // {session?, request_id?, verb?, reason}
EventType::SubscriberDropped  => "subscriber.dropped"   // {session, subscription, buffered_bytes, cap_bytes}
```
None exists in `camp-core/tests/fixtures/gc-vocab.json` (checked: gc has `controller.started`/`controller.stopped`, no `control.*`, no `session.interrupted`, no `subscriber.*`) ⇒ all four are camp-specific and additive (invariant 7).

- [ ] **Step 1: Add the variants AND their fold arms together** (cp-0 plan-gate note 3: a variant without its fold arm makes the next step's red an `E0004` compile error, not a real test failure).

`event.rs` — after `SessionStreamCapped` (event.rs:55), add the four variants with doc comments naming their spec section; add all four to `ALL` (after `SessionStreamCapped`, before `ImportAdded`) and to `as_str`.

`fold.rs` — extend the audit-only region (fold.rs:51). The payloads are **parsed and validated**, not ignored:
```rust
        EventType::SessionInterrupted => audit::<SessionInterrupted>(event),
        EventType::ControlResponded => audit::<ControlResponded>(event),
        EventType::ControlFailed => audit::<ControlFailed>(event),
        EventType::SubscriberDropped => audit::<SubscriberDropped>(event),
```
```rust
/// cp-1: the four control-plane events are AUDIT-ONLY — they project no state —
/// but their payloads are VALIDATED on the fold, so a malformed shape is a loud
/// error at append time rather than a surprise for a later reader.
/// `deny_unknown_fields` makes a typo'd key a red build, not a silently ignored
/// field.
fn audit<T: serde::de::DeserializeOwned>(event: &Event) -> Result<(), CoreError> {
    let _: T = payload(event)?;
    Ok(())
}

// The fields exist to VALIDATE the shape; nothing reads them (the fold.rs:541
// precedent). The allow is PERMANENT.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct SessionInterrupted { session: String, request_id: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
struct SubscriberDropped {
    session: String,
    subscription: String,
    buffered_bytes: u64,
    cap_bytes: u64,
}
```

- [ ] **Step 2: Run the vocab-pin test and watch it fail RED** (an assertion failure, not a compile error).

Run: `cargo test -p camp-core --test vocab_pin 2>&1 | tail -20`
Expected: FAIL in `every_event_type_is_declared_mirrored_or_camp_specific_never_both`.

- [ ] **Step 3: Declare them camp-specific.** Append to `CAMP_SPECIFIC_EVENTS` (after `"session.nudged"`): `"session.interrupted"`, `"control.responded"`, `"control.failed"`, `"subscriber.dropped"`.

- [ ] **Step 4: Run and watch pass.**

Run: `cargo test -p camp-core --test vocab_pin`
Expected: PASS — including `camp_specific_names_do_not_collide_with_gc`.

- [ ] **Step 5: Write the fold round-trip test.** Add to `mod tests` in `camp-core/src/ledger/mod.rs`, beside cp-0's `read_channel_patrol_degraded_shapes_round_trip_through_the_fold`:

```rust
    /// cp-1: the control-plane events are audit-only, but their payloads are
    /// VALIDATED on the fold (deny_unknown_fields). Each shape appends cleanly,
    /// the whole ledger refolds clean, and a typo'd key is a LOUD error.
    #[test]
    fn control_plane_event_shapes_round_trip_through_the_fold() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let shapes = [
            (EventType::SessionInterrupted,
             serde_json::json!({"session": "t/dev/1", "request_id": "camp-1"})),
            (EventType::ControlResponded,
             serde_json::json!({"session": "t/dev/1", "request_id": "camp-1",
                                "verb": "session.interrupt", "ok": true, "detail": "{}"})),
            (EventType::ControlFailed,
             serde_json::json!({"session": "t/dev/1", "request_id": "camp-1",
                                "verb": "session.interrupt",
                                "reason": "no control_response within 30s"})),
            (EventType::SubscriberDropped,
             serde_json::json!({"session": "t/dev/1", "subscription": "sub-1",
                                "buffered_bytes": 1048577u64, "cap_bytes": 1048576u64})),
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
        // A typo'd key is REFUSED at append, never silently ignored.
        assert!(
            l.append(EventInput {
                kind: EventType::SessionInterrupted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"session": "t/dev/1", "requestId": "camp-2"}),
            })
            .is_err(),
            "an unknown payload key must be a loud error"
        );
        // And the whole ledger refolds clean. NOTE the API: refold_check()
        // (refold.rs:64) — there is no `refold()`.
        let report = l.refold_check().unwrap();
        assert!(report.is_clean(), "the refold must be clean: {report:?}");
    }
```
(Match `RefoldReport`'s real API — read refold.rs:64 and mirror what `tests/refold_prop.rs` asserts on. Do not invent a method.)

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

## Task 3: `ControlRuntime` — the pending table, its deadline, and its REHYDRATION (B6)

Spec: §2.1 (*"a control response that never arrives … never a swallowed timeout"*), invariant 1 (an ARMED timer, never a tick), invariant 3 (an event must name its TRUE cause).

**Files:** Modify `crates/camp/src/daemon/control.rs`.

**Interfaces produced:**
```rust
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
pub struct ControlRuntime;
impl ControlRuntime {
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime;
    pub fn track_pending(&mut self, request_id: String, session: String, verb: &'static str, now: Timestamp);
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration>;
    pub fn expire_pending(&mut self, now: Timestamp) -> Vec<EventInput>;
    pub fn resolve(&mut self, request_id: &str, ok: bool, detail: String) -> Option<EventInput>;
    pub fn rehydrate(&mut self, ledger: &Ledger, now: Timestamp) -> anyhow::Result<usize>;
}
```
**Signature change from rev 1:** `resolve` returns `Option<EventInput>` — `None` means "already settled; saying it twice would be the false statement". That is what lets B6's restart test assert *no spurious fault* without special cases.

- [ ] **Step 1: Write the failing tests.** Append to `control.rs`'s `mod tests`:

```rust
    use camp_core::event::{EventInput, EventType};
    use camp_core::ledger::Ledger;
    use jiff::{SignedDuration, Timestamp};

    fn later(now: Timestamp, secs: i64) -> Timestamp {
        now.checked_add(SignedDuration::from_secs(secs)).unwrap()
    }

    /// Invariant 1: nothing pending => NO deadline => the idle daemon blocks in
    /// poll forever. A pending request arms exactly one deadline; an answer
    /// disarms it.
    #[test]
    fn a_pending_request_arms_a_deadline_and_an_empty_table_arms_none() {
        let now = Timestamp::now();
        let mut rt = ControlRuntime::new(1024);
        assert_eq!(rt.poll_timeout(now), None, "an idle campd must not wake");
        rt.track_pending("camp-1".into(), "t/dev/1".into(), "session.interrupt", now);
        let t = rt.poll_timeout(now).expect("a pending request arms a timer");
        assert!(t <= CONTROL_RESPONSE_TIMEOUT && !t.is_zero());
        assert!(rt.resolve("camp-1", true, "{}".into()).is_some());
        assert_eq!(rt.poll_timeout(now), None, "an answered request disarms");
    }

    /// §2.1: a control response that NEVER arrives is a durable, loud fault.
    /// Delete `expire_pending` and this dies.
    #[test]
    fn a_control_response_that_never_arrives_becomes_a_durable_fault() {
        let now = Timestamp::now();
        let mut rt = ControlRuntime::new(1024);
        rt.track_pending("camp-1".into(), "t/dev/1".into(), "session.interrupt", now);
        assert!(rt.expire_pending(now).is_empty(), "not due yet");
        let expired = rt.expire_pending(later(now, 31));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].kind, EventType::ControlFailed);
        assert_eq!(expired[0].data["request_id"], "camp-1");
        assert_eq!(expired[0].data["session"], "t/dev/1");
        assert!(expired[0].data["reason"].as_str().unwrap().contains("no control_response"));
        // Raised exactly once: the row is gone (no duplicates, no re-fire).
        assert!(rt.expire_pending(later(now, 60)).is_empty());
        assert_eq!(rt.poll_timeout(later(now, 60)), None);
    }

    #[test]
    fn a_matching_control_response_resolves_the_pending_request() {
        let now = Timestamp::now();
        let mut rt = ControlRuntime::new(1024);
        rt.track_pending("camp-1".into(), "t/dev/1".into(), "session.interrupt", now);
        let input = rt.resolve("camp-1", true, "{\"still_queued\":[]}".into()).unwrap();
        assert_eq!(input.kind, EventType::ControlResponded);
        assert_eq!(input.data["request_id"], "camp-1");
        assert_eq!(input.data["session"], "t/dev/1");
        assert_eq!(input.data["verb"], "session.interrupt");
        assert_eq!(input.data["ok"], true);
    }

    /// B6, half 1 — A RESTART MUST NOT LIE. campd dies with an interrupt in
    /// flight. cp-0 persists the stream offset only AFTER the line's ledger
    /// effect commits, so the worker's control_response is re-read in the next
    /// life. Without rehydration it hits an empty table and campd emits a
    /// `control.failed` claiming a protocol fault — an ORDINARY RESTART
    /// manufacturing an operator-visible fault, and an event whose named cause
    /// is FALSE (invariant 3).
    ///
    /// B6, half 2 — A RESTART MUST NOT FORGET. The pending row is the only thing
    /// that would ever expire a genuinely unanswered request; losing it is
    /// exactly the "swallowed timeout" §2.1 forbids.
    #[test]
    fn a_restart_across_an_in_flight_interrupt_neither_lies_nor_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        // campd life 1: two interrupts go out; one is answered before the crash.
        for (id, session) in [("camp-answered", "t/dev/1"), ("camp-orphan", "t/dev/2")] {
            ledger
                .append(EventInput {
                    kind: EventType::SessionInterrupted,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({"session": session, "request_id": id}),
                })
                .unwrap();
        }
        ledger
            .append(EventInput {
                kind: EventType::ControlResponded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": "t/dev/1", "request_id": "camp-answered",
                    "verb": "session.interrupt", "ok": true, "detail": "{}"
                }),
            })
            .unwrap();
        // ... kill -9 ...
        let now = Timestamp::now();
        let mut rt = ControlRuntime::new(1024);
        assert_eq!(
            rt.rehydrate(&ledger, now).unwrap(),
            1,
            "only the UNANSWERED interrupt is still pending"
        );
        // MUST NOT LIE: the answered request's RE-READ control_response resolves
        // to NOTHING — it is already a durable fact, and re-announcing it (or
        // faulting on it) would be the false statement.
        assert!(
            rt.resolve("camp-answered", true, "{}".into()).is_none(),
            "a response for an ALREADY-SETTLED request is recognized, never turned into a \
             control.failed"
        );
        // MUST NOT FORGET: the orphan still expires, loudly.
        let expired = rt.expire_pending(later(now, 31));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].data["request_id"], "camp-orphan");
        assert_eq!(expired[0].kind, EventType::ControlFailed);
    }

    /// An answer to a request camp NEVER sent — not merely one already settled —
    /// is a protocol fault (§2.1). Rehydration cannot explain this one away, and
    /// it stays loud.
    #[test]
    fn a_control_response_for_a_never_sent_request_id_is_a_fault() {
        let mut rt = ControlRuntime::new(1024);
        let input = rt.resolve("wat-1", true, "{}".into()).expect("a fault event");
        assert_eq!(input.kind, EventType::ControlFailed);
        assert!(input.data["reason"].as_str().unwrap().contains("never sent"));
    }
```

- [ ] **Step 2: Run and watch it fail.** `cargo test -p camp --lib daemon::control 2>&1 | tail -20` → FAIL (`cannot find type ControlRuntime`).

- [ ] **Step 3: Implement.**

```rust
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context as _, Result};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use jiff::{SignedDuration, Timestamp};

/// How long a control request may go unanswered before campd declares the
/// protocol broken (§2.1). A BOUND on one operation, not a wakeup: the deadline
/// joins `min_deadline` only while something is pending, so an idle campd with
/// nothing outstanding still blocks in `poll` forever (invariant 1).
///
/// 30 s is generous against the transport: the answer travels worker stdout ->
/// file -> notify -> campd wake -> drain (§2.3's "filesystem-event latency").
/// B5 is what makes it SAFE: the deadline is evaluated AFTER this wake's drain
/// and ingest, so a response already present in the file always beats its own
/// deadline — correctness never depends on a delivered notify event.
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

struct Pending {
    session: String,
    verb: &'static str,
    deadline: Timestamp,
}

pub struct ControlRuntime {
    pending: HashMap<String, Pending>,
    /// B6: request ids already SETTLED (answered, faulted, or settled in a
    /// previous campd life per the ledger). A re-read `control_response` for one
    /// of these is recognized and dropped: it is already a durable fact, and
    /// re-announcing or faulting on it would be the false statement. This is NOT
    /// error-silencing — an id in NEITHER map is still a loud fault.
    resolved: HashSet<String>,
    subscriber_buffer_bytes: usize,
    // (Task 8 adds the subscriber registry.)
}

impl ControlRuntime {
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime {
        ControlRuntime {
            pending: HashMap::new(),
            resolved: HashSet::new(),
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
            .checked_add(
                SignedDuration::try_from(CONTROL_RESPONSE_TIMEOUT)
                    .unwrap_or(SignedDuration::from_secs(30)),
            )
            .unwrap_or(now);
        self.pending.insert(request_id, Pending { session, verb, deadline });
    }

    /// B6: rebuild the pending table from the ledger — the ONLY durable record of
    /// an in-flight control request (there is no sidecar state; invariant 3). A
    /// `session.interrupted` with no matching `control.responded`/`control.failed`
    /// is still outstanding. This is the ledger-derived suppression pattern
    /// merged in fix-83 (#92).
    ///
    /// Deadlines are armed FRESH from `now`: the previous life's clock is not
    /// ours, and a worker waiting across a restart deserves the full window
    /// before campd calls the protocol broken.
    pub fn rehydrate(&mut self, ledger: &Ledger, now: Timestamp) -> Result<usize> {
        let sent = ledger
            .events_of_type(EventType::SessionInterrupted)
            .context("reading session.interrupted for control rehydration")?;
        let mut settled: HashSet<String> = HashSet::new();
        for kind in [EventType::ControlResponded, EventType::ControlFailed] {
            for event in ledger
                .events_of_type(kind)
                .with_context(|| format!("reading {} for control rehydration", kind.as_str()))?
            {
                if let Some(id) = event.data["request_id"].as_str() {
                    settled.insert(id.to_owned());
                }
            }
        }
        let mut restored = 0usize;
        for event in sent {
            let (Some(id), Some(session)) = (
                event.data["request_id"].as_str(),
                event.data["session"].as_str(),
            ) else {
                continue; // the fold's deny_unknown_fields guarantees both; be total anyway
            };
            if settled.contains(id) {
                // Already settled in a previous life. Its control_response may
                // STILL be re-read off the stream file (cp-0 persists the offset
                // only after the ledger effect commits), so remember it as
                // resolved — otherwise the re-read manufactures a "camp never
                // sent this" fault (B6 half 1).
                self.resolved.insert(id.to_owned());
                continue;
            }
            self.track_pending(id.to_owned(), session.to_owned(), "session.interrupt", now);
            restored += 1;
        }
        Ok(restored)
    }

    /// The earliest pending deadline as a Duration-from-now (the
    /// `PatrolRuntime::poll_timeout` mold — `min_deadline` composes them).
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration> {
        let earliest = self.pending.values().map(|p| p.deadline).min()?;
        Some(Duration::try_from(earliest.duration_since(now)).unwrap_or(Duration::ZERO))
    }

    /// Due pending requests => durable `control.failed` events. The row is
    /// REMOVED (raised exactly once) and remembered as resolved, so a LATE
    /// answer is recognized rather than faulted a second time.
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
                self.resolved.insert(id.clone());
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
                            "no control_response for {} within {}s — the worker never answered on \
                             its stdout stream (control-plane spec §2.1: a control response that \
                             never arrives is a loud fault, never a swallowed timeout)",
                            p.verb,
                            CONTROL_RESPONSE_TIMEOUT.as_secs()
                        ),
                    }),
                })
            })
            .collect()
    }

    /// Correlate a worker's control_response.
    ///   Some(ControlResponded) — it answers a request we are waiting on.
    ///   None                   — it answers one ALREADY SETTLED (re-read after a
    ///                            restart, or a late answer past the deadline):
    ///                            already durable, so saying it twice would be
    ///                            the false statement.
    ///   Some(ControlFailed)    — camp NEVER sent it: the protocol is not what
    ///                            camp believes it to be (§2.1). Still loud.
    pub fn resolve(&mut self, request_id: &str, ok: bool, detail: String) -> Option<EventInput> {
        if let Some(p) = self.pending.remove(request_id) {
            self.resolved.insert(request_id.to_owned());
            return Some(EventInput {
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
            });
        }
        if self.resolved.contains(request_id) {
            return None;
        }
        Some(EventInput {
            kind: EventType::ControlFailed,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "request_id": request_id,
                "reason": format!(
                    "a worker answered control request {request_id:?}, which camp never sent — the \
                     control protocol is not what camp believes it to be (control-plane spec §2.1)"
                ),
            }),
        })
    }
}
```

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --lib daemon::control` → PASS (11 tests).

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/control.rs
git commit -m "feat(control): pending table with an armed deadline, rehydrated from the ledger across a restart (cp-1 §2.1)"
```

---

## Task 4: the read channel hands its lines over — from BOTH drain paths

Spec: §2.3 (the transport), §4.1 (last activity), §9 (byte-offset cursors).

An EXTENSION of cp-0: the drain loop, the offsets, the cap guards, the partial-line buffering and the disposal ordering are untouched. **What IS new — and what B4 punishes — is that `drain_one` now has a per-line SIDE EFFECT, and it has TWO callers.** Task 6 harvests both.

**Files:** Modify `crates/camp/src/daemon/read_channel.rs`.

**Interfaces produced:**
```rust
pub struct StreamLine { pub session: String, pub line: String, pub offset_after: u64 }
impl ReadChannelRuntime {
    pub fn take_stream_lines(&mut self) -> Vec<StreamLine>;
    pub fn last_activity(&self, session: &str) -> Option<jiff::Timestamp>;
    pub fn tail_state(&self, session: &str) -> Option<(PathBuf, u64)>;
    pub fn take_disposed(&mut self) -> Vec<String>;   // B12: end-frame targets
}
```

- [ ] **Step 1: Write the failing tests.** Append to `read_channel.rs`'s `mod tests`:

```rust
    /// cp-1: the drain HANDS OVER the complete lines it consumed — in file
    /// order, tagged with the session and the offset AFTER each line (the cursor
    /// a subscriber resumes from, §9). cp-0 only counted them.
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
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].session, "t/dev/1");
        assert_eq!(lines[0].line, a);
        assert_eq!(lines[1].line, b, "file order is preserved");
        assert_eq!(lines[1].offset_after, (a.len() + b.len() + 2) as u64);
        assert!(rc.last_activity("t/dev/1").is_some(), "the offset advanced");
        assert!(rc.take_stream_lines().is_empty(), "mem::take-drained — never redelivered");

        // A PARTIAL line is not handed over until it is complete.
        std::fs::OpenOptions::new().append(true).open(&stdout).unwrap()
            .write_all(b"{\"type\":\"resu").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        assert!(rc.take_stream_lines().is_empty(), "a partial line is never handed over");
    }

    /// B4's other half, at the unit level: the DISPOSAL-time final drain
    /// (`apply_pending_unregisters`) also produces stream lines — and they are
    /// the worker's LAST bytes, which in cp-1 carry the control_response to an
    /// interrupt it answered just before exiting. This proves they are
    /// AVAILABLE; Task 6 proves the event loop TAKES them.
    #[test]
    fn the_disposal_time_final_drain_also_hands_over_its_lines() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let stdout = sessions_dir.join("t-dev-1.json");
        std::fs::write(&stdout, "{\"type\":\"system\"}\n").unwrap();
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let mut rc = ReadChannelRuntime::new(sessions_dir, 256 * 1024 * 1024).unwrap();
        rc.register(&mut ledger, "t/dev/1").unwrap();
        rc.drain_all(&mut ledger).unwrap();
        assert_eq!(rc.take_stream_lines().len(), 1);

        // The worker answers the interrupt and EXITS. The reap queues the
        // unregister; the last line landed after this wake's drain_all.
        let answer = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"camp-1\"}}";
        std::fs::OpenOptions::new().append(true).open(&stdout).unwrap()
            .write_all(format!("{answer}\n").as_bytes()).unwrap();
        rc.queue_unregister("t/dev/1");
        rc.apply_pending_unregisters(&mut ledger).unwrap();

        let final_lines = rc.take_stream_lines();
        assert_eq!(final_lines.len(), 1, "the final drain handed over the last line");
        assert_eq!(final_lines[0].line, answer, "the control_response survived disposal");
        assert!(!stdout.exists(), "and the file is GONE — the line now exists ONLY here");
        assert_eq!(rc.take_disposed(), vec!["t/dev/1".to_string()]);
    }
```

- [ ] **Step 2: Run and watch both fail.** (`no method named take_stream_lines`.)

- [ ] **Step 3: Implement.**

```rust
/// cp-1: one complete JSON line the drain consumed. `offset_after` is the byte
/// offset PAST it — the cursor a subscriber resumes from (§9).
#[derive(Debug, Clone)]
#[allow(dead_code)] // cp-1: consumed in Task 6 (ingest) and Task 8 (fanout)
pub struct StreamLine {
    pub session: String,
    pub line: String,
    pub offset_after: u64,
}
```
Add to `struct ReadChannelRuntime` (after `parsed_counts`) and initialize in `new()`:
```rust
    /// cp-1: complete lines consumed since the last `take_stream_lines`, in file
    /// order. BOTH `drain_one` callers fill this — `drain_all` AND the
    /// disposal-time final drain in `apply_pending_unregisters` — and the event
    /// loop harvests after EACH (B4).
    stream_lines: Vec<StreamLine>,
    /// cp-1 §4.1: when each session's offset last advanced — `sessions.list`'s
    /// "last activity". The honest signal: the last output campd consumed.
    last_activity: HashMap<String, jiff::Timestamp>,
    /// cp-1 B12: sessions disposed by the last `apply_pending_unregisters` —
    /// Task 8 sends each of their subscribers a terminal `end` frame.
    disposed: Vec<String>,
```
In `drain_one`'s existing `Ok(_v) => { … }` parse arm (read_channel.rs:635-641), keep the `parsed_counts` bump and add the hand-off. **No other line of the loop changes.** (This compiles: `self.parse_errors.push(...)` already performs the same disjoint-field borrow in this loop at read_channel.rs:643 while `t` is live.)
```rust
                    Ok(_v) => {
                        self.parsed_counts
                            .entry(session.to_owned())
                            .and_modify(|c| *c += 1)
                            .or_insert(1);
                        self.stream_lines.push(StreamLine {
                            session: session.to_owned(),
                            line: line.to_owned(),
                            offset_after: new_offset,
                        });
                        self.last_activity
                            .insert(session.to_owned(), jiff::Timestamp::now());
                    }
```
In `unregister`, beside `self.tailed.remove(session)`: `self.last_activity.remove(session);` and `self.disposed.push(session.to_owned());`.

Accessors — each with its `#[allow(dead_code)] // cp-1: consumed in Task N` per the discipline table:
```rust
    pub fn take_stream_lines(&mut self) -> Vec<StreamLine> { std::mem::take(&mut self.stream_lines) }

    /// §4.1: when this session's offset last advanced. None until the first line
    /// lands — the caller uses the registry's `spawned_ts` then, a DEFINED
    /// semantic, not an error being swallowed.
    pub fn last_activity(&self, session: &str) -> Option<jiff::Timestamp> {
        self.last_activity.get(session).copied()
    }

    /// §9: the tailed file and campd's current tail offset — what
    /// `session.subscribe` needs. None when the session is not tailed
    /// (reaped/disposed => an explicit hello error).
    pub fn tail_state(&self, session: &str) -> Option<(PathBuf, u64)> {
        self.tailed.get(session).map(|t| (t.stdout_path.clone(), t.offset))
    }

    /// B12: the sessions disposed since the last call.
    pub fn take_disposed(&mut self) -> Vec<String> { std::mem::take(&mut self.disposed) }
```

- [ ] **Step 4: Run the new tests AND the whole cp-0 suite (nothing may regress).**

Run: `cargo test -p camp --lib daemon::read_channel && cargo test -p camp --test read_channel`
Expected: PASS — the two new tests plus every cp-0 test (offsets, cap breach, disposal ordering, restart cursors).

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/read_channel.rs
git commit -m "feat(read-channel): hand every drained complete line over, from BOTH drain paths (cp-1)"
```

---

## Task 5: `dispatch::write_control` — the write half

Spec: §2 (campd holds the child's stdin as a live pipe), issue #55 (every blocking syscall on the loop carries a deadline).

**Files:** Modify `crates/camp/src/daemon/dispatch.rs` (**shared — ADDITIVE ONLY**: one enum + one method beside `nudge_via_stdin` at dispatch.rs:208).

- [ ] **Step 1: Write the failing tests.** The real scaffolds are `Dispatcher::test_insert_held_cat(...)` (dispatch.rs:352) and `Dispatcher::test_insert_held_sleeper(...)` (dispatch.rs:394 — a worker that never reads its pipe: the PR #51 finding-2 wedge shape). A `Dispatcher` is built with `Dispatcher::new(camp: CampDir, config: CampConfig)`. **Read both scaffolds' real argument lists and match them exactly.**

```rust
    /// cp-1: a control_request goes into the SAME held stdin a nudge uses, with
    /// the SAME bounded write. A session campd holds no pipe for is NoPipe,
    /// never a panic.
    #[test]
    fn write_control_delivers_into_the_held_stdin_pipe() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let mut d = Dispatcher::new(
            crate::campdir::CampDir { root: dir.path().to_path_buf() },
            camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );
        d.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");
        let line = ParentMessage::Interrupt { request_id: "camp-1".into() }.to_line().unwrap();
        assert!(matches!(d.write_control("t/dev/1", &line), ControlWrite::Delivered));
        assert!(matches!(d.write_control("t/nobody/9", &line), ControlWrite::NoPipe));
    }

    /// Issue #55 — the whole reason this method exists. A worker that NEVER READS
    /// its pipe must not wedge campd's single-threaded loop: the write fails at
    /// the STDIN_WRITE_TIMEOUT deadline, and the torn pipe is DROPPED so no later
    /// line can interleave garbage into a half-written control message.
    #[test]
    fn write_control_is_bounded_and_drops_the_torn_pipe() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let mut d = Dispatcher::new(
            crate::campdir::CampDir { root: dir.path().to_path_buf() },
            camp_core::config::CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );
        d.test_insert_held_sleeper(dir.path(), "t/dev/1", "gc-1"); // never reads stdin
        let huge = format!("{}\n", "x".repeat(2 * 1024 * 1024)); // far past any pipe buffer
        let start = std::time::Instant::now();
        let outcome = d.write_control("t/dev/1", &huge);
        assert!(
            matches!(outcome, ControlWrite::Failed(ref e) if e.contains("control write failed")),
            "got {outcome:?}"
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(10), "bounded, not wedged");
        // The torn pipe is GONE: a second write finds no pipe at all.
        assert!(matches!(d.write_control("t/dev/1", "x\n"), ControlWrite::NoPipe));
    }
```

- [ ] **Step 2: Run and watch fail.** (`no method named write_control`.)

- [ ] **Step 3: Implement** — immediately AFTER `nudge_via_stdin`, inside the EXISTING `impl Dispatcher` block:

```rust
/// cp-1: the disposition of a control-message write. The `NudgeOutcome` twin —
/// SEPARATE because a control message has no resume path: `via="none"` is a
/// legitimate answer for a turn (the CLI resumes the session instead), but there
/// is NO way to interrupt a worker campd holds no pipe to. NoPipe is therefore a
/// caller-visible FAILURE, not a designed degrade.
#[derive(Debug)]
#[allow(dead_code)] // cp-1: consumed in Task 6
pub enum ControlWrite { Delivered, NoPipe, Failed(String) }
```
```rust
    /// Write one control-protocol line into the session's held stdin (§2: the
    /// pipe camp already holds). BOUNDED exactly as `nudge_via_stdin` is (issue
    /// #55): an unbounded blocking write into the full pipe of a worker that
    /// stopped reading would wedge campd's single-threaded event loop. On a
    /// bounded failure the pipe may hold a torn partial line, so it is dropped.
    ///
    /// The line MUST come from `control::ParentMessage::to_line` — §2.1: nothing
    /// outside `control.rs` constructs a control message.
    #[allow(dead_code)] // cp-1: consumed in Task 6
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
```

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --lib daemon::dispatch` — the two new tests plus every existing dispatch test.

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/dispatch.rs
git commit -m "feat(dispatch): write_control — the bounded control-message write into the held stdin (cp-1 §2)"
```

---

## Task 6: `session.interrupt` + `session.send_turn`, and the INGEST ORDERING (B4/B5/B6)

Spec: §4.1, §4.2, §7 phase 1 (*"`interrupt` and `send_turn` first — the smallest end-to-end slice through the whole stack"*). D1, D2, D4.

**Files:** Modify `daemon/socket.rs`, `daemon/control.rs`, `daemon/event_loop.rs` (**shared**), `daemon/mod.rs` (**shared**), `cmd/nudge.rs`, `tests/fake-agent.sh`. Create `crates/camp/tests/control.rs`.

- [ ] **Step 1: Pin the new socket wire (failing test).** Append `control_plane_verbs_wire_format_is_pinned` to `socket.rs`'s `mod tests` — asserting `{"op":"session.interrupt","session":"camp/dev/1"}`, `{"op":"session.send_turn","session":"camp/dev/1","text":"status?"}`, `{"ok":true,"request_id":"camp-1"}`, `{"ok":true,"via":"stdin"}`, round-trips both ways, and that **`{"op":"nudge",…}` is now REJECTED** (D4: one verb, not two). DELETE `nudge_wire_format_is_pinned` (socket.rs:672).

- [ ] **Step 2: Run and watch fail.** (`no variant named SessionInterrupt`.)

- [ ] **Step 3: Implement the socket types.** In `Request`, REPLACE `Nudge` (D4):
```rust
    /// cp-1 §4.1: inject one user turn into a live worker's campd-held stdin.
    /// This is `camp nudge`, promoted to the protocol. It still emits
    /// `session.nudged` — the merged vocabulary for "a turn was injected";
    /// renaming the event would churn vocab/fold/cli_nudge for nothing (D4).
    #[serde(rename = "session.send_turn")]
    SessionSendTurn { session: String, text: String },
    /// cp-1 §4.1: stop the current turn. ACK-then-ASYNC (D1) — the answer carries
    /// the request_id; the worker's control_response arrives later on the read
    /// channel as `control.responded`.
    #[serde(rename = "session.interrupt")]
    SessionInterrupt { session: String },
```
(The enum is `#[serde(tag="op", rename_all="snake_case")]`; a per-variant `rename` gives the dotted verb the spec names.) In `Response`, REPLACE `Nudge` with `SendTurn { ok: bool, via: String }` and add `Interrupt { ok: bool, request_id: String }` — both BEFORE `Ok` (the untagged variant-order rule at socket.rs:47). Update `cmd/nudge.rs:42` and its two `Response::Nudge` matches (nudge.rs:47,59). **The `camp nudge` CLI verb itself is unchanged.**

- [ ] **Step 4: Implement the handlers in `control.rs`.** `serve_send_turn` is the `Request::Nudge` arm (event_loop.rs:796-844) **moved verbatim**: deliver → record → respond; `NoPipe ⇒ via:"none"` (the resume path); a post-delivery append failure surfaced to the caller. Then `serve_interrupt` (D1/D2 — the full body is in rev 1's Task 6 and is unchanged: build the line via `ParentMessage::Interrupt`, `dispatcher.write_control`, on `Delivered` append `session.interrupted` and `track_pending`, answer `Response::Interrupt{ok, request_id}`; `NoPipe` is a LOUD `Response::Error` naming "no stdin pipe", because there is no resume path for an interrupt).

`ingest(&mut self, lines: &[StreamLine], dispatcher: &mut Dispatcher) -> Vec<EventInput>` matches `parse_worker_line` over five arms:
- `ControlResponse` ⇒ `self.resolve(...)`, pushing the `Option<EventInput>` when it is `Some` (B6: `None` = already settled, and saying it twice is the false statement).
- `RequestUserDialog` ⇒ write `ParentMessage::DialogRefusal` back through `dispatcher.write_control`, then append `control.failed` naming the outcome (delivered / no pipe / write failed), **keyed on `request_id` so a repeated ask for the SAME id appends once**.
- `CanUseTool` ⇒ `control.failed` whose reason states plainly that the worker is blocked forever holding a dispatch slot and must be killed by the operator. camp takes no automatic action: the flow is structurally unreachable in cp-1 (§5.3.1), and the phase that can answer it (phase 3) also owns §5.3.2's slot rule.
- `Stream(_)` ⇒ nothing here (Task 8 fans it out).
- `Err(ControlWireError)` ⇒ `control.failed`. **Note:** cp-0's `drain_one` only hands over lines that ALREADY parsed as JSON (read_channel.rs:635, the `Ok(_v)` arm), and non-JSON lines are separately surfaced as `patrol.degraded` — so `ingest` never sees one and never double-reports. Do not add a guard for it.

**FAULT DEDUPE (non-blocking note, adopted).** A worker looping on `request_user_dialog`, or spraying malformed control lines, would otherwise drive one synchronous SQLite append per line on the event loop — loud is right, unbounded-loud is a self-DoS. So: dialog/never-sent faults are deduped per `request_id` via `self.resolved`, and a session's unparsable-control-line faults are capped at `MAX_FAULTS_PER_SESSION_PER_WAKE` (8), with the suppressed count named in the last event.

Also define here (needed by `mod.rs` in Step 6): `SUBSCRIBER_BUFFER_BYTES_DEFAULT: usize = 1024 * 1024` and `subscriber_buffer_bytes_from_env(default) -> Result<usize>` reading `CAMP_SUBSCRIBER_BUFFER_BYTES` — the exact `max_stream_bytes_from_env` twin (read_channel.rs:34-50): fail fast on a malformed or zero value, never a silent default.

- [ ] **Step 5: Wire the event loop — THE ORDERING IS THE FIX (B4 + B5).**

`min_deadline` (event_loop.rs:153-159) gains a fourth source (`control.poll_timeout(poll_now)`). Thread `control: &mut ControlRuntime` through `run` → `serve_connection` → `drain_lines` (plus the connection's `Token` through the latter two — Task 8 needs it). Add the two `drain_lines` arms (one line of logic each) and DELETE the `Request::Nudge` arm.

**The post-drain block, restated in full:**
```rust
        read_channel.apply_tracking(ledger)?;
        if let Err(e) = read_channel.drain_all(ledger) {
            eprintln!("campd: read-channel drain_all failed: {e:#}");
        }
        let mut appended = false;
        // (B4, harvest 1 of 2) the lines `drain_all` just consumed.
        appended |= control_step(ledger, control, dispatcher, read_channel, &mut conns, &mut poll)?;

        // ... the EXISTING cap-breach loop and the watch/drain/parse fault
        // events, unchanged (they set `appended` where they used to set
        // `appended_read_channel_events`) ...

        read_channel.persist_offsets(ledger)?;
        // cp-0 left a TODO here for phase 1: the offset must commit AFTER the
        // line's ledger effect. That obligation is now MET — harvest 1 appended
        // `control.responded` above, before this line.

        // The disposal-time FINAL drain (cp-0 review fix 1) — the SECOND caller
        // of `drain_one`. It produces stream lines too, and they are the
        // worker's LAST bytes: in cp-1 those carry the control_response to an
        // interrupt the worker answered just before exiting.
        appended |= read_channel.apply_pending_unregisters(ledger)?;
        // (B4, harvest 2 of 2) THE FIX. Without this, an ordinary
        // "worker answers the interrupt and exits" loses the control_response to
        // the unlink and then manufactures TWO false control.failed events (a
        // "never answered" at the deadline, and a "camp never sent this" if the
        // line is ever re-read). The lines are already in memory here — the
        // unlink cannot take them — but ONLY if we harvest them.
        appended |= control_step(ledger, control, dispatcher, read_channel, &mut conns, &mut poll)?;

        // (B5) ONLY NOW may a deadline expire. cp-0's law (event_loop.rs:406):
        // "correctness never depends on a delivered event" — so a response
        // sitting in the file because its notify was coalesced must be read and
        // ingested BEFORE campd may declare that it never arrived.
        for input in control.expire_pending(Timestamp::now()) {
            ledger.append(input)?;
            appended = true;
        }
        if appended {
            if let Err(e) = settle(/* … */) {
                eprintln!("campd: control/read-channel settle failed: {e:#}");
            }
        }
```
with the shared helper (DRY — one harvest, two call sites):
```rust
/// cp-1 (B4): harvest whatever the last drain consumed — ingest the control
/// messages, fan the rest out to subscribers, emit end-frames for disposed
/// sessions, append the durable events. Called after EVERY `drain_one` caller
/// (`drain_all` AND `apply_pending_unregisters`), because a line that is drained
/// but not ingested is a line the unlink destroys.
fn control_step(
    ledger: &mut Ledger,
    control: &mut super::control::ControlRuntime,
    dispatcher: &mut Dispatcher,
    read_channel: &mut super::read_channel::ReadChannelRuntime,
    conns: &mut HashMap<Token, Conn>,
    poll: &mut Poll,
) -> Result<bool> {
    let lines = read_channel.take_stream_lines();
    let mut appended = false;
    for input in control.ingest(&lines, dispatcher) {
        ledger.append(input)?;
        appended = true;
    }
    // Task 8 adds here: control.fanout(&lines, conns) -> append drop events +
    // deregister dropped tokens; then control.end_sessions(
    // &read_channel.take_disposed(), …) -> B12's terminal frames.
    Ok(appended)
}
```

- [ ] **Step 6: `mod.rs` — construct the runtime and REHYDRATE it (B6).** Beside the read channel (mod.rs:167), rehydrating AFTER `patrol::adopt` (so the ledger is reconciled first):
```rust
    let mut control = control::ControlRuntime::new(control::subscriber_buffer_bytes_from_env(
        control::SUBSCRIBER_BUFFER_BYTES_DEFAULT,
    )?);
    // B6: an interrupt in flight when the last campd died is still in the ledger
    // — and the ledger is the ONLY record of it. Without this, the re-read
    // control_response manufactures a false fault, and a genuinely unanswered
    // request is silently forgotten (the swallowed timeout §2.1 forbids).
    let restored = control.rehydrate(&ledger, jiff::Timestamp::now())?;
    if restored > 0 {
        eprintln!("campd: restored {restored} in-flight control request(s) from the ledger");
    }
```

- [ ] **Step 7: DELETE the module-level `#![allow(dead_code)]` from `control.rs`** (the dead_code discipline). Run `cargo clippy -p camp --all-targets --all-features -- -D warnings` and confirm it passes WITHOUT the attribute. If any item is still unreached, it is either wired here or it does not belong in cp-1.

- [ ] **Step 8: The fake worker's control loop, and the END-TO-END tests.**

`tests/fake-agent.sh` — add before the `FAKE_AGENT_NUDGE_CLOSE` block, documented in the header:
```bash
#   FAKE_AGENT_CONTROL_LOOP   cp-1: after the task line, read stdin forever. A
#                             control_request is answered on stdout with the
#                             control_response the real CLI sends (shape pinned in
#                             tests/fixtures/control/). A plain user turn ends the
#                             loop and the worker closes its bead. This is the
#                             fake's half of the interrupt round trip: campd ->
#                             stdin -> worker -> stdout file -> read channel.
#   FAKE_AGENT_EXIT_AFTER_CONTROL  cp-1 (B4): answer ONE control_request and exit
#                             IMMEDIATELY — the reap-races-the-drain shape.
if [[ -n "${FAKE_AGENT_CONTROL_LOOP:-}" ]]; then
  read -r _task_line
  while read -r line; do
    case "$line" in
      *'"control_request"'*)
        rid="$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')"
        if [[ -z "$rid" ]]; then
          echo "fake-agent: control_request with no request_id: $line" >&2
          exit 95
        fi
        emit_stream "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"still_queued\":[]}}}"
        if [[ -n "${FAKE_AGENT_EXIT_AFTER_CONTROL:-}" ]]; then
          break   # answer and DIE: the EXIT trap emits the terminal result line
        fi
        ;;
      *) break ;;   # a plain user turn: fall through to the close
    esac
  done
fi
```

Create `crates/camp/tests/control.rs`, copying the harness helpers (`munge`, `stdout_path`, `camp`, `camp_ok`, `scaffold`, `fake_agent`, `Daemon`, `connect`, `request`, `events_json`, `wait_until`) **verbatim** from `tests/read_channel.rs:1-180` — the established harness. Add `fn live_session_name(root: &Path) -> String` (the `session.woke` event's `data.name`). Five tests:

1. **`interrupt_round_trips_through_the_read_channel`** — the phase's exit criterion. Sling, wait for `session.woke`, send `{"op":"session.interrupt"}` over the REAL socket, assert `ok` + a `camp-`-prefixed `request_id`, then wait for `session.interrupted{request_id}` **and** `control.responded{request_id, ok:true, verb:"session.interrupt"}` in the ledger — and assert **no `control.failed` was invented** along the way.
2. **`a_worker_that_answers_and_exits_immediately_still_yields_control_responded`** — **B4, the regression this revision exists for.** `FAKE_AGENT_EXIT_AFTER_CONTROL=1`: the worker answers and dies, so its `control_response` is the LAST line in a stdout file campd disposes in the same wake. Assert `session.stopped`/`session.crashed` lands, then `control.responded` STILL lands, and **no `control.failed`**. *Delete harvest 2 of 2 and this goes red exactly there.*
3. **`send_turn_delivers_a_user_turn_into_the_held_pipe`** — `via:"stdin"`, `session.nudged`, and the worker's blocked `read` really unblocks (`FAKE_AGENT_NUDGE_CLOSE` ⇒ `bead.closed`).
4. **`interrupting_a_session_with_no_held_pipe_fails_loudly`** — `ok:false`, the error names "no stdin pipe".
5. **`a_campd_restart_across_an_in_flight_interrupt_invents_no_fault`** — **B6 end to end.** Interrupt, wait for `session.interrupted`, `campd.kill9()` (crash-only; the worker outlives campd, spawn.rs:255), spawn a fresh campd, and assert `control.responded{request_id}` lands **and no `control.failed` exists** — an ordinary restart must not manufacture a protocol fault.

- [ ] **Step 9: Run.** `cargo test -p camp --test control 2>&1 | tail -30` → PASS (5 tests). If test 2 hangs at `control.responded`, harvest 2 of 2 is missing or misplaced.

- [ ] **Step 10: Full suite** (the D4 blast radius: `cmd/nudge.rs`, `cli_nudge.rs`, `daemon_patrol.rs`). `cargo test --workspace` → PASS; `cli_nudge.rs` MUST still pass (the CLI verb is unchanged).

- [ ] **Step 11: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A
git commit -m "feat(control): session.interrupt + session.send_turn, ingested on every drain path (cp-1 §4.1)"
```

---

## Task 7: `sessions.list`

Spec: §4.1 (name, agent, rig, bead, state, last activity, blocked), §4.2 (by NAME, never by pid), §4.3 (campd owns the truth; clients are stateless renderers).

**Files:** Modify `daemon/socket.rs`, `daemon/control.rs`, `daemon/patrol.rs` (one accessor), `daemon/event_loop.rs` (one arm). Test: `tests/control.rs`.

- [ ] **Step 1: Write the failing tests.** In `socket.rs`, pin `{"op":"sessions.list"}`, the `SessionInfo` field order, and the full `Response::SessionsList` line — **asserting the serialized response contains no `pid`** (§4.2: *"a protocol that hands out pids is a protocol that cannot cross a machine boundary"*). In `tests/control.rs`, `sessions_list_reports_live_sessions_by_name`: sling, wait for `session.woke`, then assert exactly one session with `agent:"dev"`, `rig:"gc"`, `state:"working"`, `blocked:false`, an RFC3339 `last_activity`, a `gc-`-prefixed `bead`, a `/dev/`-containing `name`, and `s.get("pid").is_none()`.

- [ ] **Step 2: Run and watch both fail.** (`no variant named SessionsList`.)

- [ ] **Step 3: Implement.**

`socket.rs`: `SessionInfo { name, agent, rig: Option<String>, bead: Option<String>, state: String, last_activity: String, blocked: bool }` (field order IS wire order — B1); `Request::SessionsList` with `#[serde(rename = "sessions.list")]`; `Response::SessionsList { ok, sessions }` placed FIRST among the untagged variants (its `sessions` key is the discriminator).

`patrol.rs` — one accessor beside `stalled_count` (patrol.rs:233), using the actual field `stalled_count` counts:
```rust
    /// cp-1 §4.1: is this session in patrol's stalled set? `sessions.list`'s
    /// state column. (`stalled_count` counts them; this names one.)
    pub fn is_stalled(&self, session: &str) -> bool { self.stalled.contains(session) }
```

`control.rs::serve_sessions_list(ledger, patrol, read_channel) -> Response` — answers from the **LEDGER's** registry (`live_sessions()`), not campd's in-memory child map: an ADOPTED worker from a previous campd life is a live session too, and a client must see it (§4.3). `state` is **exactly two values in cp-1**: `"stalled"` when patrol's ladder has fired, `"working"` otherwise. (A third, `"blocked"`, arrives with phase 3.) `blocked` is `false`, and its producer is phase 3 (§5.3): no camp worker is spawned with `--permission-prompt-tool stdio` (§5.3.1), so the flow is structurally unreachable — and a `can_use_tool` that arrives anyway is a LOUD `control.failed`, never a quietly-flipped bit. The field is in the shape because §4.1's shape requires it: **a protocol field awaiting its producer, not a guess.** `last_activity` = `read_channel.last_activity(name)`, falling back to the registry's `spawned_ts` before the first line lands.

`event_loop.rs` — one arm delegating to it.

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --lib daemon::socket && cargo test -p camp --test control`

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A && git commit -m "feat(control): sessions.list — every session by name, never by pid (cp-1 §4.1/§4.2)"
```

---

## Task 8: `session.subscribe` — a streaming connection MODE (D6′; B7–B13)

Spec: §4.4 (all four bullets), §9 (byte-offset cursors; a reaped stream is an explicit error), §8 (the backpressure obligation), §5.2 (*"Detach freely"*).

**Files:** Modify `daemon/control.rs`, `daemon/socket.rs` (wire types only — **no client API**, B2), `daemon/event_loop.rs`, `tests/fake-agent.sh`, `tests/control.rs`.

**The frame wire — TAGGED FROM BIRTH (B12), because cp-2 and cp-4 inherit it and a retrofitted terminal frame would be a breaking change:**
```json
{"frame":"event","session":"t/dev/1","offset":123,"event":{ …the raw stream-json line, verbatim… }}
{"frame":"end","session":"t/dev/1","offset":456,"reason":"stopped"}
```
After the `end` frame campd pumps and closes the connection (EOF).

**Interfaces produced:**
```rust
pub const HISTORY_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_SUBSCRIBERS: usize = 16;   // bound the COUNT, not just the bytes
pub enum PumpOutcome { Ok, Drop(EventInput), Gone }
impl ControlRuntime {
    pub fn serve_subscribe(&mut self, token: Token, session: &str, cursor: Option<u64>,
                           read_channel: &ReadChannelRuntime) -> Response;
    pub fn pump(&mut self, token: Token, conn: &mut Conn) -> PumpOutcome;
    pub fn fanout(&mut self, lines: &[StreamLine], conns: &mut HashMap<Token, Conn>)
        -> (Vec<Token>, Vec<EventInput>);
    pub fn end_sessions(&mut self, sessions: &[String], ledger: &Ledger,
                        conns: &mut HashMap<Token, Conn>) -> Vec<Token>;
    pub fn forget(&mut self, token: Token);
    pub fn is_subscriber(&self, token: Token) -> bool;
    pub fn subscriber_count(&self) -> usize;   // #[allow(dead_code)] — test observable, PERMANENT
}
// socket.rs (wire types only)
Request::SessionSubscribe { session: String, cursor: Option<u64> }
Response::Subscribed { ok: bool, subscription: String, cursor: u64 }
```

- [ ] **Step 1: Write the failing tests.**

`socket.rs`: `subscribe_wire_format_is_pinned` — the request with `cursor: Some(4096)` and with `cursor: None` (⇒ `"cursor":null`), and the `Subscribed` hello.

`control.rs` unit test — **B13(a), the pin whose absence the panel caught**:
```rust
    /// §4.4/B13: the FRAME is a shape camp SENDS (and cp-2/cp-4 will parse), so
    /// it is pinned like every other. Tagged from birth: the `end` frame was
    /// DESIGNED IN, not retrofitted (B12).
    #[test]
    fn subscribe_frame_shapes_are_pinned() {
        let event = event_frame("t/dev/1", 123, r#"{"type":"system","subtype":"init"}"#).unwrap();
        assert_eq!(
            String::from_utf8(event).unwrap(),
            "{\"frame\":\"event\",\"session\":\"t/dev/1\",\"offset\":123,\
             \"event\":{\"type\":\"system\",\"subtype\":\"init\"}}\n"
        );
        let end = end_frame("t/dev/1", 456, "stopped").unwrap();
        assert_eq!(
            String::from_utf8(end).unwrap(),
            "{\"frame\":\"end\",\"session\":\"t/dev/1\",\"offset\":456,\"reason\":\"stopped\"}\n"
        );
    }
```
Plus a unit test for the HARD cap (B10): a `Subscriber` at `cap-1` bytes offered a frame that would cross the cap is dropped **before** the append, and the `subscriber.dropped` event's `buffered_bytes` is the ATTEMPTED size (the high-water), not the pre-append size.

`tests/control.rs` — the subscribe client is a **test helper** (B2: there is NO `socket::subscribe`; phase 2's `camp watch` is the first real client):
```rust
/// The subscribe client, as a TEST helper (B2). `camp` is a binary crate, so a
/// public wire client with no consumer would be dead code and the clippy gate
/// would reject it. This is the idiom every existing harness uses (raw
/// UnixStream + BufReader — tests/read_channel.rs).
///
/// The HELLO is read under REQUEST_TIMEOUT (5 s, socket.rs:148). AFTERWARDS the
/// read deadline is CLEARED — that is §4.4's timeout exemption, and the reason a
/// quiet stream is not a wedged daemon.
struct SubClient {
    reader: BufReader<UnixStream>,
    stream: UnixStream,
    subscription: String,
    cursor: u64,
}
impl SubClient {
    fn open(root: &Path, session: &str, cursor: Option<u64>) -> std::io::Result<SubClient>;
    /// The next frame, or None at EOF. Returns `end` frames too (the caller
    /// decides) — the B12 test needs to SEE one.
    fn next_frame(&mut self) -> Option<serde_json::Value>;
}
```
Seven integration tests:

1. **`a_wedged_campd_fails_the_subscribe_hello_fast`** — the EXIT CRITERION. A bare bound `UnixListener` IS the wedge simulator (socket.rs:751): the kernel backlog accepts, nothing answers. `SubClient::open` must return `Err` of kind `WouldBlock`/`TimedOut` **within REQUEST_TIMEOUT**, never hang. Assert elapsed < 15 s.
2. **`a_subscription_survives_a_quiet_period_longer_than_request_timeout`** — **B13(b).** Open at the tail, sleep **6 s** (> the 5 s `REQUEST_TIMEOUT`), then trigger an interrupt and assert a frame still arrives. The subscription is timeout-exempt after the hello.
3. **`a_subscriber_gets_history_from_its_cursor_then_follows_live`** — **B11.** Subscribe at cursor 0 to a session that has ALREADY emitted its `system/init` line, and assert the history frame arrives **with no new activity** (it must not sit in campd's memory waiting for a WRITABLE edge that was consumed at accept). Then interrupt and assert the live `control_response` frame follows. **D6′: ordinary history is NEVER refused.**
4. **`a_subscriber_gets_an_end_frame_when_its_session_ends`** — **B12.** `FAKE_AGENT_EXIT_AFTER_CONTROL=1`; drain frames until `{"frame":"end"}`; assert it names the session and a `reason`, and that EOF never arrives without one.
5. **`a_hung_up_subscriber_is_forgotten_and_is_never_libeled_as_backpressure`** — **B7.** Open a subscription, DROP it (the operator's Ctrl-C), drive three wakes, assert campd still answers `status` promptly and that **no `subscriber.dropped` event exists**. A normal detach is not a fault (§5.2).
6. **`a_subscriber_that_stops_reading_is_dropped_loudly_and_campd_keeps_serving`** — **§8 + B8.** Env: `FAKE_AGENT_SPAM_ON_TURN=8000`, `CAMP_SUBSCRIBER_BUFFER_BYTES=512`. Subscribe at the **tail** (`cursor: None` ⇒ empty history ⇒ a clean hello), read NOTHING, then `send_turn` to trigger the spam. Assert `subscriber.dropped{session, cap_bytes: 512, buffered_bytes > 512}`, then assert campd answers a `status` on a FRESH connection in < 5 s (it never blocked). **The 8000 lines ≈ 720 KB are chosen to exceed kernel-socket-buffer + app-cap on both platforms — macOS `net.local.stream.sendspace` ≈ 8 KiB, Linux ≈ 200 KiB. A smaller spam is absorbed entirely by the kernel, `sub.out` never grows, and the test becomes theatre.**
7. **`a_cursor_into_a_reaped_stream_or_past_the_tail_is_an_explicit_error`** — §9. Both errors, both explicit, neither a silent truncation nor a silent seek to EOF.

- [ ] **Step 2: Run and watch them fail.** (`bad request: unknown variant session.subscribe`.)

- [ ] **Step 3: Implement the registry (D6′).**

```rust
/// D6′/B9: history is read in bounded chunks, only as the socket drains — never
/// slurped. A 256 MiB stream file (the max_stream_bytes ceiling) must never
/// become a 256 MiB allocation or a 256 MiB synchronous read on campd's
/// single-threaded event loop.
pub const HISTORY_CHUNK_BYTES: usize = 64 * 1024;

/// §4.4 bounds BYTES PER CONNECTION, but nothing bounded the CONNECTION COUNT —
/// and 20 subscribers x 1 MiB is the entire <20 MB RSS budget the perf gate
/// asserts. A subscribe past this cap is an explicit, loud error at the hello.
pub const MAX_SUBSCRIBERS: usize = 16;

struct Subscriber {
    id: String,
    session: String,
    /// D6′: the open stream file. Held across disposal ON PURPOSE — on Unix an
    /// unlinked inode survives while an fd is open, so a subscriber still
    /// catching up when its session is reaped can finish its history.
    file: std::fs::File,
    /// The next byte of HISTORY to read. While `history_cursor < caught_up_at`
    /// the subscriber is CATCHING UP and live fanout lines are IGNORED — they
    /// are already in the append-only file and the history reader will reach
    /// them, so nothing is duplicated and nothing is reordered.
    history_cursor: u64,
    /// The tail offset at hello: where catch-up ends and live begins.
    caught_up_at: u64,
    /// Bytes queued for this socket. HARD-capped: a frame that would cross
    /// `subscriber_buffer_bytes` drops the subscriber BEFORE it is appended
    /// (B10 — the old code appended a whole wake's drain and only THEN tested
    /// the cap, which made the cap soft by up to max_stream_bytes).
    out: Vec<u8>,
    /// The largest `out` WOULD have reached — reported as `buffered_bytes` in
    /// `subscriber.dropped` (§4.4: "naming the session and the high-water mark").
    high_water: usize,
}
```
`event_frame(session, offset, raw_line) -> Option<Vec<u8>>` and `end_frame(session, offset, reason) -> Option<Vec<u8>>` build the two pinned shapes with `#[derive(Serialize)]` structs (B1 — declaration order).

**`serve_subscribe`:** (1) `MAX_SUBSCRIBERS` reached ⇒ explicit error; (2) `read_channel.tail_state(session)` is `None` ⇒ **not tailed** (never existed, or reaped and disposed) ⇒ explicit error citing §9; (3) `cursor > tail` ⇒ explicit error ("past the N bytes campd has consumed"); (4) **no history-size check** (D6′ — ordinary history is never refused); (5) open the file, insert the `Subscriber` with `history_cursor = cursor.unwrap_or(tail)` and `caught_up_at = tail`, return the hello. **It registers; it never writes.**

**`pump(token, conn)` — B11's fix, and the ONLY place bytes reach a socket:**
```
loop {
    if out is empty && history_cursor < caught_up_at {
        read <= HISTORY_CHUNK_BYTES from `file` at history_cursor; split COMPLETE
        lines; frame each, appending UNDER THE HARD CAP (a frame that would cross
        it => return Drop(subscriber.dropped)); advance history_cursor
    }
    if out is empty { return Ok }                     // nothing to send
    match write(out) {
        Ok(n)        => drain n,
        Ok(0)/EPIPE/ECONNRESET => return Gone,
        WouldBlock   => return Ok,                    // the kernel is full; the
                                                      // WRITABLE edge re-arms and
                                                      // calls us again
    }
}
```
Called at exactly **three** sites: (a) immediately after the hello is written, (b) on every WRITABLE readiness for a subscriber token, (c) after every `fanout`. **Invariant, stated because `respond()` uses `write_all` on a NON-BLOCKING stream (event_loop.rs:997) and a WouldBlock there drops the connection: the hello must be the FIRST bytes on the socket, and nothing may be buffered before it.**

**`fanout(lines, conns)`:** for each subscriber — if it is still CATCHING UP, skip (the history reader will get those bytes from the file); otherwise append each matching line's frame **under the hard cap**, then `pump`. Returns the tokens to close and the `subscriber.dropped` events (`buffered_bytes: high_water`, `cap_bytes`).

**`end_sessions(sessions, ledger, conns)` (B12):** for every subscriber of a disposed session, append the `end` frame (exempt from the cap — it is the last thing that connection will ever receive), `pump` once, and return the token so the event loop deregisters and drops it. The `reason` comes from `ledger.session_status(name)` (mod.rs:341): `stopped` / `crashed` / `capped`.

- [ ] **Step 4: Wire the event loop — B7's fix is here.**

- `struct Conn` → `pub(super) struct Conn { pub(super) stream: UnixStream, pub(super) buf: Vec<u8> }`.
- The accept arm registers `Interest::READABLE | Interest::WRITABLE`. (Precisely: edge-triggered epoll/kqueue reports writability ONCE at registration — an **accept-time** cost, not an idle one. That already-consumed edge is exactly why the hello's history needs an explicit `pump` — B11.)
- **The token arm — NO SHORT-CIRCUIT (B7).** If `control.is_subscriber(token)`, `pump` FIRST (a WRITABLE wake is why we are here) and handle `Drop`/`Gone` — **then fall through into `serve_connection` exactly like any other connection.** cp-0's existing `ReadStop::Eof ⇒ ConnState::Closed` is what detects a hangup; rev 1's short-circuit bypassed it, which is why a detached subscriber leaked an fd and a buffer forever and was later libeled as a backpressure drop.
- `control.forget(token)` on EVERY close path: `ConnState::Closed`, the error arm, `PumpOutcome::Gone`, and a backpressure drop. **A normal detach appends NO event** (§5.2 "Detach freely" — the ledger records faults, not client lifecycle).
- The new `drain_lines` arm: `serve_subscribe` → `respond(hello)` → `pump` (site (a)).
- In `control_step` (Task 6), after `ingest`: `fanout` (append the drop events; deregister the dropped tokens), then `end_sessions(&read_channel.take_disposed(), ledger, conns)` (B12) and deregister those tokens too.

`tests/fake-agent.sh` — the B8 mode:
```bash
#   FAKE_AGENT_SPAM_ON_TURN  cp-1 (B8): on the first USER TURN (a send_turn),
#                            emit N stream-json lines. The spam MUST come after
#                            the subscriber is registered, or the backpressure
#                            gate tests nothing (at worker start it is all
#                            drained before any subscriber exists).
```
It reads the task line, blocks on stdin, and on the first non-control line emits N `{"type":"assistant",…}` lines, then closes the bead.

- [ ] **Step 5: Run.** `cargo test -p camp --test control 2>&1 | tail -40` → PASS (12 tests in the file).

- [ ] **Step 6: Full suite + commit.**
```bash
cargo test --workspace 2>&1 | tail -20
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A
git commit -m "feat(control): session.subscribe — streamed history, a hard cap, drop-loudly, end-framed (cp-1 §4.4/§9)"
```

---

## Task 9: the §4.3 perf gate grows N idle subscribers

Spec: §4.3 (*"extend the `make perf` idle gate to hold M quiescent workers with tailed stdout files and N connected subscribers … and assert the same 0.0% CPU / <20 MB RSS numbers. Then §4.3 is a measured property, not an argument."*). cp-0 built the M half and its gate deferred the N half to the phase that builds `subscribe` — **this one** (the lead has confirmed the claim).

**Files:** Modify `crates/camp/tests/perf_daemon.rs`.

- [ ] **Step 1: Extend the EXISTING idle gate** (one measured property, one test — do not add a second gate). `perf_daemon.rs` cannot link `daemon::socket` (B2); it already talks to campd over a raw `UnixStream`. Open **N = 4** raw connections, send `{"op":"session.subscribe","session":"<s>","cursor":null}` on each (joining at the TAIL ⇒ no history, no traffic), read each hello, and HOLD them open — reading nothing, with nothing to read — across the existing idle window. Assert the existing 0.0% CPU-delta and <20 MB RSS numbers, unchanged.

```rust
    // cp-1 §4.3: N CONNECTED SUBSCRIBERS, held open, on QUIESCENT sessions. A
    // subscription must cost ZERO wakeups when its session is quiet: campd sleeps
    // on the read-channel self-pipe, a quiet worker writes nothing, so no notify
    // event fires and no fanout runs. This is the property herdr could not offer
    // (its events.wait is a 100 ms sleep loop with no fd to block on). If this
    // gate goes RED on CPU, something in the subscriber path is waking campd with
    // nothing to do — that is a REAL invariant-1 bug and it gets FIXED, never
    // accommodated.
```

- [ ] **Step 2: Run the perf gate (LOCAL-ONLY, per AGENTS.md).** `make perf 2>&1 | tail -30` → PASS: 0.0% CPU delta, <20 MB RSS, with M tailed workers AND N idle subscribers.

- [ ] **Step 3: Commit.**
```bash
git add crates/camp/tests/perf_daemon.rs
git commit -m "test(perf): the idle gate now holds N connected subscribers (cp-1 §4.3)"
```

---

## Task 10: the $0 real-claude gate — camp's own bytes, and the NO-INITIALIZE arm (B14/B15)

Spec: §2.1 (fixtures), §8 (*"Without the real layer, §2.1's mitigations are theatre: fixtures pin what camp SENDS and PARSES, never what the CLI ACCEPTS and EMITS"*), §9.

**Files:** Modify `crates/camp/tests/claude_compat.rs`.

- [ ] **Step 1: Read the evidence that settles B15 — then encode it as a standing gate.** The panel required proof that camp's SHIPPED configuration (an interrupt with NO `initialize` ever sent) is acked by the real CLI, because every recorded ack in the repo is POST-initialize and `FAKE_AGENT_CONTROL_LOOP` acks anything — §8's named trap: *"a fake ignores argv, ignores the protocol, and agrees with whatever camp does."*

**This was run against the pinned CLI on 2026-07-13 and it PASSES.** Reproduce:
```bash
export CLAUDE_CONFIG_DIR=$(mktemp -d)   # hermetic: `verbose` defaults to false
printf '{"type":"control_request","request_id":"camp-b15","request":{"subtype":"interrupt"}}\n' \
  | claude -p --output-format stream-json --verbose --input-format stream-json \
           --session-id 7bd2befc-b018-4080-8738-429d541b3646
```
Verbatim output (claude 2.1.207), exit 0, empty stderr:
```
{"type":"control_response","response":{"subtype":"success","request_id":"camp-b15","response":{"still_queued":[]}}}
```
**Ruling (b) is therefore satisfiable and is TAKEN:** camp does not send `initialize` in cp-1, and the $0 gate PROVES the exact configuration camp ships. (The `subtype!=="initialize"` rejection that does exist in the binary belongs to the `[bridge:repl]` Remote-Control transport — its error string is *"This session is outbound-only. Enable Remote Control locally to allow inbound control."* — and has nothing to do with camp's stdio path.)

- [ ] **Step 2: Make the gate send camp's OWN bytes, and add the no-initialize arm.** `camp` is a binary crate, so an integration test cannot call `ParentMessage::to_line` (the constraint `gate_core_flags_match_build_spec_held_stream_arm` already works around at claude_compat.rs:132). **The fixture is the shared truth:** Task 1's unit test asserts `ParentMessage::Interrupt{id}.to_line() == interrupt_request.json`, and this gate sends `interrupt_request.json` to the real CLI. Transitively, **the bytes camp produces are the bytes the real CLI accepts.**

**Be precise about the claim (B14):** this does NOT make `interrupt_request.json` a *recorded* shape — camp authored it. What the gate proves is **ACCEPTANCE**: the real pinned CLI takes these exact bytes and acks them. `PROVENANCE.md` says exactly that, and no more.

Replace the hand-written literal (claude_compat.rs:387-390) with `const INTERRUPT_FIXTURE: &str = include_str!("fixtures/control/interrupt_request.json");` + `fn interrupt_line(id: &str) -> String` (templating `camp-fixture-1` → `id`), and add a CI-runnable guard `the_interrupt_fixture_is_a_well_formed_control_request` (asserts `type`, `request.subtype`, the templated `request_id`, and that the fixture id is gone). Then add the third `#[ignore]`d, `CAMP_COMPAT=1`-gated arm:

```rust
/// B15 — THE CONFIGURATION CAMP ACTUALLY SHIPS. cp-1 defers the `initialize`
/// handshake to phase 3 (§9's stated purpose for it — redelivering
/// `pending_permission_requests` — cannot exist before phase 3 wires
/// `--permission-prompt-tool stdio`, §5.3.1). But EVERY interrupt ack recorded
/// anywhere in this repo is POST-initialize, and the fake worker acks anything —
/// §8's named trap verbatim. So: no initialize, ever. Just camp's own interrupt
/// bytes, straight at the real pinned CLI, before any turn. $0 (no turn is sent).
///
/// If this ever goes RED, cp-1's interrupt path is broken against the real CLI
/// and camp MUST start sending `initialize` (§9's "Camp sends it anyway"). Do NOT
/// paper over it by adding the handshake to this test.
#[test]
#[ignore = "real-claude $0 gate: run via `make compat` (CAMP_COMPAT=1)"]
fn no_initialize_pre_turn_interrupt_is_acked() {
    assert_eq!(std::env::var("CAMP_COMPAT").as_deref(), Ok("1"));
    let claude = resolve_claude();
    assert_eq!(claude_version(&claude), PINNED_VERSION.trim());
    let (mut cmd, _cfg) = claude_command(&claude, &held_stream_flags(SESSION_ID, true));
    let mut worker = Worker { child: cmd.spawn().unwrap() };
    let rx = stdout_lines(&mut worker.child);
    // NO initialize handshake. This is exactly what campd does today.
    send(&mut worker.child, &interrupt_line("camp-no-init"));
    await_success(&rx, "camp-no-init");
    eprintln!("[compat] pre-turn interrupt acked with NO initialize — cp-1's shipped config");
    worker.child.stdin.take(); // EOF
    assert!(wait_within_timeout(&mut worker.child).success());
}
```
Also update `claude_compat_zero_cost` to send `interrupt_line("camp-compat-interrupt")` instead of its literal.

- [ ] **Step 3: Run the CI-runnable half.** `cargo test -p camp --test claude_compat` → PASS (the ignored gates stay ignored).

- [ ] **Step 4: Run the $0 gate locally.** It costs $0 (no turn). If the installed `claude` does not match `ci/claude-compat/CLAUDE_VERSION`, it fails loudly by design — **do NOT widen the pin**; report the mismatch to the lead.

Run: `make compat 2>&1 | tail -30`
Expected: PASS, printing `[compat] pre-turn interrupt acked with NO initialize — cp-1's shipped config`. If no pinned `claude` is available, SAY SO in the PR — never claim the gate ran.

- [ ] **Step 5: Commit.**
```bash
git add crates/camp/tests/claude_compat.rs
git commit -m "test(compat): the \$0 gate sends camp's own bytes and proves the no-initialize interrupt (cp-1 §8)"
```

---

## Task 11: gates, PR, honest description

- [ ] **Step 1: Rebase onto main.**
```bash
git fetch origin && git rebase origin/main
```
`event_loop.rs` and `dispatch.rs` are shared with compat-2. The **four non-additive** `event_loop.rs` touches (the `min_deadline` nesting, `Conn`'s visibility, the accept `Interest`, the parameter/Token threading) are the likely conflict sites — resolve by keeping BOTH sides. If a conflict cannot be resolved that way, STOP and ask the lead.

- [ ] **Step 2: Prove the dead_code discipline held (B3).**
```bash
! grep -rn "wired in Task\|consumed in Task" crates/camp/src/ \
  || { echo "TEMPORARY dead_code allows survived — remove them"; exit 1; }
```
Expected: no matches. The only surviving `#[allow(dead_code)]`s are the two PERMANENT ones (`subscriber_count`; the four `fold.rs` audit payload structs).

- [ ] **Step 3: The three gates, in order.**
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Do not proceed on a single failure.

- [ ] **Step 4: The local-only gates** (this PR adds standing state to campd and a new real-CLI claim).
```bash
make perf 2>&1 | tail -20
make compat 2>&1 | tail -20
```

- [ ] **Step 5: Push and open the PR.** The body MUST carry these four honesty statements:

1. **The exit-criteria table** (criterion → the named test that proves it).
2. **"After cp-1, an operator still cannot interrupt anything by hand."** No `camp interrupt`, no `camp sessions`, no subscribe CLI — cp-1 ships the protocol and its proofs; phase 2 ships the first human client. `interrupt` "works end to end" **between campd and a worker**, not between a human and a worker.
3. **The unverified claim, stated plainly:** *"Every interrupt exercised anywhere in this repo — fake or real — is PRE-TURN (a no-op interrupt whose ack carries `still_queued:[]`). Whether the CLI reads control messages from stdin WHILE A TURN IS STREAMING — the operationally meaningful interrupt, stopping a RUNNING turn — is untested at every layer and cannot be tested at $0, because it requires a real turn. **cp-1 proves the TRANSPORT; the mid-turn semantics of interrupt are UNPROVEN against the real CLI.** The paid `make e2e` tier (§8) is where that gets settled, and it is a named obligation for the phase that needs it."*
4. **Fixture provenance:** every fixture is labelled `recorded-from-CLI-2.1.207`, `derived-from-CLI-2.1.207`, or `camp-authored` in `tests/fixtures/control/PROVENANCE.md`; `interrupt_request.json` is camp-authored and the $0 gate proves **acceptance**, not recording; and `dialog_refusal_response.json` carries an explicit **phase-3 validation obligation** (camp only sends it under the stdio flag, which cp-1 does not set, so no gate here can exercise it — and if its shape is wrong the worker hangs forever, the precise outcome §9 exists to prevent).

- [ ] **Step 6: CI to green.** `gh pr checks --watch`. Work is NOT complete until it is.

- [ ] **Step 7: Report to the lead** — plan doc path, branch, pushed SHA, PR number, and whether `make perf` / `make compat` ran locally and what they said. Never claim a gate ran that did not.

---

## Self-review against the contract

| Contract item | Task |
|---|---|
| §2/§2.1 — one module owns the wire; shapes pinned by fixtures; failures loud | 1 (module + labelled fixtures), 3 (never-arrived ⇒ durable fault; a restart neither lies nor forgets), 6 (`ingest`: unrecognized/unanswerable control messages ⇒ `control.failed`, deduped) |
| §4.1 `sessions.list` / `session.send_turn` / `session.interrupt` / `session.subscribe` | 7 / 6 / 6 / 8 |
| §4.4 — per-connection buffering, 1 MiB HARD cap, drop-loudly with `subscriber.dropped`, hello within `REQUEST_TIMEOUT`, timeout-exempt after | 8 — **one test each, and all four now exist** |
| §8 fixture tests / backpressure test | 1 + 10 / 8 (B8: deterministic at last) |
| §4.3 perf obligation (N subscribers) | 9 |
| §9 — byte-offset cursors; a reaped stream is an explicit error; ordinary history is NOT | 8 (D6′) |
| Exit: interrupt + send_turn end to end over the real socket vs a fake worker | 6 — **including B4's answer-and-exit race and B6's restart** |
| Exit: a wedged-campd subscribe fails fast at the hello | 8 |
| Exit: fixtures pin every shape camp sends or parses | 1 (interrupt, dialog refusal, user turn OUT; control_response ok/err, can_use_tool, request_user_dialog, stream IN) + 8 (**the subscribe frames — B13's missing pin**) + 10 |
| Exit: CI green | 11 |

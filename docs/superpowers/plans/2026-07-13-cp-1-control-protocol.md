# cp-1: the control protocol — one module owns the wire, four verbs on the socket — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (this stream is planning-only; a FRESH implementer session executes after plan-gate APPROVE). Steps use checkbox (`- [ ]`) syntax. Branch: `cp-1-control-protocol`.

**Goal:** Give campd a control plane: ONE module that owns the undocumented `claude` control wire format (pinned by fixtures whose provenance is labelled), and the first four socket verbs — `sessions.list`, `session.send_turn`, `session.interrupt`, `session.subscribe` — with `interrupt`'s `control_response` round-tripping back over cp-0's read channel, and `subscribe` as a bounded, drop-loudly, streaming connection MODE.

**Architecture:** `crates/camp/src/daemon/control.rs` is the ONLY place in camp that constructs or parses a control message (spec §2.1). It holds `ControlRuntime`: the pending-request table (rebuilt from the ledger at startup; its deadline joins `min_deadline` and is RESET by session activity), the subscriber registry (**one monotone byte cursor per subscriber, fed only from the stream file, never past what campd has drained** — rev 3's central simplification), and every socket-verb handler body, so `event_loop.rs`'s new arms are one-line delegations. campd writes control requests into the worker's already-held stdin (the `nudge_via_stdin` bounded-write mold); the worker's `control_response` returns as a line in its stdout file, which cp-0's `ReadChannelRuntime` tails to EOF on every wake.

**Tech Stack:** Rust (workspace edition), `mio`, `serde`/`serde_json`, `uuid`, `jiff`, `rusqlite` via `camp_core::ledger`, `tempfile` (dev-dep). **No new dependencies. No new cargo features** — and rev 3 makes that constraint *achievable* (C2) instead of merely asserted.

---

## What changed in rev 3, and why (after the rev-2 REJECT at a2ca188)

Rev 2 closed **B2, B5, B6, B7, B8, B9, B11, B13, B15** (panel-verified). D1–D5 and the architecture hold. The panel then found that **four of rev 2's fixes each introduced a new defect inside the answer to a rev-1 defect** (C5, C6, C7, C8 live in the fixes for B4, B9/B10, B12), each invisible to the very test written to prove the fix worked.

**Standing instruction, adopted:** every fix below states **what new failure it permits, and the test that catches THAT** — not the test that proves the old bug is gone. That is the last column.

**The lead's correction to B4 is accepted and carried:** the `control_response` of an answer-and-exit worker is **not** lost forever. `stream_lines` is `mem::take`-drained and the post-drain block runs on every wake; and the reap appends `session.stopped`/`session.crashed` **before** `settle`, so the unregister is queued before `drain_all`, which reads the worker's final bytes while the session is still in `tailed` (merged law: `read_channel.rs:258-273`; merged test: `read_channel.rs:509 a_workers_final_stdout_line_is_drained_before_the_reap_disposes_the_file`). **Harvest 2 stays — as defense-in-depth, with an honest justification (C5).** Rev 2's "delete harvest 2 and it goes red" claim is **WITHDRAWN**.

| # | Defect | Fix, and where | **What the fix could newly break, and the test for it** |
|---|---|---|---|
| **C1** | The B1 fix reintroduced B1: `spawn::user_message` is still `serde_json::json!` (spawn.rs:105-113), so its byte pin can never go green. **Verified by running it:** produced `{"message":{"content":"status?","role":"user"},"type":"user"}` vs the fixture's CLI-ordered bytes. Also a provenance falsehood — `user_turn.json` was labelled *recorded-from-CLI* but is bytes camp SENDS, and not the bytes it sends. | **Task 1 — option (a), chosen.** `user_turn.json` is rewritten to `json!`'s ACTUAL (alphabetical) output and relabelled `camp-authored (ACCEPTED by CLI 2.1.207; in production since Phase 8)`. **`spawn.rs` is NOT touched** — option (b) would change the bytes every production dispatch already sends: a behavioural change with no upside, in a file cp-1 does not own. | The fixture now pins bytes that are *correct but ugly*; a future dev may "tidy" `user_message` into a struct and silently change the launch wire. **The pin IS that test** — it goes red on any reordering, and a comment on the fixture says exactly that. |
| **C2** | `subscribe_frame_shapes_are_pinned` also cannot pass: the nested `event` is a raw stream-json line, and ANY `Value` round-trip sorts it. **Verified:** produced `…"event":{"subtype":"init","type":"system"}`. The clean escape (`serde_json/raw_value`) needs a cargo feature the Global Constraints forbid — the implementer was boxed in, on the wire cp-2/cp-4 inherit. | **Task 8 — byte-splicing, specified exactly.** `event_frame` builds the prefix with a `#[derive(Serialize)]` struct and **splices the worker's line in VERBATIM**, never re-serializing it. No new cargo feature — and it makes the stronger guarantee a subscriber actually needs: *the bytes it sees are the bytes the worker wrote.* | Splicing emits invalid JSON if the raw line is not a JSON object. `event_frame` therefore **validates before splicing** and returns `None` otherwise (matching cp-0's `Ok(_v)` arm, so the history path and the live path agree). Test: `event_frame_splices_verbatim_and_refuses_a_non_object_line`. |
| **C3** | **The plan's primary TDD command does not run.** Seven `cargo test -p camp --lib …` → *"no library targets found in package `camp`"*. Uniquely nasty: it exits nonzero, so a diligent implementer records a FALSE RED and marches on. | **All seven replaced with `cargo test -p camp --bins …`** — **verified by running it** (`--bins daemon::read_channel` → `22 passed`). | A `--bins` filter that matches nothing runs 0 tests and exits 0 — a false GREEN. **Every run step now states the expected test COUNT**, and Task 11 checks the totals. |
| **C4** | The dead_code table tracked ITEMS, not FIELDS. `ControlRuntime.subscriber_buffer_bytes` (stored Task 3, first read Task 8), `StreamLine.offset_after` (added Task 4, first read Task 8), `ControlWireError.line` — each is a *field is never read* failure at an intermediate task's `-D warnings` gate. Worse, Task 6 Step 7's own text pushed the implementer to DELETE the field Task 8 needs. | **The dead_code discipline is now FIELD-LEVEL**, naming each field's first *production* read and its removal task. **Task 6 Step 7's "delete it or it doesn't belong" text is REMOVED — it was wrong.** | A field-level allow can mask a genuinely dead field forever. Task 11's grep is the enforcement: every temporary allow carries the literal `first read in Task N`, and the grep fails the build if one survives. |
| **C5** | B4's regression test is theatre: under merged law `drain_all` already reads the final bytes, so harvest 1 gets the response and deleting harvest 2 leaves the test GREEN. Worse, harvest 2 sat AFTER `unregister` (which unlinks), and it fires under exactly the condition that makes cp-0's guard append a durable `patrol.degraded` "ORDERING VIOLATION". | **Task 4 + Task 6.** (1) The falsifiability claim is **WITHDRAWN**; harvest 2 is re-justified as **defense-in-depth for a path cp-0 declares cannot currently occur**. (2) `apply_pending_unregisters` is **SPLIT** (`final_drain_pending` → harvest → `dispose_pending`), so the harvest sits **before the unlink** — restoring cp-0's own discipline (read_channel.rs:328-340) and giving the `end` frame a defined final offset (C7). (3) The `patrol.degraded` interaction is **neither libel nor suppressed**: harvest 2 non-empty ⟺ the guard fires ⟺ a real caller-ordering bug exists. They must co-occur. | If harvest 2 ever silently starts firing, a real ordering regression is being masked. **Test 2 now asserts BOTH `control.responded` AND the absence of any `patrol.degraded` "ORDERING VIOLATION"** — so the normal path proves the merged law still holds, and a future phase that breaks it goes red *here*. That is what test 2 actually proves, stated plainly. |
| **C6** | D6′'s catch-up→live boundary had no cursor guard: `caught_up_at` was a hello snapshot, so a burst during catch-up made `fanout` skip lines that history then ran past — **silent truncation**, the very thing §9 forbids — and `pump` could read bytes campd had not drained, delivering them twice. No test could see it: test 3 had a one-line history; test 6 joined at the tail. | **Task 8 — D6″ replaces D6′ and DELETES the catch-up/live distinction entirely.** A subscriber has **ONE monotone cursor** and is fed **only from the stream file**, over `[cursor, tail)` where `tail` is what campd has actually drained. `fanout` no longer appends lines at all — **a "live" line is just `tail` advancing.** Truncation impossible (the cursor never skips); duplication impossible (it is monotone and is the sole delivery gate); reading undrained bytes impossible (bounded by `tail`). **The bug class is designed out, not patched.** | The pump now does file I/O on the event loop per subscriber, so a large catch-up could stall campd (#55's class). Bounded by `MAX_PUMP_BYTES_PER_WAKE` (256 KiB/subscriber/wake); when work remains, `poll_timeout` returns `Some(ZERO)` — an ARMED continuation, `None` the instant nobody is behind (invariant 1). **Test: `a_subscriber_catching_up_across_a_live_burst_gets_every_line_exactly_once_in_order`** — >64 KiB of history, live lines appended DURING catch-up, assert exactly-once and in-order. **This is the window nothing in rev 2 exercised.** |
| **C7** | `end_sessions` closed a subscriber mid-catch-up after a single `pump` (dropping unstreamed history behind an indistinguishable `end` frame), could drop the connection without ever writing the `end` frame at all (contradicting test 4's own assertion), contradicted `Subscriber.file`'s "can finish its history" doc, and had **no defined source** for the `end` frame's `offset`. | **Task 8 — an explicit `Closing` state.** Disposal sets `closing = Some(reason)` and pins `tail = final_offset` (captured by `dispose_pending` — C5's split). The subscriber keeps pumping across wakes until `cursor == tail` **and** `out` is empty; only THEN is the `end` frame appended, and the connection closes when that flush completes. Progress is guaranteed by socket-writability edges plus the `Some(ZERO)` continuation — no timer. | A Closing subscriber that never drains could hold an fd forever. It cannot: the hard cap drops it exactly like any other slow subscriber, with `subscriber.dropped` — honestly, because it *is* backpressure. **Test 8 now asserts the FULL history arrives before the `end` frame**, plus `a_closing_subscriber_that_stops_reading_is_still_dropped_at_the_cap`. |
| **C8** | No policy for a single frame larger than the cap. A `Read`/`Bash` result line > 1 MiB drops **every** subscriber to that session **permanently** (a re-subscribe re-reads it and drops again), reported as `subscriber.dropped` — libelling a subscriber that was reading perfectly. The B7 class, through a different door. | **Task 8 — an explicit skip policy, ON THE WIRE.** A line whose frame exceeds the cap is **never delivered and never fatal**: campd emits `{"frame":"skipped","session":…,"offset":…,"bytes":N,"reason":…}`, advances the cursor past it, and keeps the subscriber. Recorded durably once per `(session, offset)` as `patrol.degraded` (cp-0's precedent for a fault with no dedicated event). **It is NOT `subscriber.dropped`.** | A silent skip would be §9's truncation again — so the skip is *in a pinned frame the client must handle* and *in the ledger*. **Test: `a_line_larger_than_the_cap_is_skipped_loudly_and_the_subscriber_survives`** (asserts the `skipped` frame, that the NEXT line still arrives, and that **no** `subscriber.dropped` was appended). |
| **C9** | B14 still laundered one fixture: the `strings` window (110 chars) **truncated the object it claimed to recover**. `can_use_tool` really also carries `description` and `requires_user_interaction`, plus conditional `permission_suggestions` / `blocked_path` — omitted while labelled "KEYS from the bundle". | **Task 1 Step 0 — every probe re-run at a 400-char window** (verbatim output pasted below). The fixture is completed; PROVENANCE.md marks conditional keys as CONDITIONAL, records **both** construction sites, and states the method's limit: a fixed-window grep of a minified bundle recovers one site and cannot prove key-completeness. | A wider window still cannot prove completeness. So the parse is **tolerant by design** (the envelope is NOT `deny_unknown_fields`; camp reads only `request_id` + `tool_name`). **Test: `can_use_tool_with_unknown_extra_keys_still_parses`** — camp may never depend on the fixture being complete. |
| **C10** | **cp-3's OUTBOUND permission-response shape was never extracted** — the one message whose wrongness hangs a worker forever (§5.3: the CLI parks on a promise with no timer) — from a bundle sitting open on the machine. | **Task 1 Step 0 — extracted and pinned.** The CLI's own validator string IS the contract: `Expected {behavior: 'allow', updatedInput?: object} or {behavior: 'deny', message: string}.`, wrapped by `sendControlResponse({type:"control_response",response:{subtype:"success",request_id:r,response:o}})`. Two new fixtures (`permission_allow_response.json`, `permission_deny_response.json`) are pinned and labelled. **cp-1 does not wire them** (phase 3 does) — it hands phase 3 *evidence* instead of a guess. | An unwired fixture can rot. It is pinned by `the_permission_response_fixtures_match_the_cli_validator_contract`, which parses each and asserts the `behavior`/`message`/`updatedInput` contract — so a future edit cannot quietly corrupt what cp-3 inherits. |
| **C11** | A late `control_response` (after the deadline fired) was **silently discarded** as a duplicate. It is not: it is new information saying the fault was premature. And it binds to the unverified mid-turn claim — if the CLI queues control messages until a turn completes, any mid-turn interrupt on a >30 s turn yields a FALSE `control.failed` **and** a swallowed answer. | **Task 3 — two fixes. (1) D7, the activity-reset rule:** a pending request's deadline is RESET by ANY stream line from its session, so **the deadline measures SILENCE, not elapsed time** — which makes the mid-turn queueing question **non-load-bearing for correctness**. **(2)** `resolved` is SPLIT into `answered` and `timed_out`: a late answer for a timed-out id appends a **correction** (`control.responded{late:true}` naming the fault it corrects), never `None`. Invariant 3 restored. | The activity-reset could keep a request pending forever against a chatty-but-broken worker. It cannot: the reset is bounded by the session's own lifetime (a disposed session's rows are dropped), and the stall ladder still owns a worker that outputs but never answers. **Tests: `session_activity_resets_a_pending_control_deadline`** and **`a_late_control_response_after_the_deadline_appends_a_correction`**. |
| **C12** | `serve_interrupt`'s body was deleted, pointing at "rev 1's Task 6" — a document the fresh implementer will never have. `ControlWrite::Failed` was never specified: a §2.1 loudness surface left to invention. | **Task 6 Step 4 — the body is INLINED IN FULL**, covering all three `ControlWrite` arms. `Failed` ⇒ a `Response::Error` to the caller **AND** a durable `control.failed`: the write was attempted, bytes may have reached the pipe, and `write_control` tore the pipe down — that is a campd action with a consequence (invariant 3) and a protocol fault (§2.1). | The `Failed` arm appends an event on a path a caller may retry. Bounded: one socket request ⇒ one event, and each attempt mints a fresh `request_id` so nothing dedupe-collides. **Test: `an_interrupt_whose_pipe_write_fails_is_loud_in_both_the_response_and_the_ledger`.** |

**Non-blocking notes, all addressed:** `MAX_SUBSCRIBERS` arithmetic stated and the number fixed (8 × 1 MiB = 8 MiB worst case; **the <20 MB gate is explicitly an IDLE bound**, and the perf gate measures the wakeup profile, not memory) · a `fleet.subscribe` seam paragraph · **SECURITY: the socket has no permissions/peer-cred check, and `subscribe` is a new exposure class — NAMED in the PR body + an issue for the phase that owns it** · `rehydrate`'s three scans, the `answered` set's pruning, and a `MAX_PENDING_CONTROL_REQUESTS` cap · the restart-EOF-without-an-`end`-frame gap is named · B6's residual false-cause window is named · `make compat` is LOCAL-ONLY and the PR says so · `RefoldReport` has `drift`, not `is_clean()` · `is_stalled` uses `stalled_count`'s own `tracked` intersection · `SubClient` gets real bodies · D4's dependent list now includes `patrol.rs:788` (and says why it is NOT orphaned) · concurrency (two interrupts; `send_turn` racing an interrupt) is specified and its non-guarantee stated · the untagged-`Response` versioning seam is named · `event_loop.rs` is explicitly **no longer additive** · no `SCHEMA_VERSION` bump is needed — confirmed.

---

## Global Constraints

- **TDD, strictly.** Write the failing test, RUN it, watch it fail, implement, RUN it, watch it pass. **Every run step states the expected test COUNT** — a filter that matches nothing exits 0 and is a false green (C3).
- **Never commit to main.** All work on `cp-1-control-protocol`; one reviewable PR.
- **Gates green at EVERY commit:** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`.
- **`camp` is a BINARY-ONLY crate.** No lib target. Three load-bearing consequences: unit tests run under **`cargo test -p camp --bins <filter>`** (`--lib` errors out — C3); integration tests **cannot link `daemon::*`**, so no wire client lives in `socket.rs` (B2); and `dead_code` fires on unread **fields**, not just items (C4).
- **No panics in library code** (clippy `unwrap_used`/`expect_used`/`panic` denied; `unsafe_code` forbidden). Test modules opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- **Invariant 1 (idle is free).** No ticks. Both armed deadlines (`CONTROL_RESPONSE_TIMEOUT`; the subscriber catch-up continuation) return `None` when nothing is pending.
- **Invariant 3 (nothing hidden).** Every campd action is an event, and **an event must name its TRUE cause** (B6/C11).
- **Invariant 5 (fail fast).** §2.1: *"An unrecognized control message, or a control response that never arrives, is an evented, operator-visible fault — never a swallowed timeout."*
- **Extend, don't rework.** cp-0's read channel is the transport. **"Extend" is not a licence to leave a second caller of a function whose side effects you changed unaccounted for** (B4) — nor to re-derive a property cp-0 already proved (C5).
- **New events use `deny_unknown_fields` payload structs**, keep the one-transaction event+state property, satisfy the vocab-pin partition tests, keep the refold property test green.
- **No new dependencies and no new cargo features.** In particular NOT `serde_json/preserve_order` and NOT `serde_json/raw_value` — Task 8's byte-splicing (C2) is what makes the second unnecessary.
- **No test may spawn a real `claude` or spend API money**, except the `#[ignore]`d, `CAMP_COMPAT=1`-gated $0 tier (Task 10), which sends no turn.
- **Spec and code never silently diverge.** If reality contradicts the control-plane spec, STOP and escalate.
- **No co-author lines in commits. Never mention the assistant in a commit message.**

### The dead_code discipline — FIELD-LEVEL (C4)

`dead_code` fires on an item OR a **field** never read from a path reachable from `main`. `--all-targets` still compiles the plain bin target, so a test-only read does not save it (hence `read_channel.rs:445,451`'s PERMANENT allows). Every temporary allow carries the literal text `first read in Task N`; **Task 11 greps for it and fails if any survive.**

| Item / **field** | Added | Annotation | Deleted at |
|---|---|---|---|
| `control.rs` items: `ParentMessage`, `WorkerMessage`, `parse_worker_line`, `ControlWireError`, `new_request_id`, `REQUEST_ID_PREFIX`, `ControlRuntime` + methods | 1, 3 | ONE module-level `#![allow(dead_code)] // cp-1: first read in Task 6 — DELETE this attribute there` | **Task 6 Step 7** |
| **`ControlWireError.line`** (field) | 1 | covered by the module-level attribute | Task 6 Step 7 (read by `ingest`'s fault arm) |
| **`ControlRuntime.subscriber_buffer_bytes`** (field) | 3 | `#[allow(dead_code)] // cp-1: first read in Task 8 (the subscriber hard cap)` — **the module-level attribute is gone by Task 6, so this field needs its OWN** | **Task 8** |
| **`StreamLine.offset_after`** (field) | 4 | `#[allow(dead_code)] // cp-1: first read in Task 8 (event_frame's cursor)` — `ingest` reads only `session` + `line` | **Task 8** |
| `read_channel::take_stream_lines` / `last_activity` / `tail_state` / `take_disposed` / `final_drain_pending` / `dispose_pending` | 4 | per-item `#[allow(dead_code)] // cp-1: first read in Task N` (6 / 7 / 8 / 8 / 6 / 6) | the task each names |
| `dispatch::write_control`, `ControlWrite` | 5 | `#[allow(dead_code)] // cp-1: first read in Task 6` | Task 6 |
| `ControlRuntime::subscriber_count` | 8 | `#[allow(dead_code)] // PERMANENT: test observable (the read_channel.rs:445 precedent)` | never |
| `fold.rs` payload structs (4) | 2 | `#[allow(dead_code)] // PERMANENT: audit-only — the fields exist to VALIDATE the shape (deny_unknown_fields), never to be read (the fold.rs:541 precedent)` | never |

### Parallel-stream file ownership (wave-2, window W2)

- **cp-1 OWNS:** `daemon/control.rs` (new), `daemon/read_channel.rs`, `daemon/socket.rs`, `daemon/patrol.rs` (one accessor), `tests/control.rs` (new), `tests/fixtures/control/**` (new), `tests/claude_compat.rs`, `tests/fake-agent.sh`, `tests/perf_daemon.rs`.
- **SHARED:** `daemon/event_loop.rs`, `daemon/dispatch.rs`, `daemon/mod.rs`, `camp-core/src/{event,vocab}.rs`, `camp-core/src/ledger/fold.rs`. **Do NOT refactor these.**
- **NOT touched, deliberately:** `daemon/spawn.rs` (C1 chose option (a) precisely to avoid it), `Cargo.toml`/`Cargo.lock` (no new deps, no new features), `camp-core/src/config.rs` (D5).
- **compat-2 OWNS — DO NOT TOUCH:** `camp-core/src/formula/**`, `ci/gc-compat/**`.

**`event_loop.rs` is NOT additive — expect a real conflict with compat-2.** The non-additive touches: (1) `min_deadline` gains a fourth nesting; (2) `run`/`serve_connection`/`drain_lines` gain a `control` parameter, the latter two a `Token`; (3) `struct Conn` and its fields become `pub(super)`; (4) the accept arm registers `READABLE | WRITABLE`; (5) the post-drain block is restructured (`control_step` needs `&mut conns` + `&mut poll`); (6) the `Request::Nudge` arm is DELETED (a net reduction). Resolve by keeping both sides; if impossible, STOP and ask the lead.

---

## Root-cause analysis (verified against this branch at `f6b248c`)

1. **campd can hear but cannot speak.** cp-0 tails each live session's stdout by byte offset on every wake and parses each complete line — into a `Value` it merely *counts* (`read_channel.rs:635-650`). Nothing correlates a `control_response`; nothing writes a `control_request` (`nudge_via_stdin`, dispatch.rs:208-227, writes only `spawn::user_message` turns).
2. **The socket has no session verbs** (`Request`, socket.rs:26-45, is `poke|status|stop|adopt|nudge`).
3. **The socket is one-shot.** `respond()` (event_loop.rs:997) assumes *"Responses are a few bytes"* — no outbound buffering. §4.4 requires it.
4. **`drain_one` has TWO callers** — `drain_all` (event_loop.rs:428) and `apply_pending_unregisters` (read_channel.rs:301) — so a per-line side effect must be harvested on both. **But cp-0 already proves the normal path is covered by the first** (read_channel.rs:258-273 + the merged test at read_channel.rs:509); harvest 2 is defense-in-depth for a path cp-0 declares cannot currently occur (C5).

---

## Design decisions

**D1 — `interrupt` is ACK-then-ASYNC** (RATIFIED). campd's loop is single-threaded; a handler waiting on a filesystem-latency line is issue #55's wedge class, and §4.4 makes bounded-answer the law. The round trip is proven through the ledger, survives a restart (B6), and repairs a late answer (C11).

**D2 — deliver-then-record** (RATIFIED). §5.3's ledger-FIRST rule is scoped by its own rationale to permission *decisions* (making "pending in the ledger" prove "never written to the pipe" for §5.3.4's adoption kill). No kill hangs off `session.interrupted`.

**D3 — strict control surface, transparent stream surface** (RATIFIED, hardened). Strictness keys on `type.starts_with("control")`, so a future `control_notify` is a loud fault rather than content forwarded to subscribers.

**D4 — `session.send_turn` REPLACES the `nudge` SOCKET VERB** (RATIFIED). Dependents: `cmd/nudge.rs:42,47,59`, `event_loop.rs:796`, the `nudge_wire_format_is_pinned` test — **and `patrol.rs:788`, which calls `dispatcher.nudge_via_stdin` DIRECTLY. D4 deletes only the socket verb; the `Dispatcher` method survives untouched, so patrol has NO orphaned caller.** The `camp nudge` CLI verb is unchanged. `send_turn` keeps emitting `session.nudged` — the merged vocabulary for "a turn was injected"; renaming it would churn vocab/fold/`cli_nudge.rs` for nothing.

**D5 — `subscriber_buffer_bytes` = 1 MiB module constant + test-only env override** (RATIFIED) — the cp-0 `max_stream_bytes` precedent.

**~~D6~~ ~~D6′~~ → D6″ — ONE MONOTONE CURSOR; the stream file is the only source (C6).**
A `Subscriber` holds an open `File`, a single `cursor: u64` (the next byte it needs), and `tail: u64` (what campd has actually drained, refreshed every wake from `read_channel.tail_state`). **`pump` reads only `[cursor, tail)`, frames each complete line, and advances the cursor.** `fanout` no longer appends lines at all — **a "live" line is just `tail` advancing.** There is no catch-up/live distinction, hence no boundary to get wrong:
- **Truncation impossible** — the cursor never skips a byte.
- **Duplication impossible** — the cursor is monotone and is the sole delivery gate.
- **Reading undrained bytes impossible** — reads are bounded by `tail`.
- **Ordinary history is never refused** (B10's fix survives): a late joiner simply starts with a low cursor.
Bounded on the event loop by `MAX_PUMP_BYTES_PER_WAKE`; while a subscriber is behind, `poll_timeout` returns `Some(ZERO)` — an armed continuation, `None` the moment nobody is behind (invariant 1).

**D7 (new) — the deadline measures SILENCE, not elapsed time (C11).** A pending control request's deadline is RESET by ANY stream line from its session. A worker producing output is alive, and its interrupt may simply be queued behind its turn. This makes the (genuinely untested) question of whether the CLI reads stdin mid-turn **non-load-bearing for correctness**: `control.failed` now means *"the session went silent for 30 s with an unanswered request"* — a real fault under either semantics. The residual (a worker that goes silent mid-turn with the interrupt queued) is **repaired, not hidden**: a late answer appends a correction.

### Deliberately DEFERRED

`--permission-prompt-tool stdio`, `permission.pending`/BLOCKED/stall-disarm/adoption-kill (phase 3) · the `initialize` handshake (phase 3 — **and cp-1's no-initialize configuration is EMPIRICALLY PROVEN against the pinned CLI**, Task 10) · `--include-partial-messages` (phase 4) · `fleet.subscribe`, `session.permission_decision`, `set_model`, `set_permission_mode` (later phases — **but cp-3's outbound permission bytes are pinned here**, C10) · `camp watch`/`camp attach` (phases 2/4) · `subscriber_buffer_bytes` as config.

**Stated plainly in the PR body:** after cp-1 merges, **an operator still cannot interrupt anything by hand.** cp-1 ships the protocol and its proofs; phase 2 ships the first human client.

### The `fleet.subscribe` seam (non-blocking note, adopted)

The `{"frame":…}` tag lets cp-2 add `frame:"ledger"` with no breaking wire change — but `Subscriber` is hard-wired to a stream file (`file`, `cursor`, `tail`: byte offsets), while `fleet.subscribe` (§4.1: session transitions, stalls, permission requests, completions) is **ledger-event-sourced**: no file, no byte offset. **cp-2 will therefore generalize `Subscriber` into an enum over two cursor kinds (byte-offset-into-a-stream-file, and ledger-seq), inside `control.rs`.** Expected and sanctioned; named here so it is a design, not a surprise.

---

## Task 1: `control.rs` — the wire format, and fixtures whose provenance is labelled

Spec: §2, §2.1, §9.

**Files:** Create `crates/camp/src/daemon/control.rs`, `crates/camp/tests/fixtures/control/*.json`, `crates/camp/tests/fixtures/control/PROVENANCE.md`. Modify `crates/camp/src/daemon/mod.rs` (`pub mod control;`, alphabetically after `pub mod bounded;`).

**Interfaces produced:**
```rust
pub const REQUEST_ID_PREFIX: &str = "camp-";
pub fn new_request_id() -> String;                          // "camp-<uuid-v4>"
pub enum ParentMessage { Interrupt { request_id: String }, DialogRefusal { request_id: String } }
impl ParentMessage { pub fn to_line(&self) -> anyhow::Result<String>; }
pub enum WorkerMessage<'a> {
    ControlResponse { request_id: String, ok: bool, detail: String },
    CanUseTool { request_id: String, tool_name: String },
    RequestUserDialog { request_id: String },
    Stream(&'a str),
}
pub fn parse_worker_line(line: &str) -> Result<WorkerMessage<'_>, ControlWireError>;
pub struct ControlWireError { pub line: String, pub reason: String }
```

- [ ] **Step 0: RECOVER the shapes from the pinned CLI — at a 400-char window (C9).** `sdk.mjs` is not vendored; the **actual peer** is on the machine and its bundle is `strings`-greppable. **A 110-char window truncates the object mid-construction — that was C9. Use 400.**

```bash
CLI=$(readlink -f "$(command -v claude)")     # MUST equal ci/claude-compat/CLAUDE_VERSION
strings -a "$CLI" | grep -o 'subtype:"can_use_tool".\{0,400\}'
strings -a "$CLI" | grep -o 'subtype:"request_user_dialog".\{0,300\}'
strings -a "$CLI" | grep -o 'type:"control_response",response:{subtype:"error".\{0,60\}'
strings -a "$CLI" | grep -o 'type==="control_request"&&.\{0,40\}'
strings -a "$CLI" | grep -o '.\{0,150\}updatedInput?: object}.\{0,60\}'     # C10
strings -a "$CLI" | grep -o 'sendResponse(r,n).\{0,120\}'                    # C10
```
**Verbatim output, 2026-07-13, claude 2.1.207 — this is what the fixtures pin:**
```
subtype:"can_use_tool",tool_name:n,display_name:s1e(n),input:o,tool_use_id:i,description:s,
  ...a&&{permission_suggestions:a},...l&&{blocked_path:l},requires_user_interaction:c||void 0

subtype:"request_user_dialog",dialog_kind:o,payload:i,...s&&{tool_use_id:s}

type:"control_response",response:{subtype:"error",request_id:e.request_id,error:k.error}

type==="control_request"&&"request_id" in e&&"request" in e

Expected {behavior: 'allow', updatedInput?: object} or {behavior: 'deny', message: string}.

sendResponse(r,n){let o={...n};e.sendControlResponse({type:"control_response",
  response:{subtype:"success",request_id:r,response:o}})}
```
Findings:
- **`error:<string>` is CORRECT** (verified in rev 2, re-verified here).
- **`can_use_tool`** carries `tool_name`, `display_name`, `input`, `tool_use_id`, `description`, `requires_user_interaction`, plus **CONDITIONAL** `permission_suggestions` and `blocked_path` (the `...x&&{…}` spreads). A **second** construction site adds `decision_reason`, `decision_reason_type`, `classifier_approvable`, `agent_id`. **camp reads only `request_id` and `tool_name`, and the envelope is deliberately NOT `deny_unknown_fields` — because a fixed-window grep of a minified bundle can never prove key-completeness, and the parse must not depend on it.**
- **`request_user_dialog`** carries `dialog_kind`, `payload`, conditional `tool_use_id`. **`dialog_kind`'s VALUES are a minified variable and were NOT recovered — camp must never key on it**; it refuses every dialog and reads only `request_id`.
- **C10 — cp-3's OUTBOUND permission answer, now pinned:** the parent answers a `can_use_tool` with a **success** `control_response` whose inner `response` is the decision object, and the CLI's own validator names the contract: `{behavior:"allow", updatedInput?: object}` or `{behavior:"deny", message: string}`. **cp-1 does not wire this** — it hands phase 3 pinned bytes instead of a guess.

- [ ] **Step 1: Write the fixtures + `PROVENANCE.md`.** One line each, no trailing newline. **Every file carries a label; `PROVENANCE.md` carries the command that produced it and the limits of the method.**

`interrupt_request.json` — *`camp-authored`; **ACCEPTED** by CLI 2.1.207 (Task 10's $0 gate sends exactly these bytes and asserts the ack). The claim is ACCEPTANCE, not recording.*
```json
{"type":"control_request","request_id":"camp-fixture-1","request":{"subtype":"interrupt"}}
```
`control_response_success.json` — *`recorded-from-CLI-2.1.207` (observed on the wire, live $0 run)*
```json
{"type":"control_response","response":{"subtype":"success","request_id":"camp-fixture-1","response":{"still_queued":[]}}}
```
`control_response_error.json` — *`derived-from-CLI-2.1.207`*
```json
{"type":"control_response","response":{"subtype":"error","request_id":"camp-fixture-1","error":"no turn in progress"}}
```
`can_use_tool_request.json` — *`derived-from-CLI-2.1.207` (400-char window). KEYS from the bundle; VALUES illustrative. `permission_suggestions` / `blocked_path` are CONDITIONAL and omitted; a second site adds four more keys. **Completeness is NOT claimed — the parse is tolerant by design.***
```json
{"type":"control_request","request_id":"cli-fixture-2","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"cargo publish"},"tool_use_id":"toolu_fixture","description":"run cargo publish","requires_user_interaction":true}}
```
`request_user_dialog_request.json` — *KEYS `derived-from-CLI-2.1.207`; `dialog_kind`'s VALUE is `camp-invented` and **camp never reads it***
```json
{"type":"control_request","request_id":"cli-fixture-3","request":{"subtype":"request_user_dialog","dialog_kind":"unknown","payload":{},"tool_use_id":"toolu_fixture"}}
```
`dialog_refusal_response.json` — *`camp-authored`, shape mirrored from the CLI's OWN error-response construction. **UNVALIDATED against the real CLI**: camp sends it only under `--permission-prompt-tool stdio`, which is phase 3, so no $0 gate here can exercise it. **PHASE-3 OBLIGATION.** If the shape is wrong the CLI ignores it and the worker hangs forever — the outcome §9 exists to prevent.*
```json
{"type":"control_response","response":{"subtype":"error","request_id":"cli-fixture-3","error":"camp does not support interactive dialogs"}}
```
**`permission_allow_response.json`** (C10) — *`derived-from-CLI-2.1.207`. **For phase 3. cp-1 does not send it.** Validator: `Expected {behavior: 'allow', updatedInput?: object} or {behavior: 'deny', message: string}.`*
```json
{"type":"control_response","response":{"subtype":"success","request_id":"cli-fixture-2","response":{"behavior":"allow"}}}
```
**`permission_deny_response.json`** (C10) — *`derived-from-CLI-2.1.207`. For phase 3.*
```json
{"type":"control_response","response":{"subtype":"success","request_id":"cli-fixture-2","response":{"behavior":"deny","message":"denied by the operator"}}}
```
`user_turn.json` — **(C1)** *`camp-authored` — the bytes `spawn::user_message` ACTUALLY produces (`serde_json::json!` sorts keys: serde_json 1.0.150 has no `preserve_order`). ACCEPTED by the CLI: this exact envelope is probe P2 and has been in production since Phase 8. **The key order is ugly and it is CORRECT. Do not "tidy" `user_message` into a struct to make it prettier — that would change the bytes every production dispatch sends. This pin is what catches such a change.***
```json
{"message":{"content":"status?","role":"user"},"type":"user"}
```
`stream_assistant.json` — *`camp-authored` (a representative non-control stream line; camp never interprets it — D3)*
```json
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}
```

- [ ] **Step 2: Write the failing test.** Create `control.rs` with the module doc, the module-level dead_code allow (`// cp-1: first read in Task 6 — DELETE this attribute there`), and ONLY the test module. **7 tests:**

1. `parent_messages_serialize_to_the_pinned_fixture_bytes` — `ParentMessage::{Interrupt, DialogRefusal}` byte-equal their fixtures, **and** `spawn::user_message("status?")` byte-equals `user_turn.json` (C1: the fixture is now the ACTUAL output, so this CAN pass). The test carries a comment explaining why these are structs and why `user_turn.json` looks the way it does.
2. `parent_messages_are_semantically_equal_to_their_fixtures` — the order-independent `Value` guard.
3. `worker_messages_parse_from_the_pinned_fixtures` — all four inbound shapes; asserts `detail == "no turn in progress"` (the verified `error` key).
4. **`can_use_tool_with_unknown_extra_keys_still_parses`** (C9's new-failure test) — a `can_use_tool` carrying `permission_suggestions`, `blocked_path`, `decision_reason` **and a made-up `future_key`** parses cleanly to `CanUseTool { request_id, tool_name }`. **camp may never depend on the fixture being complete.**
5. `non_control_stream_lines_pass_through_verbatim_and_never_fault` (D3).
6. `an_unrecognized_control_message_is_a_loud_error` — unknown subtype, missing request_id, non-JSON, `control_cancel_request`, and a not-yet-existing `control_notify` (the prefix rule).
7. **`the_permission_response_fixtures_match_the_cli_validator_contract`** (C10) — parses both permission fixtures; asserts `type=="control_response"`, `response.subtype=="success"`, and an inner `response` that is either `{behavior:"allow"}` (optional `updatedInput`) or `{behavior:"deny", message:<string>}`. **These bytes are cp-3's contract; this test stops them rotting before cp-3 arrives.**

- [ ] **Step 3: Run it and watch it fail.** Add `pub mod control;` to `daemon/mod.rs` FIRST, or nothing compiles and the failure is vacuous.

Run: `cargo test -p camp --bins daemon::control 2>&1 | tail -20`   ← **`--bins`, NOT `--lib` (C3)**
Expected: FAIL — compile errors naming `ParentMessage`, `parse_worker_line`, `WorkerMessage`, `new_request_id`.

- [ ] **Step 4: Implement.** `ParentMessage` is built from `#[derive(Serialize)]` structs — `InterruptEnvelope { #[serde(rename="type")] kind, request_id, request: InterruptBody { subtype } }` and `ErrorResponseEnvelope { kind, response: ErrorResponseBody { subtype, request_id, error } }` — so field order is DECLARATION order (B1). `to_line()` returns `anyhow::Result<String>` (no `unwrap` in library code) and appends `'\n'`.

`parse_worker_line`: deserialize a permissive `Envelope { #[serde(rename="type")] kind: String, request_id: Option<String>, request: Option<Value>, response: Option<Value> }` — **deliberately NOT `deny_unknown_fields`** (C9). Then:
- `!kind.starts_with("control")` ⇒ `Ok(WorkerMessage::Stream(line))` (D3 — the transparent surface).
- `"control_response"` ⇒ `request_id` from **inside** `response` (verified nesting); `subtype == "success"` ⇒ `ok: true`, `detail = response["response"].to_string()`; `"error"` ⇒ `ok: false`, `detail = response["error"].as_str()` (the verified key; the `unwrap_or("…unspecified…")` placeholder is reachable only if the CLI stops sending it, in which case the fixture test is already red); any other subtype ⇒ `ControlWireError`.
- `"control_request"` ⇒ `request_id` from the **top level**; `"can_use_tool"` ⇒ `(request_id, tool_name)`; `"request_user_dialog"` ⇒ `request_id` only; any other subtype ⇒ `ControlWireError`.
- any other `control*` type ⇒ `ControlWireError` (the PREFIX rule: a future `control_notify` faults rather than being forwarded to a subscriber as content).

- [ ] **Step 5: Run and watch pass.**

Run: `cargo test -p camp --bins daemon::control 2>&1 | tail -20`
Expected: PASS — **7 tests**. *(A count of 0 means the filter matched nothing: a false green — C3.)*

- [ ] **Step 6: fmt + clippy + commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/control.rs crates/camp/src/daemon/mod.rs crates/camp/tests/fixtures/control
git commit -m "feat(control): one module owns the control wire format, pinned by provenance-labelled fixtures (cp-1 §2.1)"
```

---

## Task 2: the four new events

Spec: §2.1, §4.4, invariants 3 and 7.

**Files:** `camp-core/src/event.rs`, `camp-core/src/vocab.rs`, `camp-core/src/ledger/fold.rs`. Tests: `camp-core/tests/vocab_pin.rs` (existing), `camp-core/src/ledger/mod.rs`.

```rust
EventType::SessionInterrupted => "session.interrupted"  // {session, request_id}
EventType::ControlResponded   => "control.responded"    // {session, request_id, verb, ok, detail, late}
EventType::ControlFailed      => "control.failed"       // {session?, request_id?, verb?, reason}
EventType::SubscriberDropped  => "subscriber.dropped"   // {session, subscription, buffered_bytes, cap_bytes}
```
**`late: bool` (default false) is C11's correction field.** None of the four names exists in `gc-vocab.json` ⇒ all camp-specific, additive (invariant 7).

- [ ] **Step 1: Add the variants AND their fold arms together** (cp-0 note 3: a variant without its arm makes the next step's red an `E0004` compile error, not a test failure). `fold.rs` gets `fn audit<T: DeserializeOwned>(event) -> Result<(), CoreError>` (parse-and-discard) and four `#[serde(deny_unknown_fields)] #[allow(dead_code)]` payload structs — `ControlResponded` carrying `#[serde(default)] late: bool` and `#[serde(default)] detail: String`.

- [ ] **Step 2: Run the vocab-pin test and watch it fail RED.**

Run: `cargo test -p camp-core --test vocab_pin 2>&1 | tail -20`
Expected: FAIL in `every_event_type_is_declared_mirrored_or_camp_specific_never_both` — an assertion failure, not a compile error.

- [ ] **Step 3: Declare them camp-specific** in `CAMP_SPECIFIC_EVENTS` (after `"session.nudged"`).

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp-core --test vocab_pin` — PASS, including `camp_specific_names_do_not_collide_with_gc`.

- [ ] **Step 5: Write the fold round-trip test** in `camp-core/src/ledger/mod.rs`'s `mod tests`, beside cp-0's `read_channel_patrol_degraded_shapes_round_trip_through_the_fold`: append all four shapes (including a `late: true` `control.responded`), assert a typo'd key (`requestId`) is REFUSED at append, then refold. **The API is `refold_check()` (refold.rs:64) returning `RefoldReport { events_replayed, drift }` — assert `report.drift.is_empty()`, exactly as `refold_prop.rs:170` does. There is no `is_clean()` and no `refold()`.**

- [ ] **Step 6: Run it, then the property test.**

Run: `cargo test -p camp-core control_plane_event_shapes_round_trip_through_the_fold && cargo test -p camp-core --test refold_prop`
Expected: PASS both.

- [ ] **Step 7: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/camp-core
git commit -m "feat(events): session.interrupted, control.responded, control.failed, subscriber.dropped (cp-1 §2.1/§4.4)"
```

---

## Task 3: `ControlRuntime` — the pending table, the SILENCE deadline (D7/C11), rehydration (B6)

Spec: §2.1, invariants 1 and 3.

**Files:** `crates/camp/src/daemon/control.rs`.

**Interfaces produced:**
```rust
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_PENDING_CONTROL_REQUESTS: usize = 64;
pub struct ControlRuntime;
impl ControlRuntime {
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime;
    pub fn track_pending(&mut self, request_id: String, session: String, verb: &'static str, now: Timestamp);
    /// D7/C11: ANY stream line from a session resets its pending deadlines.
    pub fn note_activity(&mut self, session: &str, now: Timestamp);
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration>;
    pub fn expire_pending(&mut self, now: Timestamp) -> Vec<EventInput>;
    pub fn resolve(&mut self, request_id: &str, ok: bool, detail: String) -> Option<EventInput>;
    pub fn rehydrate(&mut self, ledger: &Ledger, now: Timestamp) -> anyhow::Result<usize>;
    /// Prune this session's ids (bounds `answered`); called at disposal.
    pub fn forget_session(&mut self, session: &str);
}
```

**State — `resolved` is SPLIT (C11):**
```rust
pub struct ControlRuntime {
    pending: HashMap<String, Pending>,       // { session, verb, deadline }
    /// ANSWERED (or settled in a previous life). A re-read control_response for
    /// one of these is a TRUE duplicate => None (B6).
    answered: HashSet<String>,
    /// C11: TIMED OUT — campd already appended `control.failed` saying the worker
    /// never answered. A control_response for one of these is NOT a duplicate: it
    /// is NEW INFORMATION saying that fault was PREMATURE, and it appends a
    /// CORRECTION. Conflating the two sets is how rev 2 silently swallowed a real
    /// answer on the most operationally important path the phase ships.
    timed_out: HashMap<String, Pending>,
    #[allow(dead_code)] // cp-1: first read in Task 8 (the subscriber hard cap)
    subscriber_buffer_bytes: usize,
}
```

- [ ] **Step 1: Write the failing tests** (**7**):

1. `a_pending_request_arms_a_deadline_and_an_empty_table_arms_none` — invariant 1.
2. `a_control_response_that_never_arrives_becomes_a_durable_fault` — §2.1; the row is removed, so the fault is raised exactly once.
3. `a_matching_control_response_resolves_the_pending_request` — `late == false`.
4. `a_restart_across_an_in_flight_interrupt_neither_lies_nor_forgets` (B6) — an answered id's re-read resolves to `None`; the orphan still expires.
5. `a_control_response_for_a_never_sent_request_id_is_a_fault` (§2.1).
6. **`session_activity_resets_a_pending_control_deadline`** (D7/C11) — track at T0; `note_activity` at T0+20 s; assert **nothing expires at T0+31 s** (the worker is streaming: it is alive), and that it DOES expire at T0+20 s+31 s (30 s of *silence*).
7. **`a_late_control_response_after_the_deadline_appends_a_correction`** (C11) — track, expire (⇒ `control.failed`), then `resolve` the same id. Assert `Some(ControlResponded)` with **`late == true`** and a `detail` naming the premature fault — **not `None`.** *Rev 2 discarded this answer; this test is what makes that impossible.*

- [ ] **Step 2: Run and watch fail.** `cargo test -p camp --bins daemon::control 2>&1 | tail -20` → FAIL (`cannot find type ControlRuntime`).

- [ ] **Step 3: Implement.** The constant's doc comment **must state D7 and its residual assumption**:

```rust
/// How long a session may be SILENT with a control request outstanding before
/// campd declares the protocol broken (§2.1). A BOUND on one operation, not a
/// wakeup: it joins `min_deadline` only while something is pending (invariant 1).
///
/// D7/C11 — THIS MEASURES SILENCE, NOT ELAPSED TIME. `note_activity` resets it on
/// ANY stream line from the session. That matters because of an UNVERIFIED
/// property of the CLI: it is not known whether it reads control messages from
/// stdin WHILE A TURN IS STREAMING (every interrupt exercised anywhere in this
/// repo, fake or real, is PRE-turn). If the CLI queues control messages until the
/// turn completes, an elapsed-time deadline would fire a FALSE `control.failed`
/// on any turn longer than 30s. A SILENCE deadline does not: a worker producing
/// output is alive, and `control.failed` now means "the session went quiet for
/// 30s with an unanswered request" — a real fault under EITHER semantics. The
/// residual (a worker that goes silent mid-turn with its interrupt queued) is
/// REPAIRED, not hidden: a late answer appends a correction (C11).
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
```
- `track_pending` — `deadline = now + CONTROL_RESPONSE_TIMEOUT` (jiff: `SignedDuration::try_from(...).unwrap_or(SignedDuration::from_secs(30))`; clippy denies `unwrap`).
- **`note_activity(session, now)`** — every pending row of that session gets `deadline = now + CONTROL_RESPONSE_TIMEOUT`.
- `poll_timeout` — earliest pending deadline as a `Duration`-from-now; `None` when empty. **Task 8 extends this with the subscriber continuation.**
- `expire_pending(now)` — due rows are REMOVED from `pending`, **MOVED into `timed_out`**, each yielding a `control.failed` naming the verb and the silence bound.
- `resolve(id, ok, detail)`:
  - in `pending` ⇒ remove; insert into `answered`; `Some(ControlResponded { late: false, … })`.
  - in `timed_out` ⇒ remove; insert into `answered`; **`Some(ControlResponded { late: true, detail: "… arrived after control.failed declared it unanswered — that fault was PREMATURE; this is the correction", … })`** (C11).
  - in `answered` ⇒ `None` (a true duplicate: a restart re-read — B6).
  - otherwise ⇒ `Some(ControlFailed { reason: "…camp never sent…" })` (§2.1).
- `rehydrate(ledger, now)` — scan `session.interrupted`; ids also present in `control.responded`/`control.failed` go into `answered` (so a re-read is a silent no-op — B6); everything else is `track_pending`ed with a FRESH deadline (the previous life's clock is not ours). Returns the restored count.
- `forget_session(session)` — drops that session's ids from `answered` (bounding it by live sessions).

**Bounds (non-blocking notes, adopted).** `rehydrate` does three full-type ledger scans, once per campd life, bounded by ledger size — stated. `answered` is pruned at disposal. **`pending` is capped at `MAX_PENDING_CONTROL_REQUESTS` (64)**: past it, `serve_interrupt` refuses loudly, so neither an overseer loop nor a hostile client can grow the table or the ledger without bound (the `MAX_FAULTS_PER_SESSION_PER_WAKE` dedupe protects only the INBOUND path).

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --bins daemon::control` → PASS, **14 tests** (7 from Task 1 + 7 here).

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/control.rs
git commit -m "feat(control): pending table with a SILENCE deadline, ledger rehydration, late-answer corrections (cp-1 §2.1)"
```

---

## Task 4: the read channel hands its lines over, and disposal is SPLIT (C5)

Spec: §2.3, §4.1, §9.

**Files:** `crates/camp/src/daemon/read_channel.rs`.

**Interfaces produced:**
```rust
pub struct StreamLine {
    pub session: String,
    pub line: String,
    #[allow(dead_code)] // cp-1: first read in Task 8 (event_frame's cursor)
    pub offset_after: u64,
}
pub struct Disposed { pub session: String, pub final_offset: u64 }
impl ReadChannelRuntime {
    pub fn take_stream_lines(&mut self) -> Vec<StreamLine>;
    pub fn last_activity(&self, session: &str) -> Option<jiff::Timestamp>;
    pub fn tail_state(&self, session: &str) -> Option<(PathBuf, u64)>;
    /// C5: the final drain + cp-0's ordering guard + cp-0's fault flushes.
    /// Does NOT dispose. Returns true when it appended events.
    pub fn final_drain_pending(&mut self, ledger: &mut Ledger) -> Result<bool>;
    /// C5: unlink + clear cursor — AFTER the caller has harvested.
    pub fn dispose_pending(&mut self, ledger: &mut Ledger) -> Result<()>;
    pub fn take_disposed(&mut self) -> Vec<Disposed>;
}
```
**`apply_pending_unregisters` is KEPT as a thin wrapper** (`final_drain_pending` then `dispose_pending`) so every merged cp-0 unit test that calls it stays green. The event loop calls the halves separately, with the harvest between them.

- [ ] **Step 1: Write the failing tests** (**3**):

1. `drain_all_hands_over_the_complete_lines_it_consumed` — file order; `offset_after` correct; `mem::take`-drained (never redelivered); a partial line is never handed over.
2. `the_disposal_time_final_drain_also_hands_over_its_lines` — `final_drain_pending` produces the last line **while the file still exists**; `dispose_pending` is what unlinks it; `take_disposed()` yields `Disposed { session, final_offset }` with the true final offset (**the `end` frame's offset source — C7**).
3. **`the_final_drain_and_the_disposal_are_separable`** (C5's enabling guard) — after `final_drain_pending`, the stream file **still exists** and the session is **still tailed**; only `dispose_pending` removes both. *This is what makes harvesting before the unlink possible at all.*

- [ ] **Step 2: Run and watch fail.** (`no method named take_stream_lines`.)

- [ ] **Step 3: Implement.** In `drain_one`'s existing `Ok(_v)` parse arm (read_channel.rs:635-641), keep the `parsed_counts` bump and push a `StreamLine` + stamp `last_activity`. **No other line of the drain loop changes.** (This compiles: `self.parse_errors.push` already performs the same disjoint-field borrow at read_channel.rs:643 while `t` is live.)

Split `apply_pending_unregisters` **at the seam cp-0 already documents** (read_channel.rs:328-340: *"every one of them must be consumed BEFORE the sessions are disposed below"*): everything up to and including the fault flushes becomes `final_drain_pending`; the `for session in &pending { self.unregister(...) }` loop becomes `dispose_pending`, which records each `Disposed { session, final_offset: t.offset }` before removal. **cp-0's ordering-violation guard stays exactly where cp-0 put it, unchanged** (C5).

- [ ] **Step 4: Run the new tests AND the entire cp-0 suite.**

Run: `cargo test -p camp --bins daemon::read_channel && cargo test -p camp --test read_channel`
Expected: PASS — 3 new + **every cp-0 test**, including `a_workers_final_stdout_line_is_drained_before_the_reap_disposes_the_file` (read_channel.rs:509) and the ordering-guard tests. **If any cp-0 test goes red, the split broke a merged invariant — STOP.**

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/read_channel.rs
git commit -m "feat(read-channel): hand drained lines over; split the final drain from disposal (cp-1)"
```

---

## Task 5: `dispatch::write_control` — the write half

Spec: §2, issue #55.

**Files:** `crates/camp/src/daemon/dispatch.rs` (**shared — ADDITIVE ONLY**: one enum + one method beside `nudge_via_stdin`).

- [ ] **Step 1: Write the failing tests** (**2**). The real scaffolds are `Dispatcher::test_insert_held_cat(...)` (dispatch.rs:352) and `Dispatcher::test_insert_held_sleeper(...)` (dispatch.rs:394 — a worker that never reads its pipe: the PR #51 finding-2 wedge shape). A `Dispatcher` is `Dispatcher::new(camp: CampDir, config: CampConfig)`. **Read both scaffolds' real argument lists and match them exactly.**

1. `write_control_delivers_into_the_held_stdin_pipe` — `Delivered` against `test_insert_held_cat`; `NoPipe` for an unknown session.
2. **`write_control_is_bounded_and_drops_the_torn_pipe`** — against `test_insert_held_sleeper`, a 2 MiB line fails `Failed(_)` **within the deadline** (assert elapsed < 10 s), and the torn pipe is DROPPED (a second write returns `NoPipe`). *This is the whole justification for the method existing, and rev 2 left it untested.*

- [ ] **Step 2: Run and watch fail.** (`no method named write_control`.)

- [ ] **Step 3: Implement** — `pub enum ControlWrite { Delivered, NoPipe, Failed(String) }` and `pub fn write_control(&mut self, session, line) -> ControlWrite`, a structural twin of `nudge_via_stdin`: bounded `write_bounded` with `STDIN_WRITE_TIMEOUT`; on error `worker.stdin = None` (never write after a torn line). Both carry `#[allow(dead_code)] // cp-1: first read in Task 6`. **`NoPipe` is a caller-visible FAILURE, not a designed degrade** — unlike a turn, an interrupt has no resume path.

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --bins daemon::dispatch` — the 2 new tests plus every existing dispatch test.

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy -p camp --all-targets --all-features -- -D warnings
git add crates/camp/src/daemon/dispatch.rs
git commit -m "feat(dispatch): write_control — the bounded control-message write into the held stdin (cp-1 §2)"
```

---

## Task 6: `session.interrupt` + `session.send_turn`, and the harvest ordering (B4/B5/C5/C12)

Spec: §4.1, §4.2, §7 phase 1. D1, D2, D4.

**Files:** `daemon/socket.rs`, `daemon/control.rs`, `daemon/event_loop.rs` (**shared**), `daemon/mod.rs` (**shared**), `cmd/nudge.rs`, `tests/fake-agent.sh`; create `crates/camp/tests/control.rs`.

- [ ] **Step 1: Pin the new socket wire (failing test).** `control_plane_verbs_wire_format_is_pinned` — `{"op":"session.interrupt","session":"camp/dev/1"}`, `{"op":"session.send_turn","session":"camp/dev/1","text":"status?"}`, `{"ok":true,"request_id":"camp-1"}`, `{"ok":true,"via":"stdin"}`, both directions, **and `{"op":"nudge",…}` now REJECTED** (D4). DELETE `nudge_wire_format_is_pinned` (socket.rs:672).

- [ ] **Step 2: Run and watch fail.** `cargo test -p camp --bins daemon::socket` → FAIL.

- [ ] **Step 3: Implement the socket types.** `Request::SessionSendTurn { session, text }` (`#[serde(rename = "session.send_turn")]`) and `Request::SessionInterrupt { session }` (`#[serde(rename = "session.interrupt")]`), REPLACING `Nudge`. `Response::SendTurn { ok, via }` and `Response::Interrupt { ok, request_id }`, both BEFORE `Ok` (the untagged variant-order rule, socket.rs:47). Update `cmd/nudge.rs:42,47,59`. **`patrol.rs:788` calls `dispatcher.nudge_via_stdin` DIRECTLY and is UNAFFECTED — the method survives; only the socket verb changes.**

*(Non-blocking, recorded: `Response` is `#[serde(untagged)]` with an order-dependent match. The `Subscribed` hello is the natural — and now last — free place for a protocol version/capability field; what breaks untagged resolution is a later phase adding a field to an EXISTING variant. Named so it is a choice, not an accident.)*

- [ ] **Step 4: Implement the handlers — `serve_interrupt` INLINED IN FULL (C12).**

`serve_send_turn` is the `Request::Nudge` arm (event_loop.rs:796-844) **moved verbatim**: deliver → record (`session.nudged`) → respond; `NoPipe ⇒ via:"none"` (the resume path); a post-delivery append failure surfaces to the caller.

```rust
impl ControlRuntime {
    /// §4.1 `session.interrupt`. D1 (ACK-then-ASYNC) + D2 (deliver -> record ->
    /// respond). campd does NOT wait for the control_response: its loop is
    /// single-threaded, and blocking a handler on a filesystem-latency line is
    /// issue #55's wedge class. The answer returns on the read channel (`ingest`),
    /// survives a restart (`rehydrate`, B6), and a late answer appends a
    /// correction (C11).
    ///
    /// ORDERING, and what camp does NOT promise: an interrupt and a `send_turn`
    /// are both LINES IN THE SAME held stdin pipe, written in socket-arrival
    /// order. camp makes NO guarantee that an interrupt "cancels" a turn already
    /// queued ahead of it — a caller assuming that is assuming something camp does
    /// not promise. Two concurrent interrupts mint DISTINCT request_ids and
    /// produce two independent pending rows and two `control.responded`s; that is
    /// correct and needs no coordination.
    pub fn serve_interrupt(
        &mut self,
        session: &str,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        now: Timestamp,
    ) -> Response {
        // Bound the table AND the ledger: neither an overseer loop nor a hostile
        // client may grow `pending` or append `session.interrupted` without limit.
        if self.pending.len() >= MAX_PENDING_CONTROL_REQUESTS {
            return Response::Error {
                ok: false,
                error: format!(
                    "campd already has {} unanswered control requests outstanding (the \
                     MAX_PENDING_CONTROL_REQUESTS cap) — something is issuing interrupts faster \
                     than workers answer them",
                    self.pending.len()
                ),
            };
        }
        let request_id = new_request_id();
        let line = match (ParentMessage::Interrupt { request_id: request_id.clone() }).to_line() {
            Ok(line) => line,
            Err(e) => {
                return Response::Error { ok: false, error: format!("building the interrupt: {e}") };
            }
        };
        let (rig, bead) = dispatcher
            .child_info(session)
            .map(|(r, b)| (Some(r), Some(b)))
            .unwrap_or((None, None));
        match dispatcher.write_control(session, &line) {
            // D2: deliver -> record. The ledger must not claim what was not
            // delivered, and the caller must not believe what the ledger lacks.
            ControlWrite::Delivered => match ledger.append(EventInput {
                kind: EventType::SessionInterrupted,
                rig,
                actor: "campd".into(),
                bead,
                data: serde_json::json!({"session": session, "request_id": request_id}),
            }) {
                Ok(_) => {
                    self.track_pending(
                        request_id.clone(),
                        session.to_owned(),
                        "session.interrupt",
                        now,
                    );
                    Response::Interrupt { ok: true, request_id }
                }
                Err(e) => Response::Error {
                    ok: false,
                    error: format!(
                        "interrupt delivered into {session} but recording session.interrupted \
                         failed: {e}"
                    ),
                },
            },
            // There is NO resume path for an interrupt (unlike a turn): a worker
            // campd holds no pipe to CANNOT be interrupted, and pretending
            // otherwise would be a silent no-op. Loud — and NOT evented: nothing
            // happened, so there is no campd action to record (invariant 3 records
            // ACTIONS; a refused verb is the caller's error).
            ControlWrite::NoPipe => Response::Error {
                ok: false,
                error: format!(
                    "campd holds no stdin pipe for {session} — it is not a live campd-spawned \
                     worker (exited, released, attended, or adopted from a previous campd life), \
                     and there is no other way to interrupt a turn (control-plane spec §2.3)"
                ),
            },
            // C12 — THE ARM REV 2 NEVER SPECIFIED. The write was ATTEMPTED and
            // FAILED, so bytes may already have reached the pipe and `write_control`
            // has torn it down (worker.stdin = None). That IS a campd action with a
            // consequence — the worker just lost its write channel — so it is BOTH
            // an error to the caller AND a durable fault (§2.1 loudness; invariant
            // 3). Bounded: one socket request => one event, and the request_id is
            // fresh, so a retrying caller cannot dedupe-collide.
            ControlWrite::Failed(e) => {
                let reason = format!(
                    "writing an interrupt into {session}'s held stdin failed: {e}. The pipe may \
                     hold a torn partial line, so campd dropped it — this worker can no longer be \
                     sent turns or control messages, and patrol's stall ladder now owns it"
                );
                match ledger.append(EventInput {
                    kind: EventType::ControlFailed,
                    rig,
                    actor: "campd".into(),
                    bead,
                    data: serde_json::json!({
                        "session": session,
                        "request_id": request_id,
                        "verb": "session.interrupt",
                        "reason": reason,
                    }),
                }) {
                    Ok(_) => Response::Error { ok: false, error: reason },
                    // A failing append must not MASK the write failure being
                    // reported — carry both.
                    Err(append_err) => Response::Error {
                        ok: false,
                        error: format!(
                            "{reason} (and recording control.failed ALSO failed: {append_err})"
                        ),
                    },
                }
            }
        }
    }
}
```

**`ingest(&mut self, lines: &[StreamLine], dispatcher: &mut Dispatcher, now: Timestamp) -> Vec<EventInput>`:**
- **FIRST, for every line: `self.note_activity(&sl.session, now)`** (D7/C11 — the session is producing output, so its pending deadlines reset).
- `ControlResponse` ⇒ `self.resolve(id, ok, detail)`, pushing the `Option` when `Some` (B6/C11).
- `RequestUserDialog` ⇒ write `ParentMessage::DialogRefusal` via `dispatcher.write_control`; append `control.failed` naming the outcome (delivered / no pipe / write failed), **deduped per `request_id`** so a worker re-asking the same id appends once.
- `CanUseTool` ⇒ `control.failed` stating plainly that the worker is now blocked forever holding a dispatch slot and must be killed by the operator. camp takes no automatic action: the flow is structurally unreachable in cp-1 (§5.3.1), and phase 3 owns both the answer and §5.3.2's slot rule.
- `Stream(_)` ⇒ **nothing** (D6″: subscribers are fed from the FILE by `pump`, never from here).
- `Err(ControlWireError)` ⇒ `control.failed`, **capped at `MAX_FAULTS_PER_SESSION_PER_WAKE` (8)** with the suppressed count named in the last event (loud is right; unbounded-loud is a self-DoS). *cp-0's `drain_one` hands over only already-parsed lines (the `Ok(_v)` arm) and surfaces non-JSON separately as `patrol.degraded`, so `ingest` never double-reports. Do not add a guard.*

Also define here: `SUBSCRIBER_BUFFER_BYTES_DEFAULT: usize = 1024 * 1024` and `subscriber_buffer_bytes_from_env(default) -> Result<usize>` (`CAMP_SUBSCRIBER_BUFFER_BYTES`) — the exact `max_stream_bytes_from_env` twin (read_channel.rs:34-50), failing fast on a malformed or zero value.

- [ ] **Step 5: Wire the event loop — the ordering IS the fix (B4/B5/C5).**

`min_deadline` gains a fourth nesting (`control.poll_timeout(poll_now)`). Thread `control` through `run`/`serve_connection`/`drain_lines` (plus the `Token`). Add the two arms; DELETE the `Request::Nudge` arm.

```rust
        read_channel.apply_tracking(ledger)?;
        if let Err(e) = read_channel.drain_all(ledger) { eprintln!("campd: drain_all failed: {e:#}"); }
        let mut appended = false;
        // HARVEST 1 — the lines `drain_all` just consumed. Under MERGED LAW this is
        // the harvest that gets an answer-and-exit worker's control_response: the
        // reap appends session.stopped/crashed BEFORE settle, so the unregister is
        // queued before drain_all, and drain_all reads the final bytes while the
        // session is still in `tailed` (read_channel.rs:258-273; the merged test at
        // read_channel.rs:509).
        appended |= control_step(ledger, control, dispatcher, read_channel, &mut conns, &mut poll)?;

        // ... the EXISTING cap-breach loop and the watch/drain/parse fault events,
        // unchanged (they set `appended`) ...

        read_channel.persist_offsets(ledger)?;
        // cp-0's phase-1 TODO here is now MET: harvest 1 appended control.responded
        // above, so the offset commits AFTER the line's ledger effect.

        // C5: the final drain is now SEPARATE from disposal, so the harvest sits
        // BETWEEN them — before the unlink, restoring cp-0's own discipline.
        appended |= read_channel.final_drain_pending(ledger)?;
        // HARVEST 2 — DEFENSE IN DEPTH, honestly labelled. Under merged law
        // `final_drain_pending` yields ZERO lines (harvest 1 already read them), so
        // this is normally a no-op. It exists because `drain_one` has two callers
        // and a future phase could append session.stopped from INSIDE settle, which
        // would move the worker's last bytes onto this path. It is idempotent
        // (`mem::take`): no double-ingest, no double-append, no re-fanout.
        // DO NOT claim deleting it turns a test red — it does not (rev 2 claimed
        // that; it was false).
        //
        // If this harvest is EVER non-empty, cp-0's ordering guard fires in the same
        // breath and appends a durable patrol.degraded ORDERING VIOLATION. That is
        // CORRECT, not libel: non-empty here MEANS the ordering really was violated.
        // The two must co-occur, and the tests assert both directions.
        appended |= control_step(ledger, control, dispatcher, read_channel, &mut conns, &mut poll)?;
        // Only NOW: unlink the files and clear the cursors. The `take_disposed()`
        // inside control_step already captured each session's FINAL offset for the
        // `end` frame (C7).
        read_channel.dispose_pending(ledger)?;

        // B5: ONLY NOW may a deadline expire — after EVERY ingest this wake. A
        // response sitting in the file because its notify was coalesced must be read
        // and ingested before campd may declare it never arrived (cp-0's law,
        // event_loop.rs:406: correctness never depends on a delivered event).
        for input in control.expire_pending(Timestamp::now()) {
            ledger.append(input)?;
            appended = true;
        }
        if appended {
            if let Err(e) = settle(/* … */) { eprintln!("campd: control settle failed: {e:#}"); }
        }
```
**`control_step`** (one helper, two call sites): `take_stream_lines` → `control.ingest(...)` → append → **`control.fanout(read_channel, &mut conns)`** (Task 8: refresh each subscriber's `tail`, pump, drop over-cap ones) → **`control.close_disposed(read_channel.take_disposed(), ledger, &mut conns)`** (Task 8: mark subscribers `Closing` with the true final offset; also calls `control.forget_session` to prune `answered`) → deregister returned tokens. Returns whether it appended.

- [ ] **Step 6: `mod.rs` — construct and REHYDRATE (B6).** Beside the read channel (mod.rs:167), rehydrating AFTER `patrol::adopt`:
```rust
    let mut control = control::ControlRuntime::new(control::subscriber_buffer_bytes_from_env(
        control::SUBSCRIBER_BUFFER_BYTES_DEFAULT,
    )?);
    let restored = control.rehydrate(&ledger, jiff::Timestamp::now())?;
    if restored > 0 {
        eprintln!("campd: restored {restored} in-flight control request(s) from the ledger");
    }
```

- [ ] **Step 7: DELETE the module-level `#![allow(dead_code)]` from `control.rs`.** Run `cargo clippy -p camp --all-targets --all-features -- -D warnings` and confirm it passes without it. **`ControlRuntime.subscriber_buffer_bytes` keeps its OWN field-level allow until Task 8 (C4). DO NOT delete that field — Task 8 reads it.** *(Rev 2's text here told the implementer to delete anything still unreached. That was wrong and is removed.)*

- [ ] **Step 8: The fake worker, and the END-TO-END tests.**

`tests/fake-agent.sh` — `FAKE_AGENT_CONTROL_LOOP` (answer any `control_request` with the pinned `control_response`, `request_id` extracted with `sed`; a plain user turn ends the loop and closes the bead) and `FAKE_AGENT_EXIT_AFTER_CONTROL` (answer ONE and exit immediately — the reap-races-the-drain shape). Document both in the header block.

Create `crates/camp/tests/control.rs`, copying the harness (`munge`, `stdout_path`, `camp`, `camp_ok`, `scaffold`, `fake_agent`, `Daemon`, `connect`, `request`, `events_json`, `wait_until`) **verbatim** from `tests/read_channel.rs:1-180`; add `live_session_name(root)`. **6 tests here:**

1. **`interrupt_round_trips_through_the_read_channel`** — the exit criterion. `ok` + a `camp-` `request_id`; then `session.interrupted{request_id}`; then `control.responded{request_id, ok:true, verb:"session.interrupt", late:false}`; **and no `control.failed`**.
2. **`a_worker_that_answers_and_exits_immediately_still_yields_control_responded`** — **what this ACTUALLY proves (C5, stated honestly):** that the answer-and-exit race is covered **by harvest 1 under merged law**, and that the merged law still holds. Asserts `control.responded`, **no `control.failed`**, and — the part rev 2 omitted — **no `patrol.degraded` containing "ORDERING VIOLATION"**. A future phase that moves the reap's append inside `settle` breaks the law, harvest 2 starts firing, the guard shouts, and **this test goes red on the `patrol.degraded` assertion** — the real regression signal.
3. **`an_interrupt_whose_pipe_write_fails_is_loud_in_both_the_response_and_the_ledger`** (C12) — drive `ControlWrite::Failed` (a worker that never reads its pipe) and assert `ok:false` **and** a durable `control.failed{verb:"session.interrupt"}`.
4. **`send_turn_delivers_a_user_turn_into_the_held_pipe`** — `via:"stdin"`, `session.nudged`, and the worker's blocked `read` really unblocks (⇒ `bead.closed`).
5. **`interrupting_a_session_with_no_held_pipe_fails_loudly`** — `ok:false`, "no stdin pipe".
6. **`a_campd_restart_across_an_in_flight_interrupt_invents_no_fault`** (B6) — interrupt, wait for `session.interrupted`, `kill9()`, spawn a fresh campd, assert `control.responded{request_id}` lands **and no `control.failed` exists**.

- [ ] **Step 9: Run.** `cargo test -p camp --test control 2>&1 | tail -30` → PASS, **6 tests**.

- [ ] **Step 10: Full suite** (the D4 blast radius). `cargo test --workspace` → PASS. **`cli_nudge.rs` MUST still pass** (the CLI verb is unchanged), and so must `daemon_patrol.rs` (`nudge_via_stdin` survives — D4).

- [ ] **Step 11: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A
git commit -m "feat(control): session.interrupt + session.send_turn, harvested on every drain path (cp-1 §4.1)"
```

---

## Task 7: `sessions.list`

Spec: §4.1, §4.2, §4.3.

**Files:** `daemon/socket.rs`, `daemon/control.rs`, `daemon/patrol.rs` (one accessor), `daemon/event_loop.rs` (one arm); test in `tests/control.rs`.

- [ ] **Step 1: Write the failing tests** (**2**). In `socket.rs`: pin `{"op":"sessions.list"}`, `SessionInfo`'s field order, and the full response line — **asserting it contains no `pid`** (§4.2: *"a protocol that hands out pids cannot cross a machine boundary"*). In `tests/control.rs`: `sessions_list_reports_live_sessions_by_name` — one live session with `agent:"dev"`, `rig:"gc"`, `state:"working"`, `blocked:false`, an RFC3339 `last_activity`, a `gc-` bead, a `/dev/` name, and `s.get("pid").is_none()`.

- [ ] **Step 2: Run and watch both fail.**

- [ ] **Step 3: Implement.**
- `socket.rs`: `SessionInfo { name, agent, rig: Option<String>, bead: Option<String>, state: String, last_activity: String, blocked: bool }` (declaration order IS wire order — B1); `Request::SessionsList` (`#[serde(rename = "sessions.list")]`); `Response::SessionsList { ok, sessions }` FIRST among the untagged variants.
- `patrol.rs`: `pub fn is_stalled(&self, session: &str) -> bool` — **using the SAME `tracked` intersection `stalled_count` applies** (patrol.rs:230-237: *"a missed clear can never inflate the count"*). Divergent semantics between the count and the per-session answer would be a bug, not a shortcut.
- `control.rs::serve_sessions_list(ledger, patrol, read_channel)` — answers from the **LEDGER's** registry (`live_sessions()`), not campd's child map: an ADOPTED worker from a previous campd life is a live session too (§4.3). `state` is **exactly two values in cp-1** (`"stalled"` / `"working"`) and the doc comment promises no third. `blocked` is `false`; its producer is phase 3 (§5.3); the flow is structurally unreachable (§5.3.1); and a `can_use_tool` that arrives anyway is a LOUD `control.failed`, never a quietly-flipped bit. **The field is in the shape because §4.1's shape requires it: a protocol field awaiting its producer, not a guess.** `last_activity` = `read_channel.last_activity(name)`, else the registry's `spawned_ts`.
- `event_loop.rs`: one delegating arm.

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --bins daemon::socket && cargo test -p camp --test control` (**7** integration tests now).

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A && git commit -m "feat(control): sessions.list — every session by name, never by pid (cp-1 §4.1/§4.2)"
```

---

## Task 8: `session.subscribe` — one monotone cursor, a Closing state, a skip policy (D6″; C2/C6/C7/C8)

Spec: §4.4, §9, §8, §5.2.

**Files:** `daemon/control.rs`, `daemon/socket.rs` (wire types only — **no client API**, B2), `daemon/event_loop.rs`, `tests/fake-agent.sh`, `tests/control.rs`.

### The frame wire — TAGGED FROM BIRTH, and now THREE frames (B12/C8)

```json
{"frame":"event","session":"t/dev/1","offset":123,"event":{ …the worker's line, VERBATIM BYTES… }}
{"frame":"skipped","session":"t/dev/1","offset":456,"bytes":2097152,"reason":"line exceeds subscriber_buffer_bytes"}
{"frame":"end","session":"t/dev/1","offset":789,"reason":"stopped"}
```
**`event`'s payload is the worker's line SPLICED IN VERBATIM (C2)** — never re-serialized through a `Value`, which would sort its keys and hand cp-2/cp-4 a wire camp invented by accident. After the `end` frame campd flushes and closes (EOF).

### Constants, with their arithmetic

```rust
pub const SUBSCRIBER_BUFFER_BYTES_DEFAULT: usize = 1024 * 1024;  // §4.4's number
pub const HISTORY_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_PUMP_BYTES_PER_WAKE: usize = 256 * 1024;           // per subscriber (C6)
/// §4.4 bounds BYTES PER CONNECTION; nothing bounded the CONNECTION COUNT.
/// WORST CASE, STATED: MAX_SUBSCRIBERS * SUBSCRIBER_BUFFER_BYTES = 8 MiB of
/// outbound buffers on top of campd's idle RSS. That CAN approach the spec's
/// <20 MB figure — so, plainly: <20 MB is an IDLE bound (and it is exactly what
/// `make perf` measures: N subscribers with EMPTY buffers). A campd with 8
/// SATURATED subscribers is outside that bound BY DESIGN, and this cap is what
/// keeps it bounded at all. Raising it is a spec question, not a local call.
pub const MAX_SUBSCRIBERS: usize = 8;
```

### `Subscriber` — ONE cursor (D6″/C6), and a `Closing` state (C7)

```rust
struct Subscriber {
    id: String,
    session: String,
    /// The open stream file. Held across disposal ON PURPOSE — on Unix an unlinked
    /// inode survives while an fd is open, so a Closing subscriber FINISHES ITS
    /// HISTORY (C7: rev 2's text promised this and its code foreclosed it).
    file: std::fs::File,
    /// THE ONE CURSOR (D6″): the next byte this subscriber needs. MONOTONE, and the
    /// SOLE delivery gate — there is no separate live path, so there is no boundary
    /// to get wrong (C6: rev 2's catch-up/live split could BOTH duplicate AND
    /// silently truncate, and no test could see either).
    cursor: u64,
    /// What campd has actually DRAINED. Refreshed every wake from
    /// `read_channel.tail_state`; PINNED to the final offset once the session is
    /// disposed. `pump` reads ONLY [cursor, tail) — so it can never read bytes campd
    /// has not drained (rev 2 could, and delivered them twice).
    tail: u64,
    /// C7: set at disposal (stopped | crashed | capped). A Closing subscriber keeps
    /// pumping until `cursor == tail` AND `out` is empty; only THEN does the `end`
    /// frame go out, and the connection closes when that flush completes.
    closing: Option<String>,
    /// Bytes queued for this socket. HARD-capped: a frame that would cross
    /// `subscriber_buffer_bytes` drops the subscriber BEFORE it is appended (B10).
    out: Vec<u8>,
    /// The largest `out` WOULD have reached — `buffered_bytes` in
    /// `subscriber.dropped` (§4.4: "naming the session and the high-water mark").
    high_water: usize,
}
```

- [ ] **Step 1: Write the failing tests.**

**Unit (`control.rs`, 4):**
1. **`event_frame_splices_verbatim_and_refuses_a_non_object_line`** (C2's new-failure test) — `event_frame("t/dev/1", 123, r#"{"type":"system","subtype":"init"}"#)` produces **exactly** `{"frame":"event","session":"t/dev/1","offset":123,"event":{"type":"system","subtype":"init"}}\n` — **key order preserved, because the line is SPLICED, not re-serialized.** And `event_frame(_, _, "not json")` ⇒ `None` (so the history path agrees with cp-0's `Ok(_v)` arm).
2. `subscribe_frame_shapes_are_pinned` — all three frames (`event`, `skipped`, `end`).
3. `a_frame_that_would_cross_the_cap_drops_the_subscriber_before_it_is_appended` (B10) — the HARD cap; `buffered_bytes` is the ATTEMPTED size.
4. **`a_line_larger_than_the_cap_is_skipped_not_fatal`** (C8) — a single line bigger than the whole cap yields a `skipped` frame, the cursor advances past it, and the subscriber SURVIVES.

**Integration (`tests/control.rs`, 7):**
5. **`a_wedged_campd_fails_the_subscribe_hello_fast`** — the EXIT CRITERION. A bare bound `UnixListener` is the wedge simulator (socket.rs:751). `SubClient::open` returns `Err` (`WouldBlock`/`TimedOut`) **inside REQUEST_TIMEOUT**; assert elapsed < 15 s.
6. **`a_subscription_survives_a_quiet_period_longer_than_request_timeout`** (B13) — open at the tail, sleep **6 s** (> the 5 s `REQUEST_TIMEOUT`), then interrupt and assert a frame still arrives.
7. **`a_subscriber_catching_up_across_a_live_burst_gets_every_line_exactly_once_in_order`** — **C6's test: the window rev 2 never exercised.** A session with **>64 KiB** of history (more than one `HISTORY_CHUNK_BYTES`, so catch-up spans several pumps); subscribe at **cursor 0**; the worker appends **live lines DURING catch-up** (a second `send_turn` fired immediately after the hello). Assert every line arrives **exactly once**, **in file order**, with **strictly increasing `offset`s**. *Rev 2's design silently dropped a burst here and could double-deliver.*
8. **`a_subscriber_gets_the_full_history_then_an_end_frame_when_its_session_ends`** (B12/C7) — `FAKE_AGENT_EXIT_AFTER_CONTROL`; assert **every** line arrives BEFORE the `end` frame (not a truncated prefix), the `end` frame names the session and a `reason`, its `offset` equals the session's final offset, and **EOF never arrives without an `end` frame**.
9. **`a_closing_subscriber_that_stops_reading_is_still_dropped_at_the_cap`** (C7's new-failure test) — a Closing subscriber gets NO backpressure exemption, so it can never hold an fd forever.
10. **`a_hung_up_subscriber_is_forgotten_and_is_never_libeled_as_backpressure`** (B7) — drop the subscription, drive three wakes, assert campd still answers `status` promptly and **no `subscriber.dropped` exists**. A normal detach is not a fault (§5.2).
11. **`a_subscriber_that_stops_reading_is_dropped_loudly_and_campd_keeps_serving`** (§8/B8) — `FAKE_AGENT_SPAM_ON_TURN=8000` (≈720 KB, chosen to exceed *kernel socket buffer + app cap* on both platforms: macOS `net.local.stream.sendspace` ≈ 8 KiB, Linux ≈ 200 KiB — **a smaller spam is absorbed entirely by the kernel and the test becomes theatre**), `CAMP_SUBSCRIBER_BUFFER_BYTES=512`. Subscribe at the **tail** (clean hello), read NOTHING, `send_turn` to trigger the spam. Assert `subscriber.dropped{session, cap_bytes:512, buffered_bytes>512}`, then that campd answers `status` on a FRESH connection in < 5 s.
12. **`a_cursor_into_a_reaped_stream_or_past_the_tail_is_an_explicit_error`** (§9) — both, both explicit.

**`SubClient` — real bodies** (non-blocking note, adopted). `camp` is a binary crate, so there is no `socket::subscribe` (B2); this is the harness's own idiom.
```rust
struct SubClient { reader: BufReader<UnixStream>, stream: UnixStream, subscription: String, cursor: u64 }

impl SubClient {
    fn open(root: &Path, session: &str, cursor: Option<u64>) -> std::io::Result<SubClient> {
        let stream = UnixStream::connect(root.join("campd.sock"))?;
        // The HELLO is bounded by REQUEST_TIMEOUT (5 s, socket.rs:148) — a wedged
        // campd fails HERE, which is the exit criterion.
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let req = serde_json::json!({"op":"session.subscribe","session":session,"cursor":cursor});
        (&stream).write_all(format!("{req}\n").as_bytes())?;
        // try_clone: the BufReader owns one handle; `stream` keeps the other so the
        // read deadline can be CLEARED after the hello.
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut hello = String::new();
        reader.read_line(&mut hello)?;                       // times out on a wedge
        let v: serde_json::Value = serde_json::from_str(hello.trim_end())
            .map_err(|e| std::io::Error::other(format!("bad hello {hello:?}: {e}")))?;
        if v["ok"] != true {
            return Err(std::io::Error::other(format!("subscribe refused: {v}")));
        }
        // §4.4: TIMEOUT-EXEMPT after the hello — a quiet stream is not a wedged
        // daemon. THIS LINE is the exemption, and test 6 is what proves it.
        stream.set_read_timeout(None)?;
        Ok(SubClient {
            subscription: v["subscription"].as_str().unwrap_or_default().to_owned(),
            cursor: v["cursor"].as_u64().unwrap_or(0),
            reader,
            stream,
        })
    }

    /// The next frame, or None at EOF. `end` frames ARE returned — test 8 must SEE one.
    fn next_frame(&mut self) -> Option<serde_json::Value> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(line.trim_end()).ok(),
        }
    }
}
```

- [ ] **Step 2: Run and watch them fail.** (`bad request: unknown variant session.subscribe`.)

- [ ] **Step 3: Implement.**

**`event_frame` — the byte splice (C2): the only way to keep the worker's bytes verbatim without a new cargo feature.**
```rust
#[derive(Serialize)]
struct FramePrefix<'a> { frame: &'static str, session: &'a str, offset: u64 }

/// The worker's line is SPLICED IN VERBATIM — never round-tripped through a
/// `serde_json::Value`, which would SORT its keys (serde_json 1.0.150 has no
/// `preserve_order`, and `raw_value` is a cargo feature this plan does not add).
/// A subscriber therefore sees EXACTLY the bytes the worker wrote — the guarantee
/// cp-2/cp-4 actually need, and stronger than any re-serialized pin could be.
///
/// Returns None when `raw_line` is not a JSON OBJECT: splicing it would emit
/// invalid JSON. cp-0's `drain_one` only hands over lines that already parsed, but
/// `pump` reads the FILE directly — so the history path must check too. That is
/// what keeps history and live in agreement.
fn event_frame(session: &str, offset: u64, raw_line: &str) -> Option<Vec<u8>> {
    let trimmed = raw_line.trim();
    if !trimmed.starts_with('{') || serde_json::from_str::<serde_json::Value>(trimmed).is_err() {
        return None;
    }
    let prefix = serde_json::to_string(&FramePrefix { frame: "event", session, offset }).ok()?;
    // prefix == {"frame":"event","session":"…","offset":N} — replace its final '}'
    // with ,"event":<raw>} so the raw bytes land untouched.
    let mut out = prefix.into_bytes();
    out.pop()?;                                  // drop the closing '}'
    out.extend_from_slice(b",\"event\":");
    out.extend_from_slice(trimmed.as_bytes());
    out.extend_from_slice(b"}\n");
    Some(out)
}
```
(`skipped_frame` and `end_frame` are plain `#[derive(Serialize)]` structs — they carry no verbatim payload.)

**`serve_subscribe(token, session, cursor, read_channel)`:**
1. `subscribers.len() >= MAX_SUBSCRIBERS` ⇒ explicit error naming the cap.
2. `read_channel.tail_state(session)` is `None` ⇒ **not tailed** (never existed, or reaped and disposed) ⇒ explicit error citing §9.
3. `cursor > tail` ⇒ explicit error ("past the N bytes campd has consumed"). **Ordinary history is NOT an error** (B10/D6″).
4. Open the file; insert `Subscriber { cursor: cursor.unwrap_or(tail), tail, closing: None, out: Vec::new(), high_water: 0, … }`; return the hello. **It registers; it never writes** — the hello must be the FIRST bytes on the socket (`respond()` uses `write_all` on a NON-BLOCKING stream, event_loop.rs:997, and a WouldBlock there drops the connection — B11).

**`pump(token, conn) -> PumpOutcome` — the ONE data path (D6″) and the only place bytes reach a socket (B11):**
```
pumped = 0
loop {
    if out.is_empty() && cursor < tail {
        if pumped >= MAX_PUMP_BYTES_PER_WAKE { return Ok }        // C6: bounded on the loop
        read <= min(HISTORY_CHUNK_BYTES, tail - cursor) bytes at `cursor`
        for each COMPLETE line in the chunk:
            frame = event_frame(...) or (skipped_frame if the line is not a JSON object)
            if frame.len() > cap  => append skipped_frame; record patrol.degraded ONCE
                                     per (session, offset)                       // C8
            else if out.len() + frame.len() > cap
                                  => return Drop(subscriber.dropped{ high_water }) // B10
            else                  => append frame
            cursor = offset_after; pumped += line.len()
    }
    if out.is_empty() {
        if closing.is_some() && cursor == tail {
            append end_frame(session, tail, reason)                 // C7: history FIRST
            keep flushing; when `out` drains => return Gone (close the connection)
        }
        return Ok                                                   // nothing to send
    }
    match write(out) {
        Ok(n) => drain n,
        Ok(0) | EPIPE | ECONNRESET => return Gone,
        WouldBlock => return Ok,          // the kernel is full; the WRITABLE edge re-arms
    }
}
```
Called at **three** sites: right after the hello is written; on every WRITABLE readiness; after every `fanout`.

**`poll_timeout` (extended, C6):** `min(earliest pending control deadline, ZERO if any subscriber has cursor < tail OR a non-empty out)`. **`None` when neither holds** — so an idle campd with idle subscribers still blocks forever (invariant 1; Task 9's perf gate is what proves it).

**`fanout(read_channel, conns)`** (D6″ — it no longer touches `lines` at all): for each subscriber, refresh `tail` from `read_channel.tail_state(session)` (leaving it PINNED when `closing`), then `pump`. Returns the tokens to close and the `subscriber.dropped` events.

**`close_disposed(disposed: Vec<Disposed>, ledger, conns)`** (B12/C7): for each disposed session, every subscriber gets `closing = Some(reason)` (from `ledger.session_status(name)`: stopped / crashed / capped) and `tail = final_offset` (**C7's defined offset source, produced by Task 4's `dispose_pending`**), then `pump`. It does **not** close the connection — `pump` does, once the history is finished and the `end` frame has flushed. It also calls `control.forget_session(session)` to prune `answered`.

**`forget(token)`** — drop the subscription (every close path calls it). **`is_subscriber(token)`**; **`subscriber_count()`** (PERMANENT test-observable allow).

- [ ] **Step 4: Wire the event loop — B7's fix stays.**
- `struct Conn` → `pub(super)` (fields too).
- The accept arm registers `READABLE | WRITABLE`. *(Precisely: edge-triggered epoll/kqueue reports writability ONCE at registration — an accept-time cost, not an idle one. That already-consumed edge is exactly why the hello's first bytes need an explicit `pump` — B11.)*
- **The token arm — NO SHORT-CIRCUIT (B7).** If `control.is_subscriber(token)`, `pump` first (a WRITABLE wake is why we are here) and handle `Drop`/`Gone` — **then fall through into `serve_connection` like any other connection**, so cp-0's `ReadStop::Eof ⇒ ConnState::Closed` still detects a hangup.
- `control.forget(token)` on EVERY close path (`Closed`, the error arm, `Gone`, a cap drop). **A normal detach appends NO event** (§5.2).
- The new `drain_lines` arm: `serve_subscribe` → `respond(hello)` → `pump`.
- `control_step` gains `fanout` + `close_disposed` (Task 6 Step 5).

`tests/fake-agent.sh` — `FAKE_AGENT_SPAM_ON_TURN=N`: on a USER TURN, emit N stream-json lines. **The spam must come after the subscriber is registered** (B8); test 7 fires a second turn during catch-up.

- [ ] **Step 5: Run.**

Run: `cargo test -p camp --bins daemon::control && cargo test -p camp --test control 2>&1 | tail -40`
Expected: PASS — unit **18** (7 + 7 + 4); integration **14** (6 from Task 6 + 1 from Task 7 + 7 here).

- [ ] **Step 6: Full suite + commit.**
```bash
cargo test --workspace 2>&1 | tail -20
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A
git commit -m "feat(control): session.subscribe — one monotone cursor, a Closing state, a skip policy (cp-1 §4.4/§9)"
```

---

## Task 9: the §4.3 perf gate grows N idle subscribers

Spec: §4.3. cp-0 built the M-workers half; its gate deferred the N-subscribers half to the phase that builds `subscribe` — **this one** (lead-confirmed).

**Files:** `crates/camp/tests/perf_daemon.rs`.

- [ ] **Step 1: Extend the EXISTING idle gate** (one measured property, one test). `perf_daemon.rs` cannot link `daemon::*` (B2) and already speaks raw `UnixStream`. Open **N = 4** connections, send `{"op":"session.subscribe","session":"<s>","cursor":null}` on each (joining at the TAIL ⇒ nothing to stream), read each hello, HOLD them open across the existing idle window, and assert the existing 0.0% CPU-delta and <20 MB RSS numbers.

**State what this measures — and what it does not:**
```rust
    // cp-1 §4.3: N CONNECTED SUBSCRIBERS, held open, on QUIESCENT sessions. This
    // measures the WAKEUP PROFILE — the property §4.3 asks for: a subscription must
    // cost ZERO wakeups when its session is quiet (campd sleeps on the read-channel
    // self-pipe; a quiet worker writes nothing, so no notify fires, no pump runs,
    // and `poll_timeout` returns None). RED on CPU here means something in the
    // subscriber path wakes campd with nothing to do — a REAL invariant-1 bug, to be
    // FIXED, never accommodated.
    //
    // It does NOT measure the MEMORY ceiling: these four buffers are EMPTY. The
    // loaded worst case is MAX_SUBSCRIBERS * SUBSCRIBER_BUFFER_BYTES = 8 MiB on top
    // of idle RSS, which can approach the spec's <20 MB figure — so <20 MB is an
    // IDLE bound, stated plainly in the PR body rather than implied away.
```

- [ ] **Step 2: Run it (LOCAL-ONLY, per AGENTS.md).** `make perf 2>&1 | tail -30` → PASS.

- [ ] **Step 3: Commit.**
```bash
git add crates/camp/tests/perf_daemon.rs
git commit -m "test(perf): the idle gate now holds N connected subscribers (cp-1 §4.3)"
```

---

## Task 10: the $0 real-claude gate — camp's own bytes, and the NO-INITIALIZE arm (B15)

Spec: §2.1, §8, §9.

**Files:** `crates/camp/tests/claude_compat.rs`.

- [ ] **Step 1: The evidence that settles B15 (panel-reproduced).** Camp's shipped configuration is an interrupt with **no `initialize` ever sent**, while every recorded ack in the repo is POST-initialize and `FAKE_AGENT_CONTROL_LOOP` acks anything — §8's named trap. **Run against the pinned CLI, 2026-07-13:**
```bash
export CLAUDE_CONFIG_DIR=$(mktemp -d)   # hermetic: `verbose` defaults to false
printf '{"type":"control_request","request_id":"camp-b15","request":{"subtype":"interrupt"}}\n' \
  | claude -p --output-format stream-json --verbose --input-format stream-json \
           --session-id 7bd2befc-b018-4080-8738-429d541b3646
```
Verbatim output (exit 0, empty stderr):
```
{"type":"control_response","response":{"subtype":"success","request_id":"camp-b15","response":{"still_queued":[]}}}
```
The `subtype!=="initialize"` rejection that exists in the binary is the `[bridge:repl]` Remote-Control transport (*"This session is outbound-only…"*), **not** camp's stdio path.

- [ ] **Step 2: Make the gate send camp's OWN bytes, and add the arm.** An integration test cannot call `ParentMessage::to_line` (B2 — the same constraint `gate_core_flags_match_build_spec_held_stream_arm` works around at claude_compat.rs:132). **The fixture is the shared truth:** Task 1 pins the constructor against `interrupt_request.json`, and this gate sends `interrupt_request.json` to the real CLI. Transitively, **the bytes camp produces are the bytes the CLI accepts.** *Precisely (B14): this does NOT make the fixture "recorded" — camp authored it. The gate proves ACCEPTANCE, and PROVENANCE.md says exactly that and no more.*

Replace the hand-written literal (claude_compat.rs:387-390) with `include_str!("fixtures/control/interrupt_request.json")` + `fn interrupt_line(id)` (templating `camp-fixture-1`); add the CI-runnable guard `the_interrupt_fixture_is_a_well_formed_control_request`; and add:
```rust
/// B15 — THE CONFIGURATION CAMP ACTUALLY SHIPS: no initialize, ever. Just camp's
/// own interrupt bytes, straight at the real pinned CLI, before any turn. $0.
///
/// If this ever goes RED, cp-1's interrupt path is broken against the real CLI and
/// camp MUST start sending `initialize` (§9's "Camp sends it anyway"). Do NOT paper
/// over it by adding the handshake to this test.
#[test]
#[ignore = "real-claude $0 gate: run via `make compat` (CAMP_COMPAT=1)"]
fn no_initialize_pre_turn_interrupt_is_acked() { /* spawn; send interrupt_line; await_success */ }
```

- [ ] **Step 3: Run the CI-runnable half.** `cargo test -p camp --test claude_compat` → PASS (the ignored gates stay ignored).

- [ ] **Step 4: Run the $0 gate locally.** `make compat 2>&1 | tail -30` → PASS, printing `[compat] pre-turn interrupt acked with NO initialize`. If the installed `claude` does not match the pin, it fails loudly by design — **do NOT widen the pin**; report to the lead. If no pinned `claude` exists, **SAY SO in the PR** — never claim a gate ran.

- [ ] **Step 5: Commit.**
```bash
git add crates/camp/tests/claude_compat.rs
git commit -m "test(compat): the \$0 gate sends camp's own bytes and proves the no-initialize interrupt (cp-1 §8)"
```

---

## Task 11: gates, PR, honest description

- [ ] **Step 1: Rebase onto main.** `git fetch origin && git rebase origin/main`. **`event_loop.rs` is NOT additive** — expect a real conflict with compat-2. Keep both sides; if impossible, STOP and ask the lead.

- [ ] **Step 2: Prove the dead_code discipline held (C4).**
```bash
! grep -rn "first read in Task" crates/camp/src/ \
  || { echo "TEMPORARY dead_code allows survived — remove them"; exit 1; }
```
The only surviving allows must be the two marked `PERMANENT`.

- [ ] **Step 3: The three gates.**
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Confirm the counts: `daemon::control` **18** unit tests; `tests/control.rs` **14**.

- [ ] **Step 4: The local-only gates.** `make perf` and `make compat`.

- [ ] **Step 5: Push and open the PR.** The body MUST carry **six** honesty statements:

1. **The exit-criteria table** (criterion → the named test that proves it).
2. **"After cp-1, an operator still cannot interrupt anything by hand."** No `camp interrupt`, no `camp sessions`, no subscribe CLI. `interrupt` works end to end **between campd and a worker**, not between a human and a worker.
3. **The unverified claim:** *"Every interrupt exercised anywhere in this repo — fake or real — is PRE-TURN (a no-op interrupt whose ack carries `still_queued:[]`). Whether the CLI reads control messages from stdin WHILE A TURN IS STREAMING — the operationally meaningful interrupt — is untested at every layer and cannot be tested at $0. **cp-1 proves the TRANSPORT; the mid-turn semantics of interrupt are UNPROVEN against the real CLI.** D7 is what keeps that from mattering for correctness: the response deadline measures SILENCE, not elapsed time, so an interrupt queued behind a long turn cannot produce a false fault — and a late answer appends a correction rather than being swallowed. The paid `make e2e` tier (§8) is where the semantics get settled."*
4. **Fixture provenance:** every fixture labelled (`recorded-` / `derived-from-CLI-2.1.207` / `camp-authored`); `interrupt_request.json` is camp-authored and the $0 gate proves **acceptance**, not recording; **`can_use_tool`'s key set is NOT claimed complete** (a fixed-window grep of a minified bundle cannot prove it — the parse is tolerant by design); `dialog_refusal_response.json` carries a **phase-3 validation obligation**; and **cp-3's outbound permission bytes are pinned here** (`permission_allow_response.json` / `permission_deny_response.json`, against the CLI's own validator string).
5. **`make compat` is LOCAL-ONLY — CI does not run it.** `no_initialize_pre_turn_interrupt_is_acked` is a standing gate only as far as an operator runs it (the cp-0 precedent). Say it; do not imply CI protection.
6. **SECURITY — named, not solved.** `UnixListener::bind` (socket.rs:125/136) does **no** `set_permissions`, no umask discipline, no peer-credential check. Before cp-1 the socket exposed `poke`/`status`/`stop`/`adopt`/`nudge`. **`session.subscribe` is a NEW EXPOSURE CLASS:** any local process that can open the socket path now streams the complete raw stream-json of every session — assistant reasoning, tool inputs, file contents. **And cp-3 will put `session.permission_decision` on the same socket, at which point anyone who can connect can approve `cargo publish`.** cp-1 does not solve this; it **names** it and **files an issue** for the phase that owns it, so cp-3 does not inherit it as settled.

**Also record in the PR:** (a) a campd restart kills every subscription with a bare EOF and **no `end` frame** — the client's byte cursor stays valid (§9's point) but nothing tells it campd went away; cp-2's `camp watch` will meet this immediately. (b) B6's residual: if campd AND the worker both die during the outage, the session is never re-tailed, so the answered `control_response` is never read and the rehydrated pending expires into a `control.failed` whose stated cause is false. Narrow, named, not hidden.

- [ ] **Step 6: CI to green.** `gh pr checks --watch`. Work is NOT complete until it is.

- [ ] **Step 7: Report to the lead** — plan doc, branch, SHA, PR number, and whether `make perf` / `make compat` ran and what they said. Never claim a gate ran that did not.

---

## Self-review against the contract

| Contract item | Task |
|---|---|
| §2/§2.1 — one module owns the wire; shapes pinned by fixtures; failures loud | 1 (module + labelled fixtures, incl. **cp-3's outbound shape**), 3 (never-answered ⇒ durable fault; a restart neither lies nor forgets; **a late answer is corrected, not swallowed**), 6 (`ingest`; **and `ControlWrite::Failed` is loud in both channels**) |
| §4.1 `sessions.list` / `send_turn` / `interrupt` / `subscribe` | 7 / 6 / 6 / 8 |
| §4.4 — per-connection buffering, 1 MiB HARD cap, drop-loudly, hello within `REQUEST_TIMEOUT`, timeout-exempt after | 8 — one test each; all four exist |
| §8 fixture tests / backpressure | 1 + 10 / 8 |
| §4.3 perf obligation (N subscribers) | 9 — and it states what it does NOT measure |
| §9 — byte-offset cursors; a reaped stream is an explicit error; **ordinary history is never refused and never truncated** | 8 (D6″: one monotone cursor) |
| Exit: interrupt + send_turn end to end over the real socket vs a fake worker | 6 (incl. the answer-and-exit race, a restart, and a failed pipe write) |
| Exit: a wedged-campd subscribe fails fast at the hello | 8 |
| Exit: fixtures pin every shape camp sends or parses | 1 + 8 (**the three subscribe frames, spliced verbatim**) + 10 |
| Exit: CI green | 11 |

# cp-1: the control protocol — one module owns the wire, four verbs on the socket — Implementation Plan

## Plan-gate approval

APPROVED 2026-07-13 (rev 6, b589672) after SIX adversarial review rounds
(4-panelist panels: contract-completeness / interface-regression /
execution-readiness + a completeness critic, each defaulting to BLOCK).

Rounds 1-5 found and closed, among others: a silent-data-loss reintroduction
of cp-0's own worst bug; a `pump` that livelocked campd on any stream line
> 64 KiB; a `pump` that dropped healthy fast-reading subscribers on any
backlog > 1 MiB; a cumulative one-byte-per-line offset drift that broke §9's
resume cursor; three separate "this test gates it" claims that were false;
and an invented `request_user_dialog` wire shape that cp-3 would have
inherited as pinned (corrected by extracting the real shape from the shipped
CLI bundle).

D1-D5 ratified. §4.4 amendment approved by the operator (see below).

Non-blocking notes accepted at approval: the loaded perf arm is local-only
(`make perf` is #[ignore]d and never runs in CI) — the unit tests are the
CI-side defence and the PR body must say so; only half the subscriber drop
policy transfers to cp-2 (the stall rule does; "hold the line in `partial`"
cannot, since a ledger event has no file to be held in).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (this stream is planning-only; a FRESH implementer session executes after plan-gate APPROVE). Steps use checkbox (`- [ ]`) syntax. Branch: `cp-1-control-protocol`.

**Goal:** Give campd a control plane: ONE module that owns the undocumented `claude` control wire format (pinned by fixtures whose provenance is labelled), and the first four socket verbs — `sessions.list`, `session.send_turn`, `session.interrupt`, `session.subscribe` — with `interrupt`'s `control_response` round-tripping back over cp-0's read channel, and `subscribe` as a bounded, drop-loudly, streaming connection MODE.

**Architecture:** `crates/camp/src/daemon/control.rs` is the ONLY place in camp that constructs or parses a control message (spec §2.1). It holds `ControlRuntime`: the pending-request table (rebuilt from the ledger at startup; its deadline joins `min_deadline` and is RESET by session activity), the subscriber registry (**one monotone byte cursor per subscriber, fed only from the stream file, never past what campd has drained** — rev 3's central simplification), and every socket-verb handler body, so `event_loop.rs`'s new arms are one-line delegations. campd writes control requests into the worker's already-held stdin (the `nudge_via_stdin` bounded-write mold); the worker's `control_response` returns as a line in its stdout file, which cp-0's `ReadChannelRuntime` tails to EOF on every wake.

**Tech Stack:** Rust (workspace edition), `mio`, `serde`/`serde_json`, `uuid`, `jiff`, `rusqlite` via `camp_core::ledger`, `tempfile` (dev-dep). **No new dependencies. No new cargo features** — and rev 3 makes that constraint *achievable* (C2) instead of merely asserted.

---

## What changed in rev 6 — THE FINAL PLAN ROUND (after the rev-5 REJECT at 03cf832)

**The design is not reopened.** "The cap is a STOP; the drop is a STALLED PEER" is the answer §4.1 and §9 needed, and it stands. R2, R3, R4, R5's withdrawal, R6 are CLOSED. Everything below is inside **~15 lines**: the FILL byte loop and `try_emit_line`.

### The root cause: one edit cascaded

**R3's "test the newline FIRST, before any push" broke `try_emit_line`'s own stated precondition** (*"emit ONE complete line from `partial`, **which ends with `\n`**"*). Under rev 5's only call site, **`partial` never ends with `\n`.** B1–B3 all fall out of that. I added the `+1` for the newline in the *oversize* branch and **did not make the identical fix in the normal branch** — an incomplete refactor, visible in the diff.

### THE FOURTH DIMENSION — offset fidelity. This is the one that explains all four rounds.

> *"Every revision breaks precisely the property its suite measures only RELATIVELY. Rev 3 asserted lines ARRIVE; it never asked HOW LONG. Rev 4 asserted drops HAPPEN; it never asked TO WHOM. Rev 5 asserts offsets INCREASE; it never asks whether an offset MEANS anything."*

**Not one test in rev 5 ever takes an `offset` off the wire and feeds it back as a `cursor`.** Tests 7 and 16 assert offsets are *strictly increasing* — **and drifting offsets still increase.** So B1's cumulative drift was invisible to all 23 tests, exactly as the long line was invisible in rev 3 and the reading-client backlog in rev 4. **Fourth round, same shape: the fix broke the thing no fixture measures.**

| dimension | spanned by | gap | rev 6 |
|---|---|---|---|
| line length | `FAKE_AGENT_HUGE_LINE` (2 MiB) | — | kept |
| backlog × read rate | test 16 (≥2 MiB, **reading**, default cap) | — | kept |
| frame-vs-line boundary | unit 8, 9 | — | kept |
| byte fidelity | unit 10 | **premise false (B4)** | re-aimed at the REFUSAL |
| lifetime × read state | tests 4, 5 + hard deadlines | — | kept |
| **OFFSET FIDELITY — the cursor round-trip** | **NOTHING** | **B1's per-line drift is invisible to every test** | **(i) one line in test 4: `assert_eq!(last_event.offset, end.offset)` — drift makes them differ by the line count; (ii) NEW test 13 `a_client_that_resubscribes_from_a_delivered_offset_resumes_exactly_there` — §9's resume promise, tested by nothing** |
| **two subscribers, one session** | **NOTHING** | the `over_cap` `patrol.degraded` dedupe (`HashSet<(session,offset)>`) — **its whole reason to exist** — is never exercised; cp-2 inherits it | **NEW test 14** |
| **a live subscriber across a campd restart** | **NOTHING** | named in rev-3's notes, tested by nothing | **NEW test 15** (the subscription dies with campd; the client's cursor stays valid and it resumes — §9) |

| # | Defect | Fix (one line each, in the FILL loop) | **What the fix could newly break, and the test for it** |
|---|---|---|---|
| **B1** | **`off` is off by one, PER LINE, CUMULATIVELY.** `off = cursor + partial.len()` with the `\n` no longer in `partial` ⇒ `cursor` lands **ON** the newline, drifting one byte per line. §9 makes these the **durable resume cursors**: a client reconnecting with a cursor campd handed it lands **mid-file at the wrong byte**. The `end` frame's offset (`= tail`) is correct, so the last `event` offset and the `end` offset **disagree by the line count**. | **Push the `\n` into `partial`, THEN call `try_emit_line`.** Cap-safe: R3's pre-push guard already bounds `partial.len() ≤ cap − frame_overhead`, and `body` strips the `\n`, so `frame.len() ≤ cap` still fits an empty `out`. | An off-by-one the other way (skipping a byte) would corrupt every body. **Tests: the one-line offset-equality assertion in test 4, and the round-trip in test 13** — a *relative* assertion can never catch either direction. |
| **B2** | **The stall `break` abandons the rest of the chunk while `scan` has already advanced past it — SILENT TRUNCATION.** `scan += buf.len()` up front, then `break` mid-`buf`; `Subscriber` has no field for an unconsumed remainder, and the next read is positional at `scan`. **Up to 64 KiB of lines silently lost**, plus a permanent `cursor`/`scan` desync. **Rev 4 was safe here ONLY because its early exit was `return Drop`** — my `break` keeps the subscriber alive and converts a wrong-but-LOUD drop into a silent truncation (§9: *"never a silently truncated stream"*). **Reachable in my own flagship test:** in test 16's steady state `out` sits at the cap, so `try_emit_line` returns false on most chunks — **test 16 is RED against rev 5's `pump`.** | **Advance `scan` (and `scanned`) PER BYTE ABSORBED**, not per chunk read. On a stall, the unconsumed remainder is still at `[scan, …)` and is simply re-read. The `cursor ≤ scan` gap stays exactly *"the in-progress line"*, as the doc comment already claims. | Per-byte accounting could double-count on a retry. It cannot: the retry path consumes from `partial`, not from the file, and `scan` is never rewound. **Test: `a_cap_stop_mid_chunk_loses_no_bytes`** (unit) — plus test 16, which now genuinely exercises it. |
| **B3** | **The held-line retry is DEAD CODE, and a line held at `scan == tail` strands the subscriber FOREVER.** (a) the predicate `if partial ends with b'\n'` is **always false** under R3 ⇒ the held line is never retried, the next line's bytes are appended onto it, **two lines are concatenated into one body** ⇒ `event_frame` rejects it ⇒ `skipped{not_a_json_object}` — corruption with a **false cause**. **The central rev-5 mechanism never fires.** (b) the retry sits inside `while … scan < tail`, so a line held at `scan == tail` — **the normal terminal state of any catch-up that ran at the cap** — is never re-entered; `poll_timeout`'s `subscriber_work` needs `scan < tail` ⇒ false; `blocked_since` is `None` (the peer IS reading) ⇒ **nothing armed, no wakeup source, the last line never delivered.** (c) TERMINAL then requires `partial.is_empty()` ⇒ **no `end` frame, no EOF, fd + slot leaked** — R2's exact symptom, resurrected through R1's own fix. Also `stalled` is declared outside the outer `loop` and **never reset**. | **(a)** a real **`held: bool`** flag, not a `partial` inspection. **(b)** hoist the retry out of the tail guard: `while !stalled && (sub.held || sub.scan < sub.tail) && …`. **(c)** `poll_timeout`'s `subscriber_work` gains `|| s.held`. **(d)** TERMINAL's guard becomes `!sub.held && partial.is_empty() && oversize.is_none()`. **(e)** `stalled` is reset at the top of each outer iteration. | A `held` flag that is set but never cleared would stall forever. `try_emit_line` is the ONLY writer and clears it on every success path (including the blank-line path). **Test: `a_line_held_at_the_cap_is_retried_and_never_concatenated`** (unit) — assert the held line is delivered **as its own frame** after `out` drains, and that the next line is a separate frame. |
| **B4** | **Unit test 10's premise is FALSE and the test is unsatisfiable — verified by running it.** I claimed *"JSON permits non-UTF-8 in strings"*. It does not: **JSON text is UTF-8 by definition and `serde_json::from_slice` ENFORCES it** — `Err("invalid unicode code point")`. So `event_frame` returns `None`, the body is *skipped*, and a byte-identical round-trip is **unachievable by any implementation of my own spec**. The implementer inherits a permanently-RED test they are forbidden to delete, whose only escape is to strip `event_frame`'s validation — which would put invalid JSON on the wire and break test 1. | **The R7 signature stands** (`&[u8]` + `from_slice` **REFUSES**; the `&str` + `from_utf8_lossy` path would substitute U+FFFD and splice the **corrupted** bytes — *that* is the difference worth having). **The TEST is re-aimed at the refusal**: a line carrying raw non-UTF-8 yields `skipped{reason:"not_a_json_object"}` and is **never delivered with silently-substituted bytes**. The false claim is deleted. | Re-aiming could hide a real corruption path. It cannot: the test asserts the bytes are **refused**, not merely "handled" — and asserts the U+FFFD substitution does **not** appear on the wire. |

**Mechanical fixes, all applied:** R5's replacement over-claim **struck** (the unit test calls `close_disposed` directly, so it *cannot* observe the event loop's call order — the **structural** guarantee is the real protection; this is the third false "this test gates it" claim and I am now asking, for each one, *what code change would make it red?*) · `close_disposed_emits_the_end_frame…` **moved to the UNIT list** (it uses `test_insert_subscriber`, and integration tests cannot link `daemon::*`) · **unit tests 5 and 6 and integration test 6 rewritten against the new contract** — they specified rev-4's cap-drop, which R1 **deleted** (a frame crossing the cap now **stalls**; the drop fires at the stall timeout) · **`CAMP_SUBSCRIBER_STALL_TIMEOUT` env override added** (without it, the stall tests are mandatory 30 s wall-clock and their hard deadlines would exceed the hang they exist to detect) · **counts recomputed once, carefully** (unit **31**, integration **21**) · **§4.4 is AMENDED IN THE SAME PR** (below) · the memory bound is **~2× cap per subscriber** (`out` ≤ cap **and** `partial` ≤ cap) ⇒ **~16 MiB at 8 subscribers, not 8 MiB** · the cp-2 `OutBuf` claim is **struck** (the struct I wrote is flat) and replaced with an honest note · `pump`'s signature carries `now` · the `oversize` `skipped` frame is appended without a cap check, so `out` may exceed the cap by **one small frame** — bounded, and stated.

### §4.4 IS AMENDED IN THIS PR (AGENTS.md: spec and code never silently diverge)

§4.4 says *"a subscriber whose buffer crosses the cap is dropped."* **cp-1 implements something different, deliberately**, and carrying that as an unrecorded divergence is exactly what the repo forbids. **Task 11 gains a step: amend §4.4 of `docs/superpowers/specs/2026-07-12-camp-control-plane-design.md` in the same PR**, to read (in substance):

> **`subscriber_buffer_bytes` (1 MiB) is a STOP, not a kill.** When the buffer is full campd stops framing and holds the next complete line; nothing is lost and nothing is dropped. **A subscriber is dropped when its PEER STOPS READING** — its socket has accepted zero bytes for `SUBSCRIBER_STALL_TIMEOUT` (30 s) with data buffered — reported as `subscriber.dropped` naming the session and the high-water mark. *Rationale: during catch-up the producer is a FILE read and a file always outruns a socket, so a buffer-size kill drops healthy, fast-reading clients that are merely behind — which breaks §4.1's "a late joiner gets history, then follows" and §9's "never a silently truncated stream" for any session with more than 1 MiB of output. campd still never blocks, memory is still bounded (`out` ≤ cap, `partial` ≤ cap), and a genuinely stalled peer is still dropped loudly.*

**Residual, decided and stated rather than hidden:** a peer accepting **one byte every 29 s** clears `blocked_since` on every wake and can hold 1 MiB and one of 8 slots indefinitely. **Decision: ACCEPT for cp-1** — that peer *is* reading, and any byte-rate floor is a policy number nobody has evidence for. **It is named here because cp-2 inherits this rule verbatim, and `camp watch` is precisely the thing an operator leaves open in a scrolled-back terminal all day.** A byte-rate floor, or a bound on time-at-cap, is the honest fix; it is recorded as a **cp-2 obligation**, not silently deferred.

> **CLOSED (#121), and not by cp-2 — cp-2 neither implemented this obligation nor re-deferred it, and a deferral-chain audit found it still open.** The resolution is the **bound on time-at-cap**, not the byte-rate floor: `OutBuf::at_cap_since` is stamped when the cap stops a source and cleared only when the buffer drains to empty, and a subscriber continuously at the cap for `AT_CAP_STALL_INTERVALS` (10) stall intervals is dropped with the **same** `subscriber.dropped` event as a zero-accept peer. See the §4.4 amendment in `docs/superpowers/specs/2026-07-12-camp-control-plane-design.md` for the full rationale (why not a floor; why 10; the backlog bound it implies; why it arms no timer).

---

## What changed in rev 5, and why (after the rev-4 REJECT at bd7d404)

**Scope, per the gate: `pump` and its struct. Nothing else.** Tasks 1, 2, 4, 5, 6, 7, 10, 11, the fixtures, the `cause` enum, the ceiling and the dead_code discipline are settled and are not reopened. Rev 4's G1–G11 fixes stand (the panel traced six inputs through the new data path and confirmed the livelock and the spin are dead).

### The dimensions my fixtures do not span — written FIRST, because this is the lesson of the phase

Three revisions running, a real defect became a green check because **no fixture could see it**. Rev 3's bug was invisible because no fixture emitted a long **line**. Rev 4's is invisible because no fixture builds a large **backlog for a reading client**. So, before any code:

| dimension | spanned by | **gap in rev 4** | rev 5 |
|---|---|---|---|
| **line length** | `FAKE_AGENT_HUGE_LINE` (2 MiB) — test 13 | — (closed in rev 4) | kept |
| **backlog size × read rate** | test 7 (>256 KiB, reading) · test 12 (2.7 MB, **not** reading) | **THE HOLE.** No test subscribes a **READING** client to a backlog **larger than the cap**. Test 7 is under the cap; test 12's client reads nothing (its drop is *correct*); test 13's monster is *skipped*, so `out` stays tiny. **The drop path is exercised only where dropping is right — never where it is a catastrophe.** | **NEW test 16 `a_reading_subscriber_survives_a_history_larger_than_the_cap`** — ≥ 2 MiB of *deliverable* history, at the **default** cap, client reading every frame: every line exactly once, in order, **no `subscriber.dropped`**. **Against rev 4's `pump` it is RED.** |
| **frame-vs-line boundary** | nothing | the ~60-byte band where a line is *not* over-cap but its **frame** cannot fit (R3) | unit test `a_line_whose_frame_just_exceeds_the_cap_is_skipped_not_dropped` |
| **byte fidelity** | every fixture is ASCII | a non-UTF-8 body would be silently rewritten by a `from_utf8_lossy` the code block forces (R7) | unit test `event_frame_preserves_non_utf8_bytes` |
| **session lifetime × read state** | test 8 (behind), test 9 (caught up) | both would **HANG, not fail** (R2) — only test 13 had a deadline | **every** subscriber integration test gets a hard deadline |
| **restart** | test 6, G5's test | `timed_out` unbounded across restarts (non-blocking) | `rehydrate` liveness filter |

| # | Defect | Fix | **What the fix could newly break, and the test for it** |
|---|---|---|---|
| **R1** | **`pump` DROPS HEALTHY, FAST-READING SUBSCRIBERS BY CONSTRUCTION.** FILL frames up to `MAX_PUMP_BYTES_PER_WAKE` (256 KiB) *before any flush*, while the socket accepts ~8 KiB (`net.local.stream.sendspace = 8192`, **verified on this machine**) ⇒ `out` grows ~254 KiB/wake ⇒ the 1 MiB cap is hit in **~4 wakes** and the next frame returns `Drop`. **Any client joining >1 MiB behind the tail is dropped no matter how fast it reads** — during catch-up the producer is `pump` reading a *file*, and a file always outruns a socket. §9's late-joiner promise is broken for any session with >1 MiB of stdout (one large `Read` exceeds it), reported as `subscriber.dropped` — **a false-cause event about a client that was reading perfectly** (invariant 3) — and it is **permanent** (re-subscribing re-fills and re-drops). **My rev-4 new-failure column asserted the opposite and was false on both clauses: filling does not *stop* at the cap, it *kills* at it; and the producer *always* outruns the socket during catch-up.** | **The cap and the kill are SEPARATED — the policy question answered explicitly.** **What is the cap protecting against? A peer that has stopped reading.** So: **FILL STOPS at the cap** (the complete line is *held in `partial`*, `cursor` is NOT advanced, nothing is lost); and **the DROP is triggered by the peer, not by the buffer** — `blocked_since` is stamped when a write accepts **zero** bytes with `out` non-empty, cleared the moment **any** byte is accepted, and a peer still at zero after **`SUBSCRIBER_STALL_TIMEOUT` (30 s)** is dropped with `subscriber.dropped`. A fast reader now catches up across an arbitrarily large history at ≤ cap of resident buffer. | Holding a line in `partial` could stall a subscriber forever if its frame can *never* fit an empty `out`. **That is exactly why R3 is a PREREQUISITE**: the frame-sized over-cap threshold guarantees every held line's frame fits an empty `out`, so a stalled line always drains eventually. **Tests: 16 (reading client, >cap history, no drop) AND 12 (non-reading client, still dropped — now by the STALL TIMEOUT, with `buffered_bytes` naming the true cause).** |
| **R2** | **The TERMINAL branch is an infinite loop.** `end_frame_was_sent` is referenced once, is **not a field**, and cannot be a `pump`-local (it must survive a `WouldBlock` return and the next WRITABLE re-entry). Trace a caught-up Closing subscriber whose client *is* reading: (B) appends the `end` frame → (C) writes it → `out` drains → loop → (A) no-op → **(B)'s guard is still satisfied and appends a SECOND `end` frame** → … **unbounded duplicate `end` frames, `Gone` never returned, EOF never arrives, the fd and one of 8 slots never released.** **It also falsifies my own termination proof** — the (B) iteration neither advances `scan` nor drains `out`; it *grows* `out`. | **`end_sent: bool` is a `Subscriber` FIELD.** (B) is guarded by `&& !sub.end_sent` and sets it; (C) returns `Gone` when `out.is_empty() && sub.end_sent`. **The termination proof is restated to cover (B)** (it fires at most once per subscriber, by the flag). | A flag that is set but never reaches (C) would strand the connection. (C) is the *only* exit and tests both conditions. **AND: every subscriber integration test now carries a HARD DEADLINE** — rev 4's tests 8 and 9 would have **hung, not failed**, and a hang that "passes" is precisely the failure mode I flagged for test 13 and then applied nowhere else. |
| **R3** | **The over-cap threshold tests the LINE; the drop tests the FRAME.** C8 survives in the ~60-byte band between them: a line whose raw length is in `(cap − prefix, cap]` is never *skipped*, yet its frame cannot fit an empty `out` ⇒ it takes the **`Drop`** path — a perfectly-reading subscriber dropped, re-subscribing, re-reading the same line, **dropped again, deterministically and permanently.** Rev 3 keyed the skip on `frame.len() > cap`; **rev 4 deleted that check.** **Second hole in the same branch:** the byte is pushed *before* the cap test, so when the crossing byte **is the `\n`**, the `continue` bypasses the newline check, `oversize` arms, and the scan runs to the **next** line's `\n` — **silently consuming a whole line with no frame**, and reporting a `bytes` count spanning two lines. | **`over_cap` is decided on the FRAME**: `partial.len() + 1 + frame_overhead > cap` (with `frame_overhead` measured once per subscriber at the hello), and **`b == b'\n'` is tested FIRST**, before any push or cap check. So *any* line whose frame cannot fit becomes `skipped{over_cap}` and **never** `Drop` — restoring the law rev 4 stated and then broke. | Measuring `frame_overhead` wrong would mis-classify the band. It is *measured, not computed*: `event_frame(session, u64::MAX, b"{}")!.len() - 2`, at the widest possible offset. **Test: `a_line_whose_frame_just_exceeds_the_cap_is_skipped_not_dropped`** (a line of exactly `cap - frame_overhead + 1` bytes) and **`a_line_ending_exactly_at_the_cap_boundary_is_not_conflated_with_the_next`**. |
| **R4** | **The FILL read has no short-read or error arm** ⇒ `Ok(0)` advances neither `scan` nor `out` while the `while` guard stays true ⇒ **campd hangs inside `pump`**, no timeout, no fault. An `Err` is unhandled, and `unwrap`/`panic` are denied — so the implementer must invent the arm my safety proof depends on. | **Both arms are specified.** `Ok(0)` with `scan < tail` is a genuine inconsistency (the stream file is append-only; it cannot shrink) ⇒ a durable `patrol.degraded` **and** `Gone`. `Err(Interrupted)` ⇒ retry. Any other `Err` ⇒ durable fault **and** `Gone`. | Returning `Gone` on a transient error would drop a subscriber needlessly. `Interrupted` is retried; everything else on a *file* fd is genuinely terminal for that subscriber, and the fault says so. |
| **R5** | **Test 9 — the named gate for the disposal ordering — CANNOT FAIL, and my patch for it names a mechanism that does not exist.** `on_watch_event` (read_channel.rs:806-815) sets `signal = true` in **every** arm, and `unregister`'s `remove_file` fires a `Remove` ⇒ campd **always** gets another wake ⇒ under the broken ordering the disposed list simply persists and the *next* wake emits the `end` frame **one wake late**. **Test 9 is GREEN on the broken ordering.** So G4's stated symptom ("blocks forever") is false, and **the ordering currently ships with NO GATE AT ALL.** | **THE CLAIM IS WITHDRAWN IN WRITING** (as C5's was). The **ordering fix STAYS** — correctness must not depend on a delivered event (cp-0's law, event_loop.rs:406-408) — but its guarantee is made **STRUCTURAL, not behavioural**: `take_disposed()` has **exactly one caller**, immediately after `dispose_pending`; `close_disposed` is **not reachable from `control_step`**. **Plus a deterministic UNIT test** (`close_disposed_emits_the_end_frame_for_a_caught_up_subscriber`) that drives `close_disposed` directly over a `UnixStream::pair` — no daemon, no notify, no timing. | A structural guarantee can be silently undone by a later refactor. The unit test is what fails if `close_disposed` stops being called after `dispose_pending`, and the one-caller rule is stated at the call site. **Test 9 remains, honestly relabelled: it proves the end frame ARRIVES for a caught-up subscriber — it does NOT prove the ordering, and it never could.** |
| **R6** | **`forget_session` breaks Task 6's clippy gate** (added in Task 3; its only production caller is `close_disposed`, in Task 8) — and it is absent from the dead_code table. **And Task 6 Step 5's block calls `close_disposed`, `forget` and `take_disposed` — all Task 8 — so Task 6 cannot compile as written.** | **The task split is made explicit.** **Task 6's event-loop block keeps calling the merged `apply_pending_unregisters` wrapper** (drain + dispose, exactly as main does today) — so Task 6 compiles and its gate passes. **Task 8 REPLACES that one call** with the split (`final_drain_pending` → harvest 2 → `dispose_pending` → `close_disposed`). The table is updated: `final_drain_pending` / `dispose_pending` / `tail_state` / `take_disposed` / `forget_session` all have **first read in Task 8**. | Splitting later could leave Task 6's wrapper path untested. It is the *merged* path, already covered by every cp-0 test; Task 8's Step 4 re-runs them after the swap. |
| **R7** | **`event_frame` has three mutually incompatible signatures**, and the given code block (`&str` + `trim()` + `from_str`) forces the implementer to convert bytes→`&str` inside `pump` — for which the natural move is cp-0's `from_utf8_lossy`, **which silently rewrites the worker's bytes: the exact corruption C2's byte-splice exists to prevent.** No fixture would catch it — every one is ASCII. | **ONE signature: `fn event_frame(session: &str, offset: u64, body: &[u8]) -> Option<Vec<u8>>`** — byte-level trim, `serde_json::from_slice` validation, verbatim byte splice. `pump` never decodes UTF-8, anywhere. | A byte-level API could accept a body that is valid JSON but not valid UTF-8 in a string. That is exactly what must round-trip. **Test: `event_frame_preserves_non_utf8_bytes`** — a JSON line whose string contains raw non-UTF-8 bytes emerges **byte-identical** (JSON permits it; the splice must not care). |

**Non-blocking items folded in:** `serve_interrupt`'s E0382 (clone `rig`/`bead` at the append site, not before the move) · `expire_pending` derives `cause` by comparing the two **BOUNDS**, not either against `now` (a delayed wake made a ceiling expiry report `silence_timeout` — invariant 3) · `rehydrate` gains a **liveness filter** (`session.stopped`/`session.crashed`), so `timed_out` is not rebuilt for every interrupt that ever timed out in the ledger's history — my G7 "bounded by live sessions" claim held *within* a campd life and was **false across a restart** · test 3 steps the clock past the **ceiling** (300 s), not 3× the timeout (90 s) · a **deadline-bearing `SubClient::next_frame_within(dur)`** (the §4.4 exemption clears the read timeout, so every subscriber test would otherwise hang) · **counts fixed in G9's own row** (`tests/control.rs` = **19**: 6 + 1 + 12) and the Task 5/7 "count" lines corrected to say *new tests*, not filter output · a **named `pump` unit harness** (`ControlRuntime::test_insert_subscriber` + `UnixStream::pair()`, the `dispatch::test_insert_held_cat` precedent) · the loaded perf arm is honestly labelled a **spec, not a test**, and `make perf` is `#[ignore]`d + LOCAL-ONLY, **so no CI gate catches a spin** — said plainly · the **`tail` is line-aligned** cross-module invariant is NAMED where the TERMINAL guard depends on it · the oversize scan's ~1024-wake tick-storm cost (each re-running `persist_offsets`) is acknowledged · cursors must be **campd-issued** (a mid-line cursor yields one `skipped{not_a_json_object}`, then correct behaviour) · `MAX_FAULTS…`'s summary is deferred to the **end of `ingest`** (the count is unknown when the 8th event is built) · **cp-2's seam is re-cut**: `Subscriber { out: OutBuf, source: Source }` — `OutBuf` owns the cap/stall/drop policy **exactly once**, and **whatever R1's rule becomes is what cp-2 inherits** · `permission_unanswerable`'s `reason` now names the verb (`camp stop <session>`).

---

## What changed in rev 4, and why (after the rev-3 REJECT at 3681b43)

**Rev 4's scope is deliberately narrow, per the gate:** a rewrite of **Task 8's data path (`pump`)**, **the event-loop disposal call order**, and **Task 3's rehydration + D7's ceiling** — plus the mechanical gates that stop an implementer cold (G8–G11). **Nothing else is touched.** Tasks 1, 2 (except one payload field), 5, 6 (except the Step-5 wiring), 7, 10 and the fixtures/provenance work are CLOSED and are not re-opened.

**I traced the algorithm on paper against the three inputs before writing a line**, as instructed. All three were broken, exactly as reported:

| input | rev-3 `pump` | rev-4 `pump` |
|---|---|---|
| **a single 100 KiB line** | chunk = 64 KiB contains no `\n` ⇒ the `for each COMPLETE line` body never runs ⇒ `cursor` never advances ⇒ `pumped` stays 0 ⇒ `poll_timeout` = `Some(ZERO)` ⇒ **campd re-reads the same 64 KiB forever at 100 % CPU** | a `partial` buffer spans chunks (cp-0's own discipline, read_channel.rs:588/613-624); the line completes on the second chunk and is framed |
| **a `WouldBlock` on a healthy reader** (macOS `sendspace` ≈ 8 KiB vs a ~64 KiB chunk ⇒ **every** healthy subscriber WouldBlocks on **every** chunk) | `out` non-empty ⇒ `Some(ZERO)` ⇒ poll(0) → pump → WouldBlock → poll(0) … **campd spins for the whole duration of any stream**, re-running `apply_tracking`, `drain_all` and `persist_offsets` each pass | `poll_timeout` arms `ZERO` **only on `out.is_empty() && scan < tail`** — a blocked write is unblocked by the **WRITABLE edge**, which is already registered and already relied on |
| **a subscriber exactly caught up when its session is reaped** | `take_disposed()` ran **before** `dispose_pending()` produced it ⇒ `closing` never set; the caught-up subscriber has `poll_timeout == None` ⇒ campd **blocks forever** ⇒ **no `end` frame, no EOF** — violating test 8's own assertion, on the steady state of every long-lived `camp watch` | `dispose_pending` runs **before** the harvest that consumes it; the `end` frame goes out **on the same wake** |

| # | Defect | Fix, and where | **What the fix could newly break, and the test for it** |
|---|---|---|---|
| **G1** | **`pump` LIVELOCKS on any line longer than `HISTORY_CHUNK_BYTES`.** `Subscriber` had no partial-line buffer, so a chunk with no `\n` advanced nothing. **Not pathological:** a `Read`/`Bash`/`Grep` tool-result line routinely exceeds 64 KiB, and cp-0 accepts lines up to `max_stream_bytes` (256 MiB). **It also killed C8:** an over-cap line (≥ 1 MiB ⇒ ≥ 16 chunks) could never be *lexed*, so the `skipped` frame — whose `"bytes":N` requires finding the line's end — was **structurally unreachable**. | **Task 8 — `pump` is rewritten as a real bounded reader**, copying cp-0's discipline: a `partial: Vec<u8>` that spans chunks, bounded against the cap **before** extending; plus `scan` (read position) separate from `cursor` (the resume point = end of the last complete line). A line whose bytes cross the cap switches to **OVERSIZE SCAN**: `partial` is dropped (memory freed), the bytes are *counted, not buffered*, and at the `\n` a `skipped` frame is emitted with the true `bytes`. **`MAX_PUMP_BYTES_PER_WAKE` bounds the SCAN, not just the delivered bytes** — so a 256 MiB line is consumed over ⌈256 MiB / 256 KiB⌉ wakes, each doing bounded work, and it *terminates*. | The oversize scan could itself become an unbounded read. It cannot: nothing is buffered during it, and the per-wake scan budget bounds each pass. **Tests: `pump_lexes_a_line_that_spans_many_chunks`** (unit) and **`a_line_larger_than_the_cap_is_skipped_and_campd_does_not_livelock`** (integration, driven by a NEW fake-agent mode — see below). |
| **G2** | **`poll_timeout`'s `Some(ZERO)` on a non-empty `out` is a TICK** (invariant 1, §4.3). A non-empty `out` means the last write returned `WouldBlock`; the correct wakeup is the **WRITABLE edge**, which is already registered and which C7 already relies on. Arming a zero timeout on top of it turns a blocked write into a spin — and since macOS's ≈ 8 KiB socket buffer is far smaller than a chunk's worth of frames, **every healthy subscriber hits this on every chunk**. | **Task 8 — `poll_timeout` arms `ZERO` iff `out.is_empty() && scan < tail`** (there is *pumpable file work* and no fd will signal it). The `out` disjunct is DELETED: while `out` is non-empty, no file progress is possible anyway, so only writability can unblock it. | Dropping the disjunct could strand a subscriber whose `out` never drains. It cannot: a WouldBlock arms the WRITABLE edge (edge-triggered `poll` re-arms on the transition), and a client that never drains is dropped at the cap (G3). **Test: `poll_timeout_never_arms_on_a_wouldblock_alone`**, plus the **LOADED perf arm** (below), which is the gate that would have caught this. |
| **G3** | **`out` was refilled only when EMPTY ⇒ it never held more than one chunk (~64 KiB) ⇒ against the shipped 1 MiB cap the drop path was DEAD CODE.** So: a subscriber that stops reading is **never dropped** (it holds an fd and one of only `MAX_SUBSCRIBERS` slots — 8 such connections permanently disable `subscribe` for everyone: the local DoS the cap exists to prevent); **C7's own new-failure answer was false** ("the hard cap drops it" — it could not); **the backpressure tests were theatre** (they set the cap to 512 B, *smaller than one chunk* — the only regime where the drop fires); and the stated 8 MiB worst case was wrong (the real bound was ~8 × 64 KiB). | **Task 8 — `pump` keeps FILLING `out` up to the cap even while `out` is NON-empty.** That is what makes the cap meaningful and the drop reachable: frames accumulate across many chunks, and `out.len() + frame.len() > cap` ⇒ **Drop** with `buffered_bytes` = the attempted size. | Aggressive filling could drop a *healthy* reader. It cannot: filling stops at the cap, and any reader draining at any rate keeps `out` below it unless the producer genuinely outruns it — which is exactly what backpressure means. **Test 11 now runs at the DEFAULT 1 MiB cap** (not a 512 B toy), so the shipped configuration is the one under test; plus the unit test `out_keeps_filling_while_non_empty_so_the_cap_is_reachable`. |
| **G4 / A2** | **`control_step` consumed `take_disposed()` BEFORE `dispose_pending()` produced it** ⇒ on the disposal wake `take_disposed()` is **empty**, `closing` is never set, and my inline comment was factually inverted. A *behind* subscriber recovered a wake late (which is why test 8 would have gone green); a **caught-up** subscriber — the steady state of every long-lived `camp watch`, cp-2's primary consumer — has `poll_timeout == None`, so campd **blocks forever** and it gets **no `end` frame and no EOF**. **A2 makes it disqualifying:** what "rescues" it in practice is that `unregister`'s `remove_file` fires a notify event and `on_watch_event` always signals — so the `end` frame's delivery would **depend on a delivered notify event**, which cp-0's law in the very block being edited (event_loop.rs:406-408) forbids: *"correctness never depends on a delivered event."* | **Task 6 Step 5 — the order is fixed.** `dispose_pending` runs **before** `close_disposed`, which is lifted **out of `control_step`** into its own step. The `end` frame is delivered **on the disposal wake**, for a caught-up and a behind subscriber alike, with **no dependence on the notify**. | A subscriber whose session vanishes between `fanout` and `close_disposed` could see `tail_state() == None`. Specified: `fanout` leaves `tail` **unchanged** when `tail_state` returns `None` (never zeroes it, never panics); `close_disposed` then pins `tail = final_offset` authoritatively. **Test: `a_subscriber_caught_up_at_the_tail_still_gets_an_end_frame_on_the_reap_wake`** — the case no rev-3 test reached. |
| **G5** | **`rehydrate` collapsed timed-out ids into `answered` ⇒ a late `control_response` ACROSS A RESTART was SILENTLY SWALLOWED** — rev 2's swallow, restored, on the path C11 itself calls the most operationally important the phase ships. **Root cause: `control.failed` had no machine-readable cause** — only PROSE — so `rehydrate` could not tell "timed out, an answer may still come" from "the pipe write failed, no answer can ever come". Prose is not a cause (invariant 3), and it would have handed cp-2/cp-5 a prose-matching contract. | **Task 2 + Task 3 — `control.failed` gains a `cause` DISCRIMINANT** (`silence_timeout`, `ceiling_timeout`, `write_failed`, `unknown_request`, `unparsable`, `dialog_refused`, `permission_unanswerable`, `session_ended`). `rehydrate` **routes on it**: `control.responded` and the terminal causes ⇒ `answered`; **`silence_timeout` / `ceiling_timeout` ⇒ `timed_out`** (a late answer still corrects). | A new required field breaks the fold. `cause` is added to the `ControlFailed` payload struct in Task 2 **in the same commit as the variant** (its fold test appends every cause), so `deny_unknown_fields` and the refold stay green. **Test: `a_restart_across_a_TIMED_OUT_interrupt_still_appends_the_correction`** — the seam between rev-3's test 4 (answered ids only) and test 7 (late answers within one campd life), which nothing exercised. |
| **G6 / A3** | **D7 had no absolute ceiling ⇒ a worker that never goes quiet NEVER faults an unanswered interrupt** — §2.1's swallowed timeout, through the front door. **And my stated backstop does not exist:** patrol's ladder is *also* activity-driven (`drain_touched` resets the stall timer on transcript activity), so a chatty worker is **never stalled and never control-faulted — neither mechanism can ever fire.** Both of D7's safety nets were the same net, with a hole in exactly this shape. | **Task 3 — D7 keeps the silence deadline and gains a HARD CEILING.** `Pending` carries `created_at`; a request expires at **`min(silence_deadline, created_at + CONTROL_RESPONSE_CEILING)`** (`CONTROL_RESPONSE_CEILING = 10 × CONTROL_RESPONSE_TIMEOUT` = 5 min), and the ceiling's `control.failed` carries `cause: "ceiling_timeout"` and names the true cause: *"the session produced output for N minutes but never answered request_id X"*. **The panel's argument is accepted: C11's late-correction already makes an early fault SELF-REPAIRING, so a correctable false positive is strictly better than an uncorrectable false negative.** | The ceiling could fire on a legitimately long queued interrupt. That is now the *correctable* direction: a late answer appends `control.responded{late:true}` naming the fault it corrects (C11), and rehydration preserves that across a restart (G5). **Test: `a_chatty_worker_that_never_answers_still_faults`** — a session streaming a line every 5 s for 3× the timeout eventually faults. No line of rev 3 contemplated this case. |
| **G7** | **The plan contradicted itself on a worker that exits between deliver and response.** C11's fourth column claimed *"the reset is bounded by the session's own lifetime (a disposed session's rows are dropped)"*, while `forget_session` was specified to prune **`answered` only**. If an implementer made it drop `pending` too, **the interrupt vanishes with no event** — a silently swallowed fault in the *most likely* real scenario. And `timed_out` was pruned by nothing: an unbounded map. | **Task 3 — `forget_session(session, now) -> Vec<EventInput>` is specified, and it does NOT silently drop anything.** A disposed session's **`pending` rows are EXPIRED LOUDLY** with `cause: "session_ended"` (*"the session ended with an unanswered control request"*), and its **`answered` and `timed_out` rows are pruned** (bounding both maps by live sessions — the addendum's requirement, satisfied without swallowing a fault). `Pending` also carries `rig`/`bead` captured at `serve_interrupt`, so every fault has the **same provenance** as the `session.interrupted` it answers. | Expiring at disposal could double-fault a request that also hit the silence deadline on the same wake. It cannot: `expire_pending` removes the row, and `forget_session` only sees what remains. **Test: `a_worker_that_exits_before_answering_still_faults_loudly`.** |
| **G8 / A1** | **The dead_code table was STILL not field-exhaustive — three gates fail.** Missing: `StreamLine.session` / `.line` (first production read in Task 6) ⇒ **Task 4's gate fails**; `Disposed.session` / `.final_offset` (absent entirely) ⇒ **Tasks 4–7 fail**; and `WorkerMessage::Stream`'s payload, which **no production path ever reads** ⇒ **Task 6 Step 7's "delete the module attribute" fails**. An item-level allow does **not** silence `field is never read` on the struct it returns. **A1 is worse: `StreamLine.offset_after` is production-DEAD under D6″** — `pump` derives every offset from its own cursor and `ingest` reads only `session` + `line` — so the implementer was trapped between "delete the allow at Task 8" (fails clippy) and Task 11's grep (forbids leaving it). | **`StreamLine.offset_after` is DELETED** (nothing reads it — A1's first option, taken). `StreamLine.session`/`.line`, `Disposed.session`/`.final_offset` get field-level allows naming their first production read. **`WorkerMessage::Stream`'s payload gets a PERMANENT allow** (it exists so `parse_worker_line` is TOTAL — D3's transparent surface — and so the passthrough test can assert it). | Deleting `offset_after` could strand a future consumer. The `end`-frame offset and the subscribe cursors all come from `pump`/`dispose_pending`, which own them; a phase that needs a per-line offset on `StreamLine` can add it *with* its reader. **Task 11's grep is unchanged and now actually passes.** |
| **G9** | **C3's own anti-false-green guard was miscounted** — Task 8 headed its list "(7)" and listed EIGHT; the gate demanded 14 integration tests when the true total was 15; five run steps carried no count at all; and C8's test had two different names. *A verification gate that is itself wrong is worse than no gate.* | **Every run step now carries a count, and the counts are recomputed from the enumerated lists**: `daemon::control` unit = **24** (7 + 10 + 7); `tests/control.rs` = **17** (6 + 1 + 10). C8's test is named **`a_line_larger_than_the_cap_is_skipped_and_campd_does_not_livelock`** everywhere. **A standing rule is added: if the observed count differs from the plan's, RECONCILE THE LIST — never delete a test to satisfy a gate.** | A stale count in a later revision re-creates the trap. The rule above is what defuses it: the count is a cross-check, not an authority. |
| **G10** | **`MAX_FAULTS_PER_SESSION_PER_WAKE` did not exist** — `grep` finds nothing — yet Task 6 referred to it as an existing dedupe, leaving the implementer to invent its home, its reset semantics, and (since `ControlFailed` is `deny_unknown_fields`) *where the suppressed count goes*. | **Task 6 Step 4 — it is DEFINED in `control.rs`: `pub const MAX_FAULTS_PER_SESSION_PER_WAKE: usize = 8`**, a per-`ingest`-call counter (reset at the top of every `ingest`, hence per-wake), and **the suppressed count rides the `reason` STRING of the 8th event** with `cause: "unparsable"` — a stated decision, not a guess, chosen precisely so no new payload field is needed. | Burying the count in prose makes it unqueryable. Accepted, and named: the `cause` discriminant (G5) is the machine-readable half; the count is diagnostic detail. |
| **G11** | **The `skipped` frame was overloaded and DOUBLE-REPORTED.** It covered both over-cap lines and non-JSON lines — but cp-0 **already** reports every non-JSON line as `patrol.degraded` from `drain_one`'s `Err` arm (read_channel.rs:643-650), so C8's "record `patrol.degraded` once per (session, offset)" re-reported, from the file side, a fault the ledger already carries — the exact double-report Task 6 forbids. And **blank lines**: cp-0 skips them while advancing past them (read_channel.rs:631-633), while `pump` reads the file and would have emitted a `skipped` frame for a no-op. | **Task 8 — `skipped` gains a `reason` DISCRIMINANT** (`over_cap` \| `not_a_json_object`). **Only `over_cap` appends a durable `patrol.degraded`** (deduped per `(session, offset)` in `ControlRuntime`); `not_a_json_object` emits the frame and **NO event** — cp-0 already owns that fault. **Blank lines are SKIPPED silently, advancing the cursor, exactly as cp-0 does** (no frame, no event). | Suppressing the second event could hide a real fault. It cannot: cp-0's `patrol.degraded` for that line is already durable and names the session, offset and line. **Test: `a_non_json_line_yields_a_skipped_frame_and_no_second_patrol_degraded`.** |

**Non-blocking items adopted (all cheap, none expanding scope):** test 7's history is now **> 256 KiB** (a 64–256 KiB history is consumed in ONE wake, so the live-burst window never opened) · **the perf gate gains a LOADED arm** — N subscribers with a *streaming* session, asserting bounded CPU — because the rev-3 gate (empty buffers, quiescent sessions) was *constructed to be blind* to both G1's and G2's spins · **a protocol version `"v":1` is added to the `Subscribed` hello** (named twice as "the last free place" and then not taken; `Subscribed` must precede `Ok` in the untagged `Response`, which it already does) · `session_status` can only return `live`/`stopped`/`crashed` — **`"capped"` was a phantom value** and is removed (a cap-killed session is `crashed`) · the false claim that `event_frame` "matches cp-0's `Ok(_v)` arm" is corrected (cp-0 accepts any JSON *value*; `event_frame` requires an *object*, and the resulting `skipped{reason:"not_a_json_object"}` frame is the honest difference) · **`final_drain_pending`/`dispose_pending` state threading is specified** (the merged code `mem::take`s the queue at the top, so a naive split would leave `dispose_pending` with an empty list and never unlink a file: `final_drain_pending` **peeks**, `dispose_pending` **takes**) · `pump` cannot take `&mut Ledger`, so its events ride a **`pending_events` collector** on `ControlRuntime` (cp-0's mold), drained by the caller · **UTF-8: `pump` operates on BYTES end to end** — `event_frame` takes `&[u8]` and validates with `from_slice`, because a `from_utf8_lossy` decode would silently rewrite the very bytes C2's design exists to preserve · **the ordering inversion is SANCTIONED IN WRITING**: a subscriber sees a line's bytes before that line's ledger effect commits (cp-0's law is about *offsets*, and the client owns its own cursor) · the O(subscribers × bytes) re-parse on the hot path is named and accepted, bounded by `MAX_PUMP_BYTES_PER_WAKE` × `MAX_SUBSCRIBERS`.

**Also corrected (A-addendum):** C5's *"harvest 2 non-empty ⟺ the guard fires"* is only **⟹**, not ⟺ — `drain_one` advances past blank and non-JSON lines **without** pushing a `StreamLine`, so the guard can fire with harvest 2 empty. The prose is fixed; **test 2's assertion was already the correct guard and is unchanged.**

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
| **`WorkerMessage::Stream`'s payload** (variant field) | 1 | `#[allow(dead_code)] // PERMANENT: never read in production. Subscribers are fed from the FILE by pump (D6″), not from this variant — it exists so parse_worker_line is TOTAL (D3's transparent surface) and so the passthrough test can assert the bytes are unchanged.` **(G8: no production path reads it, in ANY task — a temporary allow would fail Task 6 Step 7.)** | **never** |
| **`ControlRuntime.subscriber_buffer_bytes`** (field) | 3 | `#[allow(dead_code)] // cp-1: first read in Task 8 (the subscriber hard cap)` — **the module-level attribute is gone by Task 6, so this field needs its OWN** | **Task 8** |
| ~~`StreamLine.offset_after`~~ | — | **DELETED (A1).** Under D6″ NOTHING reads it: `pump` derives every offset from its own cursor and `ingest` reads only `session` + `line`. Rev 3 trapped the implementer between "delete the allow at Task 8" (fails clippy) and Task 11's grep (forbids leaving it). **The field does not exist in rev 4.** | n/a |
| **`StreamLine.session`, `StreamLine.line`** (fields) | 4 | `#[allow(dead_code)] // cp-1: first read in Task 6 (ingest)` — **G8: the item-level allow on `take_stream_lines` does NOT silence `field is never read` on the struct it returns** | **Task 6** |
| **`Disposed.session`, `Disposed.final_offset`** (fields) | 4 | `#[allow(dead_code)] // cp-1: first read in Task 8 (close_disposed)` — **G8: absent from rev 3's table entirely; without these, the Task 4/5/6/7 gates all fail** | **Task 8** |
| `read_channel::take_stream_lines` | 4 | `#[allow(dead_code)] // cp-1: first read in Task 6` | Task 6 |
| `read_channel::last_activity` | 4 | `#[allow(dead_code)] // cp-1: first read in Task 7` | Task 7 |
| `read_channel::tail_state` / `take_disposed` / **`final_drain_pending`** / **`dispose_pending`** | 4 | `#[allow(dead_code)] // cp-1: first read in Task 8` — **R6: NOT Task 6. Task 6 keeps calling the merged `apply_pending_unregisters` wrapper; Task 8 is what splits it.** Rev 4 said Task 6 and put Task-8 calls in Task 6's code block, so Task 6 could neither compile nor pass clippy. | **Task 8** |
| **`ControlRuntime::forget_session`** | **3** | `#[allow(dead_code)] // cp-1: first read in Task 8 (close_disposed)` — **R6: it is added in Task 3 (G7) and its only production caller is in Task 8, so Task 6 Step 7's "delete the module attribute and pass -D warnings" FAILS without this. It was absent from rev 4's table entirely.** | **Task 8** |
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

The `{"frame":…}` tag lets cp-2 add `frame:"ledger"` with no breaking wire change. But `Subscriber` now carries **six file-shaped fields** (`file`, `cursor`, `scan`, `partial`, `oversize`, `tail`) and **four transport-shaped ones** (`out`, `high_water`, `blocked_since`, `end_sent` + `frame_overhead`), while `fleet.subscribe` (§4.1: session transitions, stalls, permission requests, completions) is **ledger-event-sourced** — it needs only the second group.

**⚠ THE SEAM IS NAMED, NOT TAKEN — and the rev-5 claim that it was is STRUCK.** I wrote that `Subscriber` is `{ out: OutBuf, source: Source }` with `OutBuf` owning the policy once. **The struct cp-1 actually ships is FLAT** (`out`, `high_water`, `blocked_since`, `frame_overhead`, `end_sent` as bare fields, and `pump` interleaves file-reading with buffer policy). Claiming a refactor I did not do is the same class of error as the "this test gates it" claims I have now withdrawn twice.

**What cp-2 should do, and the honest caveat:**
```rust
struct Subscriber { out: OutBuf, source: Source }   // cp-2's cut
enum Source { Stream(StreamSource), Fleet(LedgerSource) }
```
`OutBuf` would own the flush loop, `blocked_since`, `SUBSCRIBER_STALL_TIMEOUT`, `high_water`, and the `subscriber.dropped` event — **the drop policy, in one place.** It took five revisions to get that policy right (the cap is a *stop*; the drop is a *stalled peer*, never a *large backlog*), and duplicating it into a second subscriber kind is how it gets broken again.

**But only HALF of it transfers, and cp-2 must know which half.** *"The cap is a stop — hold the line in `partial`"* is a **file-source** mechanism: a ledger event cannot be "held in `partial`" and re-read from a byte offset. **What DOES transfer is the drop rule** (a peer that accepts zero bytes for the stall timeout) and the flush loop. **What does NOT transfer is the back-off**: a ledger-sourced subscriber that cannot keep up has no file to leave the data in, so cp-2 must decide *where its unsent events live* — and that is a real design question, not a refactor.

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
EventType::ControlFailed      => "control.failed"       // {session?, request_id?, verb?, cause, reason}
EventType::SubscriberDropped  => "subscriber.dropped"   // {session, subscription, buffered_bytes, cap_bytes}
```
**`late: bool` (default false) is C11's correction field.**

**`cause: String` is G5's MACHINE-READABLE DISCRIMINANT, and it is the root fix, not a decoration.** Rev 3's `control.failed` carried only PROSE, so `rehydrate` could not tell *"timed out — an answer may still come"* from *"the pipe write failed — no answer can ever come"*, and it collapsed both into `answered`, **silently swallowing a late answer across a restart**. Prose is not a cause (invariant 3: *"every campd action is an event with its cause"*), and prose-matching is not a contract to hand cp-2/cp-5. The closed set:

| `cause` | meaning | rehydration routes it to |
|---|---|---|
| `silence_timeout` | the session went quiet for `CONTROL_RESPONSE_TIMEOUT` with the request unanswered | **`timed_out`** — a late answer still corrects (C11) |
| `ceiling_timeout` | the session kept producing output but never answered within `CONTROL_RESPONSE_CEILING` (G6) | **`timed_out`** |
| `session_ended` | the session was disposed with the request still unanswered (G7) | `answered` (terminal — the session is gone; no answer can be re-read) |
| `write_failed` | the pipe write itself failed; the request never reached the worker (C12) | `answered` (terminal) |
| `unknown_request` | a `control_response` for an id camp never sent (§2.1) | `answered` (terminal) |
| `unparsable` | a control message camp could not parse (§2.1) | `answered` (terminal) |
| `dialog_refused` | a `request_user_dialog` was answered with the deterministic refusal (§9) | `answered` (terminal) |
| `permission_unanswerable` | a `can_use_tool` arrived, which cp-1 cannot answer (§5.3.1) | `answered` (terminal) |

None of the four event names exists in `gc-vocab.json` ⇒ all camp-specific, additive (invariant 7).

- [ ] **Step 1: Add the variants AND their fold arms together** (cp-0 note 3: a variant without its arm makes the next step's red an `E0004` compile error, not a test failure). `fold.rs` gets `fn audit<T: DeserializeOwned>(event) -> Result<(), CoreError>` (parse-and-discard) and four `#[serde(deny_unknown_fields)] #[allow(dead_code)]` payload structs — `ControlResponded` carrying `#[serde(default)] late: bool` and `#[serde(default)] detail: String`, and **`ControlFailed` carrying a REQUIRED `cause: String`** alongside its `reason`. **The `cause` field lands in the SAME commit as the variant** (G5's new-failure guard): `deny_unknown_fields` means adding it later would break every already-appended event, so Task 2's fold test appends **one event per `cause` value** in the table above and asserts each folds and refolds clean.

- [ ] **Step 2: Run the vocab-pin test and watch it fail RED.**

Run: `cargo test -p camp-core --test vocab_pin 2>&1 | tail -20`
Expected: FAIL in `every_event_type_is_declared_mirrored_or_camp_specific_never_both` — an assertion failure, not a compile error.

- [ ] **Step 3: Declare them camp-specific** in `CAMP_SPECIFIC_EVENTS` (after `"session.nudged"`).

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp-core --test vocab_pin` — PASS, including `camp_specific_names_do_not_collide_with_gc`.

- [ ] **Step 5: Write the fold round-trip test** in `camp-core/src/ledger/mod.rs`'s `mod tests`, beside cp-0's `read_channel_patrol_degraded_shapes_round_trip_through_the_fold`: append all four shapes (including a `late: true` `control.responded`) **and ONE `control.failed` per `cause` value in Task 2's table** (G5 — `deny_unknown_fields` means a cause added later would break every already-appended event, so every value must fold from the first commit), assert a typo'd key (`requestId`) is REFUSED at append, then refold. **The API is `refold_check()` (refold.rs:64) returning `RefoldReport { events_replayed, drift }` — assert `report.drift.is_empty()`, exactly as `refold_prop.rs:170` does. There is no `is_clean()` and no `refold()`.**

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
/// G6: the ABSOLUTE ceiling. A silence deadline alone can be pushed forward
/// forever by a chatty worker, so an unanswered interrupt would NEVER fault —
/// §2.1's swallowed timeout, through the front door.
pub const CONTROL_RESPONSE_CEILING: Duration = Duration::from_secs(300); // 10x
pub const MAX_PENDING_CONTROL_REQUESTS: usize = 64;
pub struct ControlRuntime;
impl ControlRuntime {
    pub fn new(subscriber_buffer_bytes: usize) -> ControlRuntime;
    pub fn track_pending(&mut self, request_id: String, session: String, verb: &'static str,
                         rig: Option<String>, bead: Option<String>, now: Timestamp);
    /// D7/C11: ANY stream line from a session resets its SILENCE deadline (never
    /// its ceiling — G6).
    pub fn note_activity(&mut self, session: &str, now: Timestamp);
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration>;
    pub fn expire_pending(&mut self, now: Timestamp) -> Vec<EventInput>;
    pub fn resolve(&mut self, request_id: &str, ok: bool, detail: String) -> Option<EventInput>;
    pub fn rehydrate(&mut self, ledger: &Ledger, now: Timestamp) -> anyhow::Result<usize>;
    /// G7: the session was disposed. Its PENDING rows are EXPIRED LOUDLY (cause
    /// `session_ended`) — never silently dropped — and its `answered` / `timed_out`
    /// rows are pruned, which is what bounds both maps by LIVE sessions.
    pub fn forget_session(&mut self, session: &str, now: Timestamp) -> Vec<EventInput>;
}
```

**State — `resolved` is SPLIT (C11), and `Pending` carries its provenance (G7):**
```rust
struct Pending {
    session: String,
    verb: &'static str,
    /// G7: captured at `serve_interrupt` so EVERY fault this request produces
    /// carries the SAME provenance as the `session.interrupted` it answers. Rev 3
    /// built its fault EventInputs with rig/bead = None, so a fault and its cause
    /// disagreed about which bead they belonged to.
    rig: Option<String>,
    bead: Option<String>,
    /// G6: never moves. The ceiling is computed from this.
    created_at: Timestamp,
    /// D7: the SILENCE deadline; `note_activity` pushes it forward.
    deadline: Timestamp,
}

pub struct ControlRuntime {
    pending: HashMap<String, Pending>,
    /// ANSWERED, or settled TERMINALLY (a cause from which no answer can ever
    /// arrive — see Task 2's cause table). A re-read control_response for one of
    /// these is a TRUE duplicate => None (B6).
    answered: HashSet<String>,
    /// C11/G5: TIMED OUT (cause `silence_timeout` or `ceiling_timeout`) — campd
    /// already appended `control.failed` saying the worker never answered. A
    /// control_response for one of these is NOT a duplicate: it is NEW INFORMATION
    /// saying that fault was PREMATURE, and it appends a CORRECTION.
    ///
    /// Rev 3's `rehydrate` collapsed these into `answered`, which silently swallowed
    /// a late answer ACROSS A RESTART — the exact bug C11 exists to forbid. That
    /// was only possible because `control.failed` had no machine-readable cause;
    /// G5 adds one, and `rehydrate` routes on it.
    timed_out: HashMap<String, Pending>,
    #[allow(dead_code)] // cp-1: first read in Task 8 (the subscriber hard cap)
    subscriber_buffer_bytes: usize,
}
```

- [ ] **Step 1: Write the failing tests** (**10** — the header said 7 over a list of 10 for three revisions; recounted):

1. `a_pending_request_arms_a_deadline_and_an_empty_table_arms_none` — invariant 1.
2. `a_control_response_that_never_arrives_becomes_a_durable_fault` — §2.1; the row is removed, so the fault is raised exactly once.
3. `a_matching_control_response_resolves_the_pending_request` — `late == false`.
4. `a_restart_across_an_in_flight_interrupt_neither_lies_nor_forgets` (B6) — an answered id's re-read resolves to `None`; the orphan still expires.
5. `a_control_response_for_a_never_sent_request_id_is_a_fault` (§2.1).
6. **`session_activity_resets_a_pending_control_deadline`** (D7/C11) — track at T0; `note_activity` at T0+20 s; assert **nothing expires at T0+31 s** (the worker is streaming: it is alive), and that it DOES expire at T0+20 s+31 s (30 s of *silence*).
7. **`a_late_control_response_after_the_deadline_appends_a_correction`** (C11) — track, expire (⇒ `control.failed{cause:"silence_timeout"}`), then `resolve` the same id. Assert `Some(ControlResponded)` with **`late == true`** and a `detail` naming the premature fault — **not `None`.** *Rev 2 discarded this answer; this test makes that impossible.*
8. **`a_chatty_worker_that_never_answers_still_faults`** (G6/A3) — track at T0, then `note_activity` every 5 s **past the CEILING (300 s, i.e. 10× the timeout — NOT 3× = 90 s, which never reaches it and so cannot observe what the test asserts).** Assert the silence deadline **never** fires (the worker is streaming) but the **CEILING** does: at `created_at + CONTROL_RESPONSE_CEILING` a `control.failed{cause:"ceiling_timeout"}` appends, naming that the session produced output but never answered. *No line of rev 3 contemplated this case — and BOTH of D7's claimed backstops are dead (patrol's ladder is also activity-driven, A3), so without the ceiling campd emits nothing, ever: §2.1's swallowed timeout.*
9. **`a_restart_across_a_timed_out_interrupt_still_appends_the_correction`** (G5) — the seam nothing exercised. Append `session.interrupted{id}` then `control.failed{request_id: id, cause:"silence_timeout"}`; `rehydrate`; then `resolve(id)`. Assert **`Some(ControlResponded{late:true})`** — NOT `None`. *Rev 3 routed every `control.failed` into `answered`, so this returned `None` and the worker's real answer died with the restart.* Also assert the converse: a `control.failed{cause:"write_failed"}` id rehydrates into `answered`, so a stray response for it is **not** treated as a late correction.
10. **`a_worker_that_exits_before_answering_still_faults_loudly`** (G7) — track a pending request, then `forget_session(session)`. Assert it returns **one `control.failed{cause:"session_ended"}`** carrying the session's `rig`/`bead`, and that `pending`, `answered` and `timed_out` are all empty for that session afterwards. *The interrupt must never vanish with no event — that is the most likely real scenario (the interrupt worked; the worker died before flushing its ack).*

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
///
/// G6/A3 — AND IT IS NOT ENOUGH ON ITS OWN. A worker that NEVER goes quiet (a long
/// tool loop; anything under cp-4's --include-partial-messages) would have its
/// deadline pushed forward FOREVER, so an interrupt the CLI never processes would
/// fault NEVER — §2.1's swallowed timeout through the front door. And there is no
/// backstop: patrol's stall ladder is ALSO activity-driven (drain_touched resets
/// its timer on transcript activity), so a chatty worker is never stalled EITHER.
/// Both safety nets are the same net, with a hole in exactly this shape. Hence the
/// ABSOLUTE CEILING below, which nothing resets.
pub const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// G6: the absolute ceiling on ONE control request, measured from `created_at` and
/// RESET BY NOTHING. A worker that has been producing output for five minutes
/// without acknowledging an interrupt is broken, under either mid-turn semantics.
///
/// The trade, stated: an elapsed-time bound can fire a FALSE fault on a
/// legitimately long queued interrupt — but C11 makes that fault SELF-REPAIRING (a
/// late answer appends `control.responded{late:true}` naming the fault it
/// corrects, and G5's rehydration preserves that across a restart). D7 alone traded
/// a CORRECTABLE FALSE POSITIVE for an UNCORRECTABLE FALSE NEGATIVE, which is
/// strictly worse under invariant 3.
pub const CONTROL_RESPONSE_CEILING: Duration = Duration::from_secs(300);
```
- `track_pending(id, session, verb, rig, bead, now)` — stores `created_at = now` (G6: never moves) and `deadline = now + CONTROL_RESPONSE_TIMEOUT`, plus the `rig`/`bead` provenance (G7). (jiff: `SignedDuration::try_from(...).unwrap_or(SignedDuration::from_secs(30))` — clippy denies `unwrap`.)
- **`note_activity(session, now)`** — every pending row of that session gets `deadline = now + CONTROL_RESPONSE_TIMEOUT`. **It NEVER touches `created_at`** (G6).
- **`due_at(p) = min(p.deadline, p.created_at + CONTROL_RESPONSE_CEILING)`** — the single expiry predicate. Both `poll_timeout` and `expire_pending` use it, so they can never disagree.
- `poll_timeout` — the earliest `due_at` as a `Duration`-from-now; `None` when `pending` is empty. **Task 8 extends this with the subscriber continuation (and G2 constrains exactly when that may arm).**
- `expire_pending(now)` — rows with `due_at <= now` are REMOVED from `pending`, **MOVED into `timed_out`**, each yielding a `control.failed` with the row's `rig`/`bead` and a cause derived **by comparing the two BOUNDS, never either against `now`** (non-blocking note, adopted — a wake delayed past both bounds would otherwise report `silence_timeout` when the *ceiling* is what expired, an invariant-3 false cause):
  ```rust
  let ceiling = p.created_at + CONTROL_RESPONSE_CEILING;
  let cause = if p.deadline <= ceiling { "silence_timeout" } else { "ceiling_timeout" };
  ```
  - `silence_timeout` — *"the session went quiet for 30s with `verb` unanswered"*;
  - `ceiling_timeout` — *"the session produced output for 5m but never answered request_id X"* (G6's true cause, named).
- `resolve(id, ok, detail)`:
  - in `pending` ⇒ remove; insert into `answered`; `Some(ControlResponded { late: false, … })`.
  - in `timed_out` ⇒ remove; insert into `answered`; **`Some(ControlResponded { late: true, detail: "… arrived after control.failed declared it unanswered — that fault was PREMATURE; this is the correction", … })`** (C11).
  - in `answered` ⇒ `None` (a true duplicate: a restart re-read — B6).
  - otherwise ⇒ `Some(ControlFailed { cause: "unknown_request", reason: "…camp never sent…" })` (§2.1).
- **`rehydrate(ledger, now)` — G5, the fix.** Scan `session.interrupted` for the ids camp sent. Then, for each id, ROUTE ON THE `cause` DISCRIMINANT (Task 2's table):
  - present in `control.responded` ⇒ `answered`.
  - present in `control.failed` with `cause ∈ {silence_timeout, ceiling_timeout}` ⇒ **`timed_out`** (reconstruct the `Pending` from the event's `session`/`rig`/`bead`/`verb`) — **so a late `control_response` re-read after the restart appends a CORRECTION, not silence.** *Rev 3 put these in `answered` and swallowed the answer.*
  - present in `control.failed` with any TERMINAL cause ⇒ `answered` (no answer can ever arrive).
  - present in neither ⇒ `track_pending` with a **FRESH** `created_at` and deadline (the previous life's clock is not ours, and a worker waiting across a restart deserves the full window).
  Returns the restored count. **An unrecognized `cause` is a hard error, not a default** — a value this campd does not know means the ledger was written by a newer camp, and guessing its meaning is exactly the silent divergence invariant 5 forbids.

  **A LIVENESS FILTER, and it is required** (non-blocking note, adopted). `forget_session` prunes `timed_out` *in memory*, but the LEDGER still holds the `session.interrupted` + `control.failed{silence_timeout}` pair forever — so a `rehydrate` with no liveness filter reconstructs a `timed_out` row for **every interrupt that ever timed out in the ledger's whole history**, on every campd start. My G7 claim (*"bounded by live sessions"*) held **within** one campd life and was **false across a restart**. So: `rehydrate` **skips any request whose session has a `session.stopped` / `session.crashed`** — that session is gone, nothing can re-read its stream, and no correction can ever arrive. Only requests belonging to still-live sessions are reconstructed, which is what makes the bound true across restarts too.
- **`forget_session(session, now) -> Vec<EventInput>` — G7, and it does NOT silently drop anything.** For that session: every **`pending`** row is EXPIRED LOUDLY as `control.failed{cause: "session_ended", reason: "the session ended with an unanswered control request"}` (carrying its `rig`/`bead`), then removed; every **`answered`** and **`timed_out`** id for that session is PRUNED. That satisfies both halves of the addendum — the maps are bounded by LIVE sessions, and no fault is swallowed. *(A late answer cannot arrive after disposal: the session is no longer tailed, so there is nothing left to re-read.)* Called by `close_disposed` (Task 8), which has the disposed-session list.

**Bounds.** `rehydrate` does three full-type ledger scans, once per campd life, bounded by ledger size — stated. `answered` and `timed_out` are pruned at disposal (G7), bounding both by live sessions. **`pending` is capped at `MAX_PENDING_CONTROL_REQUESTS` (64)**: past it, `serve_interrupt` refuses loudly, so neither an overseer loop nor a hostile client can grow the table or the ledger without bound (the `MAX_FAULTS_PER_SESSION_PER_WAKE` dedupe, defined in Task 6, protects only the INBOUND path).

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --bins daemon::control` → PASS, **17 tests** (7 from Task 1 + 10 here). *(If the observed count differs, RECONCILE THE LIST — never delete a test to satisfy a gate — G9.)*

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
    #[allow(dead_code)] // cp-1: first read in Task 6 (ingest)
    pub session: String,
    #[allow(dead_code)] // cp-1: first read in Task 6 (ingest)
    pub line: String,
    // A1/G8: there is NO `offset_after`. Under D6" NOTHING reads it — `pump`
    // derives every offset from its own cursor and `ingest` reads only `session`
    // and `line`. Rev 3 carried it with an allow that could never be removed
    // (deleting it failed clippy; keeping it failed Task 11's grep). A phase that
    // needs a per-line offset here adds it TOGETHER WITH ITS READER.
}

pub struct Disposed {
    #[allow(dead_code)] // cp-1: first read in Task 8 (close_disposed)
    pub session: String,
    #[allow(dead_code)] // cp-1: first read in Task 8 (close_disposed — the end frame's offset)
    pub final_offset: u64,
}

impl ReadChannelRuntime {
    pub fn take_stream_lines(&mut self) -> Vec<StreamLine>;
    pub fn last_activity(&self, session: &str) -> Option<jiff::Timestamp>;
    pub fn tail_state(&self, session: &str) -> Option<(PathBuf, u64)>;
    /// C5: the final drain + cp-0's ordering guard + cp-0's fault flushes.
    /// Does NOT dispose, and does NOT consume the pending list.
    pub fn final_drain_pending(&mut self, ledger: &mut Ledger) -> Result<bool>;
    /// C5: unlink + clear cursor + record each `Disposed` — AFTER the caller has
    /// harvested, and BEFORE the caller consumes `take_disposed()` (G4).
    pub fn dispose_pending(&mut self, ledger: &mut Ledger) -> Result<()>;
    pub fn take_disposed(&mut self) -> Vec<Disposed>;
}
```
**`apply_pending_unregisters` is KEPT as a thin wrapper** (`final_drain_pending` then `dispose_pending`) so every merged cp-0 unit test that calls it stays green. The event loop calls the halves separately, with the harvest between them.

**The state threading between the halves — SPECIFIED (non-blocking note, adopted).** Merged `apply_pending_unregisters` begins with `let pending = std::mem::take(&mut self.pending_unregisters)` (read_channel.rs:296). **A naive split leaves `dispose_pending` re-taking an ALREADY-EMPTIED queue: it would dispose nothing, no stream file would ever be unlinked, and no cursor cleared.** So: **`final_drain_pending` PEEKS** — it iterates `&self.pending_unregisters` (cloning the names it needs) and leaves the queue in place — and **`dispose_pending` is the one that `mem::take`s it.** The wrapper preserves the merged behaviour exactly.

- [ ] **Step 1: Write the failing tests** (**3**):

1. `drain_all_hands_over_the_complete_lines_it_consumed` — file order; `mem::take`-drained (never redelivered); a partial line is never handed over. *(A1: it does NOT assert an `offset_after` — that field no longer exists.)*
2. `the_disposal_time_final_drain_also_hands_over_its_lines` — `final_drain_pending` produces the last line **while the file still exists**; `dispose_pending` is what unlinks it; `take_disposed()` yields `Disposed { session, final_offset }` with the true final offset (**the `end` frame's offset source — C7**).
3. **`the_final_drain_and_the_disposal_are_separable`** (C5's enabling guard) — after `final_drain_pending`, the stream file **still exists** and the session is **still tailed**; only `dispose_pending` removes both. *This is what makes harvesting before the unlink possible at all.*

- [ ] **Step 2: Run and watch fail.** (`no method named take_stream_lines`.)

- [ ] **Step 3: Implement.** In `drain_one`'s existing `Ok(_v)` parse arm (read_channel.rs:635-641), keep the `parsed_counts` bump and push a `StreamLine` + stamp `last_activity`. **No other line of the drain loop changes.** (This compiles: `self.parse_errors.push` already performs the same disjoint-field borrow at read_channel.rs:643 while `t` is live.)

Split `apply_pending_unregisters` **at the seam cp-0 already documents** (read_channel.rs:328-340: *"every one of them must be consumed BEFORE the sessions are disposed below"*): everything up to and including the fault flushes becomes `final_drain_pending`; the `for session in &pending { self.unregister(...) }` loop becomes `dispose_pending`, which records each `Disposed { session, final_offset: t.offset }` before removal. **cp-0's ordering-violation guard stays exactly where cp-0 put it, unchanged** (C5).

- [ ] **Step 4: Run the new tests AND the entire cp-0 suite.**

Run: `cargo test -p camp --bins daemon::read_channel && cargo test -p camp --test read_channel`
Expected: PASS — **3 new** unit tests plus **every cp-0 test** (the merged `--bins daemon::read_channel` count is 22 today, so expect **25**), including `a_workers_final_stdout_line_is_drained_before_the_reap_disposes_the_file` (read_channel.rs:509) and the ordering-guard tests. **If any cp-0 test goes red, the split broke a merged invariant — STOP.**

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

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --bins daemon::dispatch` — **2 new** tests plus every existing dispatch test (G9: a count of 2 means the filter matched only the new ones — reconcile).

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
            // Non-blocking note, adopted: CLONE at the append site. Rev 4 moved
            // `rig`/`bead` into the EventInput and then `.clone()`d them in the Ok arm
            // below — E0382, use of moved value. It does not compile.
            ControlWrite::Delivered => match ledger.append(EventInput {
                kind: EventType::SessionInterrupted,
                rig: rig.clone(),
                actor: "campd".into(),
                bead: bead.clone(),
                data: serde_json::json!({"session": session, "request_id": request_id}),
            }) {
                Ok(_) => {
                    // G7: the rig/bead go INTO the pending row, so every fault this
                    // request may later produce (silence_timeout, ceiling_timeout,
                    // session_ended) carries the SAME provenance as the
                    // session.interrupted it answers.
                    self.track_pending(
                        request_id.clone(),
                        session.to_owned(),
                        "session.interrupt",
                        rig.clone(),
                        bead.clone(),
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
                        // G5: the machine-readable cause. TERMINAL — the request never
                        // reached the worker, so no answer can ever arrive, and
                        // `rehydrate` must route this id to `answered`, never to
                        // `timed_out`.
                        "cause": "write_failed",
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
- **FIRST, for every line: `self.note_activity(&sl.session, now)`** (D7/C11 — the session is producing output, so its SILENCE deadline resets; the G6 ceiling does not).
- `ControlResponse` ⇒ `self.resolve(id, ok, detail)`, pushing the `Option` when `Some` (B6/C11).
- `RequestUserDialog` ⇒ write `ParentMessage::DialogRefusal` via `dispatcher.write_control`; append `control.failed{cause: "dialog_refused"}` naming the outcome (delivered / no pipe / write failed), **deduped per `request_id`** so a worker re-asking the same id appends once.
- `CanUseTool` ⇒ `control.failed{cause: "permission_unanswerable"}` stating plainly that the worker is now blocked forever holding a dispatch slot and must be killed by the operator. camp takes no automatic action: the flow is structurally unreachable in cp-1 (§5.3.1), and phase 3 owns both the answer and §5.3.2's slot rule.
- `Stream(_)` ⇒ **nothing** (D6″: subscribers are fed from the FILE by `pump`, never from here — which is why the variant's payload carries a PERMANENT dead_code allow, G8).
- `Err(ControlWireError)` ⇒ `control.failed{cause: "unparsable"}`. *cp-0's `drain_one` hands over only already-parsed lines (the `Ok(_v)` arm) and surfaces non-JSON separately as `patrol.degraded`, so `ingest` never double-reports. Do not add a guard.*

**`MAX_FAULTS_PER_SESSION_PER_WAKE` — DEFINED HERE (G10).** Rev 3 referred to it as an existing dedupe; `grep` finds nothing, and the implementer was left to invent its home, its reset semantics, and — since `ControlFailed` is `deny_unknown_fields` — where a suppressed count could even go. So:
```rust
/// Loud is right; UNBOUNDED-loud is a self-DoS. A worker spraying malformed control
/// lines would otherwise drive one synchronous SQLite append per line on the event
/// loop. This bounds the fault events ONE `ingest` call may emit for ONE session.
pub const MAX_FAULTS_PER_SESSION_PER_WAKE: usize = 8;
```
It is a **per-`ingest`-call counter** (a local `HashMap<&str, usize>` reset at the top of every `ingest`, hence per-wake — it is NOT runtime state). Past the cap, further faults for that session are suppressed and **the 8th event's `reason` STRING names the suppressed count** (`"… and N further unparsable control lines this wake were suppressed"`), with `cause: "unparsable"`. **This is a stated decision, not a guess:** the count rides `reason` precisely so that no new payload field is needed and Task 2's `deny_unknown_fields` fold test stays green. The `cause` discriminant (G5) is the machine-readable half; the count is diagnostic detail.

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
        //
        // PRECISELY (the A-addendum's correction to rev 3's prose): the implication
        // is ONE-WAY. harvest-2-non-empty ==> the guard fires. NOT the converse:
        // `drain_one` advances `t.offset` past BLANK lines (read_channel.rs:631-633)
        // and past NON-JSON lines (642-649) WITHOUT pushing a StreamLine, so the
        // guard can fire with this harvest empty. Test 2's assertion (no
        // patrol.degraded ORDERING VIOLATION on the normal path) is the correct guard
        // either way and is unchanged — it is the PROSE that was wrong, not the test.
        appended |= control_step(ledger, control, dispatcher, read_channel, &mut conns, &mut poll)?;

        // ═══ R6: WHAT LANDS IN TASK 6 vs TASK 8 — read this before writing a line ═══
        // TASK 6 writes ONLY the two `control_step` harvests, `expire_pending`, and the
        // settle. Where the disposal block appears below, TASK 6 KEEPS THE MERGED CALL:
        //
        //     read_channel.apply_pending_unregisters(ledger)?;   // the cp-0 wrapper
        //
        // — exactly as main does today. That is what lets Task 6 COMPILE and pass its
        // own `-D warnings` gate: `close_disposed`, `forget`, `take_disposed` and
        // `dispose_pending` are all TASK 8, and rev 4 put them in Task 6's code block,
        // where they do not exist yet.
        //
        // TASK 8 then REPLACES that one line with the split below. Nothing else moves.
        // ═══════════════════════════════════════════════════════════════════════════

        // ---- G4/A2: THE DISPOSAL HAND-OFF (TASK 8), IN THE ONLY ORDER THAT WORKS ----
        // Rev 3 consumed `take_disposed()` INSIDE control_step — i.e. BEFORE
        // `dispose_pending` had produced anything — so on the disposal wake the list
        // was EMPTY, `closing` was never set, and a subscriber that was exactly
        // CAUGHT UP (poll_timeout == None: the steady state of every long-lived
        // `camp watch`) got NO end frame and NO EOF, forever.
        //
        // A2 is why that could not be papered over: what "rescued" it in practice was
        // that `unregister`'s remove_file (read_channel.rs:433) fires a notify event
        // and `on_watch_event` always signals — so the end frame's delivery would have
        // DEPENDED ON A DELIVERED NOTIFY EVENT, which cp-0's law in this very block
        // (event_loop.rs:406-408) forbids: "correctness never depends on a delivered
        // event". It is also why a test would have passed while the design was broken.
        //
        // So: dispose FIRST (which is what RECORDS Disposed{session, final_offset}),
        // and only then hand the list to the subscriber registry. The end frame now
        // goes out ON THE DISPOSAL WAKE, for a caught-up and a behind subscriber
        // alike, with no dependence on the watch.
        read_channel.dispose_pending(ledger)?;
        let disposed = read_channel.take_disposed();
        if !disposed.is_empty() {
            // Sets `closing` + pins `tail = final_offset` + pumps (so a caught-up
            // subscriber emits its end frame immediately), and expires each disposed
            // session's still-pending control requests LOUDLY (G7's forget_session).
            let (gone, events) = control.close_disposed(&disposed, ledger, &mut conns);
            for input in events {
                ledger.append(input)?;
                appended = true;
            }
            for token in gone {
                if let Some(mut conn) = conns.remove(&token) {
                    let _ = poll.registry().deregister(&mut conn.stream);
                }
                control.forget(token);
            }
        }
        // -----------------------------------------------------------------------

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
**`control_step`** (one helper, two call sites — harvest 1 and harvest 2): `take_stream_lines` → `control.ingest(...)` → append the events → **`control.fanout(read_channel, &mut conns)`** (Task 8: refresh each subscriber's `tail`, `pump`, collect drops) → append the `subscriber.dropped` events → deregister + `forget` the dropped tokens. Returns whether it appended.

**`close_disposed` is NO LONGER inside `control_step`** (G4). It is its own step, above, and it runs **after** `dispose_pending`. That is the whole of the fix.

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

- [ ] **Step 9: Run.** `cargo test -p camp --test control 2>&1 | tail -30` → PASS, **6 tests** (Task 8 adds 11 more later; Task 7 adds 1).

- [ ] **Step 10: Full suite** (the D4 blast radius). `cargo test --workspace` → PASS, **with no test count DECREASING versus main**. **`cli_nudge.rs` MUST still pass** (the CLI verb is unchanged), and so must `daemon_patrol.rs` (**`patrol.rs:788` calls `dispatcher.nudge_via_stdin` DIRECTLY; D4 deletes only the socket verb, so the method survives and there is no orphaned caller** — the merged-caller sweep confirms this is the complete list).

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

- [ ] **Step 4: Run and watch pass.** `cargo test -p camp --bins daemon::socket` (**4** wire-pin tests: the merged 2 + `control_plane_verbs_wire_format_is_pinned` + `sessions_list_wire_format_is_pinned`; `nudge_wire_format_is_pinned` is DELETED by D4) `&& cargo test -p camp --test control` (**7** integration tests now: 6 from Task 6 + this one).

- [ ] **Step 5: Commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A && git commit -m "feat(control): sessions.list — every session by name, never by pid (cp-1 §4.1/§4.2)"
```

---

## Task 8: `session.subscribe` — one monotone cursor, a Closing state, a skip policy (D6″; C2/C6/C7/C8)

Spec: §4.4, §9, §8, §5.2.

**Files:** `daemon/control.rs`, `daemon/socket.rs` (wire types only — **no client API**, B2), `daemon/event_loop.rs`, `tests/fake-agent.sh`, `tests/control.rs`.

### The frame wire — TAGGED FROM BIRTH; THREE frames; `skipped` carries a REASON DISCRIMINANT (B12/C8/G11)

```json
{"frame":"event","session":"t/dev/1","offset":123,"event":{ …the worker's line, VERBATIM BYTES… }}
{"frame":"skipped","session":"t/dev/1","offset":456,"bytes":2097152,"reason":"over_cap"}
{"frame":"skipped","session":"t/dev/1","offset":460,"bytes":17,"reason":"not_a_json_object"}
{"frame":"end","session":"t/dev/1","offset":789,"reason":"stopped"}
{"ok":true,"v":1,"subscription":"sub-1","cursor":0}
```
**`event`'s payload is the worker's line SPLICED IN VERBATIM (C2)** — never re-serialized through a `Value`, which would sort its keys and hand cp-2/cp-4 a wire camp invented by accident.

**`skipped`'s `reason` is a DISCRIMINANT, and only one of them is evented (G11).** Rev 3 overloaded one `skipped` frame for two different faults and appended a `patrol.degraded` for both — but **cp-0 already reports every non-JSON line as `patrol.degraded`** from `drain_one`'s `Err` arm (read_channel.rs:643-650). So:
- `over_cap` ⇒ the frame **and** ONE durable `patrol.degraded`, deduped per `(session, offset)` (many subscribers hit the same line).
- `not_a_json_object` ⇒ the frame and **NO event** — cp-0 already owns that fault, and re-reporting it from the file side is exactly the double-report Task 6 forbids.
- **Blank lines are SKIPPED SILENTLY** — no frame, no event — advancing the cursor, exactly as cp-0 does (read_channel.rs:631-633). Rev 3 would have emitted a `skipped` frame for a no-op.

*(Honest correction: `event_frame` does NOT "match cp-0's `Ok(_v)` arm" — cp-0 accepts any valid JSON **value**, including a bare array or number, and counts it parsed. `event_frame` requires an **object**. The `skipped{reason:"not_a_json_object"}` frame is the honest difference, not an agreement.)*

**`"v":1` in the hello — the protocol version (non-blocking note, adopted).** Named twice as "the last free place" and then not taken. It costs one field now and is unbuyable later; cp-2/3/4/5 all extend this wire. `Subscribed` already precedes `Ok` in the untagged `Response`, so it is not shadowed.

**End-of-stream:** after the `end` frame campd flushes and closes (EOF). `reason` is `stopped` or `crashed` — **NOT `capped`** (non-blocking note, adopted: `ledger/mod.rs:341`'s column only ever holds `live`/`stopped`/`crashed`; a cap-killed session is recorded `crashed`, per event_loop.rs:533. Rev 3's `"capped"` was a **phantom value**).

### Constants, with their arithmetic

```rust
pub const SUBSCRIBER_BUFFER_BYTES_DEFAULT: usize = 1024 * 1024;  // §4.4's number
pub const HISTORY_CHUNK_BYTES: usize = 64 * 1024;
/// G1: this bounds the SCAN, not merely the delivered bytes — otherwise an
/// over-cap line (which is scanned but never buffered) would be unbounded work on
/// the event loop.
pub const MAX_PUMP_BYTES_PER_WAKE: usize = 256 * 1024;           // per subscriber
/// §4.4 bounds BYTES PER CONNECTION; nothing bounded the CONNECTION COUNT.
/// WORST CASE, STATED: MAX_SUBSCRIBERS * SUBSCRIBER_BUFFER_BYTES = 8 MiB of
/// outbound buffers on top of campd's idle RSS. That CAN approach the spec's
/// <20 MB figure — so, plainly: <20 MB is an IDLE bound (and it is what `make perf`
/// measures: N subscribers with EMPTY buffers). A campd with 8 SATURATED
/// subscribers is outside that bound BY DESIGN, and this cap is what keeps it
/// bounded at all. Raising it is a spec question, not a local call.
///
/// G3 — and this arithmetic is only TRUE in rev 4. Rev 3 refilled `out` only when
/// EMPTY, so it never held more than one chunk (~64 KiB): the real bound was
/// ~8 x 64 KiB, the 1 MiB cap was UNREACHABLE, and the drop path was DEAD CODE.
pub const MAX_SUBSCRIBERS: usize = 8;
```

### `Subscriber` — ONE monotone cursor (D6″), a PARTIAL-LINE BUFFER (G1), a `Closing` state (C7)

```rust
struct Subscriber {
    id: String,
    session: String,
    /// The open stream file. Held across disposal ON PURPOSE — on Unix an unlinked
    /// inode survives while an fd is open, so a Closing subscriber FINISHES ITS
    /// HISTORY (C7).
    file: std::fs::File,

    // ---- the reader state. THREE positions, and the distinction is what fixes G1.
    /// THE DELIVERY CURSOR (D6″): the byte offset just past the last COMPLETE line
    /// delivered. MONOTONE, and the sole delivery gate — this is what a client
    /// resumes from (§9), so it may only ever advance past a whole line.
    cursor: u64,
    /// THE READ POSITION: how far `pump` has read from the file. `scan >= cursor`
    /// always; the gap is the in-progress line.
    ///
    /// G1: rev 3 had ONLY `cursor`, and lexed "each complete line in the chunk" — so
    /// a line longer than one 64 KiB chunk contained no '\n', advanced nothing, and
    /// LIVELOCKED campd at 100% CPU (with `poll_timeout` = Some(ZERO) re-reading the
    /// same chunk forever). A `Read`/`Bash`/`Grep` tool-result line routinely exceeds
    /// 64 KiB. cp-0 solved this in `drain_one` with exactly the buffer below
    /// (read_channel.rs:588, 613-624) and rev 3 did not copy it.
    scan: u64,
    /// The bytes of the in-progress line, [cursor, scan). BOUNDED BY THE CAP: once
    /// the line's bytes would cross `subscriber_buffer_bytes` it can never be
    /// delivered, so we stop buffering it (see `oversize`) rather than growing to
    /// max_stream_bytes (256 MiB) on the event loop.
    ///
    /// B1: when a line COMPLETES, its '\n' IS PUSHED HERE before `try_emit_line` is
    /// called — because `off = cursor + partial.len()` must land PAST the newline.
    /// Rev 5 tested the newline first and never pushed it, so `cursor` landed ON the
    /// newline and drifted ONE BYTE PER LINE, cumulatively — and §9 makes these
    /// offsets the durable RESUME CURSORS, so a client reconnecting with a cursor
    /// campd handed it landed mid-file at the wrong byte.
    partial: Vec<u8>,
    /// B3: `partial` holds a COMPLETE line (terminated by its '\n') that did not fit
    /// `out`. A REAL FLAG — rev 5 tested `partial ends with b'\n'`, which R3's
    /// newline-first rule makes ALWAYS FALSE, so the held line was never retried, the
    /// next line's bytes were appended onto it, and TWO LINES WERE CONCATENATED into
    /// one body: `event_frame` rejected it and campd emitted `skipped{not_a_json_object}`
    /// — corruption reported with a false cause. The central rev-5 mechanism never
    /// fired at all.
    ///
    /// It is cleared by `try_emit_line` on EVERY success path (including the blank
    /// line path), which is the only writer.
    held: bool,
    /// OVERSIZE SCAN (G1/C8): the in-progress line has already exceeded the cap.
    /// `partial` is DROPPED (memory freed) and we merely COUNT bytes while scanning
    /// for the terminating '\n'. At the newline a `skipped{reason:"over_cap"}` frame
    /// is emitted with the true `bytes` — which is why the frame can carry a byte
    /// count at all. Rev 3's `skipped` frame was STRUCTURALLY UNREACHABLE: it could
    /// never lex a line it could not buffer.
    oversize: Option<u64>,   // bytes counted so far

    /// What campd has actually DRAINED. Refreshed every wake from
    /// `read_channel.tail_state`; PINNED to the final offset at disposal. `pump`
    /// reads ONLY [scan, tail) — so it can never read bytes campd has not drained.
    tail: u64,
    /// C7: set at disposal (`stopped` | `crashed`). A Closing subscriber keeps
    /// pumping until `scan == tail` AND `out` is empty; only THEN does the `end`
    /// frame go out, and the connection closes when that flush completes.
    closing: Option<String>,
    /// R2: the `end` frame has been APPENDED. Without this the TERMINAL branch
    /// re-fires on every loop iteration — appending an UNBOUNDED stream of duplicate
    /// `end` frames, never reaching the `out.is_empty()` test that is the ONLY path
    /// to `Gone`, so EOF never arrives and the fd and one of 8 MAX_SUBSCRIBERS slots
    /// are never released. Rev 4 referenced an `end_frame_was_sent` that was neither
    /// a field nor constructible as a local (it must survive a WouldBlock return and
    /// the next WRITABLE re-entry).
    end_sent: bool,

    /// Bytes queued for this socket. Bounded by the cap — and, R1, the cap is a
    /// STOP, never a kill (see `pump`'s FILL).
    out: Vec<u8>,
    /// R3: the exact byte cost of an `event` frame's wrapper for THIS session,
    /// MEASURED once at the hello (`event_frame(session, u64::MAX, b"{}")!.len() - 2`
    /// — the widest possible offset, so it can never under-estimate). The over-cap
    /// decision is made on the FRAME, not the raw line: rev 4 tested the line against
    /// the cap and the frame against the drop, leaving a ~60-byte band in which a
    /// perfectly-readable line was neither skipped nor deliverable, and its
    /// subscriber was dropped — permanently, on every re-subscribe.
    frame_overhead: usize,

    /// R1: when the peer last accepted ZERO bytes with data buffered. Stamped on a
    /// zero-accept write, CLEARED the moment ANY byte is accepted.
    ///
    /// THIS — not the size of `out` — is what a drop means. The cap protects campd's
    /// memory against A PEER THAT HAS STOPPED READING; it must never be a verdict on
    /// a peer that is reading fast but is simply behind, because during catch-up the
    /// producer is `pump` reading a FILE and a file ALWAYS outruns a socket
    /// (net.local.stream.sendspace is 8192 on macOS). Rev 4 conflated the two and so
    /// dropped every client that joined more than ~1 MiB behind the tail, however
    /// fast it read — breaking §9's late-joiner promise for any session with more
    /// than 1 MiB of stdout, with a `subscriber.dropped` event whose stated cause was
    /// false (invariant 3).
    blocked_since: Option<Timestamp>,
    /// The largest `out` reached — `buffered_bytes` in `subscriber.dropped` (§4.4:
    /// "naming the session and the high-water mark").
    high_water: usize,
}
```

```rust
/// R1: how long a peer may accept ZERO bytes, with data buffered for it, before campd
/// drops it. A subscriber that is merely BEHIND is not stalled; a subscriber whose
/// socket has accepted nothing for 30 s has stopped reading.
pub const SUBSCRIBER_STALL_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/// Test-only override, the `CAMP_SUBSCRIBER_BUFFER_BYTES` twin. WITHOUT IT the stall
/// tests are mandatory 30-second wall-clock tests, and their hard deadlines would have
/// to EXCEED 30 s — which makes the deadline useless as the hang detector it exists to
/// be. Fail fast on a malformed or zero value.
pub fn subscriber_stall_timeout_from_env(default: Duration) -> Result<Duration>;
    // reads CAMP_SUBSCRIBER_STALL_TIMEOUT_MS
```

**The memory bound, corrected.** Each subscriber holds **`out` ≤ cap AND `partial` ≤ cap** ⇒ **~2 MiB per subscriber, ~16 MiB at `MAX_SUBSCRIBERS = 8`** — *not* the 8 MiB rev 5 stated. (§4.3's <20 MB figure is an **idle** bound and is unaffected: idle subscribers hold neither.) Plus one bounded overshoot: **the `oversize` `skipped` frame is appended with no cap check**, so `out` may exceed the cap by one small frame (~80 bytes). Bounded, deliberate, and stated.

- [ ] **Step 1: Write the failing tests.**

**The `pump` unit harness (needed by tests 3–14).** A `Subscriber` is constructible only via `serve_subscribe` + a `Conn`. Add, on the `dispatch::test_insert_held_cat` precedent (dispatch.rs:352):
```rust
#[cfg(test)]
impl ControlRuntime {
    /// Insert a subscriber directly over a `UnixStream::pair()`, so `pump` can be
    /// driven with no daemon, no socket, and no timing. Returns the CLIENT end (the
    /// test reads it) and the `Conn` (the test passes it back into `pump`). A test
    /// that never reads its client end IS a stalled peer — which is how the R1 drop
    /// and the R2 terminal branch are exercised deterministically.
    pub fn test_insert_subscriber(&mut self, token: Token, session: &str, file: File,
                                  cursor: u64, tail: u64) -> (UnixStream, Conn);
}
```

**Unit (`control.rs`, 14):**
1. **`event_frame_splices_verbatim_and_refuses_a_non_object_line`** (C2) — `event_frame(b"t/dev/1", 123, br#"{"type":"system","subtype":"init"}"#)` produces **exactly** `{"frame":"event","session":"t/dev/1","offset":123,"event":{"type":"system","subtype":"init"}}\n` — **key order preserved, because the line is SPLICED, not re-serialized.** And `event_frame(_, _, b"not json")` ⇒ `None` (the caller emits `skipped{reason:"not_a_json_object"}`).
2. `subscribe_frame_shapes_are_pinned` — the hello (**with `"v":1`**) and all three frames; `skipped` in **both** reason flavours.
3. **`pump_lexes_a_line_that_spans_many_chunks`** (G1) — a **100 KiB** line (> `HISTORY_CHUNK_BYTES`) is buffered across chunks and delivered as ONE `event` frame; the cursor advances exactly past it. *Rev 3 livelocked here at 100 % CPU — the ordinary case on the ordinary session.*
4. **`pump_skips_an_over_cap_line_without_buffering_it`** (G1/C8) — a line **larger than the cap** switches to OVERSIZE SCAN: `partial` stays bounded (assert `partial.len() <= cap` throughout), a `skipped{reason:"over_cap", bytes:<true length>}` frame is emitted at the newline, and the cursor advances past it. *This is only reachable because the scan can lex a line it refuses to buffer — rev 3 could not, so its `skipped` frame was structurally unreachable.*
5. **`a_frame_that_would_cross_the_cap_STALLS_and_the_line_is_held`** (**REWRITTEN — R1 deleted the behaviour rev 4/5 specified here**) — with a non-draining socket, FILL fills `out` to the cap and then **STOPS**: the complete line stays in `partial`, **`held` is true**, `cursor` does **not** advance, and **no `subscriber.dropped` is emitted**. Then drain the socket and pump again: the held line goes out **as its own frame**, unconcatenated. *The old assertion (a frame crossing the cap DROPS the subscriber) is now wrong by design and would have to be deleted anyway — it is rewritten, not removed.*
6. **`out_keeps_filling_while_non_empty_up_to_the_cap`** (G3, rescoped) — with a slow socket, `out` grows across MANY chunks to the cap (proving G3's refill-while-non-empty still holds), and then stalls rather than dropping. **The drop is NOT tested here** — it fires at the stall timeout, and test 11 owns it.
7. **`poll_timeout_never_arms_on_a_wouldblock_alone`** (G2) — a subscriber with a non-empty `out` and `scan == tail` contributes **`None`** *(unless it is stalled — see test 11)*; one with an empty `out` and `scan < tail` contributes `Some(ZERO)`. *Rev 3 armed ZERO on a non-empty `out`, which spun campd for the duration of every stream.*
8. **`a_line_whose_frame_just_exceeds_the_cap_is_skipped_not_dropped`** (**R3 — the ~60-byte band**) — a line of exactly `cap - frame_overhead + 1` raw bytes. Rev 4 would neither skip it (the *line* is under the cap) nor deliver it (the *frame* is not), and took the `Drop` path: a perfectly-reading subscriber killed, permanently, on every re-subscribe. Assert `skipped{reason:"over_cap"}` and that the subscriber **survives**.
9. **`a_line_ending_exactly_at_the_cap_boundary_is_not_conflated_with_the_next`** (**R3's second hole**) — a line whose crossing byte **is the `\n`**. Rev 4 pushed first and tested after, so the `continue` bypassed the newline check, `oversize` armed, and the scan ran to the NEXT line's `\n` — **silently consuming a whole line with no frame**, reporting a byte count spanning two. Assert the next line arrives as its own frame and the `skipped{bytes:N}` count covers **one** line.
10. **`a_non_utf8_line_is_REFUSED_not_silently_corrupted`** (**R7, RE-AIMED — B4: the old premise was false, verified by running it**). JSON text is **UTF-8 by definition** and `serde_json::from_slice` **enforces** it (`Err("invalid unicode code point")`), so a byte-identical round-trip of non-UTF-8 is **unachievable by any implementation** and the old test could never pass. **The property actually worth having is the REFUSAL:** feed a line containing raw non-UTF-8 bytes; assert `event_frame` returns `None`, the client receives `skipped{reason:"not_a_json_object"}`, and **the U+FFFD replacement character NEVER appears on the wire.** *That is precisely what the `&str` + `from_utf8_lossy` path (which the rev-4 signature forced) would have produced — it substitutes U+FFFD and splices the CORRUPTED bytes. The `&[u8]` + `from_slice` signature refuses instead of corrupting, and this test is what pins that difference.*
11. **`a_peer_that_accepts_nothing_is_dropped_at_the_stall_timeout`** (**R1's drop rule**) — a client that never reads (`CAMP_SUBSCRIBER_STALL_TIMEOUT_MS=200`): assert `blocked_since` is stamped on the zero-accept write, that `poll_timeout` arms **the stall deadline** (nothing else will ever fire for it), and that `Drop(subscriber.dropped)` fires with `buffered_bytes` = the high-water. **And the converse:** a client that accepts even **one byte** clears `blocked_since` and is never dropped. *(That converse remains exactly right for THIS rule, which asks only "has the peer stopped reading?" — but it is no longer the whole policy: the slot-holding residual it used to imply is now bounded by the time-at-cap rule, #121. The test is renamed `..._is_never_dropped_by_the_zero_accept_rule` to say which rule it pins, and the time-at-cap rule has its own pair of tests: the trickle reader IS dropped, and a bursty peer that drains its buffer is NOT.)*
12. **`a_line_held_at_the_cap_is_retried_and_never_concatenated`** (**B3 — the mechanism that never fired**) — stall a line at the cap, drain the socket, pump again. Assert the held line is delivered **as its own frame**, and that the **next** line is a **separate** frame. *Under rev 5 the retry predicate was always false, so the next line's bytes were appended onto the held one and the two were emitted as a single body — rejected by `event_frame` and reported as `skipped{not_a_json_object}`: corruption with a false cause.*
13. **`a_cap_stop_mid_chunk_loses_no_bytes`** (**B2 — silent truncation**) — stall FILL **mid-chunk** with bytes still unconsumed in `buf`; drain; pump. Assert **every** line in that chunk is eventually delivered, in order, none skipped. *Rev 5 advanced `scan` over the whole chunk up front and then broke mid-`buf`, throwing away up to 64 KiB of lines while `scan` already pointed past them.*
14. **`close_disposed_emits_the_end_frame_for_a_caught_up_subscriber`** (**R5 — MOVED HERE from the integration list**, because it uses `test_insert_subscriber`, and integration tests cannot link `daemon::*` — a Global Constraint I wrote and then violated). Drive `close_disposed` directly over a `UnixStream::pair`: assert the `end` frame is on the wire and `Gone` is returned. **No daemon, no notify, no timing.** *(⚠ And the rev-5 claim that this test "fails if `close_disposed` stops being called after `dispose_pending`" is **STRUCK** — it calls `close_disposed` directly with a hand-built `&[Disposed]`, so it cannot observe the event loop's call order at all. **The STRUCTURAL guarantee is the real protection.** That is the third false "this test gates it" claim I have made; the standing correction is to ask, of every such claim, **what code change would make this red?**)*

**Integration (`tests/control.rs`, 14 — items 1–15 below, minus the one moved to the unit list; RECOUNTED ONCE, CAREFULLY — the counts have been stale four revisions running and this gate exists to catch a false green).** **EVERY test below carries a HARD DEADLINE** (R2): rev 4's tests 8 and 9 would have **hung, not failed**, and a hang that "passes" is exactly the failure mode I flagged for the huge-line test and then applied nowhere else. `SubClient` therefore gains **`next_frame_within(dur) -> Option<Value>`** — §4.4's timeout exemption clears the read deadline, so the *client* must impose one or every subscriber test can hang forever.
5. **`a_wedged_campd_fails_the_subscribe_hello_fast`** — the EXIT CRITERION. A bare bound `UnixListener` is the wedge simulator (socket.rs:751). `SubClient::open` returns `Err` (`WouldBlock`/`TimedOut`) **inside REQUEST_TIMEOUT**; assert elapsed < 15 s.
6. **`a_subscription_survives_a_quiet_period_longer_than_request_timeout`** (B13) — open at the tail, sleep **6 s** (> the 5 s `REQUEST_TIMEOUT`), then interrupt and assert a frame still arrives.
7. **`a_subscriber_catching_up_across_a_live_burst_gets_every_line_exactly_once_in_order`** (C6) — a session with **> 256 KiB** of history (**not 64 KiB**: `MAX_PUMP_BYTES_PER_WAKE` is 256 KiB, so a smaller history is consumed in ONE wake and **the live-burst window never opens** — rev 3's ">64 KiB" would not have exercised the thing it exists for); subscribe at **cursor 0**; the worker appends **live lines DURING catch-up**. Assert every line arrives **exactly once**, **in file order**, with **strictly increasing `offset`s**.
8. **`a_subscriber_gets_the_full_history_then_an_end_frame_when_its_session_ends`** (B12/C7) — `FAKE_AGENT_EXIT_AFTER_CONTROL`; assert **every** line arrives BEFORE the `end` frame (not a truncated prefix), its `offset` equals the session's final offset, and **EOF never arrives without an `end` frame**.

    **PLUS ONE LINE, AND IT WOULD HAVE CAUGHT B1 (offset fidelity):**
    ```rust
    // The last `event` frame's offset is the byte just past that line; the `end`
    // frame's offset is `tail`. In a correct stream they are THE SAME NUMBER.
    // Under B1's per-line drift they differ by the LINE COUNT — and every
    // "offsets strictly increase" assertion in this suite stays green while they do.
    assert_eq!(last_event_frame["offset"], end_frame["offset"]);
    ```
9. **`a_subscriber_caught_up_at_the_tail_gets_an_end_frame_when_its_session_is_reaped`** — subscribe, **drain every frame until fully caught up** (the steady state of every long-lived `camp watch`), then let the worker exit. Assert the `end` frame arrives (within a hard deadline) and that EOF follows it.

    **⚠ THE REV-4 CLAIM FOR THIS TEST IS WITHDRAWN (R5), exactly as C5's falsifiability claim was.** I asserted it gates the disposal ordering. **It cannot, and no black-box test can.** `read_channel::on_watch_event` (read_channel.rs:806-815) sets `signal = true` in **every** `Ok` arm — `registered` is consulted only for `rescan`, never to suppress the self-pipe write — and `unregister`'s `remove_file` (:433) fires a `Remove` on the watched directory. **So campd always gets another wake, and under the BROKEN ordering the disposed list simply persists and the next wake emits the `end` frame one wake late. This test is GREEN on the broken ordering.** My patch ("this test also asserts the end frame arrives with the notify path unable to help") **named a mechanism that does not exist** — there is no knob to disable the stream watch — and would have stopped an implementer cold.

    **What this test actually proves:** the `end` frame ARRIVES for a caught-up subscriber. That is worth having. **It does not prove the ordering.**

    **The ordering fix STAYS** — cp-0's law (event_loop.rs:406-408) is that correctness must never depend on a delivered event, and rev-4's ordering is what makes the `end` frame independent of the notify. **Its guarantee is now STRUCTURAL, not behavioural:**
    - **`take_disposed()` has EXACTLY ONE CALLER**, in the event loop, immediately after `dispose_pending()` — stated at the call site.
    - **`close_disposed` is NOT reachable from `control_step`** (rev 4 lifted it out; that is the invariant, not an implementation detail).
    - **…and it gets a deterministic UNIT test** (below) that no notify can mask.

*(`close_disposed_emits_the_end_frame_for_a_caught_up_subscriber` has MOVED to the UNIT list — it uses `test_insert_subscriber`, and integration tests cannot link `daemon::*`.)*

**THE THREE OFFSET/SHARING/RESTART TESTS — the dimensions nothing spanned:**

13. **`a_client_that_resubscribes_from_a_delivered_offset_resumes_exactly_there`** (**§9's RESUME PROMISE, tested by nothing in five revisions**) — subscribe at `cursor: 0`, read K frames, take **frame K's `offset` OFF THE WIRE**, disconnect, reconnect with **`cursor` = that exact offset**. Assert: **frame K+1 arrives FIRST**, **byte-identical** to the one the first subscription would have delivered, and **NO `skipped` frame** appears (a drifted cursor lands mid-line and produces exactly that). *This is the only test in the plan that closes the loop on what an `offset` MEANS. Every other offset assertion is relative — and **drifting offsets still increase**, which is why B1 was invisible to all 23 rev-5 tests.*
14. **`two_subscribers_on_one_session_share_an_over_cap_line_and_one_degraded_event`** — two `SubClient`s on the same session; drive one over-cap line. Assert **both** receive a `skipped{reason:"over_cap"}` frame, and that **exactly ONE `patrol.degraded` is appended**. *This is the `HashSet<(session, offset)>` dedupe's ENTIRE reason to exist, and nothing exercised it — cp-2 inherits it.*
15. **`a_subscription_dies_with_campd_and_the_client_resumes_from_its_own_cursor`** — subscribe, read some frames, `kill9()` campd, restart it, reconnect with the last delivered `offset`. Assert the stream resumes exactly there with no loss and no duplication. *§9's "durable across a campd restart for free" — named in rev-3's notes and tested by nothing. **NOTE, honestly:** the client receives a bare **EOF with no `end` frame** when campd dies. That is a known gap, already recorded in the PR body; this test PINS the client-visible behaviour so cp-2's `camp watch` inherits a documented contract rather than a surprise.*
10. **`a_closing_subscriber_that_stops_reading_is_still_dropped_at_the_stall_timeout`** (**RENAMED + REWRITTEN** — R1 deleted the cap-drop this test named) — a Closing subscriber gets **no exemption from the stall rule**, so it can never hold an fd or a slot forever. Uses `CAMP_SUBSCRIBER_STALL_TIMEOUT_MS`.
11. **`a_hung_up_subscriber_is_forgotten_and_is_never_libeled_as_backpressure`** (B7) — drop the subscription, drive three wakes, assert campd still answers `status` promptly and **no `subscriber.dropped` exists**. A normal detach is not a fault (§5.2).
12. **`a_subscriber_that_stops_reading_is_dropped_loudly_and_campd_keeps_serving`** (§8/B8/**G3/R1**) — **at the DEFAULT 1 MiB cap** (`CAMP_SUBSCRIBER_BUFFER_BYTES` is NOT set): `FAKE_AGENT_SPAM_ON_TURN=30000` (≈ 2.7 MB). Subscribe at the **tail** (clean hello), read **NOTHING**, `send_turn` to trigger the spam. Assert `subscriber.dropped{session, cap_bytes: 1048576, buffered_bytes …}` — **now fired by the STALL TIMEOUT (R1), because the peer accepted zero bytes, which is the true cause** — then that campd answers `status` on a FRESH connection in < 5 s. *(Rev 3's 512 B cap was smaller than one chunk, the only regime where its drop path fired at all — theatre. Rev 4 fired here for the wrong reason: `out` reaching a size that catch-up reaches by construction.)*

16. **`a_reading_subscriber_survives_a_history_larger_than_the_cap`** — **R1'S TEST, AND THE HOLE THREE REVISIONS COULD NOT SEE.** At the **DEFAULT 1 MiB cap**: build a history of **≥ 2 MiB of deliverable lines** (`FAKE_AGENT_SPAM_ON_TURN` sized accordingly, all short lines, all valid JSON — *deliverable*, not skipped), let campd drain it fully, THEN subscribe at **`cursor: 0`**, with a `SubClient` **reading every frame in a tight loop**. Assert:
    - **every line arrives, exactly once, in order** (strictly increasing `offset`s);
    - **NO `subscriber.dropped` is appended** — the client was reading perfectly;
    - it completes within a hard deadline.

    **Against rev 4's `pump` this test is RED**: FILL framed 256 KiB per wake while the socket accepted ~8 KiB (`net.local.stream.sendspace = 8192`, verified), so `out` grew ~254 KiB per wake and hit the 1 MiB cap in ~4 wakes — and the cap was a `return Drop`. **Any client joining more than ~1 MiB behind the tail was killed however fast it read**, because during catch-up the producer is `pump` reading a *file* and a file always outruns a socket. §9's late-joiner promise was broken for any session with >1 MiB of stdout — which is *ordinary* — and the drop was reported as backpressure about a client that was reading perfectly (invariant 3), and it was *permanent*: re-subscribing re-filled and re-dropped.

    **No rev-4 test could see it:** test 7's history is under the cap; test 12's client reads nothing (its drop is *correct*); test 13's monster is *skipped*, so `out` stays tiny. **The drop path was exercised only where dropping is right, and never where it is a catastrophe.** That is rev 3's 512 B-cap theatre, one level up.
13. **`a_line_larger_than_the_cap_is_skipped_and_campd_does_not_livelock`** (**G1 — the test the suite structurally could not contain**) — a NEW fake-agent mode emits **one genuinely huge line** (see below). Assert: a `skipped{reason:"over_cap", bytes:…}` frame arrives; **the NEXT line still arrives** (the subscriber survived and its cursor advanced past the monster); campd answers `status` on a fresh connection **in < 5 s** (it did not livelock); and no `subscriber.dropped` was appended. **Bound the whole test with a hard deadline** — a livelock manifests as a hang, and a hanging test that "passes by timing out the harness" is the failure mode this test exists to make impossible.
14. **`a_non_json_line_yields_a_skipped_frame_and_no_second_patrol_degraded`** (G11) — cp-0 already reports the line; campd must not report it twice from the file side.
15. **`a_cursor_into_a_reaped_stream_or_past_the_tail_is_an_explicit_error`** (§9) — both, both explicit.

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

/// R7: THE ONE SIGNATURE. `body` is BYTES, and `pump` never decodes UTF-8 anywhere.
/// Rev 4 gave this three mutually incompatible signatures (`&str` here, `&[u8]` at
/// the call site, `&[u8]` in the UTF-8 note) — which forced the implementer to
/// convert bytes to `&str` inside `pump`, and the natural way is cp-0's
/// `from_utf8_lossy` (read_channel.rs:629), which SILENTLY REWRITES THE WORKER'S
/// BYTES: precisely the corruption this function's byte-splice exists to prevent.
/// No fixture would have caught it — every one is ASCII.
///
/// The worker's line is SPLICED IN VERBATIM — never round-tripped through a
/// `serde_json::Value`, which would SORT its keys (serde_json 1.0.150 has no
/// `preserve_order`, and `raw_value` is a cargo feature this plan does not add). A
/// subscriber therefore sees EXACTLY the bytes the worker wrote.
///
/// Returns None when `body` is not a JSON OBJECT (splicing it would emit invalid
/// JSON); the caller emits `skipped{reason:"not_a_json_object"}`. NOTE this is
/// deliberately STRICTER than cp-0's `Ok(_v)` arm, which accepts any JSON value.
fn event_frame(session: &str, offset: u64, body: &[u8]) -> Option<Vec<u8>> {
    // Byte-level trim: no UTF-8 decode.
    let body = trim_ascii_whitespace(body);
    if body.first() != Some(&b'{') {
        return None;
    }
    // from_SLICE, not from_str: a JSON string may legally contain bytes that are not
    // valid UTF-8, and they must round-trip byte-identically (test:
    // event_frame_preserves_non_utf8_bytes).
    if serde_json::from_slice::<serde_json::Value>(body).is_err() {
        return None;
    }
    let prefix = serde_json::to_string(&FramePrefix { frame: "event", session, offset }).ok()?;
    // prefix == {"frame":"event","session":"…","offset":N} — replace its final '}'
    // with ,"event":<body>} so the raw bytes land untouched.
    let mut out = prefix.into_bytes();
    out.pop()?;                                  // drop the closing '}'
    out.extend_from_slice(b",\"event\":");
    out.extend_from_slice(body);                 // VERBATIM
    out.extend_from_slice(b"}\n");
    Some(out)
}
```
**`frame_overhead` (R3) is MEASURED, never computed:** at the hello, `frame_overhead = event_frame(session, u64::MAX, b"{}")?.len() - 2` — the widest possible offset, so it can never under-estimate. That number is what the over-cap threshold tests against, which is what guarantees every line `pump` chooses to buffer has a frame that fits an empty `out`.
(`skipped_frame` and `end_frame` are plain `#[derive(Serialize)]` structs — they carry no verbatim payload.)

**`serve_subscribe(token, session, cursor, read_channel)`:**
1. `subscribers.len() >= MAX_SUBSCRIBERS` ⇒ explicit error naming the cap.
2. `read_channel.tail_state(session)` is `None` ⇒ **not tailed** (never existed, or reaped and disposed) ⇒ explicit error citing §9.
3. `cursor > tail` ⇒ explicit error ("past the N bytes campd has consumed"). **Ordinary history is NOT an error** (B10/D6″).
4. Open the file; insert
```rust
Subscriber {
    cursor: c, scan: c,                  // invariant `cursor <= scan <= tail`, from birth
    partial: Vec::new(), held: false,    // B3: a REAL flag, never a `partial` inspection
    oversize: None,
    tail,
    closing: None, end_sent: false,      // R2
    out: Vec::new(), high_water: 0,
    frame_overhead: measure(session),    // R3: event_frame(session, u64::MAX, b"{}")!.len() - 2
    blocked_since: None,                 // R1
}
```
where `c = cursor.unwrap_or(tail)`; return the hello `{"ok":true,"v":1,"subscription":…,"cursor":c}`. **It registers; it never writes** — the hello must be the FIRST bytes on the socket (`respond()` uses `write_all` on a NON-BLOCKING stream, event_loop.rs:997, and a WouldBlock there drops the connection — B11).

*(Non-blocking note, adopted: **cursors must be campd-issued.** `serve_subscribe` rejects only `cursor > tail`, so a hand-rolled MID-LINE cursor is accepted — it yields one `skipped{not_a_json_object}` frame and then correct behaviour. Benign, and stated rather than defended.)*

**`pump(&mut self, token, conn) -> PumpOutcome` — the ONE data path (D6″), and the only place bytes reach a socket (B11). Rewritten line by line (G1/G2/G3), because it carries the phase.**

```rust
pub enum PumpOutcome {
    Ok,                  // nothing more to do RIGHT NOW (see poll_timeout for continuation)
    Drop(EventInput),    // the hard cap was crossed: subscriber.dropped (B10/G3)
    Gone,                // the peer is gone, or the end frame has flushed (C7)
}
```
```
pump(sub, conn, now):                       // `now` is REQUIRED: blocked_since uses it
  scanned = 0                               // G1: bounds the SCAN, not the output

  loop {
    stalled = false                         // B3(e): RESET every outer iteration. Rev 5
                                            // declared it OUTSIDE the loop and never reset
                                            // it, so its own comment ("the socket took
                                            // bytes, so FILL may resume") was false.

    // ---- (A) FILL: turn file bytes into frames, STOPPING at the cap ------------
    // R1: the cap is a STOP, not a kill (rev 4 returned Drop here and killed every
    // client that joined >1 MiB behind the tail, however fast it read).
    // B3(b): the guard admits a HELD line even at `scan == tail` — the normal terminal
    // state of any catch-up that ran at the cap. Rev 5 gated FILL on `scan < tail`
    // alone, so a line held there was never re-entered, nothing was armed to wake it,
    // the last line of the history was never delivered, and TERMINAL (which required
    // `partial.is_empty()`) could never fire: no end frame, no EOF, fd + slot leaked.
    while !stalled && (sub.held || sub.scan < sub.tail) && scanned < MAX_PUMP_BYTES_PER_WAKE {

        // B3(a): a COMPLETE line held over because `out` was full. A REAL FLAG — rev 5
        // tested `partial ends with b'\n'`, which R3's newline-first rule makes ALWAYS
        // FALSE, so this never fired and the next line's bytes were concatenated onto
        // the held one.
        if sub.held {
            if !try_emit_line(sub) { stalled = true; break }
            continue                                  // try_emit_line cleared `held`
        }

        n = min(HISTORY_CHUNK_BYTES, sub.tail - sub.scan)
        chunk = read n bytes from sub.file at offset sub.scan   // bounded by `tail`:
                                                                // never reads undrained bytes
        // ---- R4: the arms rev 4 omitted, either of which HANGS campd inside pump.
        buf = match chunk {
            Ok(b) if b.is_empty() =>
                // The stream file is append-only; it cannot shrink. scan < tail with a
                // zero-byte read is a genuine inconsistency, not a benign EOF.
                { push_fault(patrol.degraded, "stream file is shorter than campd's own
                              drained offset"); return Gone }
            Err(Interrupted)      => continue,
            Err(e)                => { push_fault(patrol.degraded, e); return Gone }
            Ok(b)                 => b,
        }

        // B2: `scan` and `scanned` advance PER BYTE ABSORBED — never per chunk read.
        // Rev 5 advanced `scan` over the WHOLE chunk up front and then `break`ed
        // mid-`buf` on a stall. `buf` is a local and `Subscriber` has no field for an
        // unconsumed remainder, so every byte after the stall point was THROWN AWAY
        // while `scan` already pointed past it: up to 64 KiB of SILENT LINE LOSS
        // (§9: "never a silently truncated stream") plus a permanent cursor/scan
        // desync. Rev 4 was safe here only because its early exit was `return Drop`,
        // which destroyed the subscriber instead of continuing.
        //
        // With per-byte accounting a stall simply leaves the remainder at [scan, ..),
        // and the next FILL re-reads it. Nothing is lost.
        for each byte b in buf:                       // a BYTE scan. No UTF-8 decode. Ever. (R7)
            sub.scan += 1;  scanned += 1

            // ---- oversize: the line's FRAME cannot fit the whole cap. Count, never buffer.
            if sub.oversize is Some(count):
                if b == b'\n':
                    off = sub.cursor + count + 1                    // + the newline
                    emit(skipped_frame(session, off, count, "over_cap"))
                    emit_once_per(session, sub.cursor) -> patrol.degraded  // G11
                    sub.cursor = off; sub.oversize = None
                else:
                    sub.oversize = Some(count + 1)
                continue

            // ---- R3: THE NEWLINE IS TESTED FIRST, before any push or cap check.
            if b == b'\n':
                sub.partial.push(b'\n')       // B1: THE NEWLINE GOES IN. `off` must land
                sub.held = true               // PAST it, or `cursor` drifts one byte per
                                              // line and §9's resume cursors are wrong.
                if !try_emit_line(sub) { stalled = true; break }    // R1: HOLD, never Drop
                continue

            // ---- R3: the over-cap decision is made on the FRAME, not the raw line.
            if sub.partial.len() + 1 + sub.frame_overhead > cap:
                sub.oversize = Some(sub.partial.len() + 1)
                sub.partial.clear()                                 // free the memory
                continue

            sub.partial.push(b)
    }

    // ---- (B) TERMINAL: full history FIRST, then the end frame, ONCE (C7 + R2) ---
    // B3(d): `!sub.held` — a HELD line is unfinished history, and rev 5's
    // `partial.is_empty()` was satisfied by a held line's presence being invisible.
    if !sub.end_sent
       && sub.out.is_empty()
       && sub.closing.is_some()
       && sub.scan == sub.tail
       && !sub.held
       && sub.partial.is_empty()
       && sub.oversize.is_none() {
        sub.out.extend(end_frame(session, sub.tail, reason))
        sub.end_sent = true                 // R2: WITHOUT THIS, (B) re-fires forever.
    }

    // ---- (C) FLUSH -------------------------------------------------------------
    if sub.out.is_empty() {
        // R2: the ONLY path to Gone, and it is now REACHABLE.
        if sub.end_sent { return Gone }
        return Ok        // done, or the scan budget ran out (poll_timeout continues us),
                         // or `tail` has not advanced yet, or we are stalled on the cap
                         // (impossible with an empty `out`, but harmless to state).
    }
    match conn.stream.write(&sub.out) {
        Ok(0)            => return Gone,
        Ok(n)            => { sub.out.drain(..n); sub.blocked_since = None }   // R1: it is READING
        Err(Interrupted) => continue,
        Err(WouldBlock)  => {
            // G2: the WRITABLE EDGE re-arms us. Do NOT arm a timeout here.
            // R1: but a peer that accepts ZERO bytes is a peer that has stopped
            // reading — and THAT, not the size of `out`, is what a drop means.
            if sub.blocked_since.is_none() { sub.blocked_since = Some(now) }
            if now - sub.blocked_since >= SUBSCRIBER_STALL_TIMEOUT {
                sub.high_water = max(sub.high_water, sub.out.len())
                return Drop(subscriber_dropped(sub))    // §4.4: loud, naming the high-water
            }
            return Ok
        }
        Err(_)           => return Gone,       // EPIPE / ECONNRESET: the peer is gone
    }
    // Loop: the socket took bytes, so `out` has room and FILL may resume.
  }

// R1/R3: emit ONE complete line from `partial`, or report that `out` has no room for
// it. Returns false => the caller STALLS: `partial` KEEPS the complete line, `held`
// stays true, `cursor` does NOT advance, and NOTHING is lost.
//
// PRECONDITION, now actually established by the caller (B1): `sub.held` is true and
// `sub.partial` ENDS WITH '\n'. Rev 5's doc comment claimed this and its only call
// site never produced it.
try_emit_line(sub) -> bool:
    // B1: `partial` INCLUDES the '\n', so `off` lands PAST the newline — which is
    // what makes it a valid §9 resume cursor (the start of the NEXT line). Rev 5
    // omitted the newline and drifted one byte per line, cumulatively.
    off  = sub.cursor + sub.partial.len()
    body = sub.partial without the trailing '\n' (and a trailing '\r')

    if body is empty (whitespace only):                        // G11: blank = silent no-op
        sub.cursor = off; sub.partial.clear(); sub.held = false; return true

    frame = event_frame(session, off, body)                    // &[u8] in, VERBATIM splice
             or skipped_frame(session, off, body.len(), "not_a_json_object")

    // R3's guarantee, and it survives B1's extra byte: the pre-push guard bounds
    // `partial.len() + frame_overhead <= cap` BEFORE the '\n' is pushed, and `body`
    // strips that '\n' again — so `frame.len() = frame_overhead + body.len() <= cap`
    // and this frame ALWAYS fits an EMPTY `out`. A held line therefore can never
    // stall forever: it goes out the moment the socket drains what is ahead of it.
    if sub.out.len() + frame.len() > cap: return false         // STALL (R1), never Drop

    sub.out.extend(frame); sub.high_water = max(sub.high_water, sub.out.len())
    sub.cursor = off; sub.partial.clear(); sub.held = false; return true
```

**Termination, per call — restated to cover (B) (R2 falsified rev 4's proof).** (A) is bounded by `MAX_PUMP_BYTES_PER_WAKE` and by `stalled`. **(B) fires AT MOST ONCE per subscriber, ever** (`end_sent`) — this is the branch rev 4's proof did not cover, and it neither advanced `scan` nor drained `out`, so it looped forever. (C) either drains `out`, returns `Ok`, or returns `Gone`/`Drop`. So every iteration advances `scan`, drains `out`, or sets a one-shot flag — all bounded. **`pump` always returns.**

**Progress, across calls — three states, one wakeup source each:**
1. behind, `out` empty ⇒ no fd will signal it ⇒ `poll_timeout` arms the continuation.
2. `out` non-empty ⇒ waiting on writability ⇒ the **WRITABLE edge**.
3. `out` non-empty **and the peer accepts nothing** ⇒ no edge ever comes ⇒ the **`blocked_since` deadline** (R1) arms `poll_timeout`, and the drop fires. *(This third state is why the drop needs a timer at all: a client that has stopped reading generates no events whatsoever.)*

**`tail` IS LINE-ALIGNED — a cross-module invariant the TERMINAL guard silently depends on** (non-blocking note, adopted, and named HERE because that is where it is load-bearing): cp-0's `t.offset` advances only past `\n`-terminated lines (read_channel.rs:626-652). So a worker that exits mid-line leaves those bytes **outside** `tail`, `pump` never reads them, and `partial.is_empty() && oversize.is_none()` holds at `scan == tail` — the `end` frame is reachable. **A future phase that advanced `offset` mid-line would make the `end` frame silently disappear.**

**`poll_timeout` (extended) — G2, and the disjunct rev 3 got wrong:**
```rust
// ZERO iff some subscriber has PUMPABLE WORK and no fd will signal it.
// B3(c): `|| s.held` — a line HELD at `scan == tail` (the normal terminal state of a
// catch-up that ran at the cap) is real, pending work. Rev 5 required `scan < tail`,
// so for such a subscriber NOTHING was armed: `blocked_since` is None (the peer IS
// reading), no WRITABLE edge is pending once `out` drains, and the last line of the
// history was never delivered — and the end frame never followed it.
let subscriber_work = self.subscribers.values()
    .any(|s| s.out.is_empty() && (s.held || s.scan < s.tail) && !s.end_sent);
// R1: a peer that accepts NOTHING generates NO events at all — not a WRITABLE edge,
// not an EOF. So the stall drop needs its own armed deadline, and it is the ONLY
// thing that can ever fire for that subscriber. Armed only while blocked; None
// otherwise (invariant 1).
let earliest_stall = self.subscribers.values()
    .filter_map(|s| s.blocked_since)
    .map(|t| t + SUBSCRIBER_STALL_TIMEOUT)
    .min();
min3(earliest_pending_control_due_at,
     if subscriber_work { Some(ZERO) } else { None },
     earliest_stall.map(|d| d.saturating_duration_since(now)))
```
**A non-empty `out` must NOT arm anything.** It means the last write returned `WouldBlock`, and the correct wakeup for that is the **WRITABLE edge** — already registered (Step 4), already relied on by C7. Rev 3 armed `ZERO` on it, which turned every blocked write into a spin: `poll(0)` → `pump` → `WouldBlock` → `poll(0)` … re-running `apply_tracking`, `drain_all` over every tailed file, and `persist_offsets` on each pass. **And this was the COMMON case, not the pathological one:** macOS's unix-socket send buffer is ≈ 8 KiB (the very number test 11's rationale cites) while one chunk yields up to ~64 KiB of frames — so **every healthy subscriber WouldBlocks on essentially every chunk**, and campd would have spun for the duration of any stream. Invariant 1, §4.3.

`None` when neither holds — so an idle campd with idle subscribers still blocks forever.

**`pump` cannot take `&mut Ledger`** (it is called from the token arm with a `&mut Conn` already borrowed), so its durable events ride a **`pending_events: Vec<EventInput>` collector on `ControlRuntime`** — cp-0's `cap_breaches`/`parse_errors` mold — drained by the caller and appended there. The `over_cap` `patrol.degraded` dedupe (`HashSet<(String, u64)>`, G11) lives on the runtime for the same reason: N subscribers hit the same over-cap line and must not append N events.

**UTF-8 (non-blocking note, adopted).** `pump` operates on **BYTES, end to end**: it scans for `b'\n'` in the byte buffer and hands `&[u8]` to `event_frame`, which validates with `serde_json::from_slice` and splices the raw bytes. **It must NOT `from_utf8_lossy`** — cp-0 does (read_channel.rs:629) because it only needs a `Display`able line for a fault message, but a lossy decode **silently rewrites the very bytes C2's design exists to preserve.**

Called at **three** sites: right after the hello is written (B11); on every WRITABLE readiness; after every `fanout`.

**`fanout(read_channel, conns) -> (Vec<Token>, Vec<EventInput>)`** (D6″ — it no longer touches `lines` at all): for each subscriber, refresh `tail` from `read_channel.tail_state(session)`, then `pump`. Returns the tokens to close and the durable events (`subscriber.dropped` + any `over_cap` `patrol.degraded` from the collector).

**The `tail` refresh, specified for all three cases (G4's new-failure guard):**
- `Some((_, t))` and **not** `closing` ⇒ `tail = t`.
- **`None`** (the session is no longer tailed) ⇒ **leave `tail` UNCHANGED.** Never zero it, never panic. This is the window between `dispose_pending` and `close_disposed` within one wake; `close_disposed` immediately pins the authoritative value.
- **`closing.is_some()`** ⇒ **leave `tail` PINNED** at the final offset, whatever `tail_state` says.

**`close_disposed(&[Disposed], ledger, conns) -> (Vec<Token>, Vec<EventInput>)`** (B12/C7/G4) — **called from the event loop AFTER `dispose_pending`, not from inside `control_step`.** For each `Disposed { session, final_offset }`:
1. every subscriber of that session gets `closing = Some(reason)` — where `reason` comes from `ledger.session_status(name)` and is **`stopped` or `crashed`** (never `capped`: that value does not exist in the column) — and **`tail = final_offset`**, the authoritative end (Task 4's `dispose_pending` is what recorded it);
2. `pump` each one. A **caught-up** subscriber (`out` empty, `scan == tail`) hits the TERMINAL branch immediately and its `end` frame goes out **on this wake** — the case rev 3 blocked forever on;
3. `control.forget_session(session, now)` (G7) — its still-`pending` control requests are expired LOUDLY as `control.failed{cause:"session_ended"}`, and its `answered`/`timed_out` ids are pruned. Those events are returned to the caller.

It does **not** close a connection itself — `pump` returns `Gone` once the `end` frame has flushed, and the event loop deregisters then.

**`forget(token)`** — drop the subscription (every close path calls it). **`is_subscriber(token)`**; **`subscriber_count()`** (PERMANENT test-observable allow).

**The ordering inversion, SANCTIONED IN WRITING (non-blocking note, adopted).** `pump` runs inside `control_step`, which is **before** the cap-breach/parse-fault appends and **before** `persist_offsets` — so **a subscriber sees a line's bytes before that line's ledger effect commits.** cp-0's stated law (*"offsets persist only after the line's ledger effect commits"*) is about **campd's own durable offset**, which still holds: the subscriber's cursor is the CLIENT's, held by the client, and a client that reconnects after a campd crash simply re-reads from a cursor it owns. This is a deliberate, stated divergence — not an oversight.

**Accepted cost (non-blocking note, named).** Each subscriber re-parses each line it delivers (`from_slice` in `event_frame`), duplicating work `drain_one` already did — O(subscribers × bytes) on the event loop. It is bounded by `MAX_PUMP_BYTES_PER_WAKE × MAX_SUBSCRIBERS` (2 MiB/wake worst case) and is accepted for cp-1; the alternative (sharing parsed lines from the drain) cannot serve a history read, which is a file read by construction.

- [ ] **Step 4: Wire the event loop — B7's fix stays.**
- `struct Conn` → `pub(super)` (fields too).
- The accept arm registers `READABLE | WRITABLE`. *(Precisely: edge-triggered epoll/kqueue reports writability ONCE at registration — an accept-time cost, not an idle one. That already-consumed edge is exactly why the hello's first bytes need an explicit `pump` — B11.)*
- **The token arm — NO SHORT-CIRCUIT (B7).** If `control.is_subscriber(token)`, `pump` first (a WRITABLE wake is why we are here) and handle `Drop`/`Gone` — **then fall through into `serve_connection` like any other connection**, so cp-0's `ReadStop::Eof ⇒ ConnState::Closed` still detects a hangup.
- `control.forget(token)` on EVERY close path (`Closed`, the error arm, `Gone`, a cap drop). **A normal detach appends NO event** (§5.2).
- The new `drain_lines` arm: `serve_subscribe` → `respond(hello)` → `pump`.
- `control_step` gains `fanout` + `close_disposed` (Task 6 Step 5).

`tests/fake-agent.sh` — **two modes, and the second is mandatory (G1).**

```bash
#   FAKE_AGENT_SPAM_ON_TURN=N   cp-1 (B8): on a USER TURN, emit N stream-json lines.
#                               The spam must come AFTER the subscriber is registered,
#                               or the backpressure gate tests nothing.
#
#   FAKE_AGENT_HUGE_LINE=N      cp-1 (G1): emit ONE stream-json line whose `text`
#                               field is N bytes — a SINGLE line far larger than
#                               HISTORY_CHUNK_BYTES (64 KiB) and, at N >= 1 MiB,
#                               larger than the whole subscriber cap.
#
#   WHY THIS EXISTS: **every other fixture in this repo emits SHORT lines** —
#   `emit_stream()` is `printf '%s\n' "$1"` and nothing anywhere produces a line
#   bigger than a few hundred bytes. That is precisely WHY G1 (pump livelocking on
#   any line > 64 KiB) was invisible to the entire suite, and why a real
#   Read/Bash/Grep tool-result line — which routinely exceeds 64 KiB — would have
#   hung campd in production while CI stayed green. Without this mode no gate in
#   this phase can see the phase's worst bug.
huge_line() {
  # A valid stream-json object with an N-byte payload, on ONE line.
  local n="$1"
  local pad
  pad="$(head -c "$n" /dev/zero | tr '\0' 'x')"
  printf '{"type":"assistant","message":{"role":"assistant","content":"%s"}}\n' "$pad"
}
if [[ -n "${FAKE_AGENT_HUGE_LINE:-}" ]]; then
  huge_line "$FAKE_AGENT_HUGE_LINE"
  emit_stream '{"type":"assistant","message":{"role":"assistant","content":"after the monster"}}'
fi
```
Test 13 drives it with `FAKE_AGENT_HUGE_LINE=2097152` (2 MiB — over the 1 MiB cap, so it exercises the OVERSIZE SCAN *and* the `skipped` frame) and asserts the `"after the monster"` line still arrives, which is the proof the cursor advanced past a line campd refused to buffer.

- [ ] **Step 5: Run.**

Run: `cargo test -p camp --bins daemon::control && cargo test -p camp --test control 2>&1 | tail -40`
Expected: PASS — unit **31** (7 from Task 1 + 10 from Task 3 + **14** here); integration **21** (6 from Task 6 + 1 from Task 7 + **14** here). *(G9: if an observed count differs, RECONCILE THE LIST against the enumerations above — **never delete a test to satisfy a gate**. The counts have been stale four revisions running; these are recomputed from the lists, not carried forward.)*

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

- [ ] **Step 1b: ADD THE LOADED ARM — the gate that would have caught G1 and G2** (non-blocking note, adopted, and it is the most valuable of them).

**The rev-3 idle gate was constructed to be blind to the two worst bugs this phase can ship.** G2's spin requires a **non-empty** buffer; G1's livelock requires a **large line**. The idle arm holds N subscribers with **empty** buffers on **quiescent** sessions — it can observe **neither**. A gate that cannot see the failure mode the phase introduces is not a gate.

**⚠ AND SAY THIS PLAINLY (non-blocking note, adopted): `make perf` is `#[ignore]`d and LOCAL-ONLY, so NO CI GATE CATCHES A SPIN.** This arm is a real gate only as far as an operator runs it before merging a perf-relevant PR (AGENTS.md requires exactly that, and this PR is perf-relevant). **The CPU-bounded property of `pump` is therefore defended primarily by the UNIT tests** (`poll_timeout_never_arms_on_a_wouldblock_alone`, `pump_lexes_a_line_that_spans_many_chunks`), which DO run in CI — the loaded perf arm is the belt, not the braces. Do not let its existence imply a protection CI does not provide.

```rust
    // cp-1 (G1/G2): the LOADED arm. N subscribers, ONE session actively streaming
    // (FAKE_AGENT_SPAM_ON_TURN), each subscriber READING normally. Assert:
    //
    //   (a) campd's CPU over the streaming window is BOUNDED — well under a
    //       busy-loop's 100%. G2's spin (poll(0) -> pump -> WouldBlock -> poll(0))
    //       pegs a core for the whole stream, because macOS's ~8 KiB socket buffer
    //       means EVERY healthy subscriber WouldBlocks on essentially every chunk.
    //   (b) the stream COMPLETES within a hard deadline. G1's livelock hangs
    //       forever on the first line > 64 KiB, so a deadline is what turns it from
    //       "the suite hangs" into "the gate fails".
    //   (c) every subscriber received every line, exactly once (D6"'s guarantee, at
    //       load rather than in a unit test).
    //
    // Run this arm ALSO with FAKE_AGENT_HUGE_LINE set, so the loaded gate covers a
    // line larger than a chunk — the ordinary case on any session that reads a file.
```

- [ ] **Step 2: Run it (LOCAL-ONLY, per AGENTS.md).** `make perf 2>&1 | tail -30` → PASS: the idle arm's 0.0 % CPU / <20 MB RSS, **and** the loaded arm's bounded-CPU / completes-within-deadline / exactly-once assertions.

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

- [ ] **Step 1b: AMEND §4.4 OF THE SPEC — IN THIS PR.** AGENTS.md: *"If implementation reality contradicts the spec, stop and update the spec via PR in the same change — spec and code never silently diverge."* **cp-1 deliberately does NOT implement §4.4's literal "a subscriber whose buffer crosses the cap is dropped"** — the cap is a **stop** and the drop is a **stalled peer** (R1). Carrying that as an unrecorded divergence is exactly what the repo forbids.

Edit `docs/superpowers/specs/2026-07-12-camp-control-plane-design.md` §4.4's second bullet to read, in substance:

> **`subscriber_buffer_bytes` (default 1 MiB) is a STOP, not a kill.** When the buffer is full campd stops framing and holds the next complete line; nothing is lost and nothing is dropped. **A subscriber is dropped when its PEER STOPS READING** — its socket has accepted zero bytes for `SUBSCRIBER_STALL_TIMEOUT` (30 s) with data buffered — dropped loudly, with `subscriber.dropped` naming the session and the high-water mark. It is never blocked on, and events are never silently discarded.
>
> *Rationale (cp-1): during catch-up the producer is a FILE read and a file always outruns a socket (macOS's unix-socket send buffer is 8 KiB), so a buffer-SIZE kill drops healthy, fast-reading clients that are merely BEHIND — breaking §4.1's "a late joiner gets history, then follows" and §9's "never a silently truncated stream" for any session with more than 1 MiB of output. campd still never blocks; memory is still bounded (`out` ≤ cap and `partial` ≤ cap); a genuinely stalled peer is still dropped loudly.*
>
> *Known residual, accepted for cp-1: a peer accepting one byte per interval clears the stall timer indefinitely and can hold a buffer and a subscriber slot. It is reading, so it is not stalled. A byte-rate floor is a cp-2 obligation.*
>
> ***RESOLVED (#121)** — by a bound on time-at-cap rather than a byte-rate floor. A subscriber continuously at the buffer cap for `AT_CAP_STALL_INTERVALS` (10) `SUBSCRIBER_STALL_TIMEOUT` intervals is dropped, loudly, with the same event.*

**This is a spec edit and it needs operator sign-off** (AGENTS.md, and the Global Constraints above). **Raise it with the lead as part of the PR — do not merge the code without it.**

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
**Confirm the counts (G9 — and a gate that is itself wrong is worse than no gate, so these are recomputed from the enumerated lists, not carried forward):**

| filter | expected | made of |
|---|---|---|
| `cargo test -p camp --bins daemon::control` | **31** | Task 1: 7 · Task 3: 10 · Task 8: **14** |
| `cargo test -p camp --test control` | **21** | Task 6: 6 · Task 7: 1 · Task 8: **14** |
| `cargo test -p camp --bins daemon::read_channel` | **3 NEW** + every cp-0 test (25 total today) | Task 4 |
| `cargo test -p camp --bins daemon::dispatch` | **2 NEW** + every existing | Task 5 |
| `cargo test -p camp --bins daemon::socket` | **2 NEW** wire pins (`control_plane_verbs…`, `subscribe…`, `sessions_list…` minus the DELETED `nudge_wire_format_is_pinned`) + every existing | Tasks 6–8 |

*(The "N new" columns are **counts of tests this plan adds**, not of a filter's total output — rev 4 conflated the two in Tasks 5 and 7.)*

**THE RULE (G9):** if an observed count differs from the plan's, **RECONCILE THE LIST** — find the test the plan enumerates that you did not write, or the one you wrote that it does not list. **NEVER delete a test to satisfy a count**, and never record a false failure because a count was stale. The count is a cross-check, not an authority.

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

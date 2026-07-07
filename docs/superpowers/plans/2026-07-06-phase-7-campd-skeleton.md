# Gas Camp Phase 7 — campd Skeleton Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The only standing process, crash-only and event-driven (spec §5): campd binds `<camp>/campd.sock` (liveness IS the socket), serves the pinned poke/status/stop protocol, catches up past its `cursors` row exactly once through an `EventProcessor`, auto-starts on demand from any verb that needs it, and sleeps in `poll` with no timeout — plus `camp stop` and `camp top`.

**Architecture:** Extend the merged Phase-1/Phase-3 core without reworking it. camp-core gains the *mechanism* (a generic named-cursor `process_past_cursor` that advances the cursor in the same transaction as the processor's effects, plus one `status_summary` query); the camp binary gains the *daemon* (`daemon/{mod,socket,event_loop,cursor}.rs`): a single-threaded mio event loop whose poll timeout is always `None` in this phase (the Phase-10 cron heap and Phase-11 stall timers plug into one named function), socket liveness rules, and detached auto-start driven by a readiness line on the child's stdout (an OS pipe read — no sleep/retry loop anywhere, not even in the CLI).

**Tech Stack:** Rust (edition 2024), mio (os-poll + net — see Decision A), rusqlite, serde/serde_json, clap, anyhow (bin) / thiserror (core); assert-free std unix sockets in tests; tempfile. No async runtime (spec §15.1).

## Global Constraints

Copied from AGENTS.md, the master plan, and the operator's standing rules. Every task's requirements implicitly include this section.

- **Spec is authoritative:** `docs/design/2026-07-05-gas-camp-design.md`; its §4 decision record is settled. If implementation reality contradicts the spec, stop and update the spec via PR in the same change.
- **Master plan contract:** `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md`, section "Phase 7 — campd Skeleton (`phase-7-campd-skeleton`)". Files, the pinned socket protocol, semantics, test list, and exit criteria are binding.
- **Idle is free (invariant 1 — the soul of this phase):** no ticks, no polling loops, anywhere. The idle daemon blocks in `poll` with an infinite timeout; the only sanctioned wakeups are socket events (timers arrive in Phases 10/11). No timeout-driven code path may exist in campd.
- **Pinned protocol** (newline-delimited JSON over `<camp>/campd.sock`):
  `{"op":"poke","seq":N}` → `{"ok":true}` · `{"op":"status"}` → `{"ok":true,"live_sessions":[…],"ready":N,"open":N,"campd_pid":N}` · `{"op":"stop"}` → `{"ok":true}` then graceful exit with a `campd.stopped` event.
- **Liveness = the socket accepts (spec §5):** stale socket file that refuses connections is unlinked and rebound; bind conflict on a live socket means the second daemon refuses to start. No pidfiles, no lockfiles-as-status.
- **Crash-only (spec §5):** campd holds no exclusive state; `kill -9` is a supported shutdown method; on start it opens the ledger, appends `campd.started`, processes events past its cursor, and sleeps.
- **Exactly-once catch-up:** the `cursors` row `'campd'` advances in the same `BEGIN IMMEDIATE` transaction as the processor's ledger effects; a crash or error never loses an event and never replays one.
- **Respect merged interfaces:** extend, don't rework. New event payloads use `#[serde(deny_unknown_fields)]` structs; keep the one-transaction event+state property; keep the vocab-pin partition tests and the refold property test green. (`cursors` is consumer bookkeeping, deliberately outside refold — see `schema.rs` comment.)
- **Zero role names in machinery** (spec §2.4). campd moves work; it never reasons about it.
- **Fail fast:** no silent fallbacks, no silenced errors. No panics in library code (`clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic` denied; `#![forbid(unsafe_code)]`). The ONE sanctioned ignore-errors site is the post-commit poke, which spec §7.2 defines as fire-and-forget ("if campd is down, writes still succeed and it catches up from its processed-cursor on start").
- **Vocabulary mirror (spec §15.2):** the new `campd.autostarted` event is camp-specific and additive — declared in `CAMP_SPECIFIC_EVENTS`, absent from gc's pinned registry.
- **TDD, strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything.
- **Git:** never commit to main; branch `phase-7-campd-skeleton`; no co-author lines, no self-mention. Conventional-commit style.
- **Gates before push:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- **Shared-file protocol (siblings phase-4 and phase-5 in flight):** edits to `crates/camp/src/main.rs`, `crates/camp-core/src/{event,vocab}.rs`, `crates/camp-core/src/ledger/fold.rs`, both `Cargo.toml`s, and `Cargo.lock` stay minimal and additive. When the lead reports a sibling merge: rebase onto current main, resolve, re-run all gates before continuing. Never open/update a PR from a branch not rebased on current main.
- **Nothing is complete until pushed, CI green (`gh pr checks --watch`), and every claim in the PR description verified.**

## Key Paths and Conventions

- Worktree: `/Users/kiener/code/gascamp/.claude/worktrees/agent-ab273d6563b7fee0b`, branch `phase-7-campd-skeleton` (already created).
- camp-core modified: `src/event.rs`, `src/vocab.rs`, `src/ledger/{fold,mod}.rs`.
- camp (bin) new: `src/daemon/{mod,socket,event_loop,cursor,autostart}.rs`, `src/cmd/{stop,top}.rs`, `tests/daemon_lifecycle.rs`; modified: `src/main.rs`, `src/campdir.rs`, `src/cmd/{create,claim,close,rig}.rs`, `Cargo.toml`.
- Daemon-appended events use `actor = "campd"`; CLI-appended events use `actor = "cli"` (existing convention).
- Integration tests drive the real binary via `env!("CARGO_BIN_EXE_camp")` and speak raw JSON over `std::os::unix::net::UnixStream` (a bin crate's internals are not importable from integration tests — by design, they test the wire).
- Daemon start signal: campd prints exactly one line `campd listening on <path>` to stdout once the socket is bound, then never writes stdout again. Auto-start and tests block on that line (a pipe read — an OS event), never on sleep/retry.

## Plan-Time Decision Log

Decisions made while writing this plan. **Decision A deviates from the master plan's tech-stack line and needs operator sign-off at plan approval.**

- **A. `mio` replaces `polling` (flagged for operator).** The master plan's tech stack and the Phase 7 section name the `polling` crate. Verified empirically 2026-07-06 (scratchpad compile): `polling` v3 made `Poller::add` an `unsafe fn`, so calling it is a hard compile error under `#![forbid(unsafe_code)]` — which AGENTS.md invariant 5 and the workspace lint policy mandate. The spec itself (§15.1) lists "`polling` or `mio`" as candidate crates, so using mio (`features = ["os-poll", "net"]`, whose registration API is safe) is spec-compliant; only the master plan's crate-name line is deviated from, and this plan is the in-repo record of that. The mechanism is identical: one `Poll`, listener + per-connection READABLE interests, `poll(&mut events, timeout)` where `timeout` is the earliest armed timer deadline (`None` = infinite wait). Bonus: `signal-hook-mio` exists for Phase 8's SIGCHLD self-pipe.
- **B. Readiness line instead of connect-retry.** Auto-start must "retry once, then error — no retry loop". A bare reconnect after spawn is a race (the daemon may not have bound yet); a sleep would be a tick. Resolution: campd writes `campd listening on <path>` to stdout after binding; auto-start spawns the daemon with stdout piped, blocks on reading that line (an OS pipe event), then retries the connection exactly once. EOF without the line = the daemon failed = hard error pointing at `<camp>/campd.log`. Deterministic, event-driven, zero sleeps.
- **C. Cursor mechanism in camp-core, policy in camp.** The master plan puts `cursor.rs` in `camp/src/daemon/`, but the transactional exactly-once machinery must live with the transaction owner (`Ledger` encapsulates its `Connection`; Phase 3 decision F forbids raw-connection leaks). Split: camp-core `Ledger` gains generic `cursor(name)` and `process_past_cursor(name, process_fn)` (one `BEGIN IMMEDIATE` transaction **per event**: run the callback, advance the cursor, commit — an error halts with the cursor on the last processed event); camp's `daemon/cursor.rs` keeps the campd policy (the `EventProcessor` trait, `ReadinessProcessor`, the `'campd'` cursor name, and `catch_up` which loops `process_past_cursor` to a fixpoint so events appended *by* a processor — Phase 8's dispatch — are drained in the same call). The fixpoint loop is bounded by the backlog; it is convergence, not polling.
- **D. `status` computes from the state tables at request time.** `live_sessions` = `sessions.status='live'` names (ordered); `ready` = `ready_beads(None).len()` (reuses the one readiness predicate — DRY); `open` = count of `beads.status='open'` (blocked-but-open beads count; `in_progress` and `closed` do not); `campd_pid` = `std::process::id()`. `ReadinessProcessor`'s pending list is bookkeeping for Phase 8's dispatch, not the status source — one source of truth, no cache to drift.
- **E. Poke wiring.** Every CLI verb that appends (`create`, `claim`, `close`, `rig add`) calls `poke_best_effort(&camp.socket_path(), seq)` right after `append` returns. Fire-and-forget is the spec-mandated behavior (§7.2), documented at the one call site that ignores errors. The poke's `seq` is advisory: the daemon's catch-up processes everything past the cursor regardless, so an old or lost poke is harmless. Pokes never auto-start the daemon (a poke with campd down is the "write succeeds, catch-up covers it" path).
- **F. Auto-start detach mechanics.** `request_with_autostart` probes with a plain connect; on failure it appends `campd.autostarted` (actor `cli`, data `{"verb":"top"}`) **before** spawning so the trail reads `campd.autostarted` → `campd.started` (cause before effect, spec §13.3), then spawns `current_exe() daemon --camp <root>` with stdin null, stdout piped (readiness line), stderr appended to `<camp>/campd.log`, and `process_group(0)` (safe `std::os::unix::process::CommandExt`; the CLI exits immediately after, so the daemon is reparented to init — the master plan's Phase 7 auto-start line was amended 2026-07-06 to say exactly this, replacing its double-fork/setsid wording, which would require unsafe libc calls). `campd.log` is where a detached daemon's stderr must land to satisfy "never silence errors" — a visible operational artifact inside the camp dir, not daemon-private state. The intentionally-unwaited child gets a targeted `#[allow(clippy::zombie_processes)]` with a justification comment.
- **G. Stop ordering.** On `{"op":"stop"}`: append `campd.stopped` (durable truth first) → unlink the socket → respond `{"ok":true}` → return from the loop → exit 0. `camp stop` never auto-starts; with no daemon reachable it exits 1 with `campd is not running`.
- **H. Poke-path processing errors don't kill the daemon and are never skipped.** Startup catch-up errors are fatal (fail fast — the daemon refuses to start on a poisoned backlog). A processing error during a live poke responds `{"ok":false,"error":…}`, logs to campd's stderr, and leaves the cursor *before* the failing event, so the error re-surfaces on every subsequent poke until fixed — surfaced to the caller, never silenced, no daemon suicide loop.
- **I. Client socket ops carry 5 s read/write timeouts.** These are CLI hang-prevention on a single active operation, not wakeups; the daemon's poll timeout remains `None`. The no-timeout exit criterion is about campd's idle loop.
- **J. Malformed request lines** get `{"ok":false,"error":"bad request: …"}` and the connection is closed; unknown `op` is a serde unknown-variant error on the internally-tagged enum (`deny_unknown_fields` is not supported on internally tagged enums — the variant check is the strictness). A response-write `WouldBlock` (client not reading its few-byte response) drops that connection with a stderr note; a broken client never takes campd down.

## Post-review amendments (2026-07-06, PR #8 review — all five findings addressed on the branch)

1. **Decision on stale-socket replacement (amends the liveness rules):** the probe → unlink → rebind section in `bind_or_replace` is serialized with an exclusive advisory lock on `<socket>.lock` (finding 1 — TOCTOU split-brain). The lock releases on drop/process exit (crash-only preserved); it is a serialization primitive, not status — liveness stays "the socket accepts". Test: `socket::tests::concurrent_bind_or_replace_elects_exactly_one_daemon` (proven red pre-fix).
2. **Amends Decision B:** child EOF without a readiness line is no longer an unconditional hard error — `start_detached` re-probes the socket first; a live socket means our child lost the start race to a daemon that won it (finding 2). Test: `autostart::tests::start_detached_recognizes_a_lost_race` (proven red pre-fix); integration property `daemon_lifecycle::concurrent_top_autostarts_exactly_one_campd`.
3. **Amends Decision J:** a single request line is capped at `MAX_REQUEST_BYTES` (64 KB); an oversized line gets `{"ok":false,"error":"request line exceeds …"}` and the connection is dropped, campd unharmed (finding 3). The connection map itself is deliberately uncapped (bounded by the fd limit; a local single-user socket). Test: `daemon::tests::oversized_request_line_is_rejected_and_the_connection_closed` (proven red pre-fix via read-timeout).
4. **Amends Decision C:** `process_past_cursor` drains the backlog in pages of `CATCH_UP_PAGE_SIZE` (500) events instead of materializing it whole (finding 4); the per-event exactly-once transaction is unchanged. Tests: `process_past_cursor_pages_through_a_large_backlog`, `a_mid_page_error_resumes_exactly_across_page_boundaries` (pagination-correctness pins; the memory defect has no deterministic red).
5. **Amends Decision F wording (and the master plan's Phase 7 auto-start line):** detachment is `process_group(0)` + init reparenting, not double-fork/setsid (which requires unsafe libc, forbidden). Docs and code now agree (finding 5).

## What later phases rely on (interfaces Phase 7 produces)

- **Phase 8 (dispatch):** `trait EventProcessor { fn process(&mut self, conn: &rusqlite::Connection, event: &Event) -> Result<(), CoreError>; }` — dispatch replaces/extends `ReadinessProcessor`, writing through `conn` (the open cursor transaction) so spawn bookkeeping commits atomically with the cursor; `ReadinessProcessor::take_pending()` hands over the newly-ready bead ids; `autostart::request_with_autostart(camp, &Request, verb)` is `sling`'s daemon-contact path; `daemon::socket::{Request, Response, poke_best_effort}` is the wire.
- **Phase 10 (orders):** `event_loop::poll_timeout()` is the single plug point for `CronHeap::next_deadline()`; the loop already treats `None` as infinite wait.
- **Phase 11 (patrol/adoption):** adoption slots into `daemon::run` between catch-up and the event loop; stall timers join the same poll-timeout mechanism.

## File Structure

| File | Responsibility |
|---|---|
| `crates/camp-core/src/event.rs` (mod) | add `EventType::CampdAutostarted` (`"campd.autostarted"`) |
| `crates/camp-core/src/vocab.rs` (mod) | add `"campd.autostarted"` to `CAMP_SPECIFIC_EVENTS` |
| `crates/camp-core/src/ledger/fold.rs` (mod) | `campd.autostarted` arm: validated payload, log-only |
| `crates/camp-core/src/ledger/mod.rs` (mod) | `cursor`, `process_past_cursor`, `StatusSummary`, `status_summary` |
| `crates/camp/src/campdir.rs` (mod) | `socket_path()`, `log_path()` |
| `crates/camp/src/daemon/mod.rs` (new) | `run(camp)`: open ledger → bind → `campd.started` → catch-up → readiness line → loop |
| `crates/camp/src/daemon/socket.rs` (new) | pinned protocol types, `bind_or_replace`, client `request`, `poke_best_effort` |
| `crates/camp/src/daemon/event_loop.rs` (new) | mio loop, `poll_timeout()` (always `None` here), request handling, stop |
| `crates/camp/src/daemon/cursor.rs` (new) | `EventProcessor`, `ReadinessProcessor`, `CAMPD_CURSOR`, `catch_up` |
| `crates/camp/src/daemon/autostart.rs` (new) | probe → `campd.autostarted` → detached spawn → readiness line → one retry |
| `crates/camp/src/cmd/stop.rs` (new) | `camp stop` (never auto-starts) |
| `crates/camp/src/cmd/top.rs` (new) | `camp top`: one status query, plain-text render |
| `crates/camp/src/main.rs` (mod) | argv0 `campd` dispatch, `Daemon`/`Stop`/`Top` subcommands |
| `crates/camp/src/cmd/{create,claim,close,rig}.rs` (mod) | post-commit poke |
| `crates/camp/tests/daemon_lifecycle.rs` (new) | the spec §13.3/§5 integration scenarios against the real binary |
| `crates/camp/Cargo.toml` (mod) | + mio, serde; rusqlite dev-dep → dependency |

## Watch items

- macOS `sun_path` limit is ~104 bytes; tempdir sockets (`$TMPDIR/…/.camp/campd.sock`) fit today. If a bind ever fails with `InvalidInput`, the path length is the first suspect — the bind error context includes the path.
- `clippy::zombie_processes` fires on the intentionally-unwaited detached daemon spawn; the targeted allow in Task 7.7 carries the justification.
- On sibling-merge rebases, the conflict surface is exactly: `event.rs`/`vocab.rs`/`fold.rs` (one additive arm each), `main.rs` (subcommand arms), `Cargo.toml`/`Cargo.lock`.

---

### Task 7.1: camp-core — the `campd.autostarted` event

**Files:**
- Modify: `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`
- Test: `crates/camp-core/src/ledger/mod.rs` (new cases in the existing `tests` module)

**Interfaces:**
- Consumes: existing `EventType`, `payload::<T>` fold helper, `temp_ledger()`/`input()` test helpers in `ledger/mod.rs`.
- Produces: `EventType::CampdAutostarted` with name `"campd.autostarted"` — Tasks 7.7/7.8 append it from the auto-start path.

- [ ] **Step 1: Write the failing tests** — in `crates/camp-core/src/ledger/mod.rs`'s `tests` module, next to `campd_lifecycle_events_are_log_only`:

```rust
    #[test]
    fn campd_autostarted_is_validated_and_log_only() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(input(
                EventType::CampdAutostarted,
                None,
                None,
                serde_json::json!({"verb": "top"}),
            ))
            .unwrap();
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 0);
        assert_eq!(count(&ledger, "SELECT count(*) FROM sessions"), 0);

        // missing verb fails fast, appends nothing
        assert!(
            ledger
                .append(input(EventType::CampdAutostarted, None, None, serde_json::json!({})))
                .is_err()
        );
        // unknown fields fail fast
        assert!(
            ledger
                .append(input(
                    EventType::CampdAutostarted,
                    None,
                    None,
                    serde_json::json!({"verb": "top", "extra": 1}),
                ))
                .is_err()
        );
        // empty verb fails fast
        assert!(
            ledger
                .append(input(
                    EventType::CampdAutostarted,
                    None,
                    None,
                    serde_json::json!({"verb": ""}),
                ))
                .is_err()
        );
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp-core campd_autostarted`
Expected: FAIL to compile — `EventType` has no variant `CampdAutostarted`.

- [ ] **Step 3: Implement** — three additive edits.

`crates/camp-core/src/event.rs`: add the variant after `CampdStopped` in the enum, in `ALL`, and in `as_str`:

```rust
    CampdAutostarted,
```
```rust
        EventType::CampdAutostarted,
```
```rust
            EventType::CampdAutostarted => "campd.autostarted",
```

`crates/camp-core/src/vocab.rs`: add to `CAMP_SPECIFIC_EVENTS` after `"campd.stopped"`:

```rust
    "campd.autostarted",
```

`crates/camp-core/src/ledger/fold.rs`: add a match arm above the log-only line, plus the handler:

```rust
        EventType::CampdAutostarted => campd_autostarted(event),
```
```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CampdAutostarted {
    verb: String,
}

/// `campd.autostarted` is log-only: the CLI records which verb caused the
/// spawn (spec §13.3 — every action carries its cause). The fold validates
/// the audit payload so a malformed event fails fast.
fn campd_autostarted(event: &Event) -> Result<(), CoreError> {
    let p: CampdAutostarted = payload(event)?;
    if p.verb.is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "empty verb".to_owned(),
        });
    }
    Ok(())
}
```

- [ ] **Step 4: Run the new test and the guard suites**

Run: `cargo test --package camp-core campd_autostarted && cargo test --package camp-core --test vocab_pin && cargo test --package camp-core --test refold_prop`
Expected: all PASS (the vocab partition test picks the new name up from `EventType::ALL` + `CAMP_SPECIFIC_EVENTS` automatically; `campd.autostarted` is absent from `gc-vocab.json`'s gc lists).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat: campd.autostarted event (camp-specific, validated, log-only)"
```

### Task 7.2: camp-core — named cursors with exactly-once processing

**Files:**
- Modify: `crates/camp-core/src/ledger/mod.rs`
- Test: same file, `tests` module

**Interfaces:**
- Consumes: existing `Ledger` (`conn`, `events_range`), `cursors` table (schema v1, consumer bookkeeping — outside refold by design).
- Produces (exact, used by Task 7.5):

```rust
impl Ledger {
    pub fn cursor(&self, name: &str) -> Result<Seq, CoreError>; // 0 when the row is absent
    pub fn process_past_cursor(
        &mut self,
        name: &str,
        process: &mut dyn FnMut(&rusqlite::Connection, &Event) -> Result<(), CoreError>,
    ) -> Result<Seq, CoreError>;
}
```

- [ ] **Step 1: Write the failing tests** — in `ledger/mod.rs`'s `tests` module:

```rust
    #[test]
    fn cursor_defaults_to_zero_and_tracks_processing() {
        let (_dir, mut ledger) = temp_ledger();
        assert_eq!(ledger.cursor("campd").unwrap(), 0);
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();

        let mut seen = Vec::new();
        let end = ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                seen.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert_eq!(end, 2);
        assert_eq!(seen, vec![1, 2]);
        assert_eq!(ledger.cursor("campd").unwrap(), 2);

        // nothing pending: nothing is reprocessed (exactly once)
        let mut again = Vec::new();
        ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                again.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn a_processing_error_halts_the_cursor_and_resume_repeats_nothing() {
        let (_dir, mut ledger) = temp_ledger();
        for i in 1..=3 {
            ledger
                .append(created(&format!("gc-{i}"), serde_json::json!({"title": "t"})))
                .unwrap();
        }
        let result = ledger.process_past_cursor("campd", &mut |_conn, event| {
            if event.seq == 2 {
                return Err(CoreError::Corrupt("injected".to_owned()));
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(ledger.cursor("campd").unwrap(), 1, "cursor halts before the failure");

        // resume with a healthy processor: exactly the unprocessed tail
        let mut tail = Vec::new();
        ledger
            .process_past_cursor("campd", &mut |_conn, event| {
                tail.push(event.seq);
                Ok(())
            })
            .unwrap();
        assert_eq!(tail, vec![2, 3]);
    }

    #[test]
    fn processor_effects_commit_atomically_with_the_cursor() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        // The processor writes a marker row through the transaction's
        // connection, then fails on seq 2: seq 1's effect+cursor committed,
        // seq 2's effect rolled back with its cursor advance.
        let result = ledger.process_past_cursor("campd", &mut |conn, event| {
            conn.execute(
                "INSERT INTO cursors (name, seq) VALUES ('marker', ?1)
                 ON CONFLICT(name) DO UPDATE SET seq = excluded.seq",
                [event.seq],
            )?;
            if event.seq == 2 {
                return Err(CoreError::Corrupt("injected".to_owned()));
            }
            Ok(())
        });
        assert!(result.is_err());
        let marker: i64 = ledger
            .conn
            .query_row("SELECT seq FROM cursors WHERE name = 'marker'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(marker, 1, "seq 2's effect must roll back with the cursor");
        assert_eq!(ledger.cursor("campd").unwrap(), 1);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp-core cursor`
Expected: FAIL to compile — no method `cursor` on `Ledger`.

- [ ] **Step 3: Implement** — in `ledger/mod.rs`, inside `impl Ledger` (after `events_for_bead`):

```rust
    /// The named consumer cursor's position; 0 when the consumer has never
    /// processed anything (spec §7.2: campd "catches up from its
    /// processed-cursor on start"). `cursors` is consumer bookkeeping —
    /// deliberately outside refold.
    pub fn cursor(&self, name: &str) -> Result<Seq, CoreError> {
        use rusqlite::OptionalExtension;
        let seq: Option<Seq> = self
            .conn
            .query_row("SELECT seq FROM cursors WHERE name = ?1", [name], |r| r.get(0))
            .optional()?;
        Ok(seq.unwrap_or(0))
    }

    /// Process every event past the named cursor, exactly once (spec §7.3).
    ///
    /// Each event runs in its own `BEGIN IMMEDIATE` transaction that executes
    /// `process` and advances the cursor together: a crash or a `process`
    /// error never loses an event and never replays one. `process` receives
    /// the transaction's connection, so any writes it makes commit atomically
    /// with the cursor advance. On error the cursor stays on the last
    /// successfully processed event and the error surfaces to the caller.
    /// Returns the cursor position after the run.
    pub fn process_past_cursor(
        &mut self,
        name: &str,
        process: &mut dyn FnMut(&Connection, &Event) -> Result<(), CoreError>,
    ) -> Result<Seq, CoreError> {
        let mut cursor = self.cursor(name)?;
        let pending = self.events_range(cursor + 1, None)?;
        for event in pending {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            process(&tx, &event)?;
            tx.execute(
                "INSERT INTO cursors (name, seq) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET seq = excluded.seq",
                params![name, event.seq],
            )?;
            tx.commit()?;
            cursor = event.seq;
        }
        Ok(cursor)
    }
```

Add `Connection` to the existing `use rusqlite::…` line if not already imported (it is: `rusqlite::{Connection, TransactionBehavior, params}` — verify).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp-core cursor && cargo test --package camp-core processor_effects`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/ledger/mod.rs
git commit -m "feat: named ledger cursors with transactional exactly-once processing"
```

### Task 7.3: camp-core — `status_summary`

**Files:**
- Modify: `crates/camp-core/src/ledger/mod.rs`
- Test: same file, `tests` module

**Interfaces:**
- Consumes: `readiness::ready_beads` (Phase 3), `sessions`/`beads` state tables.
- Produces (exact, used by Tasks 7.4/7.6/7.7):

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StatusSummary {
    pub live_sessions: Vec<String>,
    pub ready: u64,
    pub open: u64,
}
impl Ledger { pub fn status_summary(&self) -> Result<StatusSummary, CoreError>; }
```

- [ ] **Step 1: Write the failing test** — in `ledger/mod.rs`'s `tests` module:

```rust
    #[test]
    fn status_summary_reports_live_sessions_ready_and_open() {
        let (_dir, mut ledger) = temp_ledger();
        // empty camp: all zeroes
        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary { live_sessions: vec![], ready: 0, open: 0 }
        );

        // gc-1 ready; gc-2 open but blocked on gc-1
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two", "needs": ["gc-1"]})))
            .unwrap();
        // one live session, one stopped
        ledger.append(woke("camp/dev/1")).unwrap();
        ledger
            .append(input(
                EventType::SessionWoke,
                Some("gc"),
                None,
                serde_json::json!({"name": "camp/dev/2", "agent": "dev"}),
            ))
            .unwrap();
        ledger
            .append(input(
                EventType::SessionStopped,
                Some("gc"),
                None,
                serde_json::json!({"name": "camp/dev/2"}),
            ))
            .unwrap();

        assert_eq!(
            ledger.status_summary().unwrap(),
            StatusSummary {
                live_sessions: vec!["camp/dev/1".to_owned()],
                ready: 1,
                open: 2
            }
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp-core status_summary`
Expected: FAIL to compile — no `StatusSummary`.

- [ ] **Step 3: Implement** — in `ledger/mod.rs`. Above `impl Ledger`:

```rust
/// One `{"op":"status"}` snapshot (master plan Phase 7 protocol): computed
/// from the state tables at request time — no cached copy to drift.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StatusSummary {
    pub live_sessions: Vec<String>,
    pub ready: u64,
    pub open: u64,
}
```

Inside `impl Ledger`:

```rust
    /// Live session names, ready-bead count, and open-bead count. `open`
    /// counts `status='open'` beads (blocked ones included; claimed and
    /// closed ones not).
    pub fn status_summary(&self) -> Result<StatusSummary, CoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sessions WHERE status = 'live' ORDER BY name")?;
        let live_sessions: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let ready = crate::readiness::ready_beads(&self.conn, None)?.len() as u64;
        let open: u64 = self
            .conn
            .query_row("SELECT count(*) FROM beads WHERE status = 'open'", [], |r| r.get(0))?;
        Ok(StatusSummary { live_sessions, ready, open })
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp-core status_summary`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/ledger/mod.rs
git commit -m "feat: status_summary query for the campd status op"
```

### Task 7.4: camp — socket protocol, liveness bind, client

**Files:**
- Modify: `crates/camp/Cargo.toml`, `crates/camp/src/campdir.rs`, `crates/camp/src/main.rs` (module declaration only)
- Create: `crates/camp/src/daemon/mod.rs` (skeleton: module declarations only in this task), `crates/camp/src/daemon/socket.rs`
- Test: `socket.rs` `#[cfg(test)]` module

**Interfaces:**
- Consumes: `camp_core::ledger::StatusSummary` (Task 7.3), `camp_core::Seq`.
- Produces (exact, used by Tasks 7.5–7.8): `Request`, `Response`, `bind_or_replace(path) -> Result<std::os::unix::net::UnixListener>`, `request(path, &Request) -> Result<Response>`, `poke_best_effort(path, seq)`, `CampDir::socket_path()`, `CampDir::log_path()`.

- [ ] **Step 1: Add dependencies**

```bash
cargo add --package camp mio --features os-poll,net
cargo add --package camp serde --features derive
cargo remove --package camp --dev rusqlite
cargo add --package camp rusqlite
```

(rusqlite moves from dev-dependency to dependency: `daemon/cursor.rs` names `rusqlite::Connection` in the `EventProcessor` signature and the integration tests read `cursors` rows. Workspace resolution keeps it at 0.40.x, identical to camp-core's.)

- [ ] **Step 2: Write the failing tests** — create `crates/camp/src/daemon/mod.rs` with just:

```rust
//! campd: the only standing process (spec §5). Crash-only: no exclusive
//! state, `kill -9` is a supported shutdown method.

pub mod socket;
```

and register it in `crates/camp/src/main.rs` (below `mod campdir;`):

```rust
mod daemon;
```

Add to `crates/camp/src/campdir.rs`, inside `impl CampDir` after `config_path`:

```rust
    /// The daemon socket (spec §5: liveness IS this socket accepting).
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("campd.sock")
    }

    /// Where a detached campd's stderr lands (never silenced, never hidden).
    pub fn log_path(&self) -> PathBuf {
        self.root.join("campd.log")
    }
```

Create `crates/camp/src/daemon/socket.rs` containing ONLY the test module for now:

```rust
//! The campd socket protocol (master plan Phase 7, pinned): newline-delimited
//! JSON over `<camp>/campd.sock`. Liveness IS the socket (spec §5): alive
//! means it accepts; a stale file that refuses connections is unlinked and
//! rebound; a live listener makes a second daemon refuse to start.

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
        assert_eq!(serde_json::to_string(&Request::Status).unwrap(), r#"{"op":"status"}"#);
        assert_eq!(serde_json::to_string(&Request::Stop).unwrap(), r#"{"op":"stop"}"#);
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"poke","seq":412}"#).unwrap(),
            Request::Poke { seq: 412 }
        );
    }

    #[test]
    fn unknown_op_is_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"dance"}"#).is_err());
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
            campd_pid: 4242,
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"ok":true,"live_sessions":["camp/dev/1"],"ready":1,"open":2,"campd_pid":4242}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Error { ok: false, error: "bad request".to_owned() })
                .unwrap(),
            r#"{"ok":false,"error":"bad request"}"#
        );
        // client-side parse resolves the right variants
        assert!(matches!(
            serde_json::from_str::<Response>(r#"{"ok":true}"#).unwrap(),
            Response::Ok { ok: true }
        ));
        assert!(matches!(
            serde_json::from_str::<Response>(
                r#"{"ok":true,"live_sessions":[],"ready":0,"open":0,"campd_pid":1}"#
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        let listener = bind_or_replace(&path).unwrap();
        drop(listener);
        assert!(path.exists());
    }

    #[test]
    fn stale_socket_is_unlinked_and_rebound() {
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("campd.sock");
        let _keep = bind_or_replace(&path).unwrap();
        let err = bind_or_replace(&path).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "error was: {err:#}"
        );
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --package camp socket`
Expected: FAIL to compile — `Request`, `Response`, `bind_or_replace` undefined.

- [ ] **Step 4: Implement** — prepend to `socket.rs` (above the test module):

```rust
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use camp_core::Seq;
use camp_core::ledger::StatusSummary;
use serde::{Deserialize, Serialize};

/// One request line. Internally tagged on `op`; an unknown op is a parse
/// error (there is no wildcard arm to hide behind).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Post-commit poke (spec §7.2). `seq` is advisory: catch-up processes
    /// everything past the cursor regardless.
    Poke { seq: Seq },
    Status,
    Stop,
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
        campd_pid: u32,
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
pub fn bind_or_replace(path: &Path) -> Result<UnixListener> {
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

/// Send one request, read one response line. The timeouts bound a single
/// CLI operation against a wedged daemon (decision I) — they are not
/// wakeups; the daemon's own poll timeout stays None.
pub fn request(path: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("connecting to campd at {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut response_line = String::new();
    BufReader::new(stream).read_line(&mut response_line)?;
    if response_line.is_empty() {
        bail!("campd closed the connection without responding");
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
pub fn poke_best_effort(path: &Path, seq: Seq) {
    let _ = request(path, &Request::Poke { seq });
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp socket`
Expected: all 6 PASS. Note: `cargo build` may show dead-code warnings until Tasks 7.6/7.7 wire these functions in — expected mid-phase; the Task 7.9 clippy gate runs `--all-targets`, which counts test usage, and by then every item has a production caller.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/Cargo.toml Cargo.lock crates/camp/src/campdir.rs crates/camp/src/main.rs crates/camp/src/daemon/
git commit -m "feat: campd socket protocol, liveness bind rules, and client"
```

### Task 7.5: camp — `EventProcessor`, `ReadinessProcessor`, `catch_up`

**Files:**
- Create: `crates/camp/src/daemon/cursor.rs`
- Modify: `crates/camp/src/daemon/mod.rs` (add `pub mod cursor;`)
- Test: `cursor.rs` `#[cfg(test)]` module

**Interfaces:**
- Consumes: `Ledger::{cursor, process_past_cursor}` (Task 7.2), `camp_core::readiness::newly_ready` (Phase 3).
- Produces (exact, used by Tasks 7.6/7.8 and Phase 8):

```rust
pub const CAMPD_CURSOR: &str = "campd";
pub trait EventProcessor {
    fn process(&mut self, conn: &rusqlite::Connection, event: &Event) -> Result<(), CoreError>;
}
#[derive(Default)] pub struct ReadinessProcessor { /* pending: Vec<String> */ }
impl ReadinessProcessor { pub fn take_pending(&mut self) -> Vec<String>; }
pub fn catch_up(ledger: &mut Ledger, processor: &mut dyn EventProcessor) -> Result<Seq, CoreError>;
```

- [ ] **Step 1: Write the failing tests** — create `cursor.rs` with the test module (implementation lands in Step 3):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::event::{EventInput, EventType};
    use camp_core::ledger::Ledger;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        (dir, ledger)
    }

    fn create(l: &mut Ledger, id: &str, needs: &[&str]) {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"title": id, "needs": needs}),
        })
        .unwrap();
    }

    fn close(l: &mut Ledger, id: &str, outcome: &str) {
        l.append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"outcome": outcome}),
        })
        .unwrap();
    }

    #[test]
    fn catch_up_records_newly_ready_beads_from_pass_closes() {
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "pass");

        let mut processor = ReadinessProcessor::default();
        let end = catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(end, 3);
        assert_eq!(l.cursor(CAMPD_CURSOR).unwrap(), 3);
        assert_eq!(processor.take_pending(), vec!["gc-2".to_owned()]);
        // take_pending drains
        assert!(processor.take_pending().is_empty());
    }

    #[test]
    fn a_fail_close_unblocks_nothing() {
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "fail");

        let mut processor = ReadinessProcessor::default();
        catch_up(&mut l, &mut processor).unwrap();
        assert!(processor.take_pending().is_empty());
    }

    #[test]
    fn catch_up_is_exactly_once_across_calls() {
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "pass");

        let mut processor = ReadinessProcessor::default();
        catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(processor.take_pending(), vec!["gc-2".to_owned()]);

        // a second catch-up with no new events reprocesses nothing
        catch_up(&mut l, &mut processor).unwrap();
        assert!(processor.take_pending().is_empty());

        // new events after the cursor are picked up from there only
        create(&mut l, "gc-3", &[]);
        let end = catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(end, 4);
        assert!(processor.take_pending().is_empty(), "a create is not a close");
    }

    /// Phase 8's dispatch appends events *while processing* (e.g.
    /// session.woke); catch_up must drain to a fixpoint in one call.
    #[test]
    fn catch_up_drains_events_appended_during_processing() {
        #[derive(Default)]
        struct Recorder {
            seen: Vec<i64>,
        }
        impl EventProcessor for Recorder {
            fn process(
                &mut self,
                _conn: &rusqlite::Connection,
                event: &camp_core::event::Event,
            ) -> Result<(), camp_core::error::CoreError> {
                self.seen.push(event.seq);
                Ok(())
            }
        }
        let (_dir, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        let mut processor = Recorder::default();
        catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(processor.seen, vec![1]);
        // an append landing after the first fixpoint is drained by the next
        // catch_up from the cursor onward — nothing skipped, nothing repeated
        create(&mut l, "gc-2", &[]);
        catch_up(&mut l, &mut processor).unwrap();
        assert_eq!(processor.seen, vec![1, 2]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp cursor` (add `pub mod cursor;` to `daemon/mod.rs` first)
Expected: FAIL to compile — `ReadinessProcessor`, `catch_up`, `CAMPD_CURSOR` undefined.

- [ ] **Step 3: Implement** — prepend to `cursor.rs`:

```rust
//! campd's exactly-once event consumption (spec §7.3): the `cursors` row
//! 'campd' marks the last processed seq; catch-up replays everything past
//! it through an `EventProcessor`, and the cursor advances in the same
//! transaction as the processor's ledger effects (Task 7.2 mechanism).

use camp_core::Seq;
use camp_core::error::CoreError;
use camp_core::event::{Event, EventType};
use camp_core::ledger::Ledger;
use rusqlite::Connection;

/// campd's row in the `cursors` table.
pub const CAMPD_CURSOR: &str = "campd";

/// What campd runs over each committed event, in seq order. Ledger writes
/// must go through `conn` — the open cursor transaction — so they commit
/// atomically with the cursor advance. Phase 8 plugs dispatch in here.
pub trait EventProcessor {
    fn process(&mut self, conn: &Connection, event: &Event) -> Result<(), CoreError>;
}

/// Phase 7's processor: readiness bookkeeping only (spec §7.3 — recompute
/// the affected subgraph on each close). Phase 8's dispatcher consumes
/// `take_pending`; until then the list is the observable proof that the
/// recompute runs on the processing path.
#[derive(Default)]
pub struct ReadinessProcessor {
    pending: Vec<String>,
}

impl ReadinessProcessor {
    /// Drain the beads made ready by processed closes, in processing order.
    pub fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }
}

impl EventProcessor for ReadinessProcessor {
    fn process(&mut self, conn: &Connection, event: &Event) -> Result<(), CoreError> {
        if event.kind == EventType::BeadClosed {
            let bead = event
                .bead
                .as_deref()
                .ok_or_else(|| CoreError::InvalidEventData {
                    event_type: event.kind.as_str().to_owned(),
                    reason: "bead.closed event without a bead id".to_owned(),
                })?;
            self.pending
                .extend(camp_core::readiness::newly_ready(conn, bead)?);
        }
        Ok(())
    }
}

/// Process everything past campd's cursor, to a fixpoint: a processor may
/// itself append events (Phase 8 dispatch); re-checking until the cursor
/// stops moving drains those too. Bounded by the backlog — convergence,
/// not polling. Returns the final cursor position.
pub fn catch_up(
    ledger: &mut Ledger,
    processor: &mut dyn EventProcessor,
) -> Result<Seq, CoreError> {
    loop {
        let before = ledger.cursor(CAMPD_CURSOR)?;
        let after = ledger
            .process_past_cursor(CAMPD_CURSOR, &mut |conn, event| processor.process(conn, event))?;
        if after == before {
            return Ok(after);
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp cursor`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/cursor.rs crates/camp/src/daemon/mod.rs
git commit -m "feat: campd cursor catch-up through an EventProcessor (readiness bookkeeping)"
```

### Task 7.6: camp — the event loop and `daemon::run`

**Files:**
- Create: `crates/camp/src/daemon/event_loop.rs`
- Modify: `crates/camp/src/daemon/mod.rs`
- Test: `daemon/mod.rs` `#[cfg(test)]` module (in-process daemon over a real socket)

**Interfaces:**
- Consumes: `socket::{Request, Response, bind_or_replace}` (7.4), `cursor::{catch_up, EventProcessor, ReadinessProcessor}` (7.5), `Ledger::{append, status_summary}`, `CampDir::{db_path, socket_path}`.
- Produces: `daemon::run(camp: &CampDir) -> anyhow::Result<()>` (Task 7.7 wires it to `camp daemon`/`campd`); `daemon::READY_PREFIX` (Task 7.7's autostart and 7.8's tests match on it); `event_loop::poll_timeout()` — the Phase 10/11 plug point.

- [ ] **Step 1: Write the failing test** — replace `daemon/mod.rs` with:

```rust
//! campd: the only standing process (spec §5). Crash-only: no exclusive
//! state, `kill -9` is a supported shutdown method; on start it opens the
//! ledger, appends campd.started, catches up past its cursor, announces
//! readiness on stdout, and sleeps on the socket.

pub mod cursor;
pub mod event_loop;
pub mod socket;

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

pub fn run(camp: &CampDir) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let socket_path = camp.socket_path();
    let std_listener = socket::bind_or_replace(&socket_path)?;
    std_listener
        .set_nonblocking(true)
        .context("setting the listener non-blocking")?;
    let listener = mio::net::UnixListener::from_std(std_listener);

    ledger.append(EventInput {
        kind: EventType::CampdStarted,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({}),
    })?;

    // Startup catch-up is fatal on error: a daemon that cannot process its
    // backlog must not pretend to be up (fail fast).
    let mut processor = ReadinessProcessor::default();
    cursor::catch_up(&mut ledger, &mut processor)?;

    let mut stdout = std::io::stdout();
    writeln!(stdout, "{READY_PREFIX}{}", socket_path.display())
        .context("announcing readiness")?;
    stdout.flush().context("flushing the readiness line")?;

    event_loop::run(listener, &socket_path, &mut ledger, &mut processor)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write as _};
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
        assert_eq!(
            ledger.cursor(super::cursor::CAMPD_CURSOR).unwrap(),
            1,
            "startup catch-up covered campd.started; campd.stopped is seq 2, \
             appended after the final catch-up (the next start covers it)"
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp daemon_serves`
Expected: FAIL to compile — `event_loop` module missing.

- [ ] **Step 3: Implement** — create `crates/camp/src/daemon/event_loop.rs`:

```rust
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
                            conns.insert(token, Conn { stream, buf: Vec::new() });
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
                            // Durable truth first, then the goodbye
                            // (decision G): event → unlink → respond → exit.
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
                // surfaced, never skipped (decision H).
                let response = match cursor::catch_up(ledger, processor) {
                    Ok(_) => Response::Ok { ok: true },
                    Err(e) => {
                        eprintln!("campd: catch-up failed: {e}");
                        Response::Error { ok: false, error: format!("catch-up failed: {e}") }
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
                        Response::Error { ok: false, error: format!("status failed: {e}") }
                    }
                };
                respond(&mut conn.stream, &response)?;
            }
            Err(e) => {
                respond(
                    &mut conn.stream,
                    &Response::Error { ok: false, error: format!("bad request: {e}") },
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
    // reading — surfacing it drops that connection (decision J).
    stream.write_all(line.as_bytes()).context("writing the response")?;
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
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp daemon_serves`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/mod.rs crates/camp/src/daemon/event_loop.rs
git commit -m "feat: campd event loop — poke/status/stop, infinite poll when idle"
```

### Task 7.7: camp — CLI wiring: `campd`/`camp daemon`, `stop`, `top`, auto-start, pokes

**Files:**
- Create: `crates/camp/src/daemon/autostart.rs`, `crates/camp/src/cmd/stop.rs`, `crates/camp/src/cmd/top.rs`
- Modify: `crates/camp/src/main.rs`, `crates/camp/src/daemon/mod.rs` (add `pub mod autostart;`), `crates/camp/src/cmd/{create,claim,close,rig}.rs`
- Test: `top.rs` `#[cfg(test)]` render test (integration coverage lands in Task 7.8)

**Interfaces:**
- Consumes: `daemon::{run, READY_PREFIX}`, `socket::{request, poke_best_effort, Request, Response}`, `Ledger::append`, `CampDir::{socket_path, log_path}`.
- Produces: `camp daemon` / argv0-`campd` dispatch; `camp stop`; `camp top`; `autostart::request_with_autostart(camp, &Request, verb) -> Result<Response>` (Phase 8's `sling` reuses it).

- [ ] **Step 1: Write the failing render test** — create `crates/camp/src/cmd/top.rs`:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::ledger::StatusSummary;

    #[test]
    fn render_is_plain_text_and_stable() {
        let empty = StatusSummary { live_sessions: vec![], ready: 0, open: 0 };
        assert_eq!(
            render(&empty, 4242),
            "campd pid: 4242\nlive sessions: 0\nready: 0\nopen: 0\n"
        );
        let busy = StatusSummary {
            live_sessions: vec!["camp/dev/1".to_owned(), "camp/dev/2".to_owned()],
            ready: 1,
            open: 3,
        };
        assert_eq!(
            render(&busy, 7),
            "campd pid: 7\nlive sessions: 2 (camp/dev/1, camp/dev/2)\nready: 1\nopen: 3\n"
        );
    }
}
```

Register the new command modules in `crates/camp/src/main.rs`'s `mod cmd { … }` block (alphabetical):

```rust
    pub mod stop;
    pub mod top;
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp render_is_plain_text` (create an empty `cmd/stop.rs` so the module compiles: it gets content in Step 3)
Expected: FAIL to compile — `render` undefined.

- [ ] **Step 3: Implement.**

`crates/camp/src/cmd/top.rs` (above the tests):

```rust
use anyhow::{Result, bail};
use camp_core::ledger::StatusSummary;

use crate::campdir::CampDir;
use crate::daemon::autostart;
use crate::daemon::socket::{Request, Response};

/// `camp top`: ONE status query rendered as plain text — a query, not a
/// loop (spec §5); refresh is running it again. Auto-starts campd.
pub fn run(camp: &CampDir) -> Result<()> {
    let response = autostart::request_with_autostart(camp, &Request::Status, "top")?;
    let Response::Status { summary, campd_pid, .. } = response else {
        bail!("unexpected response to status: {response:?}");
    };
    print!("{}", render(&summary, campd_pid));
    Ok(())
}

fn render(summary: &StatusSummary, campd_pid: u32) -> String {
    let sessions = if summary.live_sessions.is_empty() {
        "0".to_owned()
    } else {
        format!(
            "{} ({})",
            summary.live_sessions.len(),
            summary.live_sessions.join(", ")
        )
    };
    format!(
        "campd pid: {campd_pid}\nlive sessions: {sessions}\nready: {}\nopen: {}\n",
        summary.ready, summary.open
    )
}
```

`crates/camp/src/cmd/stop.rs`:

```rust
use anyhow::{Context, Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp stop`: graceful daemon shutdown over the socket. Never
/// auto-starts (stopping nothing is an error, not a no-op).
pub fn run(camp: &CampDir) -> Result<()> {
    let response = socket::request(&camp.socket_path(), &Request::Stop)
        .context("campd is not running")?;
    match response {
        Response::Ok { .. } => {
            println!("campd stopped");
            Ok(())
        }
        other => bail!("unexpected response to stop: {other:?}"),
    }
}
```

`crates/camp/src/daemon/autostart.rs` (and add `pub mod autostart;` to `daemon/mod.rs`):

```rust
//! Auto-start (spec §5): a verb that needs the daemon connects; on failure
//! it records campd.autostarted (the trail carries the cause, spec §13.3),
//! spawns `camp daemon` detached, blocks on the daemon's readiness line —
//! an OS pipe read, not a sleep/retry loop — and retries the request
//! exactly ONCE. Fail fast after that.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use super::READY_PREFIX;
use super::socket::{self, Request, Response};
use crate::campdir::CampDir;

pub fn request_with_autostart(
    camp: &CampDir,
    request: &Request,
    verb: &str,
) -> Result<Response> {
    let sock = camp.socket_path();
    // Probe first: only an unreachable socket triggers auto-start; a live
    // daemon's protocol errors surface as themselves.
    if UnixStream::connect(&sock).is_ok() {
        return socket::request(&sock, request);
    }
    start_detached(camp, verb)?;
    socket::request(&sock, request).with_context(|| {
        format!(
            "campd did not come up after auto-start; see {}",
            camp.log_path().display()
        )
    })
}

// The daemon is detached BY DESIGN (spec §5): it must outlive this CLI
// process, which exits immediately; init reaps it. Never waited on.
#[allow(clippy::zombie_processes)]
fn start_detached(camp: &CampDir, verb: &str) -> Result<()> {
    // Cause before effect (spec §13.3): the trail reads
    // campd.autostarted → campd.started.
    let mut ledger = Ledger::open(&camp.db_path())?;
    ledger.append(EventInput {
        kind: EventType::CampdAutostarted,
        rig: None,
        actor: "cli".into(),
        bead: None,
        data: serde_json::json!({ "verb": verb }),
    })?;
    drop(ledger);

    let exe = std::env::current_exe().context("locating the camp binary")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(camp.log_path())
        .with_context(|| format!("opening {}", camp.log_path().display()))?;
    use std::os::unix::process::CommandExt as _;
    let mut child = Command::new(exe)
        .arg("daemon")
        .arg("--camp")
        .arg(&camp.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .process_group(0) // its own group: detached from the CLI's terminal
        .spawn()
        .context("spawning camp daemon")?;

    // Block on the readiness line. EOF without it = the daemon failed and
    // its stderr is in campd.log.
    let stdout = child.stdout.take().context("daemon stdout unavailable")?;
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .context("reading campd's readiness line")?;
    if !line.starts_with(READY_PREFIX) {
        bail!(
            "campd failed to start (no readiness line); see {}",
            camp.log_path().display()
        );
    }
    Ok(())
}
```

`crates/camp/src/main.rs` — four edits:

(a) imports near the top:

```rust
use std::ffi::OsStr;
use std::path::Path;
```

(Adjust: `PathBuf` is already imported; add `Path` and `OsStr`.)

(b) new subcommands in `enum Command` (after `Show`):

```rust
    /// Run the daemon in the foreground (also reachable via a campd symlink)
    Daemon,
    /// Stop the running daemon gracefully
    Stop,
    /// One campd status snapshot as plain text (auto-starts the daemon)
    Top,
```

(c) replace `fn main` with argv0 dispatch (decision 2: one binary, `campd` symlink):

```rust
#[derive(Parser)]
#[command(
    name = "campd",
    version,
    about = "Gas Camp daemon (the camp binary in daemon mode)"
)]
struct CampdCli {
    /// Camp directory (default: $CAMP_DIR, else walk up from cwd for .camp/)
    #[arg(long, value_name = "DIR")]
    camp: Option<PathBuf>,
}

fn main() -> ExitCode {
    if invoked_as_campd() {
        let cli = CampdCli::parse();
        return report("campd", run_daemon(cli.camp.as_deref()));
    }
    let cli = Cli::parse();
    report("camp", run(cli))
}

fn invoked_as_campd() -> bool {
    std::env::args_os()
        .next()
        .is_some_and(|arg0| Path::new(&arg0).file_stem() == Some(OsStr::new("campd")))
}

fn report(name: &str, result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_daemon(camp_flag: Option<&Path>) -> anyhow::Result<()> {
    let camp = CampDir::resolve(camp_flag)?;
    daemon::run(&camp)
}
```

(d) new arms in `fn run`'s match (after `Command::Show`):

```rust
        Command::Daemon => run_daemon(cli.camp.as_deref()),
        Command::Stop => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::stop::run(&camp)
        }
        Command::Top => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::top::run(&camp)
        }
```

Post-commit pokes (decision E) — one pattern, four files. In `crates/camp/src/cmd/create.rs` change the append + print tail to:

```rust
    let seq = ledger.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig_cfg.name.clone()),
        actor: "cli".into(),
        bead: Some(id.clone()),
        data,
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    println!("{id}");
```

In `crates/camp/src/cmd/claim.rs`:

```rust
    let seq = ledger.append(EventInput {
        kind: EventType::BeadClaimed,
        rig: None,
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data: serde_json::json!({ "session": session }),
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    println!("claimed {bead}");
```

In `crates/camp/src/cmd/close.rs`:

```rust
    let seq = ledger.append(EventInput {
        kind: EventType::BeadClosed,
        rig: None,
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data,
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    println!("closed {bead} ({outcome})");
```

In `crates/camp/src/cmd/rig.rs` (the `add` function):

```rust
    let seq = ledger.append(EventInput {
        kind: EventType::RigAdded,
        rig: Some(name.clone()),
        actor: "cli".into(),
        bead: None,
        data: serde_json::json!({ "path": abs, "prefix": prefix }),
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
```

- [ ] **Step 4: Run the render test and the whole existing suite**

Run: `cargo test --package camp`
Expected: `render_is_plain_text_and_stable` PASSES, and every pre-existing `cli_*` test stays green — they run daemon-less, which proves the poke is genuinely best-effort (a regression here means the poke broke a write path).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/main.rs crates/camp/src/daemon/ crates/camp/src/cmd/
git commit -m "feat: campd invocation, camp stop/top, auto-start, post-commit pokes"
```

### Task 7.8: integration — `daemon_lifecycle.rs`

**Files:**
- Create: `crates/camp/tests/daemon_lifecycle.rs`

**Interfaces:**
- Consumes: the `camp` binary (`env!("CARGO_BIN_EXE_camp")`), the pinned wire protocol, the `cursors`/`events` tables via rusqlite.

- [ ] **Step 1: Write the tests** (they must FAIL only if the implementation is wrong — everything they exercise exists after 7.7; watch each fail by reverting nothing, just run them and fix any defect they expose):

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 7 integration: campd lifecycle against the real binary (master
//! plan Phase 7 test obligations; spec §5, §13.3).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

fn camp_cmd(root: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_remove("CAMP_DIR").arg("--camp").arg(root);
    cmd
}

fn run_ok(root: &Path, args: &[&str]) -> String {
    let out = camp_cmd(root).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "camp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// camp init + one rig; returns the camp root (<tempdir>/.camp).
fn init_camp(dir: &Path) -> PathBuf {
    let status = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir)
        .arg("init")
        .status()
        .unwrap();
    assert!(status.success());
    let root = dir.join(".camp");
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let out = camp_cmd(&root)
        .args(["rig", "add"])
        .arg(&rig)
        .args(["--prefix", "gc", "--name", "gc"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    root
}

fn request(sock: &Path, line: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(sock).expect("connect to campd");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    stream.write_all(line.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp).unwrap();
    serde_json::from_str(resp.trim_end()).expect("campd response is JSON")
}

fn campd_cursor(root: &Path) -> i64 {
    let conn = rusqlite::Connection::open(root.join("camp.db")).unwrap();
    conn.query_row("SELECT seq FROM cursors WHERE name = 'campd'", [], |r| r.get(0))
        .unwrap()
}

fn max_seq(root: &Path) -> i64 {
    let conn = rusqlite::Connection::open(root.join("camp.db")).unwrap();
    conn.query_row("SELECT coalesce(max(seq), 0) FROM events", [], |r| r.get(0))
        .unwrap()
}

fn event_types(root: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(root.join("camp.db")).unwrap();
    let mut stmt = conn.prepare("SELECT type FROM events ORDER BY seq").unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

/// A foreground daemon child. Spawn blocks until the readiness line
/// (deterministic — no connect polling); Drop SIGKILLs and reaps.
struct Daemon {
    child: Child,
    sock: PathBuf,
}

impl Daemon {
    fn spawn(root: &Path) -> Daemon {
        let mut child = Command::new(BIN)
            .env_remove("CAMP_DIR")
            .args(["daemon", "--camp"])
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon { child, sock: root.join("campd.sock") }
    }

    fn request(&self, line: &str) -> serde_json::Value {
        request(&self.sock, line)
    }

    fn kill_dash_nine(&mut self) {
        self.child.kill().unwrap(); // SIGKILL on unix
        self.child.wait().unwrap();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Cleans up an auto-started (detached) daemon even when a test fails.
struct StopGuard {
    sock: PathBuf,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        if let Ok(mut stream) = UnixStream::connect(&self.sock) {
            let _ = stream.write_all(b"{\"op\":\"stop\"}\n");
            let mut resp = String::new();
            let _ = BufReader::new(stream).read_line(&mut resp);
        }
    }
}

#[test]
fn start_socket_accepts_and_status_is_sane() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    // seed: gc-1 ready; gc-2 open but blocked on gc-1
    run_ok(&root, &["create", "first"]);
    run_ok(&root, &["create", "second", "--needs", "gc-1"]);

    let daemon = Daemon::spawn(&root);
    let status = daemon.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    assert_eq!(status["campd_pid"], daemon.child.id());
    assert_eq!(status["ready"], 1);
    assert_eq!(status["open"], 2);
    assert_eq!(status["live_sessions"], serde_json::json!([]));

    assert!(event_types(&root).contains(&"campd.started".to_owned()));
    assert_eq!(campd_cursor(&root), max_seq(&root), "startup catch-up complete");
}

#[test]
fn a_cli_write_pokes_campd_and_the_cursor_advances() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _daemon = Daemon::spawn(&root);

    // create pokes synchronously before it exits, so this is deterministic
    run_ok(&root, &["create", "poked"]);
    assert_eq!(campd_cursor(&root), max_seq(&root));

    // the readiness recompute path runs on close, live
    run_ok(&root, &["close", "gc-1", "--outcome", "pass"]);
    assert_eq!(campd_cursor(&root), max_seq(&root));
}

#[test]
fn kill_dash_nine_stale_socket_restart_and_exactly_once_catch_up() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let mut daemon = Daemon::spawn(&root);
    daemon.kill_dash_nine();

    let sock = root.join("campd.sock");
    assert!(sock.exists(), "SIGKILL leaves the socket file behind (stale)");
    assert!(UnixStream::connect(&sock).is_err(), "stale socket refuses connections");

    // the ledger keeps accepting writes while campd is dead (poke is
    // fire-and-forget; spec §7.2)
    run_ok(&root, &["create", "while dead"]);
    run_ok(&root, &["create", "also while dead", "--needs", "gc-1"]);
    run_ok(&root, &["close", "gc-1", "--outcome", "pass"]);
    let lagging = campd_cursor(&root);
    assert!(lagging < max_seq(&root), "no live campd: cursor must lag");

    // restart: the stale socket is unlinked and rebound; catch-up processes
    // the backlog exactly once (the transactional guarantee is unit-tested;
    // here the cursor lands exactly on the head and status agrees)
    let daemon2 = Daemon::spawn(&root);
    assert_eq!(campd_cursor(&root), max_seq(&root));
    let status = daemon2.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    assert_eq!(status["ready"], 1, "gc-2 was unblocked by gc-1's pass close");
    assert_eq!(status["open"], 1);
}

#[test]
fn camp_stop_is_graceful() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let mut daemon = Daemon::spawn(&root);

    let out = run_ok(&root, &["stop"]);
    assert_eq!(out, "campd stopped\n");
    let status = daemon.child.wait().unwrap();
    assert!(status.success(), "graceful stop exits 0");
    assert!(!root.join("campd.sock").exists(), "stop unlinks the socket");
    assert!(event_types(&root).contains(&"campd.stopped".to_owned()));
}

#[test]
fn stop_errors_when_campd_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let out = camp_cmd(&root).arg("stop").output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("campd is not running"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn second_daemon_refuses_to_start_while_the_first_lives() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let daemon = Daemon::spawn(&root);

    let out = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["daemon", "--camp"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(!out.status.success(), "second daemon must refuse to start");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already running"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // the first daemon is unharmed
    let status = daemon.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
}

#[test]
fn camp_top_autostarts_campd_with_the_event_trail() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let _guard = StopGuard { sock: root.join("campd.sock") };

    let out = run_ok(&root, &["top"]);
    assert!(out.contains("campd pid: "), "top output: {out:?}");
    assert!(out.contains("ready: 0"), "top output: {out:?}");
    assert!(out.contains("open: 0"), "top output: {out:?}");

    // spec §13.3: the trail shows the cause — autostarted (by the cli, for
    // top) then started (by campd)
    let types = event_types(&root);
    let auto = types.iter().position(|t| t == "campd.autostarted").expect("autostarted");
    let started = types.iter().position(|t| t == "campd.started").expect("started");
    assert!(auto < started, "trail must read autostarted → started: {types:?}");
    let events_json = run_ok(&root, &["events", "--json"]);
    assert!(
        events_json.contains(r#""type":"campd.autostarted","actor":"cli","data":{"verb":"top"}"#),
        "events: {events_json}"
    );

    // a second top finds the daemon up: no second autostart
    run_ok(&root, &["top"]);
    let autostarts = event_types(&root)
        .iter()
        .filter(|t| t.as_str() == "campd.autostarted")
        .count();
    assert_eq!(autostarts, 1);

    // graceful shutdown of the detached daemon
    run_ok(&root, &["stop"]);
}

#[test]
fn campd_symlink_runs_daemon_mode() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let link = dir.path().join("campd");
    std::os::unix::fs::symlink(BIN, &link).unwrap();

    let mut child = Command::new(&link)
        .env_remove("CAMP_DIR")
        .args(["--camp"])
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    assert!(
        line.starts_with(READY_PREFIX),
        "campd symlink did not enter daemon mode: {line:?}"
    );

    let mut daemon = Daemon { child, sock: root.join("campd.sock") };
    let status = daemon.request(r#"{"op":"status"}"#);
    assert_eq!(status["ok"], true);
    daemon.kill_dash_nine();
}
```

- [ ] **Step 2: Run**

Run: `cargo test --package camp --test daemon_lifecycle`
Expected: all 8 PASS. Any failure is a real defect in 7.4–7.7 — debug the product code, never weaken the test.

- [ ] **Step 3: Run the whole workspace**

Run: `cargo test --workspace`
Expected: PASS (including the untouched refold property and vocab pin suites).

- [ ] **Step 4: Commit**

```bash
git add crates/camp/tests/daemon_lifecycle.rs
git commit -m "test: campd lifecycle — kill -9 recovery, stale socket, bind refusal, auto-start trail"
```

### Task 7.9: Phase gate — gates, push, PR, CI

- [ ] **Step 1: Gates**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean. Fix anything, rerun, only then continue.

- [ ] **Step 2: Idle-is-free evidence sweep** (exit criterion: "no timeout-driven code path anywhere")

Run: `grep -rn "poll_timeout\|Duration::" crates/camp/src/daemon/`
Expected: `Duration` appears ONLY in `socket.rs`'s client-side 5 s I/O timeouts (decision I — CLI safety, not daemon wakeups) and `poll_timeout()` returns a literal `None`. `grep -rn "sleep\|interval\|tick" crates/camp/src/` must return nothing (test helpers under `#[cfg(test)]` and `tests/` excepted). Record both grep outputs for the PR description.

- [ ] **Step 3: Rebase check** — if any sibling PR (phase-4/phase-5) merged since branching: `git fetch origin && git rebase origin/main`, resolve (expected surface: `event.rs`/`vocab.rs`/`fold.rs`/`main.rs`/`Cargo.toml`/`Cargo.lock`), re-run Step 1 in full.

- [ ] **Step 4: Push and open the PR**

```bash
git push -u origin phase-7-campd-skeleton
gh pr create --title "Phase 7: campd skeleton" --body "$(cat <<'EOF'
The only standing process, crash-only and event-driven (spec §5): socket
liveness, pinned poke/status/stop protocol, exactly-once cursor catch-up
through an EventProcessor, auto-start with a causal event trail, camp
stop/top.

Plan: docs/superpowers/plans/2026-07-06-phase-7-campd-skeleton.md
(decision A therein: mio instead of the polling crate — polling v3's
Poller::add is an unsafe fn, unsafe is forbidden; spec §15.1 sanctions mio).

Exit criteria evidence:
- kill -9 is a supported shutdown method, demonstrably:
  daemon_lifecycle::kill_dash_nine_stale_socket_restart_and_exactly_once_catch_up
- idle daemon blocks in poll with no timeout-driven code path: poll_timeout()
  is a literal None (event_loop.rs); grep sweeps in the PR checklist below.
- exactly-once: camp-core cursor unit tests (halt-on-error, atomic
  effects+cursor, no reprocessing) + integration cursor==max(seq).
EOF
)"
gh pr checks --watch
```

Expected: fmt, clippy, test (ubuntu), test (macos) all green. The phase is complete only when green and every claim above is verified.

---

## Exit-Criteria Traceability (master plan Phase 7)

| Contract line | Where proven |
|---|---|
| start → socket accepts → status sane | `daemon_lifecycle::start_socket_accepts_and_status_is_sane` |
| kill -9 → stale socket detected → restart → cursor caught up exactly once | `daemon_lifecycle::kill_dash_nine_…` (end-to-end) + camp-core cursor tests (transactional exactly-once) |
| stop graceful | `daemon_lifecycle::camp_stop_is_graceful` + in-process `daemon_serves_status_poke_and_stop_over_the_socket` |
| second-daemon bind refusal | `daemon_lifecycle::second_daemon_refuses_…` + `socket::live_socket_refuses_a_second_daemon` |
| auto-start path with event trail (spec §13.3) | `daemon_lifecycle::camp_top_autostarts_campd_with_the_event_trail` |
| pinned protocol | `socket::request_wire_format_is_pinned` / `response_wire_format_is_pinned` |
| poke → process → readiness bookkeeping | `cursor.rs` unit tests + `a_cli_write_pokes_campd_and_the_cursor_advances` |
| campd invocation (decision 2) | `daemon_lifecycle::campd_symlink_runs_daemon_mode` |
| idle blocks in poll, no timeout-driven path | `poll_timeout() -> None` + Task 7.9 grep evidence (0.0 % CPU number itself is Phase 13, per master plan) |

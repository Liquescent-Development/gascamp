# cp-3 — `can_use_tool` end to end, the permission flow — Implementation Plan

## Plan-gate approval
PENDING — round 1 REJECT (architecture ruled SOUND; 4 blocking findings, all test-design or wiring, fixed in this revision). A fresh 4-panel gate audits the revision. Do NOT begin implementation until this line reads APPROVED.

### Round-1 gate revisions (each verified against the code before fixing)
- **CP3-B1 (real):** `Response::Interrupt` is `{ok, request_id}` (socket.rs:182-185) in the `#[serde(untagged)]` enum — a bare `PermissionDecided {ok, request_id}` shadows it. FIXED (Task 7): `PermissionDecided { ok, request_id, decision }` — the `decision` key disambiguates untagged resolution; placed BEFORE `Interrupt`; a new wire-pin test proves BOTH round-trip to their own variant.
- **CP3-B2 (real, test-design):** the disarm and the `declare_stalls` skip mutually mask (verified against `declare_stalls`/`fire_due`). FIXED (Task 8): two INDEPENDENT unit tests — the disarm's invariant-1 guard (no armed timer after `permission.pending`) and the skip (a blocked session WITH an armed timer declares zero `agent.stalled`) — plus concretized heart-tests.
- **CP3-B3 (real, wiring):** verified `session_ended(…,"crashed")` reopens the bead UNCONDITIONALLY (`UPDATE beads SET status='open', claimed_by=NULL … WHERE claimed_by=<session> AND status='in_progress'`, fold.rs:1167), independent of reason and of patrol tracking; and `observe`'s re-hook needs `tracked.get(name)` Some + reason `starts_with("patrol restart")` (patrol.rs:308-321), which the untracked adopt arm never satisfies. FIXED (Task 11): the adoption kill mirrors the existing `"adopt: process not found"` append EXACTLY — key `"name"` (NOT `"session"`; `SessionEnd` is `deny_unknown_fields`, so `"session"` fails loud), `bead: None` — and the bead re-hook rides the FOLD crash-reopen; the `observe`/`reason_rehooks` change is DROPPED as inert; the test asserts the bead becomes dispatchable (`status='open'`, `claimed_by=NULL`).
- **CP3-B4 (real, coverage):** FIXED (Task 11): a test for the post-adoption-DISCOVERED pending taking the NAMED kill, not the stall ladder.
- **Non-blocking, folded in:** Task 13 moved to `tests/e2e.rs` under `CAMP_E2E`/`make e2e` (NOT `claude_compat.rs`, the $0 `make compat` tier); NoPipe inverse-window and re-arm are now numbered assertions; the pre-ladder double-fanout is acknowledged; the `control.rs:2121`/`:2149` test destructures need the new `tool_use_id` binding (compile-caught).

### Round-2 gate revision (one blocking finding — the ladder-drains-first heart-test could confirm-but-not-falsify)
- **CP3-R2-B1 (real).** Verified: `src/daemon/read_channel.rs:183` is the `StreamLine` struct, NOT a suppression helper — my round-1 note ("no-watch affordance confirmed at read_channel.rs:183-212") was FALSE and is retracted. The genuine suppression property is macOS-specific and lives in a DIFFERENT place: `tests/control.rs:183-186` measures that FSEvents delivers NO event for a worker's append through its **long-lived inherited stdout fd**, while a **fresh open+write+close by another process DOES fire one**. Two consequences the fix must honor: (1) a naive test-side `append().open().write()` (exactly what `tests/read_channel.rs`'s UNIT test does — which works only because it then calls `drain_all` DIRECTLY with no watcher running) FIRES notify in the integration harness → campd drains early → BLOCKED surfaces and the timer disarms BEFORE any `StallFire` pops → the test passes EVEN WITH the pre-ladder drain removed; (2) even genuine inherited-fd suppression is macOS-only — on Linux/CI, inotify delivers the append, so an INTEGRATION ladder-drains-first test cannot falsify the pre-ladder-drain removal there either. FIX (Task 8): the ladder-drains-first property is now pinned by a **platform-independent COMPONENT test** that drives the REAL event-loop ordering seam (an extracted `stall_step`) with NO watcher running — the unread line is genuinely unread by construction, and the test asserts a `StallFire` was popped AND BLOCKED surfaced AND zero `agent.stalled`; removing the pre-ladder drain makes `declare_stalls` see a not-blocked session and append `agent.stalled` (RED) on every platform. The fake-worker end-to-end is kept as a macOS-genuine confirm, no longer claimed as the falsifying guard.
- **Non-blocking (round 2), folded in:** the three disarm/skip/re-arm unit guards move INLINE to `patrol.rs`'s `#[cfg(test)] mod tests` (they seed the private `blocked` field and read `#[cfg(test)] is_armed`, both invisible to an external test) using `fixture()` (patrol.rs:1487); the adopt-arm pending kill gains the `row.woke_actor == "campd"` guard the sibling release-kill carries (patrol.rs:1200), aligning with §10 "never kill in the TUI"; the crash-reopen re-hook is noted as an implicit fold coupling, not a dedicated code path; `rearm` confirmed as the real method name (patrol.rs:734).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn "an unattended agent stalls forever on a permission it cannot get" into "the operator answers a question." A worker's `can_use_tool` request blocks, surfaces as `BLOCKED` in the fleet view, is answered over the socket, and the decision is a ledger event with its cause — while no `BLOCKED` worker is ever nudged, restarted, or killed by the stall ladder, and a worker campd can no longer answer is killed on adoption with a named, greppable cause and its bead re-hooked.

**Architecture:** cp-1 already built the whole wire seam — `WorkerMessage::CanUseTool` parses off the read channel (`control.rs:268`), the outbound `permission_allow`/`permission_deny` response bytes are pinned as fixtures (validated against the CLI validator string, `control.rs:2220`), and the `SessionInfo.blocked` wire bit exists on the fleet model awaiting its producer (`socket.rs:115`). cp-3 lights that seam up. The load-bearing decision: **`BLOCKED` is folded durable ledger state** — a new `permissions` table (schema v4), folded from two new events `permission.pending` and `permission.decided`. That single choice makes every §5.3/§9 property a literal, indexed query over ledger truth: `fleet_model`'s bit, the dispatch-slot exemption, the adoption reconciliation, and — because a `permission.decided` fold that `UPDATE … WHERE status='pending'` rolls back on zero rows changed — **first-answer-wins is enforced atomically by the ledger transaction itself** (§9: "ledger append order decides"). The read path replaces cp-1's `permission_unanswerable` fault with a `permission.pending` producer; the write path adds a `session.permission_decision` verb whose handler appends the decision to the ledger *before* writing the `control_response` to the pipe (§5.3.4's load-bearing ordering). Patrol gains a ledger-reconciled `blocked` set that disarms the stall timer on entry and exempts a blocked session from the ladder; the ladder's first act each due wake is to drain the read channel so a lost notify event surfaces as `BLOCKED` before any stall is declared. The spawn argv gains `--permission-prompt-tool stdio` per-agent, only when the resolved permission mode can ask.

**Tech Stack:** Rust, mio single-threaded event loop, serde/serde_json (newline-delimited JSON wire; declaration-order structs pin bytes), SQLite ledger with a fold-derived state projection, clap CLI. No new dependencies.

## Global Constraints

Copied verbatim from AGENTS.md invariants and the kickoff; every task's requirements implicitly include these.

- **Fail fast.** No fallbacks, no silenced errors, no placeholders. No panics in library code — clippy `unwrap_used`/`expect_used`/`panic` are DENIED outside `#[cfg(test)]`; `unsafe_code` forbidden. Every error surfaces to the caller or lands in the ledger as an event.
- **Nothing hidden.** All durable truth is the one SQLite ledger plus its fold. Every campd action is an event with its cause. `kill -9` anything; the ledger tells the whole story — including that a worker was killed on adoption, and why.
- **Idle is free.** No ticks, no polling loops. A blocked session must not add a wakeup: its stall timer is DISARMED, so it contributes nothing to the poll deadline (invariant 1). The pre-ladder drain (§5.3.3) runs only when a stall fire is actually due — the idle path pays nothing.
- **Vocabulary mirror.** `permission.pending` / `permission.decided` / `permission.saturated` are camp-specific and additive — none exists in gc's registry (`gc-vocab.json`). They go in `CAMP_SPECIFIC_EVENTS`, never `GC_MIRRORED_EVENTS`.
- **One module owns the wire (§2.1).** `crates/camp/src/daemon/control.rs` is the ONLY place that constructs or parses a `claude` control message. cp-3's outbound permission `control_response` is built there, from `#[derive(Serialize)]` declaration-order structs, byte-pinned to fixtures — never `serde_json::json!` (which alphabetizes keys). Nothing else in camp touches the wire.
- **One transaction, event + state (§7.2).** A new event's fold mutation commits in the same WAL transaction as the event row (`insert_and_fold`, `mod.rs:768`). A fold error rolls the event back — state can never lag or outrun history.
- **deny_unknown_fields on every new fold payload struct.** A payload whose shape is wrong is REFUSED at append, not stored and hoped over.
- **refold stays green.** The `permissions` state table is added to refold's `STATE_TABLES` and refold_prop's `DUMPS`; the property test (state ≡ fold(event-log), and two ledgers fed the same ops are byte-identical) must pass. Every new fold fn is a pure, deterministic function of (accepted-prefix, event) — no clock/RNG/filesystem reads; rejections append nothing.
- **Guaranteed-contention files stay ADDITIVE** (compat-4 runs in parallel; cp-4 shares `spawn.rs`): `crates/camp/src/main.rs`, `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`, `Cargo.toml`, `Cargo.lock`. cp-3 ADDS event variants + names + fold arms; it redefines nothing. `Cargo.toml`/`Cargo.lock` are untouched (no new dependency). The `spawn.rs` change is scoped tightly to the permission-flag arm; cp-4 adds `--include-partial-messages` in the stream-flags arm — a rebase between the two siblings resolves at implementation (worktree isolation handles planning).
- **Schema bump is the sanctioned path (`schema.rs:122-126`).** A fold-state schema change bumps `SCHEMA_VERSION` (3 → 4) through `FULL_DDL_PREFIX`; an existing camp then fails to open and the operator re-inits (the v1 "no auto-upgrade" contract). No backwards compatibility is assumed (kickoff). This is DISTINCT from the consumer-bookkeeping `READ_CHANNEL_DDL` path, which evolves additively without a bump — the `permissions` table carries fold truth, so it takes the version-gated path.
- **Gates green before any push:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`. Perf gate (`make perf`) is LOCAL-ONLY; run it if a task touches the idle path. The paid real-`claude` `can_use_tool` round-trip rides `make e2e` (opt-in, local-only).
- **Branch:** `cp-3-permission-flow`, cut from main at `b0617e2`. Never commit to main. No co-author lines. After any merge to main, rebase onto main and re-run the gates before continuing.

---

## The central architecture decision, stated so the plan gate can rule on it

**`BLOCKED` is folded durable ledger state, not campd in-memory state.** The alternative — an in-memory projection on `ControlRuntime`, rehydrated from the ledger like cp-1's `pending` control-request table — was considered and rejected. The folded `permissions` table wins on four spec-literal grounds:

1. **§5.3.4** says "BLOCKED state lives in the ledger (an event, not memory)" and phrases the adoption rule as "the ledger *shows* an unanswered permission request." A folded table makes that an indexed `SELECT`, not an event-log scan.
2. **§9 first-answer-wins**: "ledger append order decides — the ledger is the serialization point." A `permission.decided` fold of `UPDATE permissions SET status='decided' WHERE request_id=? AND status='pending'` that returns `Err` when zero rows change makes the DB transaction *itself* the serialization point. First-answer-wins is not handler logic that could race; it is a fold invariant.
3. **refold** — the treasured invariant — then *covers* BLOCKED: state is provably a fold of the log.
4. **DRY / single source of truth**: `fleet_model.blocked`, the dispatch-slot exemption, `max_blocked` saturation, and the adoption kill all query one table. cp-1's `pending` table is the wrong precedent to copy — it holds *transient, camp-minted, timeout-bearing* requests; BLOCKED is *durable operator-facing state* that must survive a restart and drive adoption. Different lifecycle, different home.

The cost is a `SCHEMA_VERSION` bump (3 → 4), which is the sanctioned mechanism for adding fold state and is safe against the parallel siblings (none touches `schema.rs`/`refold.rs`/`refold_prop.rs`).

## Scoping decisions (read before Task 1)

Deliberate boundaries where the spec's end-state is richer than cp-3's slice. Documented so the implementer does not "fix" them.

1. **`allow_always` sends the same wire bytes as `allow`.** The CLI's own validator names exactly two behaviours: `{behavior:'allow', updatedInput?:object}` or `{behavior:'deny', message:string}` (recovered in `PROVENANCE.md`; pinned as `permission_allow_response.json` / `permission_deny_response.json`). There is no `allow_always` on the wire. cp-3 records three DECISIONS in the ledger (`allow` / `allow_always` / `deny` — §4.1's contract) but the `control_response` for both allow-forms is `{behavior:"allow"}`. Making `allow_always` durably *suppress future asks* would require persisting a per-agent permission rule applied at the next spawn (or a live `updatedPermissions` push whose shape is not pinned) — that is an additive follow-up, not cp-3. cp-3's `allow_always` is a recorded operator intent with the same immediate wire effect as `allow`; the ledger distinguishes them so the follow-up has its data.
2. **`permission.pending` records `session`, `request_id`, `tool_name` — not the tool INPUT.** The rich `? Bash(cargo publish)` render in §5.1 is the AGENT VIEW's stream-parsing machinery (cp-4). cp-2 renders `LAST` from a timestamp; cp-3 renders `BLOCKED` from the bit. The tool input is available on the wire (`can_use_tool_request.json` carries `input`) and can be added to the event additively later; recording it now would bloat the ledger with no cp-3 consumer.
3. **`max_blocked` default is 10** (mirroring `max_workers`). The spec gives no number; the saturation fault is about operator attention, and 10 unanswered questions is the same order as the dispatch cap. It is a config knob (`[dispatch].max_blocked`) so an operator can tune it.
4. **The dedup that prevents a duplicate `permission.pending` lives in `ingest`, backed by a ledger existence-check** (`ledger.permission_exists`), not by a `permissions` UNIQUE catch-and-swallow. §2.3 requires "campd never appends a duplicate pending for a request_id the ledger already carries"; the `permissions` PRIMARY KEY is the belt-and-braces invariant (a duplicate that slipped through would be a loud append failure, not silent corruption), but the design PREVENTS the duplicate before emit rather than silencing a rejected append (fail-fast: an expected condition is handled, not an error swallowed).
5. **The adoption kill reuses patrol's existing safe-kill machinery** (`restart_non_child`'s probe-by-uuid → `kill_pid` → re-probe), with the named reason `"adoption: unanswerable permission request"`. It does not invent a second kill path. The bead re-hook rides the same `observe` → `Respawn` → `dispatch_bead` path a patrol restart uses; `observe` is extended to re-hook on the new named reason as it does on `"patrol restart"`.

---

## File structure

**camp-core (the durable substrate):**
- **Modify `crates/camp-core/src/event.rs`** — three additive `EventType` variants (`PermissionPending`, `PermissionDecided`, `PermissionSaturated`) across the four synchronized surfaces (enum body, `ALL`, `as_str`; `parse` is derived).
- **Modify `crates/camp-core/src/vocab.rs`** — three names into `CAMP_SPECIFIC_EVENTS`.
- **Modify `crates/camp-core/src/ledger/schema.rs`** — `SCHEMA_VERSION` 3→4, `FULL_DDL_PREFIX`'s `'3'`→`'4'`, the `permissions` table + index into `STATE_DDL`.
- **Modify `crates/camp-core/src/ledger/fold.rs`** — three `apply` arms + `deny_unknown_fields` payload structs + fold fns (`permission_pending`, `permission_decided`, and an audit validate for `permission.saturated`).
- **Modify `crates/camp-core/src/ledger/refold.rs`** — add `permissions` to `STATE_TABLES`.
- **Modify `crates/camp-core/src/ledger/mod.rs`** — read queries: `blocked_sessions`, `permission_exists`, `permission_decider`, `pending_permission_for_session`.
- **Modify `crates/camp-core/src/config.rs`** — `DispatchConfig.max_blocked` + default + validation.
- **Modify `crates/camp-core/tests/refold_prop.rs`** — `permissions` into `DUMPS`; `Op` generator gains permission ops.
- **`crates/camp-core/tests/vocab_pin.rs`** — no edit; the disjoint/union test forces the vocab+event edits and must stay green.

**camp daemon:**
- **Modify `crates/camp/src/daemon/control.rs`** — widen `WorkerMessage::CanUseTool`; replace the `permission_unanswerable` ingest arm with the `permission.pending` producer (dedup via ledger); add `ParentMessage::PermissionAllow`/`PermissionDeny` + serializers; add `serve_permission_decision`; flip `fleet_model`'s `blocked`.
- **Modify `crates/camp/src/daemon/socket.rs`** — `Request::SessionPermissionDecision`, `Response::PermissionDecided`, wire-pin tests.
- **Modify `crates/camp/src/daemon/event_loop.rs`** — the `session.permission_decision` dispatch arm; the pre-ladder drain (§5.3.3); `patrol.reconcile_blocked` each wake; the `max_blocked` saturation check; the steady-state adoption kill for a blocked non-child.
- **Modify `crates/camp/src/daemon/patrol.rs`** — the `blocked: HashSet<String>` + `reconcile_blocked`; `declare_stalls` skips blocked; the adoption-kill branch in `adopt`; `observe` re-hooks on the new named reason.
- **Modify `crates/camp/src/daemon/dispatch.rs`** — the two slot gates count non-blocked children; `max_blocked` plumbing.
- **Modify `crates/camp/src/daemon/spawn.rs`** — `--permission-prompt-tool stdio` per-agent (build_spec + resume_argv); the resolution/refusal function.
- **Create `crates/camp/src/cmd/decide.rs`** — the `camp decide <session> <request_id> allow|deny …` client verb. Small; folds into Task 7.
- **Modify `crates/camp/src/main.rs`** — one additive command variant + dispatch arm (contended file; additive only).
- **Modify `crates/camp/tests/fake-agent.sh`** — a `FAKE_AGENT_CAN_USE_TOOL` mode that emits a `can_use_tool` then waits for the `control_response`.
- **Modify `crates/camp/tests/control.rs`** — the end-to-end permission round-trip against a real campd + fake worker.
- **Modify `crates/camp/tests/daemon_patrol.rs`** — blocked-forever-not-killed, ladder-drains-first, the disarm/skip/re-arm unit guards, adoption both ways + post-adoption-discovered.
- **Modify `crates/camp/tests/e2e.rs`** — the PAID `can_use_tool` round-trip against the real CLI, under `CAMP_E2E`/`make e2e` (NOT `claude_compat.rs`, the $0 tier).

---

## Task 1: The `permissions` table + the two folded events (camp-core)

The durable substrate. Three events registered; `permission.pending` and `permission.decided` fold the `permissions` table; `permission.saturated` is audit-only. First-answer-wins becomes a fold invariant.

**Files:**
- Modify: `crates/camp-core/src/event.rs` (enum body ~14-106; `ALL` ~109-148; `as_str` ~150-191)
- Modify: `crates/camp-core/src/vocab.rs` (`CAMP_SPECIFIC_EVENTS` ~23-68)
- Modify: `crates/camp-core/src/ledger/schema.rs` (`SCHEMA_VERSION:14`; `STATE_DDL:18-88`; `FULL_DDL_PREFIX:90-113`)
- Modify: `crates/camp-core/src/ledger/fold.rs` (`apply` match ~17-70; payload-struct region)
- Modify: `crates/camp-core/src/ledger/refold.rs` (`STATE_TABLES` ~31-69)
- Test: `crates/camp-core/src/ledger/fold.rs` `#[cfg(test)]`; `crates/camp-core/src/event.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `EventInput`/`Event` (`event.rs:215-240`), `payload::<T>` + `audit::<T>` helpers (`fold.rs:73-80`), `CoreError::{InvalidEventData, UnknownSession}`, `insert_and_fold`/`append` (`mod.rs:768/98`).
- Produces (used by Tasks 2, 4, 5, 7, 9): the three `EventType` variants; the `permissions` table (`request_id` PK, `session`, `tool_name`, `status IN ('pending','decided')`, `decision IN ('allow','allow_always','deny')`, `decided_by`, `requested_ts`, `decided_ts`); the fold contract that a second `permission.decided` for a `request_id` already `decided` is REFUSED.

- [ ] **Step 1: Write the failing fold test for the two events + first-answer-wins**

Add to `fold.rs`'s test module (alongside the existing append-refusal tests ~1501). Uses the crate's existing test-ledger helper (grep the module for the `Ledger::open`/`FixedClock` scaffold the neighboring fold tests use, e.g. `session_woke` tests; reuse it verbatim — `test_ledger`/`woke_session` below are placeholders for that scaffold).

```rust
#[test]
fn permission_pending_then_decided_folds_and_first_answer_wins() {
    let (mut led, _tmp) = test_ledger();
    woke_session(&mut led, "t-dev-1");

    // pending → a row appears, session is blocked
    led.append(EventInput {
        kind: EventType::PermissionPending,
        rig: None, actor: "campd".into(), bead: Some("b-1".into()),
        data: serde_json::json!({"session":"t-dev-1","request_id":"cli-7","tool_name":"Bash"}),
    }).unwrap();
    assert_eq!(led.blocked_sessions().unwrap(), vec!["t-dev-1".to_owned()]);

    // FIRST answer wins: allow succeeds, unblocks
    led.append(EventInput {
        kind: EventType::PermissionDecided,
        rig: None, actor: "campd".into(), bead: Some("b-1".into()),
        data: serde_json::json!({"session":"t-dev-1","request_id":"cli-7","decision":"allow","decided_by":"operator"}),
    }).unwrap();
    assert!(led.blocked_sessions().unwrap().is_empty());
    assert_eq!(led.permission_decider("cli-7").unwrap().as_deref(), Some("operator"));

    // SECOND answer for the same id is REFUSED (nothing pending to decide)
    let loser = led.append(EventInput {
        kind: EventType::PermissionDecided,
        rig: None, actor: "campd".into(), bead: Some("b-1".into()),
        data: serde_json::json!({"session":"t-dev-1","request_id":"cli-7","decision":"deny","decided_by":"overseer","reason":"too late"}),
    });
    assert!(loser.is_err(), "first answer wins — a decision for an already-decided request is refused");
    assert_eq!(led.permission_decider("cli-7").unwrap().as_deref(), Some("operator")); // loser appended nothing
}

#[test]
fn a_deny_decision_must_carry_a_reason() {
    let (mut led, _tmp) = test_ledger();
    woke_session(&mut led, "t-dev-1");
    led.append(EventInput { kind: EventType::PermissionPending, rig: None, actor: "campd".into(), bead: None,
        data: serde_json::json!({"session":"t-dev-1","request_id":"cli-9","tool_name":"Bash"}) }).unwrap();
    let denied = led.append(EventInput { kind: EventType::PermissionDecided, rig: None, actor: "campd".into(), bead: None,
        data: serde_json::json!({"session":"t-dev-1","request_id":"cli-9","decision":"deny","decided_by":"op"}) });
    assert!(denied.is_err(), "a deny with no reason is refused");
}

#[test]
fn permission_pending_payload_rejects_unknown_fields() {
    let (mut led, _tmp) = test_ledger();
    woke_session(&mut led, "t-dev-1");
    let bad = led.append(EventInput { kind: EventType::PermissionPending, rig: None, actor: "campd".into(), bead: None,
        data: serde_json::json!({"session":"t-dev-1","request_id":"cli-9","tool_name":"Bash","surprise":1}) });
    assert!(bad.is_err(), "deny_unknown_fields refuses an unexpected key");
}
```

- [ ] **Step 2: Run — expect FAIL to compile (`EventType::PermissionPending` undefined, `blocked_sessions`/`permission_decider` undefined)**

Run: `cargo test -p camp-core permission_pending_then_decided -- --nocapture`
Expected: FAIL — unknown variant and unknown methods. (`blocked_sessions`/`permission_decider` are Task 2; co-commit them with Task 1 if you want Step 1 to compile in isolation.)

- [ ] **Step 3: Register the three `EventType` variants (four surfaces)**

In `event.rs`, add to the enum body (with a doc comment, matching every recent variant), to `ALL`, and to `as_str`:

```rust
// enum body:
    /// cp-3 (control-plane §5.3): a worker asked permission to use a tool and
    /// is now BLOCKED awaiting an operator decision. Folds a `permissions` row.
    PermissionPending,
    /// cp-3 (§5.3/§9): an operator answered a `permission.pending`. Folds the
    /// row to `decided`; a second decision for the same request is REFUSED
    /// (first-answer-wins). `decided_by` records who.
    PermissionDecided,
    /// cp-3 (§5.3.2): the count of BLOCKED sessions crossed `max_blocked` —
    /// a loud, operator-visible saturation fault. Audit-only.
    PermissionSaturated,
```
```rust
// ALL: add EventType::PermissionPending, EventType::PermissionDecided, EventType::PermissionSaturated,
// as_str:
    EventType::PermissionPending => "permission.pending",
    EventType::PermissionDecided => "permission.decided",
    EventType::PermissionSaturated => "permission.saturated",
```

- [ ] **Step 4: Add the three names to `CAMP_SPECIFIC_EVENTS` (`vocab.rs`)**

```rust
    // cp-3 (control-plane §5.3): the permission plane. gc has no `permission.*`
    // event, so all three are additive.
    "permission.pending",
    "permission.decided",
    "permission.saturated",
```

- [ ] **Step 5: Bump the schema and add the `permissions` table (`schema.rs`)**

`SCHEMA_VERSION` 3→4; the `INSERT INTO meta … VALUES ('schema_version', '3')` literal in `FULL_DDL_PREFIX` → `'4'`. Append to `STATE_DDL` (before its closing `"#`):

```sql
-- cp-3 (control-plane §5.3): permission requests and their decisions.
-- `status='pending'` on a LIVE session is what `BLOCKED` renders and what the
-- adoption kill and the dispatch-slot exemption query. A `decided` row is the
-- durable record of who allowed what (§9). request_id is the CLI-minted id
-- (no `camp-` prefix), unique per request.
CREATE TABLE permissions (
  request_id   TEXT PRIMARY KEY,
  session      TEXT NOT NULL,
  tool_name    TEXT NOT NULL,
  status       TEXT NOT NULL CHECK (status IN ('pending','decided')),
  decision     TEXT CHECK (decision IN ('allow','allow_always','deny')),
  decided_by   TEXT,
  requested_ts TEXT NOT NULL,
  decided_ts   TEXT
) STRICT;
CREATE INDEX permissions_session_status ON permissions(session, status);
```

- [ ] **Step 6: Add `permissions` to refold's `STATE_TABLES` (`refold.rs:31-69`)**

Follow the existing entry shape (table name, `cols`, `key`). `key` is `request_id`; `cols` lists every column so the `EXCEPT`-both-ways diff observes them.

- [ ] **Step 7: Write the fold fns + payload structs (`fold.rs`)**

Add the three `apply` arms (the match is exhaustive — a missing arm will not compile):

```rust
    EventType::PermissionPending => permission_pending(conn, event),
    EventType::PermissionDecided => permission_decided(conn, event),
    EventType::PermissionSaturated => audit::<PermissionSaturated>(event),
```

Payload structs + fns (place with the other control-plane fold structs ~1374):

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionPending {
    session: String,
    request_id: String,
    tool_name: String,
}

fn permission_pending(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: PermissionPending = payload(event)?;
    let live: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE name = ?1 AND status = 'live')",
        [&p.session],
        |r| r.get(0),
    )?;
    if !live {
        return Err(CoreError::UnknownSession(p.session));
    }
    // The PK on request_id makes a duplicate pending a LOUD append failure
    // (invariant 5). The producer (`ingest`) dedups upstream so this never
    // fires in practice — it is the belt-and-braces invariant, not the dedup.
    conn.execute(
        "INSERT INTO permissions (request_id, session, tool_name, status, requested_ts)
         VALUES (?1, ?2, ?3, 'pending', ?4)",
        params![p.request_id, p.session, p.tool_name, event.ts],
    )?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionDecided {
    session: String,
    request_id: String,
    decision: String,
    decided_by: String,
    #[serde(default)]
    reason: Option<String>,
}
const PERMISSION_DECISIONS: &[&str] = &["allow", "allow_always", "deny"];

fn permission_decided(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: PermissionDecided = payload(event)?;
    if !PERMISSION_DECISIONS.contains(&p.decision.as_str()) {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("unknown decision {:?} (allow|allow_always|deny)", p.decision),
        });
    }
    if p.decision == "deny" && p.reason.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "a deny decision must carry a non-empty reason".to_owned(),
        });
    }
    // FIRST-ANSWER-WINS (§9): only a STILL-PENDING request may be decided. The
    // ledger transaction is the serialization point — zero rows changed means
    // the request was already decided (a losing decider) or never pending
    // (unknown id). Either way the append rolls back and appends nothing.
    let changed = conn.execute(
        "UPDATE permissions
            SET status = 'decided', decision = ?1, decided_by = ?2, decided_ts = ?3
          WHERE request_id = ?4 AND status = 'pending'",
        params![p.decision, p.decided_by, event.ts, p.request_id],
    )?;
    if changed == 0 {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!(
                "no PENDING permission request {:?} to decide — already decided, or never pending",
                p.request_id
            ),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // audit-only: validated at append, never read back.
struct PermissionSaturated {
    blocked: u64,
    max_blocked: u64,
}
```

- [ ] **Step 8: Run the fold tests**

Run: `cargo test -p camp-core permission_ -- --nocapture`
Expected: PASS once Task 2's queries are present (co-commit or land Task 2 first). The payload/deny/first-answer-wins logic is exercised here.

- [ ] **Step 9: Add an `event.rs` round-trip test for the three names**

Mirror the existing camp-specific round-trip test (`event.rs:336`): assert `EventType::PermissionPending.as_str() == "permission.pending"` and `EventType::parse("permission.pending") == Ok(EventType::PermissionPending)` for all three.

- [ ] **Step 10: Run the vocab-pin test**

Run: `cargo test -p camp-core --test vocab_pin`
Expected: PASS — `every_event_type_is_declared_mirrored_or_camp_specific_never_both` is green because the three variants and their three names were added together; `camp_specific_names_do_not_collide_with_gc` is green (gc has no `permission.*`).

- [ ] **Step 11: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs \
        crates/camp-core/src/ledger/schema.rs crates/camp-core/src/ledger/fold.rs \
        crates/camp-core/src/ledger/refold.rs
git commit -m "feat(camp-core): permission.pending/decided/saturated events + permissions fold (schema v4)"
```

**Mutation each test catches:** first-answer-wins test dies if the `WHERE status='pending'` guard is dropped (a second decision would succeed) or if `changed == 0` is not an error. The deny-reason test dies if the reason check is removed. The unknown-fields test dies if `deny_unknown_fields` is dropped.

---

## Task 2: Ledger read queries (camp-core)

Four indexed queries the daemon uses. All are `&self` reads (no fold), mirroring `live_sessions`/`session_status` (`mod.rs:395/420`).

**Files:**
- Modify: `crates/camp-core/src/ledger/mod.rs` (near the other read queries, ~420-640)
- Test: `crates/camp-core/src/ledger/mod.rs` `#[cfg(test)]` (or the `fold.rs` tests, which Task 1 already exercises)

**Interfaces:**
- Produces:
  - `pub fn blocked_sessions(&self) -> Result<Vec<String>, CoreError>` — LIVE sessions with a `pending` permission.
  - `pub fn permission_exists(&self, request_id: &str) -> Result<bool, CoreError>` — any row (pending or decided) for the id. Used by `ingest`'s dedup.
  - `pub fn permission_decider(&self, request_id: &str) -> Result<Option<String>, CoreError>` — `decided_by` if decided, else `None`. Used by the "already decided by X" response.
  - `pub fn pending_permission_for_session(&self, session: &str) -> Result<Option<String>, CoreError>` — the `request_id` of a live pending permission for a session (adoption kill + steady-state non-child kill).

- [ ] **Step 1: Write the failing test** (fold into Task 1's tests if co-committed)

```rust
#[test]
fn blocked_sessions_lists_only_live_pending_and_permission_exists_dedups() {
    let (mut led, _tmp) = test_ledger();
    woke_session(&mut led, "t-dev-1");
    led.append(EventInput { kind: EventType::PermissionPending, rig: None, actor: "campd".into(), bead: None,
        data: serde_json::json!({"session":"t-dev-1","request_id":"cli-7","tool_name":"Bash"}) }).unwrap();
    assert!(led.permission_exists("cli-7").unwrap());
    assert!(!led.permission_exists("cli-nope").unwrap());
    assert_eq!(led.blocked_sessions().unwrap(), vec!["t-dev-1".to_owned()]);
    assert_eq!(led.pending_permission_for_session("t-dev-1").unwrap().as_deref(), Some("cli-7"));

    // ending the session drops it from the blocked set (the live join)
    end_session(&mut led, "t-dev-1"); // the module's session.stopped helper
    assert!(led.blocked_sessions().unwrap().is_empty());
}
```

- [ ] **Step 2: Run — expect FAIL (methods undefined)**

Run: `cargo test -p camp-core blocked_sessions_lists_only_live_pending -- --nocapture`

- [ ] **Step 3: Implement the four queries**

```rust
/// cp-3 (§5.3): LIVE sessions with an undecided permission request — what
/// `BLOCKED` renders, what the dispatch-slot exemption subtracts, and what the
/// adoption kill scans. The join with `sessions.status='live'` keeps a decided-
/// -then-ended session (whose pending row may linger for the record) out.
pub fn blocked_sessions(&self) -> Result<Vec<String>, CoreError> {
    let mut stmt = self.conn.prepare(
        "SELECT DISTINCT p.session
           FROM permissions p JOIN sessions s ON p.session = s.name
          WHERE p.status = 'pending' AND s.status = 'live'
          ORDER BY p.session",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn permission_exists(&self, request_id: &str) -> Result<bool, CoreError> {
    Ok(self.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM permissions WHERE request_id = ?1)",
        [request_id], |r| r.get(0),
    )?)
}

pub fn permission_decider(&self, request_id: &str) -> Result<Option<String>, CoreError> {
    Ok(self.conn.query_row(
        "SELECT decided_by FROM permissions WHERE request_id = ?1 AND status = 'decided'",
        [request_id], |r| r.get::<_, Option<String>>(0),
    ).optional()?.flatten())
}

pub fn pending_permission_for_session(&self, session: &str) -> Result<Option<String>, CoreError> {
    Ok(self.conn.query_row(
        "SELECT request_id FROM permissions WHERE session = ?1 AND status = 'pending' LIMIT 1",
        [session], |r| r.get::<_, String>(0),
    ).optional()?)
}
```
(`optional()` is `rusqlite::OptionalExtension` — confirm the `use` in `mod.rs`; it is already used by neighboring queries.)

- [ ] **Step 4: Run — expect PASS.** `cargo test -p camp-core blocked_sessions_lists_only_live_pending`

- [ ] **Step 5: Commit** `git commit -am "feat(camp-core): permission ledger queries"`

**Mutation caught:** dropping the `s.status='live'` join lets a dead session's stale pending render as blocked — the end-session assertion dies.

---

## Task 3: refold property + config (camp-core)

Keep refold green with the new fold table, and add `max_blocked`.

**Files:**
- Modify: `crates/camp-core/tests/refold_prop.rs` (`DUMPS` ~173-187; `Op` enum ~12-57; `op_strategy` ~144-158)
- Modify: `crates/camp-core/src/config.rs` (`DispatchConfig` ~55-76; defaults ~78)
- Test: the proptest itself; a config round-trip test

**Interfaces:**
- Produces: `DispatchConfig.max_blocked: usize` (default 10, rejected at 0); `Op::PermissionPending`/`Op::PermissionDecide` proptest variants.

- [ ] **Step 1: Add `permissions` to `DUMPS`** so `dump_state` observes it in the "two ledgers byte-identical" property.

- [ ] **Step 2: Extend the `Op` generator** with `PermissionPending { session_idx, request_idx }` and `PermissionDecide { request_idx, decision }`, wired into `op_strategy` and the apply-op harness so proptest exercises fold+refold over permission events. Only emit a `PermissionPending` against a woke session; a `PermissionDecide` picks an existing request id (a decide against an unknown/already-decided id is expected to be refused and appends nothing — the harness must tolerate the `Err` exactly as it does other rejections).

- [ ] **Step 3: Run the property test**

Run: `cargo test -p camp-core --test refold_prop`
Expected: PASS — `refold_matches_incremental_fold` holds (state ≡ fold(log); `permissions` diffs clean both ways).

- [ ] **Step 4: Add `max_blocked` to `DispatchConfig`**

```rust
    /// cp-3 (§5.3.2): the count of BLOCKED (permission-waiting) sessions past
    /// which campd raises a loud `permission.saturated` fault. A blocked worker
    /// does NOT hold a dispatch slot, so this bounds operator attention, not
    /// concurrency. Rejected at 0.
    #[serde(default = "default_max_blocked")]
    pub max_blocked: usize,
```
```rust
fn default_max_blocked() -> usize { 10 }
```
Add the `> 0` validation beside `max_workers`'s (`config.rs:366-371`), and a round-trip test asserting the default and the 0-rejection.

- [ ] **Step 5: Run config tests + commit**

Run: `cargo test -p camp-core config`
```bash
git commit -am "feat(camp-core): max_blocked config + refold coverage for permissions"
```

**Mutation caught:** omitting `permissions` from `DUMPS`/`STATE_TABLES` lets a fold bug pass refold silently; the byte-identical property dies if the table is not dumped and the two ledgers diverge on a permission op.

---

## Task 4: The `permission.pending` producer — replace the `permission_unanswerable` fault (control.rs)

Widen the parse, then replace cp-1's stopgap fault arm with the real producer: emit `permission.pending` (deduped via the ledger), which folds `BLOCKED`.

**Files:**
- Modify: `crates/camp/src/daemon/control.rs` (`WorkerMessage::CanUseTool` ~161-164 + parse ~268-271; `ingest` signature ~1310 + the CanUseTool arm ~1386-1418)
- Modify: `crates/camp/src/daemon/event_loop.rs` (`control_step` call site ~746-764 — thread `&Ledger` into `ingest`)
- Test: `control.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `parse_worker_line` (`control.rs:206`), `StreamLine` (`read_channel.rs:183`), `ledger.permission_exists` (Task 2).
- Produces: `ingest(&mut self, lines: &[StreamLine], dispatcher: &mut Dispatcher, ledger: &Ledger, now: Timestamp) -> Vec<EventInput>` (adds `ledger: &Ledger`); a `permission.pending` `EventInput` per NEW `can_use_tool` request id.

- [ ] **Step 1: Widen `WorkerMessage::CanUseTool`** to carry `tool_use_id` (optional; the CLI sends it; the audit value is cheap). Keep the envelope permissive (NOT `deny_unknown_fields`, per C9). Update the parse arm:

```rust
    CanUseTool {
        request_id: String,
        tool_name: String,
        tool_use_id: Option<String>,
    },
```
```rust
    Some("can_use_tool") => Ok(WorkerMessage::CanUseTool {
        request_id,
        tool_name: body["tool_name"].as_str().unwrap_or_default().to_owned(),
        tool_use_id: body["tool_use_id"].as_str().map(str::to_owned),
    }),
```
Widening the variant is compile-forcing: BOTH existing test destructures — `worker_messages_parse_from_the_pinned_fixtures` (`control.rs:2121`) and `can_use_tool_with_unknown_extra_keys_still_parses` (`control.rs:2146`) — must add the `tool_use_id` binding, and both stay green (the fixture carries no `tool_use_id`, so it parses to `None`; the extra-keys test still proves the permissive envelope tolerates unknown keys).

- [ ] **Step 2: Write the failing ingest test**

```rust
#[test]
fn a_can_use_tool_becomes_a_permission_pending_and_dedups() {
    let mut rt = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
    let (mut led, _tmp) = test_ledger_with_live_session("t-dev-1");
    let mut disp = test_dispatcher();
    let line = StreamLine { session: "t-dev-1".into(),
        line: r#"{"type":"control_request","request_id":"cli-2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo publish"}}}"#.into() };

    let events = rt.ingest(std::slice::from_ref(&line), &mut disp, &led, ts());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventType::PermissionPending);
    assert_eq!(events[0].data["request_id"], "cli-2");
    assert_eq!(events[0].data["tool_name"], "Bash");

    // Commit it, then re-ingest the SAME line: no duplicate (§2.3).
    led.append(events[0].clone()).unwrap();
    let again = rt.ingest(std::slice::from_ref(&line), &mut disp, &led, ts());
    assert!(again.is_empty(), "a request_id the ledger already carries never re-emits pending");
}
```

- [ ] **Step 3: Run — expect FAIL (signature mismatch / still emits ControlFailed)**

- [ ] **Step 4: Replace the CanUseTool arm** (`control.rs:1386-1418`). Delete the `permission_unanswerable` fault; emit `permission.pending` only if the ledger does not already carry the id:

```rust
    Ok(WorkerMessage::CanUseTool { request_id, tool_name, tool_use_id: _ }) => {
        // §5.3: the worker is asking to run a tool its allowlist does not
        // cover. Mark it BLOCKED by appending permission.pending (which folds
        // the `permissions` row). §2.3 dedup: a re-read line whose pending
        // already committed must NOT append a second — the ledger is the guard.
        match ledger.permission_exists(&request_id) {
            Ok(true) => {}  // already recorded (a re-drain after restart) — skip
            Ok(false) => {
                let (rig, bead) = dispatcher.child_info(&sl.session)
                    .map(|(r, b)| (Some(r), Some(b))).unwrap_or((None, None));
                events.push(EventInput {
                    kind: EventType::PermissionPending,
                    rig, actor: "campd".into(), bead,
                    data: serde_json::json!({
                        "session": sl.session, "request_id": request_id, "tool_name": tool_name,
                    }),
                });
            }
            Err(e) => {
                // A ledger read failing here is a real fault — surface it, never
                // silently drop a permission request (invariant 5).
                push_fault(&mut events, &mut faults, &sl.session, EventInput {
                    kind: EventType::ControlFailed, rig: None, actor: "campd".into(), bead: None,
                    data: serde_json::json!({"session": sl.session, "request_id": request_id,
                        "cause": "permission_unanswerable",
                        "reason": format!("checking whether the permission request is already recorded failed: {e}")}),
                });
            }
        }
    }
```
Note: `note_activity` at the top of `ingest`'s loop (`control.rs:1325`) still runs for the can_use_tool line — it resets any *outstanding camp-minted control request*'s silence deadline (unrelated to the stall ladder Task 8 disarms). Do not remove it.

- [ ] **Step 5: Thread `&Ledger` into `ingest`** at the `control_step` call (`event_loop.rs:760`): `control.ingest(&lines, dispatcher, ledger, now)`. The immutable borrow ends when `ingest` returns, before the `for input in … { ledger.append(input)? }` loop takes `&mut`.

- [ ] **Step 6: Run — expect PASS.** `cargo test -p camp a_can_use_tool_becomes_a_permission_pending`

- [ ] **Step 7: Update cp-1's stopgap tests.** Grep control.rs/tests for `permission_unanswerable` — cp-1 asserted a `can_use_tool` produces `control.failed{cause:"permission_unanswerable"}`. That behaviour is now GONE; replace that assertion with the `permission.pending` expectation. The `PermissionUnanswerable` `ControlFailureCause` variant STAYS (the ledger-read-failure arm above uses it, and rehydration routing must keep classifying it — it is `is_terminal`).

- [ ] **Step 8: Commit** `git commit -am "feat(control): can_use_tool → permission.pending producer with ledger dedup"`

**Mutation caught:** the dedup test dies if the `permission_exists` guard is removed (a re-drain double-appends). The producer test dies if the arm still emits `ControlFailed`.

---

## Task 5: `fleet_model.blocked` from the ledger (control.rs)

Flip the hardcoded `blocked: false` (`control.rs:1243`) to real ledger truth so `camp watch` renders `BLOCKED` — fleet delivery is automatic (cp-2's diff pushes the changed `SessionInfo`).

**Files:**
- Modify: `crates/camp/src/daemon/control.rs` (`fleet_model` ~1217-1250)
- Test: `control.rs` `#[cfg(test)]` — update cp-2's `fleet_model_returns_one_row_per_live_session` (~3222) and `crates/camp/tests/control.rs:642` (the `blocked:false` integration assertion)

- [ ] **Step 1: Write/adjust the failing test** — a live session with a pending permission renders `blocked: true`; after a decision, `false`.

- [ ] **Step 2: Implement.** Compute the blocked set once, then set the bit:

```rust
pub fn fleet_model(&self, ledger: &Ledger, patrol: &PatrolRuntime,
    read_channel: &ReadChannelRuntime) -> anyhow::Result<Vec<SessionInfo>> {
    let rows = ledger.live_sessions()?;
    let blocked: std::collections::HashSet<String> =
        ledger.blocked_sessions()?.into_iter().collect();
    Ok(rows.into_iter().map(|row| SessionInfo {
        last_activity: read_channel.last_activity(&row.name).map(|t| t.to_string()).unwrap_or(row.spawned_ts),
        state: if patrol.is_stalled(&row.name) { "stalled".into() } else { "working".into() },
        blocked: blocked.contains(&row.name),
        name: row.name, agent: row.agent, rig: row.rig, bead: row.bead,
    }).collect())
}
```

- [ ] **Step 3: Update cp-2's pinned assertions.** `fleet_model_returns_one_row_per_live_session` asserted `!model[0].blocked` with "cp-2 never sets blocked — cp-3 owns the producer". Change it: with no pending permission the bit stays `false` (keep as the baseline), and ADD a case with a pending permission asserting `true`. Fix `crates/camp/tests/control.rs:642` similarly.

- [ ] **Step 4: Run — PASS.** `cargo test -p camp fleet_model`

- [ ] **Step 5: Commit** `git commit -am "feat(control): fleet_model.blocked reflects a live pending permission"`

**Mutation caught:** reverting to `blocked: false` fails the new positive assertion; a bad set membership (keying on bead not name) fails it too.

---

## Task 6: Outbound permission `control_response` bytes (control.rs)

Add the `ParentMessage` variants and serializers that answer a `can_use_tool`, byte-pinned to cp-1's already-recovered fixtures.

**Files:**
- Modify: `crates/camp/src/daemon/control.rs` (`ParentMessage` ~93-133; serializer structs ~66-91; the `to_line` match)
- Test: `control.rs` `#[cfg(test)]` (extend `parent_messages_serialize_to_the_pinned_fixture_bytes` ~2030)

**Interfaces:**
- Consumes: fixtures `permission_allow_response.json`, `permission_deny_response.json` (already `include_str!`'d at ~2007).
- Produces: `ParentMessage::PermissionAllow { request_id }`, `ParentMessage::PermissionDeny { request_id, message }`; `to_line()` bytes byte-equal to the fixtures.

- [ ] **Step 1: Write the failing byte-pin test** (into `parent_messages_serialize_to_the_pinned_fixture_bytes`):

```rust
let allow = ParentMessage::PermissionAllow { request_id: "cli-fixture-2".into() }.to_line().unwrap();
assert_eq!(allow, format!("{PERMISSION_ALLOW_RESPONSE}\n"),
    "the allow answer camp sends must be byte-equal to its recovered fixture");
let deny = ParentMessage::PermissionDeny { request_id: "cli-fixture-2".into(), message: "denied by the operator".into() }.to_line().unwrap();
assert_eq!(deny, format!("{PERMISSION_DENY_RESPONSE}\n"),
    "the deny answer camp sends must be byte-equal to its recovered fixture");
```

- [ ] **Step 2: Run — expect FAIL (variants undefined)**

- [ ] **Step 3: Add serializer structs (declaration order = fixture byte order)** near the outbound structs (~66-91):

```rust
#[derive(Serialize)]
struct PermissionResponseEnvelope<'a, D: Serialize> {
    #[serde(rename = "type")]
    kind: &'static str,          // "control_response"
    response: PermissionSuccessBody<'a, D>,
}
#[derive(Serialize)]
struct PermissionSuccessBody<'a, D: Serialize> {
    subtype: &'static str,       // "success"
    request_id: &'a str,
    response: D,                 // the decision object
}
#[derive(Serialize)]
struct AllowDecision { behavior: &'static str }             // {"behavior":"allow"}
#[derive(Serialize)]
struct DenyDecision<'a> { behavior: &'static str, message: &'a str } // {"behavior":"deny","message":…}
```

- [ ] **Step 4: Add the `ParentMessage` variants + `to_line` arms:**

```rust
    /// §5.3 step 5: answer a `can_use_tool` with `{behavior:"allow"}`. Both
    /// `allow` and `allow_always` send these bytes (scoping decision 1).
    PermissionAllow { request_id: String },
    /// §5.3 step 5: answer with `{behavior:"deny", message:…}`. The message is
    /// the operator's reason and is REQUIRED (the CLI validator demands it).
    PermissionDeny { request_id: String, message: String },
```
```rust
    ParentMessage::PermissionAllow { request_id } => serde_json::to_string(&PermissionResponseEnvelope {
        kind: "control_response",
        response: PermissionSuccessBody { subtype: "success", request_id, response: AllowDecision { behavior: "allow" } },
    })?,
    ParentMessage::PermissionDeny { request_id, message } => serde_json::to_string(&PermissionResponseEnvelope {
        kind: "control_response",
        response: PermissionSuccessBody { subtype: "success", request_id, response: DenyDecision { behavior: "deny", message } },
    })?,
```

- [ ] **Step 5: Run — expect PASS.** `cargo test -p camp parent_messages_serialize_to_the_pinned_fixture_bytes`
The existing `the_permission_response_fixtures_match_the_cli_validator_contract` (C10, ~2220) stays green and now the bytes camp *produces* are pinned to those fixtures too.

- [ ] **Step 6: Commit** `git commit -am "feat(control): outbound permission allow/deny control_response bytes"`

**Mutation caught:** any field reorder or key rename (e.g. `json!`-ifying the builder, which alphabetizes) breaks byte-equality against the fixture — exactly the CLI-compat regression the pin exists to catch.

---

## Task 7: The `session.permission_decision` verb — ledger before pipe, first-answer-wins (socket + control + event_loop + cmd)

The write path. Append `permission.decided` FIRST (the serialization point), then write the `control_response`. A losing decider gets "already decided by X".

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs` (`Request` ~22-89; `Response` ~118-193; wire-pin tests ~822/851)
- Modify: `crates/camp/src/daemon/control.rs` (new `serve_permission_decision`)
- Modify: `crates/camp/src/daemon/event_loop.rs` (dispatch arm in `drain_lines` ~1013-1078)
- Create: `crates/camp/src/cmd/decide.rs` + wire into `crates/camp/src/main.rs`
- Test: `control.rs` `#[cfg(test)]` (handler); `socket.rs` wire pin

**Interfaces:**
- Consumes: `ParentMessage::PermissionAllow/Deny` (Task 6), `dispatcher.write_control` (`dispatch.rs:258`), `ledger.permission_decider` (Task 2), `EventType::PermissionDecided`.
- Produces:
  - `Request::SessionPermissionDecision { session: String, request_id: String, decision: String, #[serde(default)] message: Option<String> }` renamed `"session.permission_decision"`.
  - `Response::PermissionDecided { ok: bool, request_id: String, decision: String }` — the `decision` key is the untagged DISCRIMINANT (CP3-B1): `Response::Interrupt` is `{ok, request_id}` (socket.rs:182), so a bare `{ok, request_id}` would shadow it. `PermissionDecided` is placed BEFORE `Interrupt`; its extra REQUIRED `decision` field means an interrupt ack `{"ok":..,"request_id":..}` fails to match it (no `decision`) and falls through to `Interrupt`, while a decision reply `{"ok":..,"request_id":..,"decision":".."}` matches `PermissionDecided` first. `decision` also echoes what was recorded — useful, not a bare marker.
  - `pub fn serve_permission_decision(&mut self, session: &str, request_id: &str, decision: &str, message: Option<&str>, ledger: &mut Ledger, dispatcher: &mut Dispatcher) -> Response`.

- [ ] **Step 1: Write the failing handler test** (in `control.rs` tests):

```rust
#[test]
fn permission_decision_appends_decided_before_writing_and_first_answer_wins() {
    let (mut rt, mut led, mut disp, session) = permission_scaffold("cli-2"); // live session + held-stdin fake + pending in ledger
    let r = rt.serve_permission_decision(&session, "cli-2", "allow", None, &mut led, &mut disp);
    assert!(matches!(r, Response::PermissionDecided { ok: true, .. }));
    assert_eq!(led.permission_decider("cli-2").unwrap().as_deref(), Some("operator")); // durable
    assert!(disp.last_control_written(&session).unwrap().contains(r#""behavior":"allow""#)); // delivered

    // a SECOND decider loses — no second write, an explicit refusal
    let r2 = rt.serve_permission_decision(&session, "cli-2", "deny", Some("no".into()), &mut led, &mut disp);
    match r2 { Response::Error { error, .. } => assert!(error.contains("already decided by operator")),
        other => panic!("expected already-decided error, got {other:?}") }
}

#[test]
fn a_decision_is_durable_even_when_the_pipe_is_gone_inverse_window() {
    // §5.3.4 inverse window: the decision is recorded FIRST, so a worker whose
    // stdin campd no longer holds still shows ANSWERED in the ledger.
    let (mut rt, mut led, mut disp, session) = permission_scaffold_no_pipe("cli-5"); // pending, but write_control → NoPipe
    let r = rt.serve_permission_decision(&session, "cli-5", "allow", None, &mut led, &mut disp);
    assert!(matches!(r, Response::Error { .. }), "delivery failed, so the caller is told so");
    assert_eq!(led.permission_decider("cli-5").unwrap().as_deref(), Some("operator"),
        "but the decision is DURABLE — ledger-before-pipe means answered-in-ledger even when undelivered");
    assert!(led.blocked_sessions().unwrap().is_empty(), "and the session is no longer BLOCKED");
}
```
(`permission_scaffold`/`permission_scaffold_no_pipe`/`last_control_written` are small helpers mirroring the interrupt-test dispatcher scaffold — the no-pipe variant registers the session but seeds no held stdin, so `write_control` returns `NoPipe`.)

- [ ] **Step 2: Run — expect FAIL**

- [ ] **Step 3: Add the `Request`/`Response` variants + the disambiguation wire-pin test** in `socket.rs`. Add `SessionPermissionDecision` to `Request` (internally-tagged on `op`, so additive) and `PermissionDecided { ok, request_id, decision }` to `Response` placed BEFORE `Interrupt` (CP3-B1). Extend `control_plane_verbs_wire_format_is_pinned` with the new verb's exact request bytes, AND add a dedicated untagged-resolution test that is the CP3-B1 regression guard:

```rust
#[test]
fn interrupt_and_permission_decided_do_not_shadow_each_other() {
    // the interrupt ack bytes must round-trip to Interrupt, not PermissionDecided
    let ack: Response = serde_json::from_str(r#"{"ok":true,"request_id":"camp-1"}"#).unwrap();
    assert!(matches!(ack, Response::Interrupt { .. }), "an interrupt ack must not be captured by PermissionDecided");
    // the decision reply bytes must round-trip to PermissionDecided
    let dec: Response = serde_json::from_str(r#"{"ok":true,"request_id":"cli-2","decision":"allow"}"#).unwrap();
    assert!(matches!(dec, Response::PermissionDecided { .. }), "the decision reply must resolve to its own variant");
    // and each serializes back to exactly its own bytes (the pin)
    assert_eq!(serde_json::to_string(&Response::Interrupt { ok: true, request_id: "camp-1".into() }).unwrap(),
        r#"{"ok":true,"request_id":"camp-1"}"#);
    assert_eq!(serde_json::to_string(&Response::PermissionDecided { ok: true, request_id: "cli-2".into(), decision: "allow".into() }).unwrap(),
        r#"{"ok":true,"request_id":"cli-2","decision":"allow"}"#);
}
```
Run BOTH this and the pre-existing `control_plane_verbs_wire_format_is_pinned` — the latter (socket.rs:912) must stay green, proving the placement did not mis-type any cp-1 response.

- [ ] **Step 4: Implement `serve_permission_decision`** (the §5.3.4 ordering — ledger FIRST):

```rust
pub fn serve_permission_decision(
    &mut self, session: &str, request_id: &str, decision: &str,
    message: Option<&str>, ledger: &mut Ledger, dispatcher: &mut Dispatcher,
) -> Response {
    // Validate the wire shape BEFORE touching the ledger (a bad verb is the
    // caller's error, not a campd action to record).
    if !matches!(decision, "allow" | "allow_always" | "deny") {
        return Response::Error { ok: false, error: format!("unknown decision {decision:?} (allow|allow_always|deny)") };
    }
    let reason = message.map(str::trim).unwrap_or("");
    if decision == "deny" && reason.is_empty() {
        return Response::Error { ok: false, error: "a deny decision must carry a message (the operator's reason)".into() };
    }
    let (rig, bead) = dispatcher.child_info(session).map(|(r,b)|(Some(r),Some(b))).unwrap_or((None,None));

    // (1) LEDGER FIRST — the serialization point. First-answer-wins is enforced
    // by the fold (UPDATE … WHERE status='pending'): a losing decider's append
    // FAILS, and we read who won.
    let decided = ledger.append(EventInput {
        kind: EventType::PermissionDecided, rig, actor: "campd".into(), bead,
        data: serde_json::json!({ "session": session, "request_id": request_id,
            "decision": decision, "decided_by": "operator",
            "reason": (!reason.is_empty()).then_some(reason) }),
    });
    if let Err(e) = decided {
        return match ledger.permission_decider(request_id) {
            Ok(Some(who)) => Response::Error { ok: false, error: format!("already decided by {who}") },
            _ => Response::Error { ok: false, error: format!("recording the decision failed: {e}") },
        };
    }

    // (2) THEN the pipe. allow_always sends the allow bytes (scoping decision 1).
    let msg = match decision {
        "deny" => ParentMessage::PermissionDeny { request_id: request_id.to_owned(), message: reason.to_owned() },
        _ => ParentMessage::PermissionAllow { request_id: request_id.to_owned() },
    };
    let line = match msg.to_line() {
        Ok(l) => l,
        Err(e) => return Response::Error { ok: false, error: format!("building the permission response: {e}") },
    };
    match dispatcher.write_control(session, &line) {
        ControlWrite::Delivered => Response::PermissionDecided { ok: true, request_id: request_id.to_owned(), decision: decision.to_owned() },
        // The decision is DURABLE (answered) but we could not hand it to the
        // worker. §5.3.4's inverse window: the worker is no longer answerable —
        // its re-armed stall timer (Task 8) fires, the ladder drains, finds NO
        // pending (it is decided), and walks its normal bounded restart. Loud to
        // the caller; the ledger truth is correct.
        ControlWrite::NoPipe => Response::Error { ok: false, error: format!(
            "decision recorded, but campd holds no stdin pipe for {session} (adopted or released) — the worker cannot receive it; the stall ladder now owns it") },
        ControlWrite::Failed(e) => Response::Error { ok: false, error: format!(
            "decision recorded, but delivering it to {session} failed: {e}") },
    }
}
```
Re-arm of the stall timer is NOT done here — Task 8's `reconcile_blocked` re-arms on the blocked→unblocked edge on this same wake (the decision is a socket wake). This keeps `serve_permission_decision` free of a `patrol` dependency (layering).

- [ ] **Step 5: Add the dispatch arm** in `event_loop.rs` `drain_lines` (mirror `SessionInterrupt`, ~1048):

```rust
    Ok(Request::SessionPermissionDecision { session, request_id, decision, message }) => {
        let response = control.serve_permission_decision(
            &session, &request_id, &decision, message.as_deref(), ledger, dispatcher);
        respond(&mut conn.stream, &response)?;
    }
```

- [ ] **Step 6: Add the `camp decide` client** (`cmd/decide.rs`): connect, send `session.permission_decision`, print the outcome. Wire an additive `Decide` variant into `main.rs`. `camp watch` shows the BLOCKED row; `camp decide <session> <request_id> allow|deny [--reason …]` answers it.

- [ ] **Step 7: Run the handler + wire tests — PASS.** `cargo test -p camp permission_decision`

- [ ] **Step 8: Commit** `git commit -am "feat(control): session.permission_decision — ledger-before-pipe, first-answer-wins"`

**Mutation caught:** swapping the order (pipe before ledger) breaks the "recorded before delivered" guarantee — `a_decision_is_durable_even_when_the_pipe_is_gone_inverse_window` asserts `permission_decider` is `Some` even when `write_control` returns `NoPipe`, and dies. Dropping the first-answer-wins error handling makes the second decider return `PermissionDecided` (double-answer) — the scaffold test dies. Placing `PermissionDecided` after `Interrupt`, or dropping its `decision` field, breaks `interrupt_and_permission_decided_do_not_shadow_each_other` (CP3-B1).

---

## Task 8: The stall ladder — disarm on BLOCKED, ladder-drains-first, re-arm on decision (patrol + event_loop)

§5.3.3. The two heart-tests live here: **blocked-forever-not-killed** and **ladder-drains-first**.

**Files:**
- Modify: `crates/camp/src/daemon/patrol.rs` (add `blocked: HashSet<String>`; `reconcile_blocked`; `declare_stalls` skip ~664-676; `#[cfg(test)] pub fn is_armed`; the three inline unit guards in `mod tests`)
- Modify: `crates/camp/src/daemon/event_loop.rs` (extract `stall_step`; call it at ~214-215; `reconcile_blocked` each wake; the `stall_step` component test in `mod tests`)
- Test: `crates/camp/src/daemon/event_loop.rs` `#[cfg(test)]` (component, falsifiable); `crates/camp/src/daemon/patrol.rs` `#[cfg(test)]` (the three unit guards); `crates/camp/tests/daemon_patrol.rs` (the integration confirm)

**Interfaces:**
- Consumes: `ledger.blocked_sessions` (Task 2), `self.timers.{arm,disarm}` (the `PatrolTimers` API patrol already uses in `rearm`/`declare_stalls`), `read_channel.drain_all` + `control_step`.
- Produces: `pub fn reconcile_blocked(&mut self, ledger: &Ledger, now: Timestamp) -> anyhow::Result<()>` on `PatrolRuntime`; a test accessor `#[cfg(test)] pub fn is_armed(&self, session: &str) -> bool` (reads `self.timers`) for the invariant-1 disarm guard.

- [ ] **Step 1a: Write the failing COMPONENT test for ladder-drains-first — the platform-independent, genuinely FALSIFIABLE guard (CP3-R2-B1).** It lives in `event_loop.rs`'s `#[cfg(test)] mod tests` and drives the REAL `stall_step` seam (Step 5) with NO watcher running, so the unread `can_use_tool` line is unread BY CONSTRUCTION on every platform — no dependence on FSEvents vs inotify. Removing the pre-ladder drain from `stall_step` makes `declare_stalls` see a not-blocked session and append `agent.stalled` → RED everywhere.

```rust
#[test]
fn stall_step_drains_the_read_channel_before_declaring_a_stall() {
    // Build the runtimes in-process (no mio watcher runs in this test):
    let dir = tempfile::tempdir().unwrap();
    let mut led = /* Ledger::open under dir; append a live session `s` (woke campd)
                     that has claimed an in_progress bead `b-1` */;
    let sessions_dir = dir.path().join("sessions");
    let stdout = sessions_dir.join("s.json"); // the tailed stream file
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::write(&stdout, b"").unwrap();
    let mut read_channel = ReadChannelRuntime::new(sessions_dir.clone(), MAX_STREAM_BYTES_DEFAULT).unwrap();
    read_channel.register(&mut led, "s").unwrap();
    let mut control = ControlRuntime::new(SUBSCRIBER_BUFFER_BYTES_DEFAULT);
    let mut dispatcher = /* test Dispatcher that reports `s` as a live child */;
    let mut patrol = /* PatrolRuntime with `s` tracked and its stall timer armed
                        with a deadline ALREADY IN THE PAST at `now` */;
    let mut conns: HashMap<Token, Conn> = HashMap::new();
    let mut poll = Poll::new().unwrap();
    let now = /* past the armed deadline */;

    // A can_use_tool is sitting UNREAD in the stream file — no watcher, no drain yet.
    std::fs::OpenOptions::new().append(true).open(&stdout).unwrap()
        .write_all(b"{\"type\":\"control_request\",\"request_id\":\"cli-9\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\"}}\n").unwrap();
    assert!(patrol.is_armed("s"), "precondition: the stall timer is armed and past-deadline");

    // Drive the REAL seam.
    stall_step(&mut led, &mut patrol, &mut control, &mut dispatcher, &mut read_channel, &mut conns, &mut poll, now).unwrap();

    // The ladder's FIRST act drained the channel: the pending surfaced BEFORE the stall.
    assert!(led.blocked_sessions().unwrap().contains(&"s".to_owned()), "the unread can_use_tool surfaced as BLOCKED");
    assert_eq!(led.events_of_type(EventType::AgentStalled).unwrap().len(), 0, "no stall was declared against the waiting worker");
    // The StallFire was genuinely POPPED-and-skipped (not disarmed early by a phantom
    // notify): the timer is now disarmed by reconcile_blocked/the skip.
    assert!(!patrol.is_armed("s"), "the popped fire was skipped and the timer disarmed");
}
```

- [ ] **Step 1b: Write the failing INTEGRATION confirm** (`daemon_patrol.rs`) — the end-to-end "a BLOCKED worker is never killed," mirroring `ladder_exhaustion_emits_and_stops`'s style (count events + daemon still live). This is a CONFIRM, not the falsifying guard (its falsifiability is platform-dependent — see the mechanism note); the falsifiability lives in Step 1a + the three unit guards (Step 6).

```rust
#[test]
fn a_blocked_worker_is_never_nudged_restarted_or_killed_past_the_stall_threshold() {
    // The fake worker (CAN_USE_TOOL mode) emits the can_use_tool through its OWN
    // long-lived inherited stdout fd — genuinely notify-suppressed on macOS
    // (tests/control.rs:183-186); on Linux inotify surfaces it, which still
    // reaches BLOCKED, just via the notify path.
    let d = Daemon::scaffold(/* command = fake-agent CAN_USE_TOOL, short stall_after */);
    d.dispatch_one("b-1");
    d.wait_until(|| d.sessions_list().iter().any(|s| s.blocked)); // BLOCKED reached
    let before = d.events_of_type("agent.stalled").len();
    d.advance_and_pump(/* 3 × stall_after */);
    assert_eq!(d.events_of_type("agent.stalled").len(), before, "a BLOCKED worker is never declared stalled");
    assert_eq!(d.events_of_type("session.crashed").iter().filter(|e| e.data["name"] == sess).count(), 0,
        "a BLOCKED worker is never killed");
    assert_eq!(d.events_of_type("session.woke").iter().filter(|e| e.data["name"] == sess).count(), 1,
        "a BLOCKED worker is never respawned");
    assert!(d.sessions_list().iter().any(|s| s.blocked), "still blocked, still live");
    assert!(d.is_alive(), "campd did not wedge");
}
```
(`advance_and_pump`/`is_alive` are thin wrappers over the harness's clock-advance + liveness; add them if not already on `Daemon`. Do NOT use a test-side open+write+close to inject the can_use_tool — per CP3-R2-B1 that FIRES notify and defeats the point; the worker self-emits.)

- [ ] **Step 2: Run — expect FAIL (today `stall_step` does not exist and the ladder restarts a silent worker)**

- [ ] **Step 3: Add the blocked set + `reconcile_blocked` to patrol.** Add `blocked: HashSet<String>` beside `stalled`/`activity` (`patrol.rs:147-155`). Reconcile edge-triggered from ledger truth:

```rust
/// §5.3.3: bring patrol's timers in line with the ledger's BLOCKED set. On the
/// working→blocked edge, DISARM (a waiting worker is not a stalled worker — it
/// must add no wakeup and take no ladder action). On blocked→working (a
/// decision), RE-ARM from `now` (the worker is presumed working again).
pub fn reconcile_blocked(&mut self, ledger: &Ledger, now: Timestamp) -> anyhow::Result<()> {
    let ledger_blocked: HashSet<String> = ledger.blocked_sessions()?.into_iter().collect();
    for s in ledger_blocked.difference(&self.blocked).cloned().collect::<Vec<_>>() {
        self.timers.disarm(&s);
        self.blocked.insert(s);
    }
    for s in self.blocked.difference(&ledger_blocked).cloned().collect::<Vec<_>>() {
        if let Some(t) = self.tracked.get(&s).cloned() {
            self.rearm(&s, &t, now);  // re-arm from zero
        }
        self.blocked.remove(&s);
    }
    Ok(())
}
```

- [ ] **Step 4: Make `declare_stalls` skip a blocked session** (`patrol.rs:664`, before `on_fire`):

```rust
    let Some(tracked) = self.tracked.get(&fire.session) else { continue; };
    // §5.3.3: a BLOCKED session is exempt from the ENTIRE ladder — no
    // agent.stalled, no on_fire (which would burn a restart-budget increment),
    // no action. Belt-and-braces with reconcile_blocked's disarm.
    if self.blocked.contains(&fire.session) {
        self.timers.disarm(&fire.session);
        continue;
    }
    let tracked = tracked.clone();
```

- [ ] **Step 5: Extract the pre-ladder drain + declare into a TESTABLE seam `stall_step`, and call it from the event loop.** The ordering must be pinned by a test that drives the REAL sequence, not one that re-implements it (that is the confirm-but-not-falsify trap of CP3-R2-B1). So factor the `fire_due` → (drain+ingest+reconcile) → `declare_stalls` sequence into one function and have the loop call it in place of the current lines 214-215:

```rust
/// §5.3.3: pop due stall fires, and — the ladder's FIRST act — drain the read
/// channel so a `can_use_tool` whose notify event was lost surfaces as BLOCKED
/// before any stall is declared against a worker that is only waiting on us.
/// Returns whether ledger work was appended (drives `wake_ledger_work`).
/// Guarded on `!is_empty` so the idle path pays nothing (invariant 1).
#[allow(clippy::too_many_arguments)]
fn stall_step(
    ledger: &mut Ledger, patrol: &mut PatrolRuntime, control: &mut ControlRuntime,
    dispatcher: &mut Dispatcher, read_channel: &mut ReadChannelRuntime,
    conns: &mut HashMap<Token, Conn>, poll: &mut Poll, now: Timestamp,
) -> anyhow::Result<bool> {
    let stall_fires = patrol.fire_due(now);
    if !stall_fires.is_empty() {
        if let Err(e) = read_channel.drain_all(ledger) {
            eprintln!("campd: pre-ladder drain failed: {e:#}");
        }
        control_step(ledger, control, dispatcher, patrol, read_channel, conns, poll)?;
        patrol.reconcile_blocked(ledger, now)?;
    }
    Ok(patrol.declare_stalls(ledger, &stall_fires, now)?)
}
```
In the loop (replacing event_loop.rs:214-215):
```rust
    wake_ledger_work |= stall_step(ledger, patrol, control, dispatcher, read_channel, &mut conns, &mut poll, now)?;
```
And add `patrol.reconcile_blocked(ledger, now)?;` in the common post-harvest path (after the main `control_step` ~500) so the NORMAL path (a can_use_tool that arrived this wake, or a decision this wake) disarms/re-arms same-wake. `reconcile_blocked` is idempotent, so both call sites are safe. **Acknowledged (non-blocking):** the `stall_step` `control_step` re-runs the full harvest INCLUDING subscriber fanout, so on a stall-fire wake fanout runs twice; this is idempotent (fanout pumps only NEW file bytes, and the second call finds none — the byte cursor advanced), so no double-delivery. If a future profile shows this matters, narrow it to an ingest-only drain; for cp-3 the guarded double-harvest is correct and rare.

- [ ] **Step 6: Write the three INDEPENDENT unit guards INLINE in `patrol.rs`'s `#[cfg(test)] mod tests`** (beside `declare_stalls_appends…`, using `fixture()` at patrol.rs:1487) — NOT `daemon_patrol.rs`. They seed the PRIVATE `blocked` field and read the `#[cfg(test)] pub fn is_armed` accessor, both invisible to an external integration test. Each pins one mechanism SEPARATELY (CP3-B2) — the integration confirm (Step 1b) passes under either single mutation:

```rust
#[test]
fn permission_pending_disarms_the_stall_timer_so_a_blocked_worker_adds_no_wakeup() {
    // The INVARIANT-1 guard: a BLOCKED session must contribute NOTHING to the
    // poll deadline. Track a session (armed), then reconcile it as blocked.
    let mut patrol = /* PatrolRuntime with session s tracked+armed */;
    let led = /* ledger where s has a pending permission */;
    assert!(patrol.is_armed("s"), "precondition: an armed stall timer");
    patrol.reconcile_blocked(&led, now).unwrap();
    assert!(!patrol.is_armed("s"), "permission.pending DISARMS — a blocked worker adds no wakeup (invariant 1)");
}

#[test]
fn declare_stalls_declares_nothing_for_a_blocked_session_even_with_an_armed_timer() {
    // The SKIP guard, exercised on its own: feed declare_stalls a Stall fire for
    // a session that is in patrol.blocked. Assert ZERO agent.stalled and the
    // timer ends disarmed.
    let mut patrol = /* PatrolRuntime, s tracked, s ∈ patrol.blocked, an armed timer */;
    let mut led = /* empty ledger */;
    let fire = /* a synthetic StallFire for s at `now` */;
    let declared = patrol.declare_stalls(&mut led, &[fire], now).unwrap();
    assert!(!declared, "a blocked session declares nothing");
    assert_eq!(led.events_of_type(EventType::AgentStalled).unwrap().len(), 0);
    assert!(!patrol.is_armed("s"), "the swallowed fire disarms the timer");
}

#[test]
fn a_decision_re_arms_the_stall_timer_from_zero() {
    // The re-arm edge: a blocked-then-decided session gets a fresh armed timer,
    // and subsequent silence stalls normally.
    let mut patrol = /* s tracked, s ∈ patrol.blocked, disarmed */;
    let led = /* ledger where s's permission is now DECIDED (not blocked) */;
    patrol.reconcile_blocked(&led, now).unwrap();
    assert!(patrol.is_armed("s"), "a decision re-arms from zero — the worker is presumed working again");
}
```

- [ ] **Step 7: Run every Task-8 test — PASS.** `cargo test -p camp --lib stall_step_drains_the_read_channel_before_declaring` (component), `cargo test -p camp --test daemon_patrol a_blocked_worker` (integration confirm), and `cargo test -p camp --lib permission_pending_disarms declare_stalls_declares_nothing a_decision_re_arms` (the inline unit guards).

- [ ] **Step 8: Commit** `git commit -am "feat(patrol): BLOCKED disarms the stall ladder; stall_step drains the read channel first (§5.3.3)"`

**Mutation caught (each mechanism pinned INDEPENDENTLY):** removing the pre-ladder drain from `stall_step` — `stall_step_drains_the_read_channel_before_declaring_a_stall` dies on EVERY platform (no watcher, so the line is unread until `stall_step` drains it; without the drain, `declare_stalls` appends `agent.stalled`). Removing `reconcile_blocked`'s disarm — `permission_pending_disarms_the_stall_timer…` dies (timer stays armed → invariant-1 violation). Removing the `declare_stalls` skip — `declare_stalls_declares_nothing_for_a_blocked_session…` dies. Removing the re-arm — `a_decision_re_arms_the_stall_timer_from_zero` dies.

---

## Task 9: Slot exemption + `max_blocked` saturation (dispatch + event_loop)

§5.3.2. A BLOCKED worker does not hold a dispatch slot; crossing `max_blocked` raises a loud fault.

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` (the two gates: `converge` ~514, `dispatch_bead` ~550)
- Modify: `crates/camp/src/daemon/event_loop.rs` (saturation check after ingest)
- Test: `crates/camp/tests/daemon_dispatch.rs`

**Interfaces:**
- Consumes: `ledger.blocked_sessions`, `config.dispatch.max_blocked`.
- Produces: gates that count non-blocked children; a deduped `permission.saturated` emission.

- [ ] **Step 1: Write the failing test** — spawn `max_workers` workers, block all but one; assert a NEW bead still dispatches (a blocked worker freed its slot). Assert crossing `max_blocked` blocked workers emits exactly one `permission.saturated`.

- [ ] **Step 2: Exempt blocked from the slot count.** Both gates change from `self.children.len() >= max_workers` to a non-blocked count. `converge`/`dispatch_bead` hold `&mut Ledger`:

```rust
    let blocked: std::collections::HashSet<String> = ledger.blocked_sessions()?.into_iter().collect();
    let live_slots = self.children.values().filter(|w| !blocked.contains(&w.session)).count();
    if live_slots >= self.config.dispatch.max_workers { return Ok(()); }
```
Apply the same predicate at `dispatch_bead:550`. (Confirm both already thread `ledger`; the explorer confirms `converge(ledger)` and `dispatch_bead(ledger, bead)`.)

- [ ] **Step 3: Emit the saturation fault** in the event loop, after the main harvest, on the crossing edge. Keep a `saturated: bool` on the loop to dedup — emit only on the `<=max → >max` transition, clear on the way back:

```rust
    let n_blocked = ledger.blocked_sessions()?.len();
    let over = n_blocked > config.dispatch.max_blocked;
    if over && !saturated {
        ledger.append(EventInput { kind: EventType::PermissionSaturated, rig: None, actor: "campd".into(), bead: None,
            data: serde_json::json!({ "blocked": n_blocked as u64, "max_blocked": config.dispatch.max_blocked as u64 }) })?;
    }
    saturated = over;
```
(Place where `config` is in scope; thread it if needed. Audit-only event — no fold/refold change.)

- [ ] **Step 4: Run — PASS.** `cargo test -p camp --test daemon_dispatch`

- [ ] **Step 5: Commit** `git commit -am "feat(dispatch): a blocked worker frees its slot; max_blocked saturation fault (§5.3.2)"`

**Mutation caught:** the slot test dies if the blocked filter is removed (ten blocked workers deadlock dispatch — the bug §5.3.2 names). The saturation test dies if the edge-dedup is dropped (it fires every wake) or the threshold comparison is off-by-one.

---

## Task 10: The per-agent `--permission-prompt-tool stdio` flag + incoherent-combo refusal (spawn)

§5.3.1. The flag routes decisions only for a mode that can ask; `bypassPermissions` spawns unchanged; an unclassifiable mode is refused at spawn.

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs` (`build_spec` shared tail after ~214; `resume_argv` ~129-144)
- Modify: `crates/camp/src/daemon/dispatch.rs` (`prepare` ~601 — the fail-fast refusal seam that yields a `dispatch.failed` reason)
- Test: `crates/camp/src/daemon/spawn.rs` `#[cfg(test)]` (the argv assertions ~785-805)

**Interfaces:**
- Produces: `pub fn permission_prompt_flag(permission_mode: Option<&str>) -> Result<Option<&'static str>, String>` — `Ok(Some("stdio"))` for an askable mode (`None`→CLI default, `"default"`, `"acceptEdits"`, `"plan"`), `Ok(None)` for `"bypassPermissions"`, `Err(...)` for an unrecognized mode string.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn permission_prompt_flag_is_added_only_for_askable_modes() {
    for mode in [None, Some("default"), Some("acceptEdits"), Some("plan")] {
        let argv = argv_for_mode(mode); // helper building a HeldStream spec
        assert!(pair_present(&argv, "--permission-prompt-tool", "stdio"),
            "mode {mode:?} can ask → the stdio flag routes its decisions");
    }
    let argv = argv_for_mode(Some("bypassPermissions"));
    assert!(!argv.iter().any(|a| a == "--permission-prompt-tool"),
        "bypassPermissions never asks → no flag, no behaviour change");
    assert!(permission_prompt_flag(Some("wat")).is_err(),
        "an unclassifiable mode is refused, never guessed (invariant 5)");
}
```

- [ ] **Step 2: Run — expect FAIL**

- [ ] **Step 3: Implement the resolution fn + insert the flag** (shared tail, after the `--permission-mode` block at spawn.rs:214, guarded to `HeldStream` — dispatch mode; Null-mode `json` spawns never stream a control plane):

```rust
/// §5.3.1: the flag routes ONLY decisions the CLI would otherwise ask about.
/// A mode that can never ask (bypassPermissions) gets NO flag — adding it would
/// make the CLI refuse the argv. An unrecognized mode cannot be classified, so
/// it is refused at spawn rather than guessed (invariant 5).
pub fn permission_prompt_flag(permission_mode: Option<&str>) -> Result<Option<&'static str>, String> {
    match permission_mode {
        None | Some("default") | Some("acceptEdits") | Some("plan") => Ok(Some("stdio")),
        Some("bypassPermissions") => Ok(None),
        Some(other) => Err(format!(
            "unknown --permission-mode {other:?}: camp cannot tell whether it can ask for a \
             permission decision, so it refuses to spawn rather than guess (control-plane §5.3.1)")),
    }
}
```
In `build_spec`'s `HeldStream` tail:
```rust
        if stdin_mode == StdinMode::HeldStream {
            if let Some(flag) = permission_prompt_flag(agent.permission_mode.as_deref())? {
                arg("--permission-prompt-tool");
                arg(flag);
            }
        }
```
Mirror in `resume_argv` (spawn.rs:141) using `pins.permission_mode` — a resumed turn under an askable mode must keep routing decisions to campd.

- [ ] **Step 4: Route the refusal through `prepare`.** `build_spec` is currently infallible. The cleanest fail-fast seam is `prepare` (`dispatch.rs:601`, already `Result<Prep, String>` whose `String` becomes a `dispatch.failed` reason). Prefer: call `permission_prompt_flag(...)` in `prepare` before `build_spec` and short-circuit with the error (keeps `build_spec` pure-and-total for the argv tests, and puts the evented refusal where dispatch failures already live). If instead you make `build_spec` return `Result`, propagate in `prepare`. Add a test asserting an unknown mode yields a `dispatch.failed` with the §5.3.1 reason.

- [ ] **Step 5: Update the existing argv pin tests** (`spawn.rs:785-805`) — the `permission_mode: None` fixture now also carries `--permission-prompt-tool stdio`; add it to that fixture's expected argv (or switch to an askable-mode fixture). Coordinate the exact argv order with cp-4 at rebase (cp-4 adds `--include-partial-messages` in the stream-flags arm; cp-3's flag is in the shared tail — distinct positions).

- [ ] **Step 6: Run — PASS.** `cargo test -p camp permission_prompt_flag build_spec`

- [ ] **Step 7: Commit** `git commit -am "feat(spawn): --permission-prompt-tool stdio per askable agent; refuse an unclassifiable mode (§5.3.1)"`

**Mutation caught:** dropping the `bypassPermissions → None` arm adds the flag to a bypass agent → the CLI refuses the argv (the F7 regression §5.3.1 warns about); the bypass case dies. Dropping the `Err` arm silently guesses an unknown mode; the unknown-mode assertion dies.

---

## Task 11: The adoption kill — both directions (patrol + event_loop)

§5.3.4. A worker campd can no longer answer is killed with a named, greppable cause and its bead re-hooked; an *answered* quiet worker is NOT killed by adoption (the stall ladder owns it).

**Files:**
- Modify: `crates/camp/src/daemon/patrol.rs` (`adopt` Some(pid) arm ~1192-1245; a shared `crash_unanswerable_permission` helper reusing `kill_pid`/`probe_alive`)
- Modify: `crates/camp/src/daemon/event_loop.rs` (steady-state: a blocked NON-child is killed after the harvest)
- Test: `crates/camp/tests/daemon_patrol.rs`

**Interfaces:**
- Consumes: `ledger.pending_permission_for_session` (Task 2), `ledger.session_by_name(&name).pid` (`mod.rs:428`), `dispatcher.is_child` (`dispatch.rs:204`), `kill_pid` (`patrol.rs:1457`) + `probe_alive`.
- Produces: `const ADOPTION_PERMISSION_REASON: &str = "adoption: unanswerable permission request";` and a helper that kills + appends the named `SessionCrashed`.

**The re-hook mechanism, verified (CP3-B3):** the bead re-hook rides the FOLD's crash-reopen, NOT `observe→Respawn`. `session_ended(…,"crashed")` runs `UPDATE beads SET status='open', claimed_by=NULL … WHERE claimed_by=<session> AND status='in_progress'` (fold.rs:1167) for ANY crash reason, whether or not the session is tracked. `observe`'s re-hook needs the session to be in `patrol.tracked` AND the reason to `starts_with("patrol restart")` (patrol.rs:308-321) — the adopt arm never tracks the killed worker, so broadening `reason_rehooks` would be INERT. So this task appends `SessionCrashed` exactly like the existing `"adopt: process not found"` append (patrol.rs:1181-1191) — **`data:{"name":…, "reason":ADOPTION_PERMISSION_REASON}`, `bead:None`** — and the fold reopens the bead. (Key MUST be `"name"`: `SessionEnd` is `deny_unknown_fields`, so a `"session"` key fails loud at append.) No `observe` change is made.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn adoption_kills_a_worker_with_an_unanswered_permission_and_re_hooks_the_bead() {
    // Ledger: live session (woke_actor=campd, a real killable pid) with an
    // in_progress bead it claimed + a PENDING permission, and NO live child.
    let (mut led, ...) = /* seed woke(campd) + bead in_progress claimed_by=sess + permission.pending */;
    let summary = patrol::adopt(&mut led, &mut patrol, &mut dispatcher).unwrap();
    assert_eq!(summary.crashed, 1);
    // the NAMED, greppable crash:
    let crash = led.events_of_type(EventType::SessionCrashed).unwrap().pop().unwrap();
    assert_eq!(crash.data["name"], sess);
    assert_eq!(crash.data["reason"], "adoption: unanswerable permission request");
    // the bead is DISPATCHABLE AGAIN via the fold crash-reopen:
    let bead = led.get_bead("b-1").unwrap().unwrap();
    assert_eq!(bead.status, "open");
    assert!(bead.claimed_by.is_none(), "reopened + unclaimed → the readiness processor re-dispatches it");
}

#[test]
fn adoption_does_not_kill_an_answered_but_quiet_worker() {
    // Ledger: the request is DECIDED (answered) + a quiet adopted worker with an
    // open bead. Run adopt. Assert: NO "adoption: unanswerable" crash — it is
    // re-armed like any living adopted worker (summary.rearmed == 1); the stall
    // ladder owns its silence (§5.3.4 inverse window).
    assert!(!led.events_of_type(EventType::SessionCrashed).unwrap().iter()
        .any(|e| e.data["reason"] == "adoption: unanswerable permission request"));
    assert_eq!(summary.rearmed, 1);
}

#[test]
fn a_pending_discovered_after_adoption_takes_the_named_kill_not_the_stall_ladder() {
    // CP3-B4: an ADOPTED worker (re-armed at startup, no held stdin) emits a
    // can_use_tool via tailing AFTER adoption. The steady-state event-loop branch
    // must give it the SAME named kill, not the generic ladder.
    let d = Daemon::scaffold_with_adopted_worker(&sess, "b-1"); // tracked, non-child, no pipe
    // Append a can_use_tool to the adopted worker's stdout. The SURFACING
    // mechanism is irrelevant here (a plain test-side append fires notify, which
    // is fine — this test pins the KILL ROUTING, not the drain trigger): once
    // BLOCKED, a non-child must take the NAMED kill, not the ladder.
    d.append_can_use_tool(&sess, CAN_USE_TOOL_LINE);
    d.pump(/* harvest → BLOCKED → steady-state non-child kill */);
    let crash = d.events_of_type("session.crashed").into_iter()
        .find(|e| e.data["name"] == sess).expect("the discovered pending was killed");
    assert_eq!(crash.data["reason"], "adoption: unanswerable permission request");
    assert_eq!(d.events_of_type("agent.stalled").len(), 0, "NOT the stall ladder");
    assert_eq!(d.get_bead("b-1").status, "open"); // re-hooked
}
```

- [ ] **Step 2: Run — expect FAIL**

- [ ] **Step 3: Add the shared kill helper + the startup adoption branch.** Define the helper (mirrors the existing `"adopt: process not found"` append EXACTLY — key `"name"`, `bead: None`):

```rust
/// §5.3.4: a worker campd cannot answer (no live stdin) with an unanswered
/// permission. Kill it and record the NAMED, greppable crash. The fold reopens
/// the bead (session_ended-on-crash), so it becomes dispatchable to a fresh
/// worker — no observe/Respawn is involved (the worker is not, or no longer,
/// tracked for a Respawn; the fold crash-reopen is the mechanism).
fn crash_unanswerable_permission(
    ledger: &mut Ledger, session: &str, rig: Option<String>, pid: i64, exec_timeout: Duration,
) -> Result<()> {
    kill_pid(pid, exec_timeout)?;
    ledger.append(EventInput {
        kind: EventType::SessionCrashed, rig, actor: "campd".into(), bead: None,
        data: serde_json::json!({ "name": session, "reason": ADOPTION_PERMISSION_REASON }),
    })?;
    Ok(())
}
```
In `adopt`'s `Some(pid)` arm, BEFORE the `bead_open` re-arm (`patrol.rs:1197`). The `row.woke_actor == "campd"` guard mirrors the sibling release-kill (patrol.rs:1200) — §10 "never kill in the TUI" (an attended session never gets `--permission-prompt-tool`, so it never folds a `permission.pending`, but the guard is the defensive belt-and-braces):

```rust
    if row.woke_actor == "campd"
        && ledger.pending_permission_for_session(&row.name)?.is_some()
    {
        // §5.3.4: the ledger-before-pipe ordering of §5.3 proves pending ⇒ the
        // response was never sent, so this never kills an ANSWERED worker.
        crash_unanswerable_permission(ledger, &row.name, row.rig.clone(), pid, exec_timeout)?;
        summary.crashed += 1;
        continue; // do NOT adopt_from_row: it is dead; the fold reopened its bead
    }
```
**Note — implicit fold coupling:** the bead re-hook here is not a dedicated code path; it is an implicit coupling to `session_ended`'s crash-reopen (ANY `SessionCrashed` reopens the bead via `claimed_by`, fold.rs:1167). The `adoption_kills_…` test's `bead.status=="open"`/`claimed_by.is_none()` assertions are what pin that coupling so a future fold change cannot silently strand the bead.

- [ ] **Step 4: (No `observe` change.)** CP3-B3 confirmed that broadening `observe`'s `reason_rehooks` is inert for the untracked adoption case — the bead re-hook is the fold crash-reopen (Step 3's `SessionCrashed` → `session_ended` → `UPDATE beads SET status='open'`). Do NOT touch `observe`. This step exists to record the deliberate decision so a later reviewer does not "add" the re-hook that is already handled by the fold.

- [ ] **Step 5: The steady-state (post-adoption-discovered) branch.** In the event loop, after the harvest + `reconcile_blocked`, for any session in `ledger.blocked_sessions()` that is NOT a live child (`!dispatcher.is_child(&s)`), look up its pid (`ledger.session_by_name(&s)?.and_then(|r| r.pid)`) and call the SAME `crash_unanswerable_permission` — a `can_use_tool` that arrived via tailing for an adopted worker takes the same named kill, not the generic stall ladder. The `crashed` session leaves `blocked_sessions` next wake (the live-join in `blocked_sessions`), so the set-shrink dedups a re-kill; if `pid` is `None` (already reaped) skip it — there is nothing to kill and the session is no longer live.

- [ ] **Step 6: Run — PASS.** `cargo test -p camp --test daemon_patrol adoption_kills adoption_does_not_kill a_pending_discovered_after_adoption`

- [ ] **Step 7: Commit** `git commit -am "feat(patrol): adoption kills an unanswerable permission worker with a named cause; bead re-hooks via the fold crash-reopen (§5.3.4)"`

**Mutation caught:** the positive test dies if the pending check or the named-reason append is removed, AND its `bead.status=="open"`/`claimed_by.is_none()` assertions die if the append uses the wrong key `"session"` (loud fold failure) or if the fold crash-reopen is somehow bypassed. The inverse test dies if the kill fires on an *answered* worker (the check keys on `pending`, not "has a permission row"). The CP3-B4 test dies if the steady-state branch routes a discovered pending to the ladder instead of the named kill.

---

## Task 12: End-to-end — a fake worker blocks, surfaces, is answered, continues (tests)

The exit-criteria test: drive the whole stack against a real campd + fake worker.

**Files:**
- Modify: `crates/camp/tests/fake-agent.sh` (a `FAKE_AGENT_CAN_USE_TOOL` mode)
- Modify: `crates/camp/tests/control.rs` (the round-trip, reusing the `Daemon`/`scaffold`/`dispatch_one`/`wait_until` harness)

- [ ] **Step 1: Add the fake-worker mode.** A branch that: reads the task line, emits a `can_use_tool` on stdout (`{"type":"control_request","request_id":"cli-2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo publish"}}}`), then blocks reading stdin until it receives a `control_response` for `cli-2`, then emits a stream line proving it continued, then closes its bead. Mirror the existing `FAKE_AGENT_CONTROL_LOOP` shape (`fake-agent.sh:199`).

- [ ] **Step 2: Write the end-to-end test:**

```rust
#[test]
fn a_worker_blocks_on_can_use_tool_is_answered_and_continues() {
    let d = Daemon::scaffold(/* max_workers, command=fake-agent CAN_USE_TOOL */);
    d.dispatch_one("b-1");
    // (1) it surfaces: sessions.list shows blocked:true
    d.wait_until(|| d.sessions_list().iter().any(|s| s.blocked));
    let sess = /* the blocked session name */;
    // (2) a permission.pending event exists with its cause
    assert!(d.events_of_type("permission.pending").iter().any(|e| e.data["tool_name"] == "Bash"));
    // (3) answer it
    let r = d.request(Request::SessionPermissionDecision { session: sess.clone(), request_id: "cli-2".into(),
        decision: "allow".into(), message: None });
    assert!(matches!(r, Response::PermissionDecided { ok: true, .. }));
    // (4) the decision is a ledger event with who/what
    let dec = d.events_of_type("permission.decided").pop().unwrap();
    assert_eq!(dec.data["decision"], "allow");
    assert_eq!(dec.data["decided_by"], "operator");
    // (5) the worker CONTINUED and unblocked
    d.wait_until(|| !d.sessions_list().iter().any(|s| s.blocked));
}
```

- [ ] **Step 3: Run — PASS (this is the exit criterion).** `cargo test -p camp --test control a_worker_blocks_on_can_use_tool`

- [ ] **Step 4: Commit** `git commit -am "test(control): end-to-end can_use_tool block → answer → continue"`

---

## Task 13: The real-`claude` compat gate (§8) — the paid tier + fixture exercise

§8: a fake worker validates the state machine but "can never validate the contract with a binary camp does not control." Move the permission fixtures from "pinned but not exercised" toward exercised. **The `can_use_tool` round-trip needs a REAL TURN, so it spends API money — it MUST live in the paid `make e2e` tier, NOT `claude_compat.rs`** (whose header documents it as the $0 `CAMP_COMPAT`/`make compat` tier that "spends NO API money"; a real-turn test there would make `make compat` silently spend — flagged by both gate panels).

**Files:**
- Modify: `crates/camp/tests/e2e.rs` (the `CAMP_E2E`-gated, opt-in, local-only real-`claude` suite — the sanctioned envelope for API spend)
- Modify: `crates/camp/tests/fixtures/control/PROVENANCE.md`

- [ ] **Step 1: Add the paid `can_use_tool` round-trip** to `tests/e2e.rs` under the existing `CAMP_E2E` env gate (`make e2e`, opt-in, local-only). Spawn the real pinned CLI under `--permission-prompt-tool stdio` with a mode that can ask, force a tool call the allowlist does not cover, receive the `can_use_tool`, answer with camp's `PermissionAllow` bytes, and assert the worker continues. This is the only test that can prove `permission_allow_response.json`'s bytes are ACCEPTED (PROVENANCE lists it "pinned but not exercised"). Do NOT add any real-turn assertion to `claude_compat.rs`.

- [ ] **Step 2: Add the `dialog_refusal` exercise** to the same `e2e.rs` tier if reachable under `stdio` — assert camp's deterministic refusal does not hang the worker (PROVENANCE's phase-3 obligation: "if the shape is wrong the CLI ignores it and the worker hangs forever"). If it can only be reached with a real turn, it too rides `make e2e`.

- [ ] **Step 3: Update `PROVENANCE.md`** — move `permission_allow_response.json`/`permission_deny_response.json` from "PINNED BUT NOT EXERCISED" to "VERIFIED" once the gate is green, exactly as it did for the interrupt bytes. Keep the pin bumpable.

- [ ] **Step 4: Run the gate locally** (`make e2e`) and record the result. Commit.

```bash
git commit -am "test(compat): paid can_use_tool round-trip + permission fixture exercise (make e2e)"
```

---

## Self-review

**Spec coverage (§5.3 in full, §4.1, §8, §9):**
- §5.3.1 per-agent stdio flag + incoherent-combo refusal → Task 10.
- §5.3.2 slot exemption + max_blocked → Task 9.
- §5.3.3 disarm/re-arm + ladder-drains-first → Task 8.
- §5.3.4 ledger-before-pipe + adoption kill (both ways) + re-hook → Tasks 7 (ordering) + 11 (adoption).
- §4.1 `session.permission_decision` → Task 7; `BLOCKED` in `sessions.list`/fleet → Task 5.
- §9 first-answer-wins (fold invariant) → Task 1; `request_user_dialog` deterministic refusal → UNCHANGED from cp-1 (still handled at `control.rs:1339`); the `initialize` handshake carrying `pending_permission_requests` — cp-1 already sends `initialize`, and §5.3.4 rules that an adopted worker has no live stdin to re-`initialize` on, so cp-3 relies on the adoption kill (Task 11), not redelivery — no new handshake work.
- §8 tests: blocked-forever (Task 8), ladder-drains-first (Task 8), adoption both ways (Task 11), read-on-wake/append-only cursors/backpressure — those are cp-0/cp-1's already-merged tests, unchanged; end-to-end (Task 12); paid real-CLI tier (Task 13).

**Placeholder scan:** every code step carries real code; every test step carries a real assertion and a named mutation. The `test_ledger`/`woke_session`/`permission_scaffold`/`argv_for_mode` names are the module's existing scaffolds (or trivial wrappers over them) — the implementer reuses the neighboring tests' helpers rather than inventing new ones.

**Type consistency:** `permission_prompt_flag`, `serve_permission_decision`, `reconcile_blocked`, `blocked_sessions`/`permission_exists`/`permission_decider`/`pending_permission_for_session`, `ParentMessage::PermissionAllow`/`PermissionDeny`, `Request::SessionPermissionDecision`, `Response::PermissionDecided`, `EventType::{PermissionPending,PermissionDecided,PermissionSaturated}`, `ADOPTION_PERMISSION_REASON`, `DispatchConfig.max_blocked` — each defined once and referenced consistently across tasks.

**Open coordination note (not a spec gap):** Task 10 and cp-4 both edit `spawn.rs`'s argv block; the flags occupy distinct positions (shared tail vs stream-flags arm) and the rebase between siblings is expected — keep cp-3's diff scoped to the permission arm.

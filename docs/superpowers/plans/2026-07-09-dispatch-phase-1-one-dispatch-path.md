# Dispatch Phase 1 — One Dispatch Path + Converse Verb Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **APPROVAL NOTE:**
> date: 2026-07-09 · verdict: **APPROVE** (r2, fresh pass, Opus 4.8 plan
> reviewer, relayed by the team lead — after one REJECT round whose sole
> blocking finding was the `-p camp --lib` invocations against the bin-only
> `camp` crate, fixed in r2). Non-blocking notes carried forward (none gate):
> (1) full nudge text in the ledger is consistent with the ledger's free-text
> role — no Phase 1 change; (2) the §3 consistency line ("live while it runs
> (camp nudge)") is mildly over-broad for attended sessions with no held pipe
> — accepted as the compressed summary matching §8.4's own phrasing; (3)
> Task 2's test may pass immediately as documented — an unexpected FAIL there
> is a real #29-class bug: stop and report to the lead, never patch around it.
> Reviewer-accepted deviations: none. Both in-plan proposals ratified: the
> `camp nudge` verb name and the `session.nudged {session, via, text}` event.
>
> **Ratified ahead of approval (plan review, 2026-07-09):** the two proposals in
> this plan are APPROVED by the reviewer — (a) verb name **`camp nudge`** (gc's
> user-facing analog is `gc nudge`; camp already spells the mechanism "nudge");
> (b) event **`session.nudged {session, via, text}`** (additive, correctly
> partitioned, deny_unknown_fields, log-only; CI `check_vocab.sh` is the
> authoritative additivity backstop).
>
> **Revision history:** r2 (2026-07-09) — plan-review fix: the `camp` crate is
> BIN-ONLY, so every `cargo test -p camp --lib` invocation was corrected to
> `cargo test -p camp --bin camp` (Tasks 6/7); recorded the two ratifications;
> folded in the accepted note to fix the stale `cmd/sling.rs` doc comment
> (Task 1). No design, scope, or test changes.

**Goal:** Remove the second spawner (`/camp:sling`'s attended-teammate instruction) so campd is the sole dispatcher (#29 structurally dissolved), and add the user-facing converse verb `camp nudge` that delivers a turn to any running or exited session over campd's held stdin pipe or `claude --resume`.

**Architecture:** `/camp:sling` becomes a thin wrapper over `camp sling` (enqueue only — same single path as the CLI). A new socket op `Request::Nudge` lets the CLI reach the held-stdin pipe that only campd owns (`Dispatcher::nudge_via_stdin`, already built for patrol); when there is no pipe (worker exited, released, attended session, campd down) the CLI runs `<dispatch.command> -p --resume <sid> "<text>"` synchronously and prints the reply (fixture fact A4/F6). Every delivered turn lands in the ledger as a new camp-specific, log-only event `session.nudged` (invariant 3: nothing hidden). Spec §8.4's "attended teammate is the one surface exception" is deleted in this same PR (operator approved 2026-07-09).

**Tech Stack:** Rust (clap, serde, rusqlite via existing `camp-core::Ledger`), bash test stubs, Claude Code plugin markdown.

**Tracking:** Issue #46; fixes #29. PR branch `phase-1-one-dispatch-path` → `main`. PR description must include "Fixes #29" and "Closes #46".

## Authoritative inputs (read before executing)

1. `docs/design/2026-07-05-gas-camp-design.md` — the spec. §4 decision record is settled.
2. `docs/design/2026-07-09-dispatch-lifecycle.md` — "Final settled model", the Reframe section, and §9 "Phase 1" are the contract. Q1–Q7 are RESOLVED; do not reopen.
3. `docs/design/2026-07-06-assumption-findings.md` — F1–F7 and A4 are BINDING for the two delivery paths (held-stdin live; `--resume` after the turn; A4-4: concurrent resume of a live session is safe).

## Global Constraints

- Branch `phase-1-one-dispatch-path`; never commit to main; no co-author lines in commits.
- TDD strictly: write the failing test, run it, watch it fail, implement, watch it pass.
- Gates before push: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- No panics/unwrap/expect in library code (clippy denies); `#![forbid(unsafe_code)]`; fail fast, no fallbacks, no silenced errors.
- Six primitives, zero roles in code; campd moves work, never reasons about it.
- Vocabulary mirror (invariant 7): new event names go in `CAMP_SPECIFIC_EVENTS` and must not exist in gc source (CI `check_vocab.sh` enforces); new events use `deny_unknown_fields` payload structs; keep the one-transaction event+state property and the refold property green.
- SCOPE EXCLUSIONS (operator phase split): no pack/role content (delivery-aware coder / committer / overseer agents — Phase 3); do NOT touch `crates/camp-core/src/pack.rs` isolation default or spec §12 (Phase 2 owns both); do NOT touch the two flaky daemon tests owned by issue #44 (`worktree_isolation_creates_then_reaps_on_pass`, `a_dead_reader_nudge_fails_loudly`).
- Shared-file caution (parallel siblings): keep edits to `crates/camp/src/main.rs`, `Cargo.toml`/`Cargo.lock`, `event.rs`, `vocab.rs`, `fold.rs` minimal and additive. When a sibling PR merges, rebase onto main and re-run all gates before opening/updating the PR.
- Known flake (issue #44): the two tests above may spuriously fail — rerun to confirm, never silence.
- Crate layout (verified): the `camp` crate is BIN-ONLY — no `lib.rs`; `daemon`/`cmd`/`campdir` are modules of `main.rs`. Its unit tests run as `cargo test -p camp --bin camp <filter>` (or bare `cargo test -p camp <filter>`); `cargo test -p camp --lib` errors with "no library targets found" and is never the intended red/green. `camp-core` is a lib crate; bare `-p camp-core <filter>` is fine.

## Settled design decisions this plan implements (with rationale)

1. **Verb name: `camp nudge <session> "<text>"`** — RATIFIED (plan review 2026-07-09; the design pins the semantics, not the name). Rationale: the vocabulary-mirror invariant; Gas City's user-facing analog is literally `gc nudge` (`cmd/gc/cmd_nudge.go`; the session-message API is its plumbing). Camp already uses "nudge" for exactly this mechanism internally (patrol's `nudge_via_stdin`, the `agent.stalled action="nudge"` vocabulary), so one word means one thing across gc, campd, and the CLI. Rejected: `camp converse`/`say`/`tell`/`message` (no gc analog — snowflake naming, invariant 7).
2. **Two delivery paths, resolved mechanically, never a "mode":** live path = `Request::Nudge` over the campd socket → `Dispatcher::nudge_via_stdin` (campd is the only process holding the pipe); resume path = the CLI itself runs `<dispatch.command> -p --resume <sid> "<text>" --output-format json` synchronously (same argv shape patrol uses) and prints the reply's `result` text (F2 parse rule). campd down is a *designed* state (on-demand daemon; a fresh campd holds no pipes anyway), so an unreachable socket routes to resume — this is the A4 dual path from the design, not an error fallback. A campd that answers with an *error* (broken pipe write, ledger failure) is a hard CLI failure — fail fast.
3. **`camp nudge` never auto-starts campd.** Nudging is conversation, not dispatch; a freshly autostarted campd could not hold the target's pipe anyway. (Mirrors `camp top --statusline`'s no-autostart precedent.)
4. **Durable record: new camp-specific log-only event `session.nudged`** with payload `{session, via: "stdin"|"resume", text}` — RATIFIED (plan review 2026-07-09). Invariant 3 (nothing hidden: an injected turn changes an agent's behavior; the ledger must tell the story) and gc precedent (gc events its session-message requests as `request.result.session.message`; camp's additive spelling avoids adopting gc's request-plumbing namespace). The deliverer appends it: campd (actor `campd`) on the stdin path, the CLI (actor `cli`) on the resume path, in both cases only after confirmed delivery. Order on the stdin path is deliver → append → respond; if the append fails after delivery, campd answers an error naming that (the error surfaces to the caller — invariant 5; the ledger cannot claim a nudge that wasn't delivered).
5. **Resume cwd is resolved from recorded facts, fail-fast:** the session's recorded worktree if any (must still exist on disk — a reaped worktree is an honest, explained error: its project dir context is gone), else the session's rig path from config. Never a silent guess (F3: claude finds the conversation via the project dir derived from cwd).
6. **No reservation, anywhere.** Nothing in this PR adds bead fields, events, or query changes for attended coordination. Test obligation (iv) pins this as a regression guard.
7. **Spec §8.4 edited in this same PR** (operator approved 2026-07-09) plus the minimal consistency edits in §3/§5/§6/§7.4/§8.1/§10 that state the deleted teammate-spawn behavior — spec and code never silently diverge. §12 and §17 are NOT touched (§12 is Phase 2's; §17's A1/A2 are historical probe records, not behavior promises).
8. **A `/camp:nudge` plugin wrapper ships too** (thin: `camp nudge $ARGUMENTS`) so the drive experience the design describes — the human session as overseer conversing with any worker — is discoverable where slinging happens. It replaces the affordance `/camp:sling` loses.

## File map

| File | Action | Why |
|---|---|---|
| `plugin/commands/sling.md` | rewrite | remove the teammate spawn; thin wrapper (Task 1) |
| `plugin/commands/nudge.md` | create | `/camp:nudge` wrapper (Task 8) |
| `plugin/README.md` | modify | `/sling` row loses the teammate clause; add `/nudge` row (Tasks 1, 8) |
| `README.md` | modify | line ~157: teammate promise → nudge/attach story (Task 1) |
| `plugin/skills/worker/SKILL.md` | modify (2 lines) | drop "slung as a teammate" wording (Task 1) |
| `crates/camp/src/cmd/sling.rs` | modify (doc comment) | stale "attended-teammate surface is Phase 12" drift (Task 1) |
| `crates/camp/tests/plugin_parity.rs` | modify | wrapper-count 4→5; new single-path content test (Tasks 1, 8) |
| `crates/camp-core/src/event.rs` | modify | `EventType::SessionNudged` (Task 4) |
| `crates/camp-core/src/vocab.rs` | modify | `"session.nudged"` in `CAMP_SPECIFIC_EVENTS`; reservation-guard tests read these lists (Tasks 3, 4) |
| `crates/camp-core/src/ledger/fold.rs` | modify | `SessionNudged` payload struct + validation (Task 4) |
| `crates/camp-core/src/ledger/mod.rs` | modify | `SessionRow.status` field + `session_by_name()` (Task 5) |
| `crates/camp-core/src/readiness.rs` | tests only | sling-shaped bead is immediately dispatchable (Task 3) |
| `crates/camp/src/daemon/socket.rs` | modify | `Request::Nudge`, `Response::Nudge`, `request_if_up()` (Task 6) |
| `crates/camp/src/daemon/dispatch.rs` | modify | `Dispatcher::child_info()` (Task 6) |
| `crates/camp/src/daemon/event_loop.rs` | modify | `Request::Nudge` arm (Task 6) |
| `crates/camp/src/main.rs` | modify | `Command::Nudge` (Task 7) |
| `crates/camp/src/cmd/nudge.rs` | create | the verb (Task 7) |
| `crates/camp/tests/cli_nudge.rs` | create | e2e obligations (ii) live + resume + overseer (Task 7) |
| `crates/camp/tests/cli_sling.rs` | modify | reservation regression guard (Task 3) |
| `crates/camp/tests/daemon_dispatch.rs` | modify | obligation (i): one woke across wakes (Task 2) |
| `docs/design/2026-07-05-gas-camp-design.md` | modify | §8.4 edit + consistency lines (Task 9) |

## Test-obligation → test mapping (the exit criteria)

| Obligation (design §9 Phase 1 / kickoff) | Test(s) |
|---|---|
| (i) one sling → exactly ONE `session.woke` / one campd dispatch, incl. across subsequent converge wakes | `daemon_dispatch::a_single_sling_dispatches_exactly_once_across_subsequent_wakes` (Task 2); reinforced by existing `tier0_sling_runs_the_whole_contract_with_a_causal_trail` |
| (ii) converse verb delivers to a live worker (held-stdin) AND an exited worker (`claude --resume`), e2e with the fake agent | `cli_nudge::nudge_delivers_into_a_live_workers_held_stdin`, `cli_nudge::nudge_resumes_an_exited_worker_and_prints_the_reply` (Task 7) |
| (iii) `/camp:sling` and `camp sling` behaviorally identical (same single path) | `plugin_parity::sling_wrapper_is_a_thin_wrapper_with_no_second_spawner` (Task 1) + the whole existing `cli_sling.rs` suite (the CLI is the behavior; the wrapper provably adds nothing) |
| (iv) no reservation state exists in the ledger | `cli_sling::sling_creates_an_open_unclaimed_bead_with_no_reservation_state`, `readiness` unit test `a_freshly_slung_bead_is_immediately_dispatchable`, `vocab` test `no_reservation_vocabulary_exists` (Task 3) |

---

### Task 1: Remove the second spawner — `/camp:sling` becomes a thin wrapper

**Files:**
- Modify: `plugin/commands/sling.md`
- Modify: `crates/camp/tests/plugin_parity.rs`
- Modify: `plugin/README.md` (the `/sling` table row, line ~16)
- Modify: `README.md` (line ~157)
- Modify: `plugin/skills/worker/SKILL.md` (description line 3; line ~15)
- Modify: `crates/camp/src/cmd/sling.rs` (doc comment only, line ~16 — no behavior change)

**Interfaces:**
- Consumes: `plugin_parity.rs` helpers `plugin_dir()`, `scannable()`.
- Produces: a `sling.md` whose only executable content is `camp sling $ARGUMENTS` (test obligation iii).

- [ ] **Step 1: Write the failing test** — append to `crates/camp/tests/plugin_parity.rs`:

```rust
/// Test obligation (iii), dispatch-lifecycle Phase 1 (#29, Q6): /camp:sling
/// and `camp sling` are the SAME single path. The wrapper's executable
/// surface is exactly one `camp sling` invocation, and the markdown carries
/// no second-spawner instruction (no teammate spawn). The CLI's behavior is
/// pinned by cli_sling.rs; this proves the wrapper adds nothing to it.
#[test]
fn sling_wrapper_is_a_thin_wrapper_with_no_second_spawner() {
    let md = std::fs::read_to_string(plugin_dir().join("commands/sling.md")).unwrap();
    let scan = scannable(&md);
    let executable: Vec<&str> = scan
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("camp "))
        .collect();
    assert_eq!(
        executable,
        vec!["camp sling $ARGUMENTS"],
        "the wrapper must invoke `camp sling` once and nothing else"
    );
    let lower = md.to_lowercase();
    for banned in ["teammate", "spawn the", "as a teammate", "worker` skill"] {
        assert!(
            !lower.contains(banned),
            "sling.md must not instruct a second spawner (found {banned:?})"
        );
    }
}
```

Note: `scannable()` includes the argument-hint line, which contains no `camp ` token, so the `executable` filter sees only the `!` block.

- [ ] **Step 2: Run it, watch it fail**

Run: `cargo test -p camp --test plugin_parity sling_wrapper_is_a_thin_wrapper_with_no_second_spawner`
Expected: FAIL (current sling.md contains "teammate").

- [ ] **Step 3: Rewrite `plugin/commands/sling.md`** to exactly:

```markdown
---
description: Sling work into the camp — a Tier-0 bead or a formula run. Wraps the camp CLI.
argument-hint: "\"<title>\" [--agent A] [--rig R]  |  --formula NAME [--rig R]"
allowed-tools: Bash(camp:*)
---
Create the work; campd — the one dispatcher — takes it from there
(Tier 0 = one worker dispatch, ~3 ledger writes):

```!
camp sling $ARGUMENTS
```

This command only enqueues; there is no second dispatch path (spec §8.4).
Watch progress with /camp:status or `camp top`. To converse with the running
worker, use /camp:nudge (`camp nudge <session> "<message>"`) — delivered live
into its current turn, or via `claude --resume` after the turn if it has
exited. Report the created bead id (or run id) to the user.
```

- [ ] **Step 4: Update the three prose docs** (no test pins these; keep them truthful):
  - `plugin/README.md` `/sling` row → `| /sling | camp sling | Create work — a Tier-0 bead or a --formula run. Enqueue only; campd is the sole dispatcher (spec §8.4). |`
  - `README.md` line ~157: replace the sentence `…and, when you're present, spawns it as a teammate you can talk to mid-run.` so the paragraph reads: `` `/camp:sling` hands the bead to a **real Claude Code worker** dispatched by campd that follows the plugin's **worker skill** (recall → claim → work → emit milestones → remember → close → exit). Talk to it mid-run with `camp nudge <session> "<message>"`, or attach any time with `claude --resume <session-id>`. `` (keep the following "That one step needs an authenticated `claude` CLI…" sentence unchanged).
  - `plugin/skills/worker/SKILL.md` line 3: description → `Use when you are a camp worker (spawned by campd) assigned a bead — the claim → work → milestones → remember → close lifecycle contract that makes your work durable and visible in the camp ledger.`; line ~15: `(campd passes it to you).` replacing `(campd passes it to you; if you were slung as a teammate, use the name you were given).`
  - `crates/camp/src/cmd/sling.rs` doc comment (line ~16, accepted plan-review note — stale drift): replace the sentence `campd does the spawning; the attended-teammate surface is Phase 12.` with `campd does the spawning; there is no second dispatch path (dispatch-lifecycle Phase 1, #29).` Comment only — the diff must contain zero behavior change.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p camp --test plugin_parity && cargo test -p camp --test plugin_worker_skill`
Expected: PASS (including the pre-existing `every_command_wrapper_uses_only_real_cli_flags` with count still 4 — the `/nudge` wrapper comes in Task 8).

- [ ] **Step 6: Commit**

```bash
git add plugin/commands/sling.md plugin/README.md README.md plugin/skills/worker/SKILL.md crates/camp/src/cmd/sling.rs crates/camp/tests/plugin_parity.rs
git commit -m "fix(plugin): /camp:sling no longer spawns a teammate — one dispatch path (#29)"
```

---

### Task 2: Obligation (i) — a single sling dispatches exactly once, across subsequent wakes

**Files:**
- Modify: `crates/camp/tests/daemon_dispatch.rs` (append one test; reuse existing helpers `fake_agent()`, `camp_ok()`, `scaffold()`, `write_agent()`, `events_json()`, `wait_until()`, `count()`, `Daemon::spawn`)

**Interfaces:**
- Consumes: `scaffold(dir, max_workers, rig_extra) -> (root, rig)`; fake agent env `FAKE_AGENT_HOLD_DIR` (worker blocks until `$DIR/$CAMP_BEAD` exists).
- Produces: nothing downstream; the regression pin itself.

- [ ] **Step 1: Write the failing-or-green test** (this may pass immediately — that is fine; it is the durable pin for obligation (i). Verify it RUNS and asserts what we claim by temporarily reading its output):

```rust
/// Test obligation (i), dispatch-lifecycle Phase 1 (#29): ONE sling → exactly
/// ONE session.woke — including across later converge wakes. converge()
/// re-queries the full dispatchable set on EVERY wake (dispatch.rs), so bead
/// A being held live while bead B's sling pokes campd is precisely the
/// re-dispatch hazard; the sessions-bound exclusion in dispatchable_beads()
/// must keep A invisible. No reservation, no second spawner.
#[test]
fn a_single_sling_dispatches_exactly_once_across_subsequent_wakes() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    write_agent(&root, "dev", "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _daemon = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())],
    );

    let bead_a = camp_ok(&root, &["sling", "bead a", "--agent", "dev"]);
    // A is dispatched and its worker holds (claimed, alive, not closing).
    wait_until(&root, "bead a claimed", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.claimed" && ev["bead"] == bead_a.as_str())
    });

    // A later, unrelated wake: converge() re-runs the full dispatchable
    // query. Bead A must not be dispatched a second time.
    let bead_b = camp_ok(&root, &["sling", "bead b", "--agent", "dev"]);
    wait_until(&root, "bead b claimed", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.claimed" && ev["bead"] == bead_b.as_str())
    });

    // Release both holds; both close pass.
    std::fs::write(hold.join(&bead_a), "").unwrap();
    std::fs::write(hold.join(&bead_b), "").unwrap();
    wait_until(&root, "both beads closed", |e| {
        count(e, "bead.closed") == 2
    });

    let events = events_json(&root);
    let wokes_a = events
        .iter()
        .filter(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead_a.as_str())
        .count();
    let wokes_b = events
        .iter()
        .filter(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead_b.as_str())
        .count();
    assert_eq!(wokes_a, 1, "bead a: exactly one dispatch, ever");
    assert_eq!(wokes_b, 1, "bead b: exactly one dispatch, ever");
    assert_eq!(count(&events, "session.woke"), 2, "no third spawn of any kind");
}
```

(Adjust the `camp_ok` sling argv if the existing scaffold routes via a default agent — copy the argv shape from `tier0_sling_runs_the_whole_contract_with_a_causal_trail` verbatim. If `bead.claimed` events carry the bead in `ev["bead"]` vs `ev["data"]` differently, copy the accessor used by the existing tests.)

- [ ] **Step 2: Run it**

Run: `cargo test -p camp --test daemon_dispatch a_single_sling_dispatches_exactly_once_across_subsequent_wakes -- --nocapture`
Expected: PASS (the exclusion already exists — this is the durable regression pin). If it FAILS, stop: that is a real #29-class bug in campd; debug before proceeding (systematic-debugging skill).

- [ ] **Step 3: Commit**

```bash
git add crates/camp/tests/daemon_dispatch.rs
git commit -m "test(campd): pin one-dispatch-per-sling across converge wakes (#29 obligation i)"
```

---

### Task 3: Obligation (iv) — regression guard: no reservation state in the ledger

**Files:**
- Modify: `crates/camp/tests/cli_sling.rs` (append one test; reuse `camp()`, `scaffold()`, `write_agent()`, `events_json()`, `stop_campd()`)
- Modify: `crates/camp-core/src/readiness.rs` (append one unit test in its `#[cfg(test)]` module, following the existing test style there)
- Modify: `crates/camp-core/src/vocab.rs` (append one unit test module if none exists, else extend)

**Interfaces:**
- Consumes: `Ledger::append`, `camp_core::readiness::dispatchable_beads` via `Ledger` (copy the setup shape from the existing readiness tests in that file).
- Produces: nothing downstream; regression pins.

- [ ] **Step 1: Write the three failing-or-green tests.**

In `crates/camp/tests/cli_sling.rs`:

```rust
/// Test obligation (iv), dispatch-lifecycle Phase 1: no reservation state.
/// A sling writes ONE bead.created whose payload is exactly {title,
/// assignee} — no dispatch/reserved/attended key — and the bead is born
/// open and unclaimed (claim-at-creation was the DEPRECATED design).
#[test]
fn sling_creates_an_open_unclaimed_bead_with_no_reservation_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    let out = camp(&root, &["sling", "reservation guard"]);
    assert!(out.status.success());
    stop_campd(&root);
    let events = events_json(&root);
    let created = events
        .iter()
        .find(|e| e["type"] == "bead.created")
        .expect("sling appends bead.created");
    let keys: Vec<&str> = created["data"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["assignee", "title"], "payload is exactly title+assignee");
    // The event log is append-only truth: nothing between creation and a
    // worker's own claim may transition the bead (no reservation event).
    for e in &events {
        let ty = e["type"].as_str().unwrap();
        assert!(
            !ty.contains("reserv") && !ty.contains("attended"),
            "no reservation vocabulary may appear in the ledger: {ty}"
        );
    }
}
```

(JSON object key iteration is sorted in `serde_json` map order; if the assertion is order-sensitive on your run, compare as a `BTreeSet<&str>` of `["assignee", "title"]` instead.)

In `crates/camp-core/src/readiness.rs` tests (copy the fixture style of the neighboring tests in that module — create a rig'd bead via `Ledger::append(EventInput{ kind: BeadCreated, data: json!({"title": "t", "assignee": "dev"}), .. })`):

```rust
/// Obligation (iv): a freshly slung bead (title+assignee, open, unclaimed)
/// is IMMEDIATELY visible to campd's dispatchable query — nothing reserves
/// or hides it. One dispatch path.
#[test]
fn a_freshly_slung_bead_is_immediately_dispatchable() {
    // ...setup exactly as sibling tests do...
    let dispatchable = ledger.dispatchable_beads().unwrap();
    assert_eq!(dispatchable.len(), 1);
    assert_eq!(dispatchable[0].id, bead_id);
    assert_eq!(dispatchable[0].assignee.as_deref(), Some("dev"));
}
```

In `crates/camp-core/src/vocab.rs` (tests module):

```rust
/// Obligation (iv): the deprecated reservation design leaked no vocabulary.
#[test]
fn no_reservation_vocabulary_exists() {
    for name in GC_MIRRORED_EVENTS.iter().chain(CAMP_SPECIFIC_EVENTS) {
        assert!(
            !name.contains("reserv") && !name.contains("attended"),
            "reservation-era name leaked into the vocabulary: {name}"
        );
    }
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p camp --test cli_sling sling_creates_an_open_unclaimed_bead && cargo test -p camp-core a_freshly_slung_bead_is_immediately_dispatchable && cargo test -p camp-core no_reservation_vocabulary_exists`
Expected: PASS (guards). Any FAIL is a real finding — stop and investigate.

- [ ] **Step 3: Commit**

```bash
git add crates/camp/tests/cli_sling.rs crates/camp-core/src/readiness.rs crates/camp-core/src/vocab.rs
git commit -m "test: regression-guard that no reservation state exists in the ledger (obligation iv)"
```

---

### Task 4: `session.nudged` — the event type, vocabulary entry, and fold

**Files:**
- Modify: `crates/camp-core/src/event.rs` (`EventType::SessionNudged` in the enum, `ALL`, `as_str`)
- Modify: `crates/camp-core/src/vocab.rs` (`"session.nudged"` appended to `CAMP_SPECIFIC_EVENTS`)
- Modify: `crates/camp-core/src/ledger/fold.rs` (match arm + payload struct + tests)

**Interfaces:**
- Produces: `EventType::SessionNudged` (string `"session.nudged"`), payload contract `{session: String, via: "stdin"|"resume", text: String}` — consumed by Tasks 6 and 7. The event is log-only (no state mutation), `rig` = the session's rig (may be null), `bead` = the session's bead if any, actor = deliverer (`campd`|`cli`).

- [ ] **Step 1: Write the failing tests.** In `crates/camp-core/src/ledger/fold.rs` tests module (follow the sibling fold-test style — the existing tests build a ledger, append prerequisite events, then assert accept/reject):

```rust
/// session.nudged (dispatch-lifecycle Phase 1): log-only record of a turn
/// delivered into a session — via the campd-held stdin pipe ("stdin") or
/// claude --resume ("resume"). The session must exist (fail fast on typos);
/// text must be non-empty; unknown fields and unknown vias are rejected
/// (deny_unknown_fields).
#[test]
fn session_nudged_is_log_only_and_validated() {
    let (mut ledger, _dir) = test_ledger(); // whatever the sibling fixture is
    // a registered session to nudge
    ledger.append(EventInput {
        kind: EventType::SessionWoke,
        rig: Some("gc".into()),
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({"name": "camp/dev/1", "agent": "dev", "rig": "gc"}),
    }).unwrap();

    // accepted: stdin and resume
    for via in ["stdin", "resume"] {
        ledger.append(EventInput {
            kind: EventType::SessionNudged,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"session": "camp/dev/1", "via": via, "text": "status?"}),
        }).unwrap();
    }
    // rejected: unknown session
    assert!(ledger.append(EventInput {
        kind: EventType::SessionNudged,
        rig: None, actor: "cli".into(), bead: None,
        data: serde_json::json!({"session": "camp/dev/99", "via": "stdin", "text": "x"}),
    }).is_err());
    // rejected: bogus via
    assert!(ledger.append(EventInput {
        kind: EventType::SessionNudged,
        rig: None, actor: "cli".into(), bead: None,
        data: serde_json::json!({"session": "camp/dev/1", "via": "carrier-pigeon", "text": "x"}),
    }).is_err());
    // rejected: empty text
    assert!(ledger.append(EventInput {
        kind: EventType::SessionNudged,
        rig: None, actor: "cli".into(), bead: None,
        data: serde_json::json!({"session": "camp/dev/1", "via": "stdin", "text": "  "}),
    }).is_err());
    // rejected: unknown field (deny_unknown_fields)
    assert!(ledger.append(EventInput {
        kind: EventType::SessionNudged,
        rig: None, actor: "cli".into(), bead: None,
        data: serde_json::json!({"session": "camp/dev/1", "via": "stdin", "text": "x", "mode": "attended"}),
    }).is_err());
}
```

(Adapt `test_ledger()` and the `session.woke` payload keys to the exact fixtures used by neighboring fold tests — `session_woke_registers_and_end_events_update` in `ledger/mod.rs` shows the minimal accepted woke payload.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p camp-core session_nudged_is_log_only_and_validated`
Expected: FAIL to COMPILE (`EventType::SessionNudged` does not exist) — that is the failing state.

- [ ] **Step 3: Implement.**

`event.rs`: add `SessionNudged,` to the enum (after `SessionCrashed`), to `ALL` (same position), and to `as_str`: `EventType::SessionNudged => "session.nudged",`.

`vocab.rs`: append `"session.nudged",` to `CAMP_SPECIFIC_EVENTS`.

`fold.rs`: add the match arm `EventType::SessionNudged => session_nudged(conn, event),` and:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionNudged {
    session: String,
    via: String,
    text: String,
}

const NUDGE_VIAS: &[&str] = &["stdin", "resume"];

/// `session.nudged` is log-only (dispatch-lifecycle Phase 1, #29): a turn
/// was delivered into a session's conversation — live over the campd-held
/// stdin pipe, or via `claude --resume` after the turn (A4). The named
/// session must exist (fail fast on typos), like worker.milestone's bead.
fn session_nudged(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: SessionNudged = payload(event)?;
    non_empty(event, "text", &p.text)?;
    if !NUDGE_VIAS.contains(&p.via.as_str()) {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("unknown via {:?} (stdin|resume)", p.via),
        });
    }
    let known: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE name = ?1)",
        [&p.session],
        |r| r.get(0),
    )?;
    if !known {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("unknown session {:?}", p.session),
        });
    }
    Ok(())
}
```

(Use the existing `non_empty` helper; if its signature differs, match it.)

- [ ] **Step 4: Run the full core suite** (the vocab-pin partition test must also pass — it fails if the new variant is missing from `CAMP_SPECIFIC_EVENTS`, and the round-trip test in `event.rs` covers the new name automatically):

Run: `cargo test -p camp-core`
Expected: PASS, including `vocab_pin::every_event_type_is_declared_mirrored_or_camp_specific_never_both` and the refold/doctor property tests.

Note: CI's `check_vocab.sh` (gc-compat job) will authoritatively verify `session.nudged` is absent from gc source at the pinned ref; it is absent from the pinned extraction in `gc-vocab.json`, so no collision is expected. Watch that job on the PR.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs
git commit -m "feat(core): session.nudged — log-only record of a delivered converse turn"
```

---

### Task 5: `Ledger::session_by_name` (+ `SessionRow.status`)

**Files:**
- Modify: `crates/camp-core/src/ledger/mod.rs`

**Interfaces:**
- Produces: `pub fn session_by_name(&self, name: &str) -> Result<Option<SessionRow>, CoreError>` returning ANY-status rows; `SessionRow` gains `pub status: String` (`"live"|"stopped"|"crashed"`). Consumed by Task 7 (CLI) and Task 6 (campd enrichment uses `Dispatcher::child_info` instead — see there).

- [ ] **Step 1: Write the failing test** in `ledger/mod.rs` tests (model it on `live_sessions_returns_registry_rows_with_their_woke_provenance`, which shows the full woke payload with `claude_session_id` and `worktree`):

```rust
/// The converse verb's registry lookup (dispatch-lifecycle Phase 1): any
/// session by name, ANY status — an exited worker must be findable for the
/// resume path — carrying claude_session_id, rig, bead, worktree, status.
#[test]
fn session_by_name_finds_live_and_ended_rows_with_provenance() {
    // append session.woke for "camp/dev/1" with claude_session_id +
    // worktree (copy the payload from the live_sessions test), then
    // session.stopped for it.
    // ...
    let row = l.session_by_name("camp/dev/1").unwrap().expect("row exists");
    assert_eq!(row.status, "stopped");
    assert_eq!(row.claude_session_id.as_deref(), Some("7bd2befc-b018-4080-8738-429d541b3646"));
    assert_eq!(row.worktree.as_deref(), Some("/camps/x/worktrees/gc-1"));
    assert!(l.session_by_name("nobody/here/9").unwrap().is_none());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p camp-core session_by_name_finds_live_and_ended_rows_with_provenance`
Expected: FAIL to compile (`session_by_name` and `status` field missing).

- [ ] **Step 3: Implement.** Add `pub status: String,` to `SessionRow`. Extend `live_sessions()`'s SELECT with `s.status` and fill the field. Add:

```rust
/// One registry row by name, any status (dispatch-lifecycle Phase 1: the
/// converse verb must reach exited sessions for the resume path). Same
/// woke-provenance join as live_sessions; a registered session without its
/// session.woke event is ledger corruption.
pub fn session_by_name(&self, name: &str) -> Result<Option<SessionRow>, CoreError> {
    // same SELECT as live_sessions but `WHERE s.name = ?1` and s.status
    // included; return Ok(None) when no row matches.
}
```

Factor the shared row-mapping closure if that stays DRY without contortions; otherwise duplicate the small mapping with a comment tying the two queries together.

- [ ] **Step 4: Run the core suite**

Run: `cargo test -p camp-core`
Expected: PASS (compile errors from the new field point at the one construction site in `live_sessions`; fix them there).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/ledger/mod.rs
git commit -m "feat(core): session_by_name registry lookup (any status) for the converse verb"
```

---

### Task 6: campd side — `Request::Nudge` over the socket, delivered via the held pipe

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs` (`Request::Nudge`, `Response::Nudge`, `request_if_up`, wire pins)
- Modify: `crates/camp/src/daemon/dispatch.rs` (`Dispatcher::child_info`)
- Modify: `crates/camp/src/daemon/event_loop.rs` (the `Request::Nudge` arm)

**Interfaces:**
- Consumes: `Dispatcher::nudge_via_stdin(&mut self, session, text) -> NudgeOutcome` (exists), `spawn::user_message` (wrapped inside it), `EventType::SessionNudged` (Task 4).
- Produces: wire op `{"op":"nudge","session":"<name>","text":"<msg>"}`; responses `{"ok":true,"via":"stdin"}` (delivered) and `{"ok":true,"via":"none"}` (no held pipe — caller resumes); `pub fn request_if_up(path: &Path, request: &Request) -> Result<Option<Response>>` (Ok(None) when the socket does not accept); `pub fn child_info(&self, session: &str) -> Option<(String, Option<String>)>` returning (rig, bead) for a live child. Consumed by Task 7.

- [ ] **Step 1: Write the failing wire-pin tests** in `socket.rs`'s tests:

```rust
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
        Request::Nudge { session: "s".into(), text: "t".into() }
    );
    // Response: untagged — the Nudge variant must win for {"ok":..,"via":..}
    assert_eq!(
        serde_json::to_string(&Response::Nudge { ok: true, via: "stdin".into() }).unwrap(),
        r#"{"ok":true,"via":"stdin"}"#
    );
    assert!(matches!(
        serde_json::from_str::<Response>(r#"{"ok":true,"via":"none"}"#).unwrap(),
        Response::Nudge { via, .. } if via == "none"
    ));
}

#[test]
fn request_if_up_returns_none_when_no_daemon_listens() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("campd.sock");
    // no listener at all
    assert!(request_if_up(&sock, &Request::Status).unwrap().is_none());
    // a stale file that refuses connections is also "not up"
    drop(UnixListener::bind(&sock).unwrap());
    assert!(request_if_up(&sock, &Request::Status).unwrap().is_none());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p camp --bin camp daemon::socket`
Expected: FAIL to compile (no `Request::Nudge`, `Response::Nudge`, `request_if_up`).

- [ ] **Step 3: Implement in `socket.rs`.**

```rust
// in Request (after Adopt):
    /// Deliver one user turn into a live worker's campd-held stdin pipe
    /// (dispatch-lifecycle Phase 1, #29 — the converse verb's live path).
    Nudge { session: String, text: String },

// in Response — INSERT BEFORE Error/Ok (untagged: variant order is the
// deserializer's precedence; {"ok":..,"via":..} must not fall through to Ok):
    /// Nudge disposition: via="stdin" (delivered into the held pipe) or
    /// via="none" (no held pipe — the caller uses the resume path).
    Nudge { ok: bool, via: String },

/// Like `request`, but campd-not-listening is a NORMAL state, not an error
/// (the converse verb's resume path; camp top --statusline's degrade). A
/// socket that accepts but then errors still surfaces as Err — only
/// connect-refused/absent maps to Ok(None).
pub fn request_if_up(path: &Path, request: &Request) -> Result<Option<Response>> {
    if UnixStream::connect(path).is_err() {
        return Ok(None);
    }
    self::request(path, request).map(Some)
}
```

- [ ] **Step 4: Implement `Dispatcher::child_info` in `dispatch.rs`** (next to `is_child`):

```rust
/// (rig, bead) of a live child by session — the nudge handler's event
/// enrichment; None when the session is not our child.
pub fn child_info(&self, session: &str) -> Option<(String, Option<String>)> {
    self.children
        .values()
        .find(|w| w.session == session)
        .map(|w| (w.rig.clone(), Some(w.bead.clone())))
}
```

- [ ] **Step 5: Implement the event-loop arm** in `event_loop.rs::drain_lines`, after the `Request::Adopt` arm:

```rust
Ok(Request::Nudge { session, text }) => {
    // Deliver → record → respond (invariant 3: the injected turn is a
    // ledger fact; invariant 5: a post-delivery append failure surfaces
    // to the caller — the ledger must not claim what was not delivered,
    // and the caller must not believe what the ledger does not hold).
    let response = match dispatcher.nudge_via_stdin(&session, &text) {
        NudgeOutcome::Delivered => {
            let (rig, bead) = dispatcher
                .child_info(&session)
                .map(|(r, b)| (Some(r), b))
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
                Ok(_) => Response::Nudge { ok: true, via: "stdin".into() },
                Err(e) => Response::Error {
                    ok: false,
                    error: format!(
                        "turn delivered into {session} but recording session.nudged failed: {e}"
                    ),
                },
            }
        }
        NudgeOutcome::NoPipe => Response::Nudge { ok: true, via: "none".into() },
        NudgeOutcome::Failed(e) => Response::Error {
            ok: false,
            error: format!("stdin nudge of {session} failed: {e}"),
        },
    };
    respond(&mut conn.stream, &response)?;
}
```

(Bring `NudgeOutcome` into scope; `EventType`/`EventInput` are already imported in this module — check and reuse.)

- [ ] **Step 6: Run the daemon unit/integration tests**

Run: `cargo test -p camp --bin camp && cargo test -p camp --test daemon_lifecycle`
Expected: PASS, including `unknown_op_is_rejected` (nudge is now a known op) and the untagged-order pins.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/daemon/socket.rs crates/camp/src/daemon/dispatch.rs crates/camp/src/daemon/event_loop.rs
git commit -m "feat(campd): Request::Nudge — deliver a user turn into a worker's held stdin"
```

---

### Task 7: `camp nudge` — the CLI verb, both delivery paths, end-to-end (obligation ii)

**Files:**
- Create: `crates/camp/src/cmd/nudge.rs`
- Modify: `crates/camp/src/main.rs` (mod decl + `Command::Nudge` + match arm)
- Create: `crates/camp/tests/cli_nudge.rs`
- Create (test asset): `crates/camp/tests/claude-or-agent.sh` (executable)

**Interfaces:**
- Consumes: `Ledger::session_by_name` (Task 5), `socket::{request_if_up, Request::Nudge, Response}` (Task 6), `EventType::SessionNudged` (Task 4), `CampConfig::{load, rig}`, `config.dispatch.command`, fake agent env contract (`FAKE_AGENT_NUDGE_CLOSE`), F2/F6/A4 mechanics.
- Produces: `camp nudge <session> <text>` — stdin path prints a delivered notice; resume path prints the reply's `result` text; both append `session.nudged`.

- [ ] **Step 1: Write the failing unit tests for the envelope parser** (in `cmd/nudge.rs`'s own `#[cfg(test)]` module — write the module and tests first; the file won't compile until Step 3, which is the failing state):

```rust
#[test]
fn parse_result_text_extracts_the_result_element() {
    let envelope = r#"[
        {"type":"system","subtype":"init"},
        {"type":"assistant"},
        {"type":"result","is_error":false,"result":"NUDGE-REPLY","session_id":"sid"}
    ]"#;
    assert_eq!(parse_result_text(envelope.as_bytes()).unwrap(), "NUDGE-REPLY");
}

#[test]
fn parse_result_text_fails_fast_on_error_results_and_junk() {
    let err_env = r#"[{"type":"result","is_error":true,"result":"boom","session_id":"s"}]"#;
    assert!(parse_result_text(err_env.as_bytes()).is_err());
    assert!(parse_result_text(b"[]").is_err());
    assert!(parse_result_text(b"not json").is_err());
}
```

- [ ] **Step 2: Write the failing e2e tests** in `crates/camp/tests/cli_nudge.rs`. Copy the harness helpers from `daemon_dispatch.rs` (`fake_agent()`, `camp()`, `camp_ok()`, `scaffold()`, `write_agent()`, `events_json()`, `wait_until()`, `count()`, `Daemon`) — each test file in this repo is self-contained; adjust `scaffold` so `[dispatch] command` is parameterizable (the resume test points it at `claude-or-agent.sh`).

Test asset `crates/camp/tests/claude-or-agent.sh` (chmod +x):

```bash
#!/usr/bin/env bash
# Dual-role claude stand-in for the converse-verb e2e (dispatch-lifecycle
# Phase 1). As campd's [dispatch].command it execs the fake agent (worker
# contract). Invoked with --resume (the CLI's nudge resume path) it records
# its argv + cwd and prints an F2-shaped result envelope.
set -euo pipefail
for arg in "$@"; do
  if [ "$arg" = "--resume" ]; then
    : "${NUDGE_STUB_LOG:?claude-or-agent: NUDGE_STUB_LOG must be set for the resume role}"
    printf 'argv:%s\ncwd:%s\n' "$*" "$(pwd)" > "$NUDGE_STUB_LOG"
    echo '[{"type":"result","is_error":false,"result":"STUB-REPLY","session_id":"stub"}]'
    exit 0
  fi
done
: "${FAKE_AGENT:?claude-or-agent: FAKE_AGENT must point at fake-agent.sh}"
exec "$FAKE_AGENT" "$@"
```

The three e2e tests:

```rust
/// Obligation (ii), live half: the converse verb delivers a turn into a
/// LIVE worker over the campd-held stdin pipe. FAKE_AGENT_NUDGE_CLOSE makes
/// the fake agent read the task line then BLOCK until a later stdin line
/// arrives; the nudge is that line, and the agent then closes pass — the
/// delivery is proven by the close.
#[test]
fn nudge_delivers_into_a_live_workers_held_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    write_agent(&root, "dev", "");
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT_NUDGE_CLOSE", "1")]);
    let bead = camp_ok(&root, &["sling", "hold for a nudge", "--agent", "dev"]);
    wait_until(&root, "claimed", |e| {
        e.iter().any(|ev| ev["type"] == "bead.claimed" && ev["bead"] == bead.as_str())
    });
    let events = events_json(&root);
    let session = events
        .iter()
        .find(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str())
        .unwrap()["data"]["name"]
        .as_str()
        .unwrap()
        .to_owned();

    let out = camp(&root, &["nudge", &session, "please wrap up"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("delivered"), "stdout: {stdout}");

    wait_until(&root, "nudged worker closes", |e| count(e, "bead.closed") == 1);
    let events = events_json(&root);
    let nudged = events.iter().find(|e| e["type"] == "session.nudged").unwrap();
    assert_eq!(nudged["data"]["via"], "stdin");
    assert_eq!(nudged["data"]["session"], session.as_str());
    assert_eq!(nudged["data"]["text"], "please wrap up");
    assert_eq!(nudged["actor"], "campd");
    assert_eq!(count(&events, "session.woke"), 1, "converse never dispatches");
}

/// Obligation (ii), resume half: an EXITED worker is reached via
/// `<command> -p --resume <sid> <text>` run from the session's recorded
/// cwd, and the reply's result text is printed. campd is STOPPED first —
/// resume needs no daemon (A4/F6).
#[test]
fn nudge_resumes_an_exited_worker_and_prints_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    // scaffold with [dispatch].command = claude-or-agent.sh
    let (root, rig) = scaffold_with_command(dir.path(), 4, &claude_or_agent_path());
    write_agent(&root, "dev", "");
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT", &fake_agent())]);
    let bead = camp_ok(&root, &["sling", "run and exit", "--agent", "dev"]);
    wait_until(&root, "worker done", |e| {
        count(e, "bead.closed") == 1 && count(e, "session.stopped") >= 1
    });
    let events = events_json(&root);
    let woke = events
        .iter()
        .find(|e| e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str())
        .unwrap();
    let session = woke["data"]["name"].as_str().unwrap().to_owned();
    let sid = woke["data"]["claude_session_id"].as_str().unwrap().to_owned();
    camp_ok(&root, &["stop"]); // resume path must not need campd

    let log = dir.path().join("stub.log");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_camp"))
        .args(["nudge", &session, "how did it go?"])
        .env("CAMP_DIR", &root)
        .env("NUDGE_STUB_LOG", &log)
        .env("FAKE_AGENT", fake_agent())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "STUB-REPLY");

    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains(&format!("--resume {sid}")), "log: {logged}");
    assert!(logged.contains("how did it go?"));
    let cwd_line = logged.lines().find(|l| l.starts_with("cwd:")).unwrap();
    assert_eq!(
        std::fs::canonicalize(cwd_line.trim_start_matches("cwd:")).unwrap(),
        std::fs::canonicalize(&rig).unwrap(),
        "resume runs in the session's recorded rig cwd (F3)"
    );
    let events = events_json(&root);
    let nudged = events.iter().find(|e| e["type"] == "session.nudged").unwrap();
    assert_eq!(nudged["data"]["via"], "resume");
    assert_eq!(nudged["actor"], "cli");
}

/// "Any running session — worker or overseer": a live hook-registered
/// attended session is not campd's child (no pipe), so campd answers
/// via="none" and the CLI converses over concurrent resume (A4-4).
#[test]
fn nudge_reaches_a_live_attended_session_via_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold_with_command(dir.path(), 4, &claude_or_agent_path());
    write_agent(&root, "dev", "");
    let _daemon = Daemon::spawn(&root, &[]); // campd up: the via="none" branch
    camp_ok(&root, &[
        "session", "register", "--name", "attended/abc", "--agent", "attended",
        "--rig", "gc", "--session-id", "0e0e0e0e-1111-4222-8333-444444444444",
    ]);
    let log = dir.path().join("stub.log");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_camp"))
        .args(["nudge", "attended/abc", "overseer ping"])
        .env("CAMP_DIR", &root)
        .env("NUDGE_STUB_LOG", &log)
        .env("FAKE_AGENT", fake_agent())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "STUB-REPLY");
    assert!(std::fs::read_to_string(&log).unwrap().contains("--resume 0e0e0e0e"));
    let _ = rig; // rig used by scaffold only
}
```

Plus the failure-shape tests (fail fast, clear messages):

```rust
#[test]
fn nudge_unknown_session_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let out = camp(&root, &["nudge", "no/such/session", "hello"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no session named"), "stderr: {err}");
}

#[test]
fn nudge_without_a_claude_session_id_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    camp_ok(&root, &[
        "session", "register", "--name", "attended/nosid", "--agent", "attended", "--rig", "gc",
    ]);
    let out = camp(&root, &["nudge", "attended/nosid", "hello"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("claude session id"), "stderr: {err}");
}
```

(Exact helper adjustments: `scaffold_with_command` = the `daemon_dispatch::scaffold` body with the `[dispatch] command = "<path>"` line parameterized; `claude_or_agent_path()` = `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/claude-or-agent.sh")`. If `camp()` in the copied harness passes `CAMP_DIR` via env already, use it for the nudge invocations and add the two extra envs. If `session register` requires the rig to exist in config, the scaffold already writes one rig named `gc` — reuse its name.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p camp --test cli_nudge`
Expected: FAIL to compile / "unrecognized subcommand 'nudge'" — the failing state.

- [ ] **Step 4: Implement `crates/camp/src/cmd/nudge.rs`:**

```rust
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::{Ledger, SessionRow};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp nudge <session> "<text>"` (dispatch-lifecycle Phase 1, #29 — the
/// converse verb, mirror of `gc nudge`/session-message): send one user turn
/// to any registered session. Live path: campd holds the worker's stream
/// stdin (Decision C) — the turn lands in its CURRENT conversation now.
/// Resume path (worker exited, pipe released, attended session, campd
/// down): `<dispatch.command> -p --resume <sid> "<text>"` from the
/// session's recorded cwd — the turn lands after its last one (A4/F6) and
/// the reply prints here. Interactivity is a harness capability, never a
/// dispatch mode: this verb never dispatches, reserves, or spawns workers,
/// and it never auto-starts campd.
pub fn run(camp: &CampDir, session: String, text: String) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let ledger = Ledger::open(&camp.db_path())?;
    let row = ledger.session_by_name(&session)?.ok_or_else(|| {
        anyhow!("no session named {session:?} in the registry; `camp top` lists live sessions")
    })?;
    drop(ledger); // the resume path re-opens for the append; campd may write meanwhile

    if row.status == "live" {
        // campd-not-listening is a normal state (on-demand daemon), and a
        // fresh campd holds no pipes — Ok(None) routes to resume (A4).
        match socket::request_if_up(
            &camp.socket_path(),
            &Request::Nudge { session: session.clone(), text: text.clone() },
        )? {
            Some(Response::Nudge { via, .. }) if via == "stdin" => {
                println!(
                    "delivered into {session}'s live turn (held stdin); \
                     watch `camp events` or its transcript for the reply"
                );
                return Ok(());
            }
            Some(Response::Nudge { .. }) => {} // via="none": no pipe → resume
            Some(other) => bail!("unexpected response to nudge: {other:?}"),
            None => {} // campd down → resume
        }
    }
    resume(camp, &config, &row, &session, &text)
}

fn resume(
    camp: &CampDir,
    config: &CampConfig,
    row: &SessionRow,
    session: &str,
    text: &str,
) -> Result<()> {
    let sid = row.claude_session_id.as_deref().ok_or_else(|| {
        anyhow!("session {session:?} has no recorded claude session id; cannot resume it")
    })?;
    let cwd = resume_cwd(config, row)?;
    // Same argv shape as patrol's nudge-resume (dispatch.rs) — one command
    // vocabulary for resuming a session.
    let out = std::process::Command::new(&config.dispatch.command)
        .arg("-p")
        .arg("--resume")
        .arg(sid)
        .arg(text)
        .arg("--output-format")
        .arg("json")
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {} --resume", config.dispatch.command.display()))?;
    if !out.status.success() {
        bail!(
            "resume of {session} (claude session {sid}) failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let reply = parse_result_text(&out.stdout)?;
    let mut ledger = Ledger::open(&camp.db_path())?;
    ledger.append(EventInput {
        kind: EventType::SessionNudged,
        rig: row.rig.clone(),
        actor: "cli".into(),
        bead: row.bead.clone(),
        data: serde_json::json!({ "session": session, "via": "resume", "text": text }),
    })?;
    println!("{reply}");
    Ok(())
}

/// The session's recorded working directory — where claude computes the
/// project dir that holds this conversation (F3). Recorded worktree first
/// (must still exist: a reaped worktree's project context is gone — an
/// honest error, not a silent wrong-cwd guess), else the rig path.
fn resume_cwd(config: &CampConfig, row: &SessionRow) -> Result<PathBuf> {
    if let Some(wt) = &row.worktree {
        let path = PathBuf::from(wt);
        if !path.is_dir() {
            bail!(
                "session {}'s worktree {} no longer exists (reaped on close); \
                 its conversation cannot be resumed from its project context",
                row.name,
                path.display()
            );
        }
        return Ok(path);
    }
    let rig = row.rig.as_deref().ok_or_else(|| {
        anyhow!(
            "session {:?} has no rig or worktree recorded; cannot choose a resume cwd",
            row.name
        )
    })?;
    Ok(config.rig(rig)?.path.clone())
}

/// F2 parse rule: the envelope is a JSON array; the element with
/// type=="result" carries the reply. is_error==true fails fast with the
/// result text.
fn parse_result_text(stdout: &[u8]) -> Result<String> {
    let envelope: serde_json::Value =
        serde_json::from_slice(stdout).context("resume output is not the F2 JSON envelope")?;
    let result = envelope
        .as_array()
        .and_then(|a| a.iter().rev().find(|e| e["type"] == "result"))
        .ok_or_else(|| anyhow!("resume envelope has no result element (F2)"))?;
    let text = result["result"]
        .as_str()
        .ok_or_else(|| anyhow!("resume result element has no result text (F2)"))?;
    if result["is_error"].as_bool() == Some(true) {
        bail!("resume reported an error: {text}");
    }
    Ok(text.to_owned())
}
```

(Adjust to real signatures: `config.dispatch.command` is a `PathBuf` — check `CampConfig`; `config.rig(name)` returns `Result<&RigConfig, _>` — clone the path.)

- [ ] **Step 5: Wire into `main.rs`:** add `pub mod nudge;` to the `cmd` module list; add to `Command`:

```rust
    /// Send a turn to any running or exited session (the converse verb):
    /// live over campd's held stdin when possible, else `claude --resume`
    /// after its current turn
    Nudge {
        /// Session registry name (see `camp top`)
        session: String,
        /// The message to deliver
        text: String,
    },
```

and the match arm:

```rust
        Command::Nudge { session, text } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::nudge::run(&camp, session, text)
        }
```

- [ ] **Step 6: Run all the new tests, watch them pass**

Run: `cargo test -p camp --test cli_nudge && cargo test -p camp --bin camp cmd::nudge`
Expected: PASS (6 tests). If the live test hangs: the fake agent's `FAKE_AGENT_NUDGE_CLOSE` reads exactly two stdin lines — confirm campd wrote the task line first (it does, at spawn) and that the nudge text is one line.

- [ ] **Step 7: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: PASS (known #44 flakes excepted — rerun those to confirm flake, never silence).

- [ ] **Step 8: Commit**

```bash
git add crates/camp/src/cmd/nudge.rs crates/camp/src/main.rs crates/camp/tests/cli_nudge.rs crates/camp/tests/claude-or-agent.sh
git commit -m "feat(camp): camp nudge — converse with any session, live or via resume (#29)"
```

---

### Task 8: `/camp:nudge` plugin wrapper

**Files:**
- Create: `plugin/commands/nudge.md`
- Modify: `crates/camp/tests/plugin_parity.rs` (wrapper count 4 → 5)
- Modify: `plugin/README.md` (add the `/nudge` row)

**Interfaces:**
- Consumes: `camp nudge <session> <text>` (Task 7).
- Produces: the `/camp:nudge` slash command.

- [ ] **Step 1: Write the failing test change:** in `plugin_parity.rs::every_command_wrapper_uses_only_real_cli_flags`, change the final assertion to `assert_eq!(checked, 5, "expected exactly five command wrappers");`

- [ ] **Step 2: Run it, watch it fail**

Run: `cargo test -p camp --test plugin_parity every_command_wrapper_uses_only_real_cli_flags`
Expected: FAIL (only 4 wrappers exist).

- [ ] **Step 3: Create `plugin/commands/nudge.md`:**

```markdown
---
description: Converse with any running or exited camp session — deliver a turn live (held stdin) or via resume. Wraps the camp CLI.
argument-hint: "<session> \"<message>\""
allowed-tools: Bash(camp:*)
---
Send the message to the session (live into its current turn when campd holds
its stdin; otherwise via `claude --resume` after its turn — the reply prints
below):

```!
camp nudge $ARGUMENTS
```

Session names come from /camp:status or `camp top`. Report the outcome (and
any printed reply) to the user.
```

- [ ] **Step 4: Add the plugin README row** after the `/sling` row:

`| /nudge | camp nudge | Converse with any session — live over campd's held stdin, else via claude --resume. |`

- [ ] **Step 5: Run the plugin test suites**

Run: `cargo test -p camp --test plugin_parity && cargo test -p camp --test plugin_policy`
Expected: PASS (parity scans `camp nudge` → `camp nudge --help` exists; no flags in the hint).

- [ ] **Step 6: Commit**

```bash
git add plugin/commands/nudge.md plugin/README.md crates/camp/tests/plugin_parity.rs
git commit -m "feat(plugin): /camp:nudge — the converse verb in the overseer session"
```

---

### Task 9: Spec §8.4 amendment (+ consistency lines) — same PR, docs only

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md` (§3 line ~72, §5 line ~138, §6 line ~173, §7.4 lines ~289-292, §8.1 line ~339, §8.4 lines ~436-453, §10 line ~543). Do NOT touch §12 (Phase 2 owns it) or §17 (probe records stand as history).

**Interfaces:** none (prose). The design record's "§8.4 disposition" section is the authority for the content.

- [ ] **Step 1: §8.4 — the approved edit.** Replace bullet 1's opening clause `**`campd` dispatches all graph work** — formula steps, orders, patrol respawns —` with `**`campd` dispatches all work** — Tier-0 beads, formula steps, orders, patrol respawns —`. Replace the entire second bullet (“**The one surface exception:** … a UX tweak, not a structural change.)”) with:

```markdown
- **There is no second spawner.** `/camp:sling` and `camp sling` are the
  same single path: enqueue a bead → `campd` dispatches → the worker
  claims. Conversing with any running session — worker or overseer — is a
  uniform verb, `camp nudge <session> "<message>"` (mirror of Gas City's
  `gc nudge`/session-message): delivered live over the worker's campd-held
  stdin pipe, or via `claude --resume` after its current turn (assumption
  A4, §17); every delivered turn is a `session.nudged` ledger event.
  Interactivity is a runtime/harness capability, never a dispatch mode.
  The interactive overseer is the human's own Claude Code session + camp
  plugin (the §4 mental model made literal); persistent overseer / coder /
  committer roles are pack content over the six primitives (§11), mirroring
  Gas City's swarm pack. *(History: v1 shipped an "attended teammate is the
  one surface exception" here; removed 2026-07-09 — it was a second spawner
  racing campd (#29) with no Gas City analog. Decision record:
  docs/design/2026-07-09-dispatch-lifecycle.md, Q6.)*
```

Replace the following paragraph (`Put differently: the *dispatcher* for graph work is always campd, and the *surface* … never with whether you may see it.`) with:

```markdown
Put differently: `campd` is the only dispatcher, for Tier-0 and graph work
alike. How you talk to a worker — live nudge, resume, transcript — never
changes who spawned it or whether you may see it.
```

- [ ] **Step 2: Consistency edits** (each states the deleted behavior; spec and code must not diverge):
  - §3 (line ~72): `— live when attended, by resume when not, by transcript forever` → `— live while it runs (`camp nudge`), by resume after it exits, by transcript forever`.
  - §5 diagram (line ~138): `│  (+ teammates) │  Tier-0 workers spawn here, in view` → `│ (the overseer) │  converse with any worker: camp nudge` (preserve the box column alignment).
  - §6 Agent row (line ~173): `agent definitions; teammates (spawned while you are present); headless-but-present sessions (campd-dispatched)` → `agent definitions; headless-but-present sessions (campd-dispatched; converse via camp nudge / claude --resume)`.
  - §7.4 ladder (lines ~289-292): replace items 1–2 with `1. live worker → send a turn into its running conversation: `camp nudge <session> "<message>"` (campd-held stdin, live); tail its transcript now, or attach with `claude --resume <session-id>`;` and renumber old 3→2, 4→3.
  - §8.1 example (line ~339): `camp:  gc-142 open → worker gc-dev-1 (teammate)` → `camp:  gc-142 open → worker gc/dev/1 (campd-dispatched; converse: camp nudge gc/dev/1 "…")`.
  - §10 (line ~543): `Attended teammates are in the user's face already;` → `Attended sessions (the operator's own) are in the user's face already;`.

- [ ] **Step 3: Verify no code/test pins the old spec text**

Run: `grep -rn "one surface exception\|(+ teammates)" crates/ plugin/ packs/ ci/`
Expected: no hits.

Run: `cargo test --workspace`
Expected: PASS (docs-only change).

- [ ] **Step 4: Commit**

```bash
git add docs/design/2026-07-05-gas-camp-design.md
git commit -m "docs(spec): §8.4 — delete the attended-teammate surface exception; one dispatch path + converse verb (Q6, operator-approved 2026-07-09)"
```

---

### Task 10: Gates, push, PR, CI to a terminal result

- [ ] **Step 1: Full gates locally**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all green. If a #44-owned test fails, rerun that test alone to confirm flake — never silence, never touch those tests.

- [ ] **Step 2: Rebase check.** If any sibling PR (Phase 2 / issue-44) merged to main since branching: `git fetch origin && git rebase origin/main`, resolve (expected touchpoints: `main.rs`, `vocab.rs`, `event.rs`, `fold.rs`, `Cargo.lock`), and re-run Step 1 in full. Never open/update the PR from a branch not rebased on current main.

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin phase-1-one-dispatch-path
gh pr create --base main --title "dispatch Phase 1: one dispatch path + camp nudge converse verb" --body "$(cat <<'EOF'
Implements dispatch-lifecycle Phase 1 (docs/design/2026-07-09-dispatch-lifecycle.md §9):

- /camp:sling no longer spawns an attended teammate — it is a thin wrapper over `camp sling` (enqueue only). campd is the sole dispatcher; the #29 race is structurally gone.
- New converse verb `camp nudge <session> "<text>"` (+ /camp:nudge): delivers a turn to any registered session — live over the campd-held stdin pipe (Request::Nudge), or via `claude --resume` after the turn (A4/F6). Every delivered turn is a `session.nudged` ledger event (camp-specific, log-only, deny_unknown_fields).
- Spec §8.4 amended in this same PR (operator approved 2026-07-09): the "attended teammate is the one surface exception" is deleted; one dispatch path + converse verb + pack-defined drive.

Test obligations (design §9 Phase 1):
- (i) one sling → exactly ONE session.woke across converge wakes: daemon_dispatch::a_single_sling_dispatches_exactly_once_across_subsequent_wakes
- (ii) converse delivers to a live worker (held-stdin) and an exited worker (--resume), e2e with the fake agent: cli_nudge::nudge_delivers_into_a_live_workers_held_stdin, cli_nudge::nudge_resumes_an_exited_worker_and_prints_the_reply
- (iii) /camp:sling ≡ camp sling: plugin_parity::sling_wrapper_is_a_thin_wrapper_with_no_second_spawner (+ the cli_sling suite)
- (iv) no reservation state: cli_sling::sling_creates_an_open_unclaimed_bead_with_no_reservation_state, readiness::a_freshly_slung_bead_is_immediately_dispatchable, vocab::no_reservation_vocabulary_exists

Out of scope (deferred per the operator's phase split): pack/role content (Phase 3), isolation default / spec §12 (Phase 2).

Fixes #29
Closes #46
EOF
)"
```

- [ ] **Step 4: Watch CI to a TERMINAL result**

Run: `gh pr checks --watch`
Expected: all checks green, including the gc-compat vocab job (validates `session.nudged` is additive). On a #44-flake failure: rerun the job, confirm, note it in the PR. Never stop at "CI is running".

- [ ] **Step 5: Report to the team lead** — PR number, CI status, and the four test obligations + exit criteria quoted line by line with evidence (test names, command outputs).

---

## Self-review notes (performed at plan-writing time)

- **Spec coverage:** kickoff scope items → Task 1 (remove spawner), Tasks 4–8 (converse verb, both paths, e2e), Task 9 (spec §8.4), Tasks 2–3 (obligations i & iv), Task 10 (gates/PR/CI). Scope exclusions honored: no `pack.rs`, no §12, no pack agents, no changes to the #44 tests.
- **Verb name and event are RATIFIED** (`camp nudge`, `session.nudged` — plan review 2026-07-09); everything else in the plan is settled design.
- **Reviewer-strikeable extras, called out honestly:** the `/camp:nudge` wrapper (Task 8), the README/plugin-README/worker-SKILL wording touch-ups (Task 1 Step 4), and the §3/§5/§6/§7.4/§8.1/§10 spec consistency lines (Task 9 Step 2) go slightly beyond the narrowest reading of the binding scope; each exists because the narrowest edit would leave a doc stating deleted behavior. If the lead strikes any, the exit criteria still hold.
- **Type consistency check:** `Request::Nudge{session,text}` / `Response::Nudge{ok,via}` / `request_if_up` (Task 6) match their uses in Task 7; `session_by_name` + `SessionRow.status/worktree/bead/rig/claude_session_id` (Task 5) match `cmd/nudge.rs`; `EventType::SessionNudged` payload keys `{session,via,text}` are identical in fold tests (Task 4), event-loop append (Task 6), and CLI append (Task 7).
- **Known adaptation points** (helpers whose exact shape the executor must copy from the named existing tests rather than trust this plan verbatim): `scaffold`/`write_agent`/`wait_until` signatures in `daemon_dispatch.rs`; the fold-test fixture in `fold.rs`; `serde_json` map key ordering in the Task 3 assertion.

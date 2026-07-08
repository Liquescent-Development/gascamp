# Phase 12 — Plugin and Packs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **APPROVED** by the plan-review pass on 2026-07-08 (rev 2, commit 5af1c10). Both blockers verified fixed against merged code (BLOCKER 1 stalled-set clear sites traced; BLOCKER 2 SubagentStop drop loses no contract-required event; `session_status` additive). D1/D5 confirmed. In-PR spec edits sanctioned: the D5 §11 + master-plan corrections (the only spec edit carried) and the additive status-socket `red` field + `camp session register/end` verbs. Nit (no action): a bead-carrying `Owned::Annotate` teammate that stalls correctly counts toward `red`.

**Goal:** Ship the camp Claude Code plugin (machinery only, zero shipped roles) and a starter pack (content), so a Claude Code session drives a camp end to end — sling/status/adopt/events slash commands, lifecycle hooks, a worker skill, and a fleet statusline — all as thin wrappers over the `camp` CLI, with the plugin provably shipping zero agent definitions.

**Architecture:** The plugin is a standard Claude Code plugin under `plugin/` — a `.claude-plugin/plugin.json` manifest, `commands/*.md` slash commands that shell out to the `camp` binary (identical scripting surface, spec §13 guarantee 6 / §13.6), `hooks/` shell scripts registered in `hooks/hooks.json` that append session-lifecycle events fire-and-forget (throttled per spec §16), a `skills/worker/SKILL.md` that IS the worker lifecycle contract, and a `statusline/` snippet that queries the campd socket and renders `▲live ●ready ✖red`. To let the hooks and statusline be *thin* wrappers, two additive CLI/socket surfaces are added to the merged phases (Decisions D1, D2 below): hook-facing session-lifecycle verbs (`camp session register` / `camp session end`) and a `red` count on the status socket response. The starter pack under `packs/starter/` is pure content — two agent definitions, an example order file, and a `guarded-change.toml` formula symlinked into the already-gc-validated corpus so there is one source of truth.

**Tech Stack:** Rust (clap CLI, rusqlite ledger, existing `camp_core`/`camp` crates); POSIX shell for hooks/statusline; Claude Code plugin format (manifest JSON, hooks JSON, command markdown, SKILL.md); Gas City formula-v2 TOML subset. Tests are Rust integration tests under `crates/camp/tests/` (so they run under `cargo test --workspace`, gated by the existing CI `test` checks) driving the plugin scripts against recorded fixture stdin payloads under `plugin/tests/fixtures/`.

**Revision log (rev 2 — addresses plan-review REJECT of rev 1):**
- BLOCKER 1 (D2 `red` mechanism) — fixed: `red` is now backed by a genuinely-new bounded `stalled: HashSet<String>` on `PatrolRuntime`, SET when a stall is declared (nudge/restart/annotate) and CLEARED at every timer-reset/untrack site — because `fire_due` removes fired timers and `declare_stalls` immediately re-arms, so there is no persistent "currently fired" state to read. See revised D2 and Task 2.
- BLOCKER 2 (SubagentStop unsound) — fixed: SubagentStop is **dropped** from the session-end wiring (its payload's `session_id` is the *parent*; ending it would kill the attended session, a §10 violation; and no subagent is registered under any SubagentStop-derivable name). Only the attended top-level session (SessionStart→SessionEnd) and campd-spawned workers (campd birth/SIGCHLD) get lifecycle events. See revised D5 and Task 5.
- D5 correction — the spec/master-plan edits now cover ALL three occurrences (spec §11; master-plan Files line; master-plan content-contract line) and reflect the SubagentStop drop. See Task 10.
- Non-blocking notes folded in: parity test extracts from the fenced `` ```! `` block only (Task 4); register idempotency edge acknowledged (Task 5); rebase reconciles the real daemon-file overlap with phase-13 in socket.rs/event_loop.rs/patrol.rs (Task 11).

**Post-review fix pass (rev 3 — PR #23 Opus code review APPROVE, two LOW findings):**
- LOW 1 (phantom-live attended sessions) — **document-only + follow-up** (my call, reviewer-sanctioned): a hard-killed TUI (no SessionEnd) leaves a `status="live"` row forever, since adopt keeps attended rows and patrol never tracks them. A correct reaper needs a grace threshold (transcript-creation race) and must not mark a live-but-idle session "stopped" (§10/UX tradeoffs) — dedicated design, not a LOW fix. Documented on `patrol::adopt`, `Ledger::live_sessions`, and `plugin/README.md`; follow-up requested from the lead.
- LOW 2 (statusline settings-key mismatch) — **fixed**: docs verified (statusline.md §"Subagent status lines") that `subagentStatusLine` is valid but has a *different* stdin schema (`tasks` array, per-teammate row body), so wiring the fleet-badge script there is a semantic mismatch. Removed `plugin/.claude-plugin/settings.json`; the fleet badge is the operator-wired main `statusLine` only (D6 corrected). Script/README clarify the two keys.

## Global Constraints

Copied verbatim from AGENTS.md invariants, CLAUDE.md, and the kickoff — every task's requirements implicitly include these:

- **Never commit to main.** All work lands via PR branch `phase-12-plugin-packs`. No co-author lines; never mention the assistant in commits.
- **Fail fast.** No fallbacks, no silenced errors, no placeholders. No panics in library code (clippy `unwrap_used`/`expect_used`/`panic` are denied; `unsafe_code` forbidden). Every error surfaces to the caller or lands in the ledger as an event. (Exception, sanctioned by spec §7.2/§16: hooks are *fire-and-forget* — they never block the session and always exit 0 — but they must still emit a visible stderr note on failure, never silently swallow.)
- **TDD strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything.
- **Six primitives, zero roles in code.** If a line of camp *machinery* (plugin included) names a role, it is a bug. Roles are pack content only. Enforced by the zero-agent-definitions repo-policy test (Task 10).
- **Vocabulary mirror.** No new event types or outcome names. Reuse the existing `session.woke` / `session.stopped` event types (already in `camp-core` `EventType`, `vocab.rs`, and `fold.rs`); new event payload structs use `#[serde(deny_unknown_fields)]`; the one-transaction event+state property holds (append via `Ledger::append`, then `poke_best_effort`).
- **Identical scripting surface (spec §13 guarantee 6):** every verb works identically from slash commands and the `camp` CLI. Slash commands are thin wrappers; the parity test (Task 4) enforces flag agreement.
- **Gates green before push:** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`. Then push and FOREGROUND-watch `gh pr checks --watch` to a terminal result (five checks: fmt, clippy, test ×2, gc-compat).
- **File ownership:** own `plugin/`, `packs/starter/`, plugin-parity tests, and the additive core surfaces in D1/D2. The sibling `phase-13-perf-volume` owns `Makefile`, `camp backup`, perf tests — do NOT create a `Makefile` or touch those. A one-line `Command` enum addition in `main.rs` is the only expected overlap point; rebase when they merge.
- **Attended Tier-0 (A1 resolved HOLDS):** attended `camp sling` spawns the worker as a teammate exactly per spec §8.4; the headless+instant-attach fallback is NOT needed and MUST NOT be built. Behavior note for the worker skill and `/sling` copy: a message to an attended teammate is delivered at its next step boundary and answered at the agent's discretion.

---

## Key architectural decisions (flag for plan review)

**D1 — Hook-facing session-lifecycle CLI verbs (additive core surface).**
The plugin hooks must register an attended session and mark its end, but the CLI today only appends `worker.milestone` (via `camp event emit`). The event *types* `session.woke` / `session.stopped` already exist in `camp-core` (added by Phase 8/11) and the ledger already models a hook-registered attended session (`crates/camp-core/src/ledger/mod.rs` test: `woke_actor == "hook:session-start"`, comment "anything else is hook-registered (annotate-only)"). What is missing is the thin CLI verb a hook wraps. This plan adds a `camp session` subcommand group — `register` (appends `session.woke`) and `end` (appends `session.stopped`). No new event types, no vocab changes, no spec change: this is the CLI surface that realizes the already-designed hook-registration path. It reuses the exact `Ledger::append` + `poke_best_effort` pattern of `cmd/event_emit.rs`.

**D2 — `red` on the status socket response (additive, for the fleet badge). [ACCEPTED semantic; MECHANISM revised per BLOCKER 1.]**
The statusline badge `▲live ●ready ✖red` is an explicit Phase 12 deliverable (master plan; spec §11) "fed by the campd socket," but the socket `Status` response (`Response::Status` in `daemon/socket.rs`, built from `StatusSummary`) carries only `live_sessions`, `ready`, `open` — no `red`. Spec §10 defines the red source: "patrol only annotates (`agent.stalled` event + **statusline badge**)". So `red` = the count of sessions currently **stalled** by patrol. The semantic and placement are accepted: campd's status handler (`daemon/event_loop.rs:478`) runs single-threaded inside campd with `PatrolRuntime` already in scope.

**Mechanism (revised — the rev-1 version was wrong).** There is NO persistent "currently fired" state to read: `StallTimers::fire_due` REMOVES fired sessions from the timer store, and `declare_stalls` (patrol.rs:554–633) immediately RE-ARMS a fresh future deadline for nudge/restart/annotate (or untracks on `exhausted`). `PatrolRuntime.activity` is a reset-*pending* set, not a stalled set. So a rev-1 `stalled_count()` reading "fired timers" would be ≈0 at every steady-state status query. Instead, introduce genuinely new bounded in-memory state: **`stalled: HashSet<String>` on `PatrolRuntime`** (initialized empty in `new`).
- **SET** in `declare_stalls`, per action branch: `nudge`, `restart`, and `annotate` (the `_` arm) → `self.stalled.insert(fire.session.clone())`. `exhausted` → do NOT insert; it untracks, so also `self.stalled.remove(&fire.session)` there.
- **CLEAR** at every timer-reset / untrack site: the `apply_tracking` activity loop (patrol.rs:433–435, after `timers.reset`), the `apply_tracking` `TrackOp::Untrack` arm (425–430, after `tracked.remove`), and `drain_touched` (526, after `timers.reset`). (Track/arm of a fresh session also `remove`s defensively against name reuse.)
- **`stalled_count(&self) -> u64`** = `self.stalled.iter().filter(|s| self.tracked.contains_key(*s)).count() as u64` — intersect with `tracked` so a missed clear can never inflate the count (belt-and-suspenders; CLEAR-on-untrack already keeps it a subset).

**Semantic note (state in the plan):** patrol only tracks sessions carrying a bead (workers), so `red` correctly counts stalled **workers**; an attended session registered with no `--bead` is never tracked and never contributes — the right meaning for a fleet badge. Additive only; `camp_core`'s `StatusSummary` (pure ledger derivation) is untouched; the pinned wire-format test is updated.

**D3 — Starter formula is a symlink into the gc-validated corpus (single source of truth).**
`packs/starter/formulas/guarded-change.toml` is a *relative symlink* to `crates/camp-core/tests/fixtures/formulas/valid/guarded-change.toml`. The gc-compat CI job (`.github/workflows/ci.yml` line 66) already validates that corpus directory against the real `gc` compiler, and `crates/camp-core/tests/formula_corpus.rs` pins it by name. The symlink means the starter pack ships *the* corpus file — "passes the Phase 6 gc gate" holds transitively with zero CI-workflow change and zero drift. A Phase 12 test (Task 9) asserts the symlink resolves to the corpus file and that `camp doctor --formula` accepts the pack path.

**D4 — Tests are Rust integration tests; fixtures live under `plugin/tests/fixtures/`.**
The master plan says "hook tests under `plugin/tests/`." To gate them on the existing CI `test` checks (no new workflow job), the executable drivers are Rust integration tests under `crates/camp/tests/` that shell out to the `plugin/` scripts and read recorded stdin payloads from `plugin/tests/fixtures/`. This satisfies "tests under plugin/tests/" (the fixtures + a documented expectation table live there) while running under `cargo test --workspace`.

**D5 — Session-end hook is `SessionEnd`, not `Stop`; and SubagentStop is dropped from session-end wiring. [D5 VERIFIED CORRECT by lead; SubagentStop drop resolves BLOCKER 2.]**
Spec §11 and master plan Phase 12 say the plugin emits session-end events on "Stop and SubagentStop." Both are wrong against current Claude Code docs (hooks.md lifecycle table; retrieved 2026-07-08):
- **`Stop` fires once per *turn*** (after every assistant response), not at session termination — using it for session-end would append N `session.stopped` events per session and the second would hit the fold's "session already ended" error. The hook that fires exactly once at true termination is **`SessionEnd`** (payload `{ session_id, cwd, hook_event_name, source }`, `source ∈ {clear, resume, logout, prompt_input_exit, bypass_permissions_disabled, other}`; exit codes/output ignored — fire-and-forget by design).
- **`SubagentStop` cannot soundly emit a session-end.** Its payload's `session_id` is the **parent** session; `agent_id`/`agent_type` identify the finished subagent. Deriving `attended/{session_id}` from it resolves to the *parent's* registered row — so `camp session end` would end the attended session (a §10 "never kill the attended session" violation), and there is no registration path that gives a subagent a SubagentStop-reconstructable name. So SubagentStop is **not wired** in v1. Attended Tier-0 teammates are visible directly in the agent panel (A1) and record their own ledger events (claim/milestone/close via `--session`, per the worker skill); campd-spawned workers get lifecycle from campd (birth in `dispatch.rs`, end via SIGCHLD §10). Neither needs a hook-driven `session.stopped`.

Therefore the wiring is: **SessionStart → register + adopt (attended top-level session); SessionEnd → session end.** No `Stop`, no `SubagentStop`. Per AGENTS.md ("if implementation reality contradicts the spec, stop and update the spec via PR in the same change"), Task 10 corrects all three occurrences (spec §11; master-plan Files line; master-plan content-contract line) to match, in this same PR.

**D6 — Statusline ships as an opt-in script, not a plugin-set `statusLine`.**
A plugin's bundled `settings.json` supports only `agent` and `subagentStatusLine` (plugins-reference; retrieved 2026-07-08) — it cannot set the main `statusLine`. This matches spec §11's word "*optional* statusline snippet": the plugin ships `plugin/statusline/statusline.sh` and documents wiring it into the user's `~/.claude/settings.json` `statusLine.command`. The script's data path is `camp top --statusline` (Task 3).

*Correction (post-review LOW 2, 2026-07-08):* rev-2 also auto-wired the same script as `subagentStatusLine`. The docs (statusline.md §"Subagent status lines") show that key has a **different** stdin schema — a `tasks` array rendering one row body *per teammate*, not a single-session `{cwd, workspace}` — so reusing the fleet-badge script there is a semantic mismatch. The fleet badge is a main-session concept; the plugin ships NO `settings.json` and the badge is operator-wired into the main `statusLine` only. A per-teammate `subagentStatusLine` would be a separate, purpose-built script (out of scope).

---

## File Structure

**Plugin (machinery only — ships ZERO agent definitions):**
- `plugin/.claude-plugin/plugin.json` — plugin manifest (name, version, description; component dirs auto-discovered).
- `plugin/commands/sling.md` — thin wrapper → `camp sling`.
- `plugin/commands/status.md` — thin wrapper → `camp top`.
- `plugin/commands/adopt.md` — thin wrapper → `camp adopt`.
- `plugin/commands/events.md` — thin wrapper → `camp events`.
- `plugin/hooks/hooks.json` — registers SessionStart and SessionEnd only (NOT Stop, NOT SubagentStop — per D5/BLOCKER 2; PostToolUse breadcrumb present but NOT registered — off by default, §10).
- `plugin/hooks/lib.sh` — shared helpers: locate the camp dir, `throttle` marker check, and a `camp_or_note` fire-and-forget wrapper (runs camp, notes to stderr on failure, always returns 0). JSON parsing happens in `camp --hook-stdin` (Rust), not in shell — no `jq` dependency.
- `plugin/hooks/session-start.sh` — SessionStart: `camp session register --hook-stdin` (idempotent) + `camp adopt`; always exit 0.
- `plugin/hooks/session-end.sh` — SessionEnd: `camp session end --hook-stdin --if-registered`; always exit 0.
- `plugin/hooks/post-tool-use.sh` — OPTIONAL breadcrumb (`camp event emit`), time-window `throttle`d; shipped but unregistered; documented as off-by-default (§10 — patrol watches transcripts).
- `plugin/skills/worker/SKILL.md` — the worker lifecycle contract.
- `plugin/statusline/statusline.sh` — badge renderer → `camp top --statusline`.
- `plugin/README.md` — "machinery only, zero roles" note.
- `plugin/tests/fixtures/*.json` — recorded hook stdin payloads + an expectation table (README).

**Starter pack (content, not machinery):**
- `packs/starter/agents/dev.md` — Claude Code agent definition (frontmatter model/tools/permission + prompt).
- `packs/starter/agents/reviewer.md` — Claude Code agent definition.
- `packs/starter/formulas/guarded-change.toml` — relative symlink → the corpus file (D3).
- `packs/starter/orders.toml` — example §9 `[[order]]` file (one cron, one event order).
- `packs/starter/README.md` — "example to copy, not a dependency."

**Core CLI / daemon (additive — D1, D2):**
- Create `crates/camp/src/cmd/session.rs` — `camp session register` / `camp session end`.
- Modify `crates/camp/src/main.rs` — add `Session { subcommand }` command + `SessionCommand` enum; add `--statusline` flag to `Top`; dispatch arms.
- Modify `crates/camp/src/cmd/top.rs` — `--statusline` non-autostart badge mode.
- Modify `crates/camp/src/daemon/socket.rs` — add `red: u64` to `Response::Status`; update the pinned wire-format test.
- Modify `crates/camp/src/daemon/event_loop.rs` — status handler sets `red` from `patrol.stalled_count()`.
- Modify `crates/camp/src/daemon/patrol.rs` — add `pub fn stalled_count(&self) -> u64`.

**Tests (Rust integration, `crates/camp/tests/`):**
- `cli_session.rs` — session register/end verbs.
- `cli_statusline.rs` — `camp top --statusline` badge + visible degradation.
- `plugin_hooks.rs` — each hook vs. fixture stdin (exit codes, appended events, throttle).
- `plugin_parity.rs` — command markdown ↔ CLI flag parity.
- `plugin_policy.rs` — plugin ships zero agent definitions.
- `starter_pack.rs` — starter formula passes `doctor --formula`; symlink integrity.
- `plugin_worker_skill.rs` — worker SKILL.md references every contract verb.

---

## Task 1: Hook-facing session-lifecycle CLI verbs (`camp session register` / `camp session end`)

Realizes Decision D1. The foundation the SessionStart and SessionEnd hooks wrap. No new event types — appends the existing `session.woke` / `session.stopped` via the `event_emit.rs` pattern.

**Files:**
- Create: `crates/camp/src/cmd/session.rs`
- Modify: `crates/camp/src/main.rs` (add `Session` command + `SessionCommand` subcommand enum + dispatch)
- Test: `crates/camp/tests/cli_session.rs`

**Interfaces:**
- Consumes: `camp_core::event::{EventInput, EventType}` (`EventType::SessionWoke`, `EventType::SessionStopped`); `camp_core::ledger::Ledger` (`open`, `append`); `crate::campdir::CampDir`; `crate::daemon::socket::poke_best_effort`. The `session.woke` payload fold (`crates/camp-core/src/ledger/fold.rs:705`) accepts `{ name (req), agent (req), rig?, claude_session_id?, transcript_path?, pid?, bead?, worktree? }` with `deny_unknown_fields`. The `session.stopped` payload (`SessionEnd`, same file) accepts `{ name (req), exit_code?, signal?, reason?, cause_seq? }`.
- Produces: `pub fn register(camp, args...) -> Result<()>` and `pub fn end(camp, args...) -> Result<()>` in `cmd/session.rs`; a `Session { command: SessionCommand }` variant on `Command` with `SessionCommand::{Register{...}, End{...}}`.

**CLI shape (final):**
- `camp session register --name <N> --agent <A> [--rig <R>] [--session-id <ID>] [--transcript <PATH>] [--pid <P>] [--bead <B>] [--worktree <W>] [--actor <ACTOR>]` — `--actor` defaults to `hook:session-start` (matches the ledger's hook-registration provenance); event `actor` = `--actor`, `kind` = `SessionWoke`, `rig` = `--rig`, `data` = the woke payload.
- `camp session end --name <N> [--reason <R>] [--exit-code <C>] [--signal <S>] [--actor <ACTOR>]` — `--actor` defaults to `hook:session-end`; `kind` = `SessionStopped`, `data` = `{ name, reason?, exit_code?, signal? }`.

**Steps:**

- [ ] **Step 1: Write the failing test** (`crates/camp/tests/cli_session.rs`). Mirror the harness style of `crates/camp/tests/cli_event_emit.rs` (init a temp camp, add a rig, run `camp`, assert via `camp events --json`).

```rust
use assert_cmd::Command;
use tempfile::TempDir;

fn camp(dir: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("camp").unwrap();
    c.arg("--camp").arg(dir.join(".camp"));
    c
}

fn init(dir: &std::path::Path) {
    camp(dir).arg("init").assert().success();
    camp(dir).args(["rig", "add", dir.to_str().unwrap(), "--name", "r", "--prefix", "r"])
        .assert().success();
}

#[test]
fn register_appends_a_session_woke_the_status_shows_it_then_end_stops_it() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init(d);

    camp(d).args(["session", "register", "--name", "camp/attended/1",
                  "--agent", "attended", "--session-id", "abc-123",
                  "--transcript", "/tmp/abc-123.jsonl"])
        .assert().success();

    let events = camp(d).args(["events", "--json"]).output().unwrap();
    let text = String::from_utf8(events.stdout).unwrap();
    assert!(text.lines().any(|l|
        l.contains("\"session.woke\"") && l.contains("camp/attended/1")
        && l.contains("hook:session-start")),
        "expected a hook-registered session.woke, got:\n{text}");

    camp(d).args(["session", "end", "--name", "camp/attended/1", "--reason", "user quit"])
        .assert().success();

    let events2 = camp(d).args(["events", "--json"]).output().unwrap();
    let text2 = String::from_utf8(events2.stdout).unwrap();
    assert!(text2.lines().any(|l|
        l.contains("\"session.stopped\"") && l.contains("camp/attended/1")),
        "expected a session.stopped, got:\n{text2}");
}

#[test]
fn ending_an_unknown_session_fails_loudly() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init(d);
    camp(d).args(["session", "end", "--name", "camp/nobody/9"])
        .assert().failure(); // fold's session_ended returns UnknownSession
}
```

- [ ] **Step 2: Run the test, watch it fail.** Run: `cargo test -p camp --test cli_session`. Expected: FAIL — no `session` subcommand (`error: unrecognized subcommand 'session'`).

- [ ] **Step 3: Implement `cmd/session.rs`.** Model on `cmd/event_emit.rs`. `register` builds the `session.woke` `serde_json::json!` payload from the flags (omitting `None` fields so `deny_unknown_fields` + `#[serde(default)]` accept it), appends with `actor` = the resolved `--actor`, and pokes. `end` builds the `session.stopped` payload and appends. Both `Ledger::open` → `append` → `poke_best_effort`.

- [ ] **Step 4: Wire into `main.rs`.** Add to `enum Command`: `/// Register or end an attended session (hook surface, spec §8.4/§13.2) \n Session { #[command(subcommand)] command: SessionCommand }`. Add `#[derive(Subcommand)] enum SessionCommand { Register {...}, End {...} }` with the flags above. Add the dispatch arm in `run()`: `Command::Session { command } => match command { Register{..} => cmd::session::register(&camp, ..), End{..} => cmd::session::end(&camp, ..) }` (both need `CampDir::resolve`). Add `mod session;` to the `cmd` module.

- [ ] **Step 5: Run tests, watch them pass.** Run: `cargo test -p camp --test cli_session`. Expected: PASS (both tests). Then `cargo clippy -p camp --all-targets -- -D warnings` clean.

- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/cmd/session.rs crates/camp/src/main.rs crates/camp/tests/cli_session.rs
git commit -m "feat(cli): camp session register/end — hook-facing session lifecycle verbs"
```

---

## Task 2: `red` on the status socket response (stalled-session count)

Realizes Decision D2. Additive field so the statusline (Task 3) and `/status` can render the fleet-health triple.

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs` (`Response::Status` gains `red: u64`; update `response_wire_format_is_pinned`)
- Modify: `crates/camp/src/daemon/patrol.rs` (add `pub fn stalled_count(&self) -> u64`)
- Modify: `crates/camp/src/daemon/event_loop.rs` (status handler sets `red`)
- Modify: `crates/camp/src/cmd/top.rs` (render tolerates the new field)
- Test: extend `crates/camp/src/daemon/socket.rs` unit test + a daemon integration assertion in `crates/camp/tests/cli_statusline.rs` (Task 3)

**Interfaces:**
- Consumes: `PatrolRuntime` (in `event_loop.rs`'s status handler at `crates/camp/src/daemon/event_loop.rs:478`); `camp_core::ledger::StatusSummary` (unchanged).
- Produces: `Response::Status { ok, #[serde(flatten)] summary: StatusSummary, red: u64, campd_pid: u32 }`; a new field `stalled: HashSet<String>` on `PatrolRuntime`; `PatrolRuntime::stalled_count(&self) -> u64` = `self.stalled.iter().filter(|s| self.tracked.contains_key(*s)).count() as u64`.

**Steps:**

- [ ] **Step 1a: Write the failing wire-format test.** In `socket.rs`'s `response_wire_format_is_pinned`, update the `Status` case to include `red` (positioned after `open`, before `campd_pid`):

```rust
let status = Response::Status {
    ok: true,
    summary: StatusSummary { live_sessions: vec!["camp/dev/1".to_owned()], ready: 1, open: 2 },
    red: 1,
    campd_pid: 4242,
};
assert_eq!(
    serde_json::to_string(&status).unwrap(),
    r#"{"ok":true,"live_sessions":["camp/dev/1"],"ready":1,"open":2,"red":1,"campd_pid":4242}"#
);
```

- [ ] **Step 1b: Write the failing `stalled_count` unit test in `patrol.rs`** — drive REAL state through the existing test harness (`fixture()`, `woke_event(...)`, `apply_tracking`, `fire_due`, `declare_stalls`; mirror `declare_stalls_appends_agent_stalled_with_the_ladder_action_and_cause` at patrol.rs:1613). Add a `milestone_event(...)` helper next to `woke_event` that appends a `worker.milestone` with `actor` = the session name (so `observe` counts it as activity):

```rust
#[test]
fn stalled_count_counts_stalled_workers_and_clears_on_activity() {
    let (dir, mut ledger, _config, mut patrol) = fixture();
    let transcript = dir.path().join("projects/-p/sid.jsonl");
    let woke = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
    patrol.observe(&woke);
    patrol.apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z")).unwrap();
    assert_eq!(patrol.stalled_count(), 0, "a freshly tracked worker is not stalled");

    // stall timer fires (600s default) → nudge declared → worker is red
    let fires = patrol.fire_due(ts("2026-07-07T07:10:00Z"));
    patrol.declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:10:00Z")).unwrap();
    assert_eq!(patrol.stalled_count(), 1, "a stalled worker counts red");

    // a ledger event from the worker resets its timer → cleared
    let beat = milestone_event(&mut ledger, "t/dev/1", "gc-1"); // actor = "t/dev/1"
    patrol.observe(&beat);
    patrol.apply_tracking(&mut ledger, ts("2026-07-07T07:11:00Z")).unwrap();
    assert_eq!(patrol.stalled_count(), 0, "worker activity clears the stalled flag");
}
```

- [ ] **Step 2: Run, watch both fail.** Run: `cargo test -p camp --lib daemon::socket::` then `cargo test -p camp --lib daemon::patrol::stalled_count`. Expected: FAIL (`red` not a field; `stalled_count`/`milestone_event` not defined).

- [ ] **Step 3: Implement the `stalled` set + `red`.** Add `stalled: HashSet<String>` to `PatrolRuntime`, `stalled: HashSet::new()` in `new()`. Wire SET/CLEAR exactly per D2: in `declare_stalls` insert on `nudge`/`restart`/`_`(annotate), and `remove` on `exhausted`; in `apply_tracking` remove for each session in the `activity` reset loop (after `timers.reset`) and in the `TrackOp::Untrack` arm (after `tracked.remove`); in `drain_touched` remove after `timers.reset`. Add `pub fn stalled_count(&self) -> u64` (the intersect-with-`tracked` form from Interfaces). Add `red: u64` to `Response::Status`; in `event_loop.rs`'s status handler set `red: patrol.stalled_count()`. Update `top.rs::render` to append red on its own line (`"…\nred: {red}\n"`) and update the pinned `top` render unit tests to match.

- [ ] **Step 4: Run all touched tests, watch pass.** Run: `cargo test -p camp --lib daemon`. Expected: PASS (socket wire, patrol stalled_count, top render). Then `cargo test -p camp --test daemon_lifecycle` (status-over-socket integration) — update any status-response assertions to include `red`.

- [ ] **Step 5: Commit.**

```bash
git add crates/camp/src/daemon/socket.rs crates/camp/src/daemon/patrol.rs \
        crates/camp/src/daemon/event_loop.rs crates/camp/src/cmd/top.rs
git commit -m "feat(daemon): status response carries a red (stalled-session) count for the fleet badge"
```

---

## Task 3: `camp top --statusline` — non-autostart fleet badge with visible degradation

The statusline's data path. `--statusline` does a read-only socket `Status` query WITHOUT auto-starting campd, renders `▲{live} ●{ready} ✖{red}`, and on campd-down prints empty stdout + a one-line stderr note and exits 0 — visible degradation, not silence (spec §11 / master plan).

**Files:**
- Modify: `crates/camp/src/main.rs` (add `--statusline` to `Top`)
- Modify: `crates/camp/src/cmd/top.rs` (badge mode via `socket::request`, not `autostart::request_with_autostart`)
- Test: `crates/camp/tests/cli_statusline.rs`

**Interfaces:**
- Consumes: `crate::daemon::socket::{request, Request, Response}` (direct, non-autostart); `crate::campdir::CampDir::socket_path`.
- Produces: `pub fn statusline(camp: &CampDir) -> Result<()>` in `top.rs` (or a `statusline: bool` branch in `top::run`).

**Steps:**

- [ ] **Step 1: Write the failing test** (`crates/camp/tests/cli_statusline.rs`):

```rust
use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn statusline_degrades_visibly_when_campd_is_down() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    Command::cargo_bin("camp").unwrap().arg("--camp").arg(d.join(".camp"))
        .arg("init").assert().success();
    // campd is NOT running; --statusline must NOT auto-start it.
    let out = Command::cargo_bin("camp").unwrap().arg("--camp").arg(d.join(".camp"))
        .args(["top", "--statusline"]).assert().success().get_output().clone();
    assert!(out.stdout.is_empty(), "stdout must be empty when campd is down");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("campd"), "must emit a visible stderr note, got: {stderr:?}");
    // and it must not have started a daemon
    assert!(!d.join(".camp").join("campd.sock").exists()
            || std::os::unix::net::UnixStream::connect(d.join(".camp").join("campd.sock")).is_err());
}
```
Add a second test that starts campd (via `camp top` once, which auto-starts) then asserts `camp top --statusline` stdout matches `^▲\d+ ●\d+ ✖\d+$`.

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --test cli_statusline`. Expected: FAIL (`--statusline` unknown).

- [ ] **Step 3: Implement.** Add `#[arg(long)] statusline: bool` to `Command::Top`. In `top::run`, when `statusline`, call `socket::request(&camp.socket_path(), &Request::Status)`; on `Ok(Response::Status{ summary, red, .. })` print `▲{n_live} ●{ready} ✖{red}` where `n_live = summary.live_sessions.len()`; on `Err` (connect refused / no socket) print nothing to stdout, write `eprintln!("camp: campd is down — statusline unavailable")`, and return `Ok(())` (exit 0). No autostart in this path.

- [ ] **Step 4: Run, watch pass.** Run: `cargo test -p camp --test cli_statusline`. Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/camp/src/main.rs crates/camp/src/cmd/top.rs crates/camp/tests/cli_statusline.rs
git commit -m "feat(cli): camp top --statusline — non-autostart fleet badge, visible degradation"
```

---
## Task 4: Plugin manifest + thin command wrappers + CLI parity test

The four slash commands (`/sling`, `/status`, `/adopt`, `/events`) as thin wrappers over the `camp` CLI, plus the manifest. Parity test enforces the identical-scripting-surface guarantee.

**Files:**
- Create: `plugin/.claude-plugin/plugin.json`
- Create: `plugin/commands/sling.md`, `plugin/commands/status.md`, `plugin/commands/adopt.md`, `plugin/commands/events.md`
- Test: `crates/camp/tests/plugin_parity.rs`

**Interfaces:**
- Consumes: the built `camp` binary (`assert_cmd::Command::cargo_bin("camp")`); the four subcommands and their clap flags (`sling`, `top`, `adopt`, `events` from `main.rs`).
- Produces: the plugin directory the hooks/skills/statusline tasks extend; a parity harness `fn wrapped_subcommand_and_flags(md: &str) -> (String, Vec<String>)`.

**Schema facts (confirmed 2026-07-08, plugins-reference.md):** manifest at `.claude-plugin/plugin.json`; only `name` required (kebab-case); `commands/` auto-discovered (no manifest field needed). Command markdown supports a ` ```! ` fenced block that executes the shell command before Claude sees the content, `$ARGUMENTS` substitution, and frontmatter `description`/`argument-hint`/`allowed-tools`. `${CLAUDE_PLUGIN_ROOT}` is available in commands/hooks.

**Steps:**

- [ ] **Step 1: Write the failing parity test** (`crates/camp/tests/plugin_parity.rs`). For each `plugin/commands/*.md`: extract the wrapped subcommand (first `camp <word>` token in the file) and every `--flag` token in the file (frontmatter + body); run `camp <subcommand> --help`; assert each referenced `--flag` appears in the help output. Map the plugin verb names to CLI subcommands (`status`→`top`, others 1:1).

```rust
use assert_cmd::Command;
use std::path::PathBuf;

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
}
fn help(sub: &str) -> String {
    let out = Command::cargo_bin("camp").unwrap().args([sub, "--help"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap()
}
// Note (a): scan ONLY the executable `!` fenced block + the argument-hint
// frontmatter line — never free prose in the description (which may mention
// another verb and mis-target).
fn bang_block(md: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```!") { in_block = true; continue; }
        if in_block && line.trim_start().starts_with("```") { in_block = false; continue; }
        if in_block { out.push_str(line); out.push('\n'); }
        if let Some(hint) = line.trim().strip_prefix("argument-hint:") { out.push_str(hint); out.push('\n'); }
    }
    out
}
fn wrapped(md: &str) -> (String, Vec<String>) {
    let scan = bang_block(md);
    let sub = scan.split_whitespace()
        .collect::<Vec<_>>().windows(2)
        .find(|w| w[0] == "camp")
        .map(|w| w[1].trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .expect("command md's `!` block must invoke `camp <sub>`");
    let flags = scan.split_whitespace()
        .filter(|t| t.starts_with("--"))
        .map(|t| t.trim_matches(|c: char| !(c.is_alphanumeric() || c == '-')).to_string())
        .filter(|f| f.len() > 2)
        .collect();
    (sub, flags)
}

#[test]
fn every_command_wrapper_uses_only_real_cli_flags() {
    for entry in std::fs::read_dir(plugin_dir().join("commands")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
        let md = std::fs::read_to_string(&path).unwrap();
        let (sub, flags) = wrapped(&md);
        let h = help(&sub);
        for flag in flags {
            assert!(h.contains(&flag),
                "{}: --{} is not a real `camp {}` flag", path.display(), flag, sub);
        }
    }
}
```

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --test plugin_parity`. Expected: FAIL — `plugin/commands` does not exist.

- [ ] **Step 3: Create the manifest** (`plugin/.claude-plugin/plugin.json`):

```json
{
  "name": "camp",
  "version": "0.1.0",
  "description": "Gas Camp control plane for Claude Code — machinery only, zero roles. Thin wrappers over the `camp` CLI, session lifecycle hooks, the worker skill, and a fleet statusline.",
  "author": { "name": "Gas Camp" },
  "keywords": ["gas-camp", "agents", "ledger", "orchestration"]
}
```

- [ ] **Step 4: Create the four command wrappers.** Each is a thin wrapper; `/sling` additionally carries the §8.4/A1 attended-teammate note (prose to Claude, not extra CLI). Example `plugin/commands/sling.md`:

````markdown
---
description: Sling work into the camp — a Tier-0 bead or a formula run. Wraps `camp sling`.
argument-hint: "\"<title>\" [--agent A] [--rig R]  |  --formula NAME [--rig R]"
allowed-tools: Bash(camp:*)
---
Create the work; campd dispatches it (Tier 0 = one worker spawn, ~3 ledger writes):

```!
camp sling $ARGUMENTS
```

Attended (§8.4, A1 resolved HOLDS): if you created a Tier-0 bead and the operator is present, spawn the bead's pack agent as a teammate in this session and have it follow the `worker` skill (recall → claim → work → emit milestones → remember → close → exit). A message you send that teammate lands at its next step boundary and it answers at its discretion — delivery is not preemption. Do NOT fall back to headless+attach; the teammate surface is the design.
````

`plugin/commands/status.md` (wraps `camp top`), `plugin/commands/adopt.md` (wraps `camp adopt`), `plugin/commands/events.md` (wraps `camp events $ARGUMENTS`, argument-hint `"[--json] [--from N] [--to N]"`) follow the same shape with a single ` ```! ` block and no attended note.

- [ ] **Step 5: Run, watch pass.** Run: `cargo test -p camp --test plugin_parity`. Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add plugin/.claude-plugin/plugin.json plugin/commands crates/camp/tests/plugin_parity.rs
git commit -m "feat(plugin): manifest + thin /sling /status /adopt /events wrappers with CLI parity test"
```

---

## Task 5: Lifecycle hooks + `--hook-stdin` + throttle (fixture-driven tests)

SessionStart (register+adopt) and SessionEnd (session end), plus an off-by-default PostToolUse breadcrumb. Hooks are trivial shell that always exit 0; JSON parsing + idempotency live in tested Rust via a `--hook-stdin` mode on `camp session`.

**Files:**
- Modify: `crates/camp/src/cmd/session.rs` (add `--hook-stdin`/`--if-registered` to `register`/`end`; lenient `HookInput` parse)
- Modify: `crates/camp-core/src/ledger/mod.rs` (add `pub fn session_status(&self, name: &str) -> Result<Option<String>, CoreError>` if absent — the existence check for idempotent register)
- Create: `plugin/hooks/hooks.json`, `plugin/hooks/lib.sh`, `plugin/hooks/session-start.sh`, `plugin/hooks/session-end.sh`, `plugin/hooks/post-tool-use.sh`
- Create: `plugin/tests/fixtures/session-start.json`, `session-end.json`, `post-tool-use.json`, and `plugin/tests/fixtures/README.md` (expectation table). (No `subagent-stop.json` — SubagentStop is not wired, per D5/BLOCKER 2.)
- Test: `crates/camp/tests/plugin_hooks.rs`, plus `--hook-stdin` cases in `crates/camp/tests/cli_session.rs`

**Interfaces:**
- Consumes: Task 1's `cmd::session::{register,end}`; the hook stdin schemas (SessionStart `{session_id, transcript_path, cwd, source, model?}`; SessionEnd `{session_id, source}`; PostToolUse `{session_id, tool_name, tool_input}`) — parsed leniently (ignore unknown harness fields).
- Produces: name derivation `attended/<session_id>` (SessionStart register and SessionEnd end derive it identically from the SAME top-level `session_id` — both are the attended top-level session, so they always agree; SubagentStop, whose `session_id` is the parent, is not involved). `register --hook-stdin` is idempotent: it queries `session_status(name)` and no-ops with a note if a row for `name` exists in ANY status (covers repeat SessionStart and — see Step 3 note — a resumed session whose row already ended). `end --hook-stdin --if-registered` no-ops if `name` is not currently live, else appends `session.stopped` (fold enforces live→stopped). `lib.sh`: `throttle <key> <window_secs>` (0 = proceed, 1 = skip), `camp_or_note` (runs camp, notes to stderr on failure, never non-zero).

**Steps:**

- [ ] **Step 1: Write failing `--hook-stdin` tests** in `cli_session.rs`. Both use only the attended top-level session; there is NO SubagentStop-to-end case (dropped per BLOCKER 2):

```rust
#[test]
fn hook_stdin_register_is_idempotent_and_session_end_stops_the_registered_session() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init(d);
    let start = r#"{"session_id":"S-1","transcript_path":"/t/S-1.jsonl","cwd":"/x","source":"startup","hook_event_name":"SessionStart"}"#;
    camp(d).args(["session","register","--hook-stdin"]).write_stdin(start).assert().success();
    // repeat SessionStart (resume/clear reuses the id) → idempotent no-op, still success
    camp(d).args(["session","register","--hook-stdin"]).write_stdin(start).assert().success();
    let text = String::from_utf8(camp(d).args(["events","--json"]).output().unwrap().stdout).unwrap();
    assert_eq!(text.lines().filter(|l| l.contains("\"session.woke\"") && l.contains("attended/S-1")).count(), 1,
        "SessionStart must register exactly once, got:\n{text}");

    // SessionEnd for the SAME top-level session → exactly one session.stopped
    let end = r#"{"session_id":"S-1","source":"prompt_input_exit","hook_event_name":"SessionEnd"}"#;
    camp(d).args(["session","end","--hook-stdin","--if-registered"]).write_stdin(end).assert().success();
    // a second SessionEnd (already ended) → --if-registered no-op, still exactly one
    camp(d).args(["session","end","--hook-stdin","--if-registered"]).write_stdin(end).assert().success();
    let text2 = String::from_utf8(camp(d).args(["events","--json"]).output().unwrap().stdout).unwrap();
    assert_eq!(text2.lines().filter(|l| l.contains("\"session.stopped\"") && l.contains("attended/S-1")).count(), 1);
}

#[test]
fn if_registered_end_is_a_noop_for_a_never_registered_session() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init(d);
    let end = r#"{"session_id":"NEVER","source":"other","hook_event_name":"SessionEnd"}"#;
    camp(d).args(["session","end","--hook-stdin","--if-registered"]).write_stdin(end).assert().success();
    let text = String::from_utf8(camp(d).args(["events","--json"]).output().unwrap().stdout).unwrap();
    assert_eq!(text.lines().filter(|l| l.contains("\"session.stopped\"")).count(), 0,
        "no session.stopped for a name that was never registered");
}
```

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --test cli_session hook_stdin`. Expected: FAIL (`--hook-stdin` unknown).

- [ ] **Step 3: Implement `--hook-stdin`.** Add `#[arg(long)] hook_stdin: bool` and `#[arg(long)] if_registered: bool` to `SessionCommand::{Register, End}`. In `cmd/session.rs`, when `hook_stdin`: read stdin, `serde_json::from_str` into a lenient `HookInput { session_id: String, transcript_path: Option<String>, cwd: Option<String>, source: Option<String>, .. }` (NO `deny_unknown_fields` — this parses the harness payload, not a camp event). Derive `name = format!("attended/{session_id}")`, `agent = "attended"`, no `--bead` (so patrol never tracks it — annotate-only, and it never contributes to `red`, per D2's note).
  - `register --hook-stdin`: call `ledger.session_status(&name)?`; if `Some(_)` (a row exists in ANY status), print `note: session {name} already registered` and return `Ok(())` (idempotent — covers repeat SessionStart). Else append `session.woke` (actor `hook:session-start`) and poke.
    - **Resume-after-end edge (non-blocking note b, acknowledged):** if the operator resumes a session whose row already `ended`, `session_status` returns `Some("ended"/"crashed")`, so register no-ops — the session is NOT re-registered. This is intentional and harmless: session names are fold-unique forever, attended registry rows are best-effort (the session still drives the camp fine, and `camp adopt` reconciles liveness). Documented in `plugin/README.md`.
  - `end --hook-stdin --if-registered`: call `ledger.session_status(&name)?`; if not `Some("live")`, return `Ok(())` (no-op). Else append `session.stopped` (actor `hook:session-end`, `reason = source`) and poke.
  - Add `Ledger::session_status(&self, name) -> Result<Option<String>, CoreError>` (SELECT status FROM sessions WHERE name=?1) if it does not already exist — mirrors the query the fold's `session_ended` already runs.

- [ ] **Step 4: Run, watch pass.** Run: `cargo test -p camp --test cli_session`. Expected: PASS.

- [ ] **Step 5: Write the failing hook-script test** (`crates/camp/tests/plugin_hooks.rs`). Drives each shell hook with its fixture stdin under a real temp camp, asserting exit code 0 (fire-and-forget), the exact appended events, and throttle behavior.

```rust
use assert_cmd::Command;
use std::path::PathBuf;
use std::process::Stdio;
use tempfile::TempDir;

fn plugin() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin") }
fn fixture(name: &str) -> String {
    std::fs::read_to_string(plugin().join("tests/fixtures").join(name)).unwrap()
}
// runs a hook script with CAMP_DIR set and the built `camp` on PATH, feeding stdin; returns the output
fn run_hook(script: &str, stdin: &str, camp_dir: &std::path::Path) -> std::process::Output {
    use std::io::Write;
    let camp_bin_dir = assert_cmd::cargo::cargo_bin("camp");
    let camp_bin_dir = camp_bin_dir.parent().unwrap();
    let path = format!("{}:{}", camp_bin_dir.display(), std::env::var("PATH").unwrap_or_default());
    let mut child = std::process::Command::new("sh")
        .arg(plugin().join("hooks").join(script))
        .env("CAMP_DIR", camp_dir)
        .env("PATH", path)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn session_start_hook_registers_once_and_exits_zero() {
    let tmp = TempDir::new().unwrap();
    // init a camp at tmp/.camp; put the built `camp` on PATH
    // run session-start.sh twice with the same fixture
    // assert: exit 0 both times; exactly one session.woke in `camp events --json`
}

#[test]
fn breadcrumb_hook_is_throttled_within_the_window() {
    // run post-tool-use.sh three times quickly with the same fixture and a 2s window
    // assert: exit 0 each; exactly one worker.milestone appended (2nd/3rd throttled)
}

#[test]
fn hooks_exit_zero_even_when_campd_and_db_are_unavailable() {
    // point CAMP_DIR at a dir with no camp; run each hook; assert exit 0 + a stderr note
}
```

Fill `run_hook` with `std::process::Command::new("sh").arg(script)`, `.env("CAMP_DIR", camp_dir)`, `.env("PATH", built_camp_bin_dir:$PATH)`, `.stdin(piped)`, write `stdin`, `.output()`. Use `assert_cmd::cargo::cargo_bin("camp")` to locate the binary dir.

- [ ] **Step 6: Run, watch it fail.** Run: `cargo test -p camp --test plugin_hooks`. Expected: FAIL — hook scripts do not exist.

- [ ] **Step 7: Implement `lib.sh`, the hook scripts, hooks.json, and fixtures.**

`plugin/hooks/lib.sh` (POSIX sh; no jq):
```sh
# Resolve the camp dir: $CAMP_DIR, else the hook payload's cwd, else PWD.
# throttle KEY WINDOW_SECS -> exit 0 to proceed, 1 to skip (touch a marker under $dir/hooks).
# camp_or_note ARGS... -> run `camp ARGS`; on failure print "camp hook: <err>" to stderr; ALWAYS return 0.
```
`plugin/hooks/session-start.sh`: `INPUT=$(cat); printf '%s' "$INPUT" | camp_or_note session register --hook-stdin; camp_or_note adopt; exit 0` (`camp_or_note` forwards its stdin to `camp`).
`plugin/hooks/session-end.sh`: `INPUT=$(cat); printf '%s' "$INPUT" | camp_or_note session end --hook-stdin --if-registered; exit 0` (SessionEnd only — no `hook_event_name` branching, since SubagentStop is not wired).
`plugin/hooks/post-tool-use.sh`: `throttle breadcrumb 5 || exit 0; ... camp_or_note event emit "tool: $tool_name"; exit 0` (unregistered; off by default).

`plugin/hooks/hooks.json`:
```json
{
  "hooks": {
    "SessionStart": [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/hooks/session-start.sh" }] }],
    "SessionEnd":   [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/hooks/session-end.sh" }] }]
  }
}
```
(NO `Stop`, NO `SubagentStop`, NO `PostToolUse` — Stop fires per-turn (D5), SubagentStop can't soundly end a session (BLOCKER 2), and the breadcrumb ships unregistered.)

Fixtures under `plugin/tests/fixtures/` are the confirmed payload shapes; `README.md` there is the expectation table (event appended, exit code, throttle) per fixture.

- [ ] **Step 8: Run all hook tests, watch pass.** Run: `cargo test -p camp --test plugin_hooks && cargo test -p camp --test cli_session`. Expected: PASS. `chmod +x plugin/hooks/*.sh`.

- [ ] **Step 9: Commit.**

```bash
git add crates/camp/src/cmd/session.rs plugin/hooks plugin/tests/fixtures \
        crates/camp/tests/plugin_hooks.rs crates/camp/tests/cli_session.rs
git commit -m "feat(plugin): SessionStart/SessionEnd hooks — fire-and-forget, --hook-stdin, throttle"
```

---

## Task 6: Worker skill — the lifecycle contract

`plugin/skills/worker/SKILL.md` IS the worker lifecycle contract a pack worker follows. A test pins that every contract verb is present.

**Files:**
- Create: `plugin/skills/worker/SKILL.md`
- Test: `crates/camp/tests/plugin_worker_skill.rs`

**Interfaces:**
- Consumes: the `camp` CLI verbs `recall`, `claim`, `event emit`, `remember`, `close`.
- Produces: the shipped contract text; a test asserting each verb + "exit" is present.

**Steps:**

- [ ] **Step 1: Write the failing test** (`crates/camp/tests/plugin_worker_skill.rs`):

```rust
#[test]
fn worker_skill_documents_every_lifecycle_verb() {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugin/skills/worker/SKILL.md");
    let s = std::fs::read_to_string(&p).expect("worker SKILL.md must exist");
    for needle in ["camp recall", "camp claim", "camp event emit", "camp remember", "camp close", "exit"] {
        assert!(s.contains(needle), "worker skill must document `{needle}`");
    }
    assert!(s.starts_with("---") && s.contains("name: worker"), "must have skill frontmatter");
}
```

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --test plugin_worker_skill`. Expected: FAIL (file missing).

- [ ] **Step 3: Write `plugin/skills/worker/SKILL.md`.** Frontmatter `name: worker` + a `description` (when a spawned pack worker should follow the camp lifecycle). Body walks the contract with exact commands: `recall` (`camp recall "<topic>"` before starting — reuse prior findings), `claim` (`camp claim <bead> --session <name>`), work, `emit` milestones (`camp event emit "<what happened>" --bead <bead> --session <name>` at each non-trivial step — keep tool-level noise OUT, §7.6), `remember` non-obvious findings (`camp remember "<durable fact>"`), `close` with outcome (`camp close <bead> --outcome pass|fail [--reason ...] [--transient] [--output-json -]`), then `exit`. Note the fail-fast rule (a `campd`-spawned worker runs non-interactive; anything the agent def has not pre-allowed fails fast and lands in the ledger — do not hang).

- [ ] **Step 4: Run, watch pass.** Run: `cargo test -p camp --test plugin_worker_skill`. Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add plugin/skills/worker/SKILL.md crates/camp/tests/plugin_worker_skill.rs
git commit -m "feat(plugin): worker skill — the claim→work→emit→remember→close lifecycle contract"
```

---

## Task 7: Statusline snippet (opt-in) + wiring

`plugin/statusline/statusline.sh` renders the fleet badge from `camp top --statusline` (Task 3), reading the harness statusline JSON to resolve the workspace cwd. Ships as an opt-in script (D6); documented wiring + optional `subagentStatusLine` registration.

**Files:**
- Create: `plugin/statusline/statusline.sh`
- Create/Modify: `plugin/.claude-plugin/settings.json` (register the script as `subagentStatusLine` — the one plugin-native slot)
- Test: extend `crates/camp/tests/plugin_hooks.rs` (or a `plugin_statusline.rs`) driving the script with statusline fixture stdin.

**Interfaces:**
- Consumes: `camp top --statusline` (Task 3); statusline stdin JSON `{cwd, workspace:{current_dir}, session_id, ...}`.
- Produces: badge on stdout when campd up; empty stdout + stderr note when down.

**Steps:**

- [ ] **Step 1: Write the failing test.** Feed `plugin/tests/fixtures/statusline.json` to `statusline.sh` with `CAMP_DIR` unset and no campd → assert exit 0 and empty stdout (degrades). With a running campd (started via `camp top`) → assert stdout matches `^▲\d+ ●\d+ ✖\d+$`.

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --test plugin_hooks statusline`. Expected: FAIL (script missing).

- [ ] **Step 3: Implement `plugin/statusline/statusline.sh`.** Read stdin JSON; `cd` into `workspace.current_dir`/`cwd` (so `camp` resolves the right `.camp`); `exec camp top --statusline`. (camp handles the badge + visible degradation; the script is a locator.) `chmod +x`.

- [ ] **Step 4: Wire `plugin/.claude-plugin/settings.json`.**
```json
{ "subagentStatusLine": { "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/statusline/statusline.sh" } }
```

- [ ] **Step 5: Run, watch pass.** Run: `cargo test -p camp --test plugin_hooks`. Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add plugin/statusline plugin/.claude-plugin/settings.json crates/camp/tests/plugin_hooks.rs
git commit -m "feat(plugin): fleet statusline snippet (opt-in) + subagentStatusLine wiring"
```

---

## Task 8: Starter pack (content — agents, formula symlink, orders)

`packs/starter/` — pure content: two agent definitions, an example order file, and the `guarded-change.toml` formula symlinked into the gc-validated corpus (D3).

**Files:**
- Create: `packs/starter/agents/dev.md`, `packs/starter/agents/reviewer.md`
- Create (symlink): `packs/starter/formulas/guarded-change.toml` → `../../../crates/camp-core/tests/fixtures/formulas/valid/guarded-change.toml`
- Create: `packs/starter/orders.toml`, `packs/starter/README.md`
- Test: `crates/camp/tests/starter_pack.rs`

**Interfaces:**
- Consumes: `camp doctor --formula <path>` (existing); the corpus file (D3); Claude Code agent frontmatter (model/tools/permission).
- Produces: the starter pack a user copies; the symlink whose target is the corpus file.

**Steps:**

- [ ] **Step 1: Write the failing test** (`crates/camp/tests/starter_pack.rs`):

```rust
use assert_cmd::Command;
use std::path::PathBuf;

fn repo_root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..") }

#[test]
fn starter_formula_is_the_corpus_file_and_doctor_accepts_it() {
    let pack_formula = repo_root().join("packs/starter/formulas/guarded-change.toml");
    let corpus = repo_root().join("crates/camp-core/tests/fixtures/formulas/valid/guarded-change.toml");
    assert!(std::fs::symlink_metadata(&pack_formula).unwrap().file_type().is_symlink(),
        "starter formula must be a symlink into the gc-validated corpus (single source of truth)");
    assert_eq!(std::fs::canonicalize(&pack_formula).unwrap(), std::fs::canonicalize(&corpus).unwrap());
    Command::cargo_bin("camp").unwrap()
        .args(["doctor", "--formula"]).arg(&pack_formula)
        .assert().success();
}

#[test]
fn starter_pack_ships_agent_definitions() {
    for a in ["dev", "reviewer"] {
        let p = repo_root().join(format!("packs/starter/agents/{a}.md"));
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.starts_with("---") && s.contains("description:"),
            "{a}.md must be a Claude Code agent definition with frontmatter");
    }
}
```

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --test starter_pack`. Expected: FAIL (paths missing).

- [ ] **Step 3: Create the content.** `agents/dev.md` and `agents/reviewer.md` — Claude Code agent definitions: frontmatter (`name`, `description`, `model`, `tools`, and a permission stance) + a role prompt that references the `worker` skill lifecycle. Create the symlink:
```bash
ln -s ../../../crates/camp-core/tests/fixtures/formulas/valid/guarded-change.toml \
      packs/starter/formulas/guarded-change.toml
```
`orders.toml` — one cron and one event order in §9 `[[order]]` form (e.g. `on = "cron:0 7 * * 1-5"` / `on = "event:bead.closed[label=ci-red]"`). `README.md` — "example to copy, not a dependency; `camp.toml` imports packs via `packs = [\"packs/starter\"]`."

- [ ] **Step 4: Run, watch pass.** Run: `cargo test -p camp --test starter_pack`. Expected: PASS. Confirm the symlink is committed as a symlink: `git config core.symlinks true` is the default; verify `git ls-files -s packs/starter/formulas/guarded-change.toml` shows mode `120000`.

- [ ] **Step 5: Commit.**

```bash
git add packs/starter crates/camp/tests/starter_pack.rs
git commit -m "feat(packs): starter pack — dev/reviewer agents, orders example, guarded-change symlinked to the corpus"
```

---

## Task 9: Zero-shipped-agent-definitions repo-policy test

The plugin is machinery only — it MUST ship no agent definitions (spec §11: "if the machinery mentions a role, it is a bug"). A repo-policy test enforces it, with the starter pack as the positive control.

**Files:**
- Test: `crates/camp/tests/plugin_policy.rs`

**Steps:**

- [ ] **Step 1: Write the failing test:**

```rust
use std::path::PathBuf;
fn repo_root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..") }

#[test]
fn plugin_ships_zero_agent_definitions() {
    let plugin = repo_root().join("plugin");
    // No agents/ directory anywhere under plugin/
    for entry in walkdir(&plugin) {
        assert!(entry.file_name().and_then(|n| n.to_str()) != Some("agents"),
            "the plugin must ship no agents/ directory: {}", entry.display());
    }
    // Manifest must not declare an `agents` component path
    let manifest = std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert!(v.get("agents").is_none(), "plugin.json must not declare `agents`");
}

#[test]
fn roles_are_pack_content_not_machinery() {
    // positive control: roles live in the pack
    assert!(repo_root().join("packs/starter/agents/dev.md").exists());
}
```
(`walkdir` = a small recursive `read_dir` helper in the test file; no new dependency.)

- [ ] **Step 2: Run, watch pass** (it should already pass after Tasks 4–8 — this test guards a property, so also add a temporary `plugin/agents/x.md`, watch it FAIL, then remove it and watch it PASS, proving the guard bites). Run: `cargo test -p camp --test plugin_policy`.

- [ ] **Step 3: Commit.**

```bash
git add crates/camp/tests/plugin_policy.rs
git commit -m "test(plugin): repo-policy — plugin ships zero agent definitions"
```

---

## Task 10: Plugin/pack docs + in-PR spec correction (D5)

**Files:**
- Create: `plugin/README.md` (`packs/starter/README.md` is created in Task 8)
- Modify: `docs/design/2026-07-05-gas-camp-design.md` §11 AND `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md` Phase 12 — D5 is lead-confirmed, so apply now.

**All three occurrences to correct (verbatim targets — the spec-never-diverges rule requires ALL of them):**
1. Spec §11 (design doc, ~line 568): `SessionStart (register/adopt), Stop and SubagentStop (session end)` → `SessionStart (register/adopt), SessionEnd (session end)`.
2. Master-plan Files line (~line 903): `hooks/` (SessionStart, Stop, SubagentStop, optional PostToolUse breadcrumb — off by default)` → `hooks/` (SessionStart, SessionEnd, optional PostToolUse breadcrumb — off by default)`.
3. Master-plan content-contract line (~line 908): `Stop/SubagentStop append session-end events` → `SessionEnd appends the session-end event`.

**Steps:**

- [ ] **Step 1: Write `plugin/README.md`** — machinery only, zero roles; the four commands; the TWO lifecycle hooks (SessionStart register+adopt, SessionEnd end) and why Stop/SubagentStop are deliberately not wired (D5/BLOCKER 2); the resume-after-end registry caveat (Task 5 Step 3); the worker skill; the opt-in statusline and how to wire it into `~/.claude/settings.json`.
- [ ] **Step 2: Apply the three corrections** above (grep to confirm no other `Stop`-as-session-end occurrence remains: `grep -n "Stop" docs/design/2026-07-05-gas-camp-design.md docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md`). Cite the hooks.md lifecycle finding + this plan's D5. Confirm spec-and-code-never-diverge holds.
- [ ] **Step 3: Commit.**
```bash
git add plugin/README.md docs/design/2026-07-05-gas-camp-design.md docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md
git commit -m "docs(plugin): README + spec §11 correction (session-end hook is SessionEnd, not Stop)"
```

---

## Task 11: Full gates, push, and CI

**Steps:**

- [ ] **Step 0: Rebase onto current origin/main and reconcile the real daemon overlap (note c).** When the lead says phase-13-perf-volume merged (or before final push regardless), `git fetch && git rebase origin/main`. phase-13 owns `Makefile`/`camp backup`/perf tests, but confirm whether it also touched the daemon files this phase edits — `git diff origin/main...HEAD -- crates/camp/src/daemon/socket.rs crates/camp/src/daemon/event_loop.rs crates/camp/src/daemon/patrol.rs crates/camp/src/main.rs` and inspect the corresponding origin/main changes — reconcile the `Response::Status`/`stalled`/status-handler edits and the `Command` enum (Session verb vs. phase-13's Backup verb) cleanly, then re-run all gates. Never push a branch not rebased on current main.

- [ ] **Step 1: Run the full gate suite locally.**
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: all green. Fix any failure before proceeding (fail fast).

- [ ] **Step 2: End-to-end sanity (the exit criterion).** With a scratch camp + rig, confirm driving from a session works: `/status` shows the snapshot; `camp session register --hook-stdin` + `camp adopt` register the attended session; `camp sling` creates a bead; `camp top --statusline` renders `▲n ●n ✖n`; the worker-skill verbs (`recall/claim/event emit/remember/close`) run against the ledger. Record the transcript in the PR description.

- [ ] **Step 3: Push and open the PR.**
```bash
git push -u origin phase-12-plugin-packs
gh pr create --title "Phase 12: plugin and packs" --body "<exit criteria quoted with evidence>"
```

- [ ] **Step 4: FOREGROUND-watch CI to a terminal result.**
```bash
gh pr checks --watch
```
Expected: five green (fmt, clippy, test ×2, gc-compat). Do NOT background this and end the turn.

- [ ] **Step 5: Report to the lead** — PR number, CI status, and each master-plan Phase 12 exit-criterion quoted line-by-line with its evidence.

---

## Self-Review (spec coverage)

- **Thin command wrappers over the CLI (§13.6):** Task 4 + parity test (fenced-block scan, note a). ✓
- **Session lifecycle hooks (§10, §16):** Task 5 — SessionStart (register+adopt), SessionEnd (end); Stop→SessionEnd corrected and SubagentStop dropped per D5/BLOCKER 2. ✓
- **Fire-and-forget + throttle, verified by test (§16):** Task 5 (always exit 0 even with campd/db down; idempotent register via `session_status`; time-window breadcrumb throttle). ✓
- **Optional PostToolUse breadcrumb OFF by default (§10):** Task 5 (shipped unregistered). ✓
- **Worker skill = lifecycle contract:** Task 6 + verb-completeness test. ✓
- **Statusline ▲live ●ready ✖red, visible degradation (§11):** Tasks 2 (`stalled` set → `red`, BLOCKER 1 fix), 3, 7. ✓
- **Starter pack content; formula passes doctor + gc gate (corpus symlink):** Task 8 (D3). ✓
- **Zero shipped agent definitions (repo-policy test):** Task 9. ✓
- **Attended Tier-0 as teammate per §8.4/A1, no headless+attach fallback:** Task 4 `/sling` note. ✓
- **Exit criteria (drive a camp from a session end to end; zero agent defs; CI green):** Task 11. ✓

**Decision status (rev 2):** D1 ACCEPTED; D2 semantic ACCEPTED, mechanism corrected (BLOCKER 1 — `stalled` HashSet); D5 VERIFIED (SessionEnd) with SubagentStop dropped (BLOCKER 2); D3/D4/D6 settled. Non-blocking notes a/b/c folded in (Tasks 4/5/11). No open decisions remain — resubmitting for a fresh plan-review pass.

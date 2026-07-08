# Phase 12 — Plugin and Packs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the camp Claude Code plugin (machinery only, zero shipped roles) and a starter pack (content), so a Claude Code session drives a camp end to end — sling/status/adopt/events slash commands, lifecycle hooks, a worker skill, and a fleet statusline — all as thin wrappers over the `camp` CLI, with the plugin provably shipping zero agent definitions.

**Architecture:** The plugin is a standard Claude Code plugin under `plugin/` — a `.claude-plugin/plugin.json` manifest, `commands/*.md` slash commands that shell out to the `camp` binary (identical scripting surface, spec §13 guarantee 6 / §13.6), `hooks/` shell scripts registered in `hooks/hooks.json` that append session-lifecycle events fire-and-forget (throttled per spec §16), a `skills/worker/SKILL.md` that IS the worker lifecycle contract, and a `statusline/` snippet that queries the campd socket and renders `▲live ●ready ✖red`. To let the hooks and statusline be *thin* wrappers, two additive CLI/socket surfaces are added to the merged phases (Decisions D1, D2 below): hook-facing session-lifecycle verbs (`camp session register` / `camp session end`) and a `red` count on the status socket response. The starter pack under `packs/starter/` is pure content — two agent definitions, an example order file, and a `guarded-change.toml` formula symlinked into the already-gc-validated corpus so there is one source of truth.

**Tech Stack:** Rust (clap CLI, rusqlite ledger, existing `camp_core`/`camp` crates); POSIX shell for hooks/statusline; Claude Code plugin format (manifest JSON, hooks JSON, command markdown, SKILL.md); Gas City formula-v2 TOML subset. Tests are Rust integration tests under `crates/camp/tests/` (so they run under `cargo test --workspace`, gated by the existing CI `test` checks) driving the plugin scripts against recorded fixture stdin payloads under `plugin/tests/fixtures/`.

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

**D2 — `red` on the status socket response (additive, for the fleet badge).**
The statusline badge `▲live ●ready ✖red` is an explicit Phase 12 deliverable (master plan; spec §11) "fed by the campd socket," but the socket `Status` response (`Response::Status` in `daemon/socket.rs`, built from `StatusSummary`) carries only `live_sessions`, `ready`, `open` — no `red`. Spec §10 defines the red source: "patrol only annotates (`agent.stalled` event + **statusline badge**)". So `red` = the count of live sessions currently flagged **stalled** by patrol. campd is the natural place to compute it: the status handler in `daemon/event_loop.rs` already runs inside campd, which holds `PatrolRuntime` in memory (real-time-accurate stall state). This plan adds `red: u64` to `Response::Status`, populated from a new `PatrolRuntime::stalled_count()`, leaving `camp_core`'s `StatusSummary` (pure ledger derivation) untouched. Additive only; the pinned wire-format test is updated. *Alternative considered and rejected as hairier:* deriving "currently stalled" from the ledger via SQL ("live sessions whose latest patrol signal is `agent.stalled`") — there is no durable "un-stalled" event, so the in-memory patrol count is both simpler and more correct. **Plan review: confirm the `red` = stalled-live-sessions semantic and the socket extension.**

**D3 — Starter formula is a symlink into the gc-validated corpus (single source of truth).**
`packs/starter/formulas/guarded-change.toml` is a *relative symlink* to `crates/camp-core/tests/fixtures/formulas/valid/guarded-change.toml`. The gc-compat CI job (`.github/workflows/ci.yml` line 66) already validates that corpus directory against the real `gc` compiler, and `crates/camp-core/tests/formula_corpus.rs` pins it by name. The symlink means the starter pack ships *the* corpus file — "passes the Phase 6 gc gate" holds transitively with zero CI-workflow change and zero drift. A Phase 12 test (Task 9) asserts the symlink resolves to the corpus file and that `camp doctor --formula` accepts the pack path.

**D4 — Tests are Rust integration tests; fixtures live under `plugin/tests/fixtures/`.**
The master plan says "hook tests under `plugin/tests/`." To gate them on the existing CI `test` checks (no new workflow job), the executable drivers are Rust integration tests under `crates/camp/tests/` that shell out to the `plugin/` scripts and read recorded stdin payloads from `plugin/tests/fixtures/`. This satisfies "tests under plugin/tests/" (the fixtures + a documented expectation table live there) while running under `cargo test --workspace`.

**D5 — Session-end hook is `SessionEnd`, not `Stop` (spec-vs-reality correction — NEEDS LEAD SIGN-OFF).**
Spec §11 and master plan Phase 12 say the plugin emits session-end events on "Stop and SubagentStop." Confirmed against current Claude Code docs (hooks.md lifecycle table; retrieved 2026-07-08): **`Stop` fires once per *turn* (after every assistant response), not at session termination** — using it for session-end would append N `session.stopped` events per session and the second one would hit the fold's "session already ended" error. The hook that fires exactly once at true session termination is **`SessionEnd`** (payload `{ session_id, cwd, hook_event_name, source }`, `source ∈ {clear, resume, logout, prompt_input_exit, bypass_permissions_disabled, other}`; exit codes/output ignored — fire-and-forget by design). Therefore this plan wires **SessionStart → register+adopt, SessionEnd → session end, SubagentStop → session end (for a registered worker/teammate)**, and does NOT register a `Stop` hook. Per AGENTS.md ("if implementation reality contradicts the spec, stop and update the spec via PR in the same change"), the fix is a one-line correction to spec §11 and master plan Phase 12 replacing "Stop" with "SessionEnd", landing in this same PR. **Plan review / lead: confirm the SessionEnd substitution and the in-PR spec edit before execution.**

**D6 — Statusline ships as an opt-in script, not a plugin-set `statusLine`.**
A plugin's bundled `settings.json` supports only `agent` and `subagentStatusLine` (plugins-reference; retrieved 2026-07-08) — it cannot set the main `statusLine`. This matches spec §11's word "*optional* statusline snippet": the plugin ships `plugin/statusline/statusline.sh` and documents wiring it into the user's `~/.claude/settings.json` `statusLine.command`. Optionally, the plugin registers the same script as `subagentStatusLine` (the one plugin-native statusline slot) so teammates show a camp badge. The script's data path is `camp top --statusline` (Task 3).

---

## File Structure

**Plugin (machinery only — ships ZERO agent definitions):**
- `plugin/.claude-plugin/plugin.json` — plugin manifest (name, version, description; component dirs auto-discovered).
- `plugin/commands/sling.md` — thin wrapper → `camp sling`.
- `plugin/commands/status.md` — thin wrapper → `camp top`.
- `plugin/commands/adopt.md` — thin wrapper → `camp adopt`.
- `plugin/commands/events.md` — thin wrapper → `camp events`.
- `plugin/hooks/hooks.json` — registers SessionStart, SessionEnd, SubagentStop (NOT Stop — per D5; PostToolUse breadcrumb present but NOT registered — off by default, §10).
- `plugin/hooks/lib.sh` — shared helpers: locate the camp dir, `throttle` marker check, fire-and-forget wrapper, JSON field extraction (`jq`-free, portable).
- `plugin/hooks/session-start.sh` — SessionStart: register this session (`camp session register`) + `camp adopt`; idempotent via a per-session marker; always exit 0.
- `plugin/hooks/session-end.sh` — SessionEnd + SubagentStop end handler (`camp session end`, guarded by a registration check for SubagentStop); always exit 0.
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

Realizes Decision D1. The foundation the SessionStart/Stop/SubagentStop hooks wrap. No new event types — appends the existing `session.woke` / `session.stopped` via the `event_emit.rs` pattern.

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
- Produces: `Response::Status { ok, #[serde(flatten)] summary: StatusSummary, red: u64, campd_pid: u32 }`; `PatrolRuntime::stalled_count(&self) -> u64` returning the number of tracked sessions whose stall timer has currently fired (in-memory).

**Steps:**

- [ ] **Step 1: Write the failing test.** In `socket.rs`'s `response_wire_format_is_pinned`, update the `Status` case to include `red` and assert the new canonical JSON (`red` positioned after `open`, before `campd_pid`):

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
Also add a `patrol.rs` unit test asserting `stalled_count()` == number of fired timers for a fabricated runtime state (follow the existing patrol test fixtures).

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --lib daemon::socket` and `cargo test -p camp --lib daemon::patrol`. Expected: FAIL (`red` not a field / `stalled_count` not defined).

- [ ] **Step 3: Implement.** Add `red: u64` to `Response::Status`. Add `PatrolRuntime::stalled_count()` returning the count of currently-fired (stalled) tracked timers — read the same in-memory structure `fire_due`/`declare_stalls` use to know which timers have fired. In `event_loop.rs`, the status handler (currently `ledger.status_summary()`) sets `red: patrol.stalled_count()`. Update `top.rs::render` to accept and (optionally) show the red count without changing its existing text lines — the pinned `top` render tests must still pass, so add red on its own line, e.g. `"…\nred: {red}\n"`, and update those unit tests accordingly.

- [ ] **Step 4: Run all touched tests, watch pass.** Run: `cargo test -p camp --lib daemon`. Expected: PASS. Then `cargo test -p camp --test daemon_lifecycle` (status-over-socket integration) — update any status-response assertions to include `red`.

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
fn wrapped(md: &str) -> (String, Vec<String>) {
    let sub = md.split_whitespace()
        .collect::<Vec<_>>().windows(2)
        .find(|w| w[0] == "camp")
        .map(|w| w[1].trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .expect("command md must invoke `camp <sub>`");
    let flags = md.split_whitespace()
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

SessionStart (register+adopt), SessionEnd + SubagentStop (session end), and an off-by-default PostToolUse breadcrumb. Hooks are trivial shell that always exit 0; JSON parsing + idempotency live in tested Rust via a `--hook-stdin` mode on `camp session`.

**Files:**
- Modify: `crates/camp/src/cmd/session.rs` (add `--hook-stdin` to `register`/`end`; add a lenient `HookInput` parse)
- Create: `plugin/hooks/hooks.json`, `plugin/hooks/lib.sh`, `plugin/hooks/session-start.sh`, `plugin/hooks/session-end.sh`, `plugin/hooks/post-tool-use.sh`
- Create: `plugin/tests/fixtures/session-start.json`, `session-end.json`, `subagent-stop.json`, `post-tool-use.json`, and `plugin/tests/fixtures/README.md` (expectation table)
- Test: `crates/camp/tests/plugin_hooks.rs`, plus `--hook-stdin` cases in `crates/camp/tests/cli_session.rs`

**Interfaces:**
- Consumes: Task 1's `cmd::session::{register,end}`; the hook stdin schemas (SessionStart `{session_id, transcript_path, cwd, source, model?}`; SessionEnd `{session_id, source}`; SubagentStop `{session_id, agent_id, agent_type, agent_name?}`; PostToolUse `{session_id, tool_name, tool_input}`) — parsed leniently (ignore unknown harness fields).
- Produces: name derivation `attended/<session_id>` (register and end derive it identically from stdin `session_id`); `register --hook-stdin` is idempotent (no-op success if the derived name is already a live session); `end --hook-stdin` is if-registered (no-op success if not live). `lib.sh`: `throttle <key> <window_secs>` (0 = proceed, 1 = skip), `camp_or_note` (runs camp, notes to stderr on failure, never non-zero).

**Steps:**

- [ ] **Step 1: Write failing `--hook-stdin` tests** in `cli_session.rs`:

```rust
#[test]
fn hook_stdin_register_is_idempotent_and_end_is_if_registered() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    init(d);
    let start = r#"{"session_id":"S-1","transcript_path":"/t/S-1.jsonl","cwd":"/x","source":"startup","hook_event_name":"SessionStart"}"#;
    // first register → one session.woke
    camp(d).args(["session","register","--hook-stdin"]).write_stdin(start).assert().success();
    // second identical SessionStart (resume/clear) → idempotent no-op, still success
    camp(d).args(["session","register","--hook-stdin"]).write_stdin(start).assert().success();
    let text = String::from_utf8(camp(d).args(["events","--json"]).output().unwrap().stdout).unwrap();
    assert_eq!(text.lines().filter(|l| l.contains("\"session.woke\"") && l.contains("attended/S-1")).count(), 1,
        "SessionStart must register exactly once, got:\n{text}");
    // SubagentStop for an unregistered subagent → if-registered no-op success (no event)
    let sub = r#"{"session_id":"S-1","agent_id":"AG-9","agent_type":"Explore","hook_event_name":"SubagentStop"}"#;
    camp(d).args(["session","end","--hook-stdin","--if-registered"]).write_stdin(sub).assert().success();
    // SessionEnd for the registered attended session → one session.stopped
    let end = r#"{"session_id":"S-1","source":"prompt_input_exit","hook_event_name":"SessionEnd"}"#;
    camp(d).args(["session","end","--hook-stdin"]).write_stdin(end).assert().success();
    let text2 = String::from_utf8(camp(d).args(["events","--json"]).output().unwrap().stdout).unwrap();
    assert_eq!(text2.lines().filter(|l| l.contains("\"session.stopped\"") && l.contains("attended/S-1")).count(), 1);
}
```

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p camp --test cli_session hook_stdin`. Expected: FAIL (`--hook-stdin` unknown).

- [ ] **Step 3: Implement `--hook-stdin`.** Add `#[arg(long)] hook_stdin: bool` and `#[arg(long)] if_registered: bool` to `SessionCommand::{Register, End}`. In `cmd/session.rs`, when `hook_stdin`: read stdin, `serde_json::from_str` into a lenient `HookInput { session_id: String, transcript_path: Option<String>, cwd: Option<String>, source: Option<String>, agent_id: Option<String>, .. }` (NO `deny_unknown_fields` — this parses the harness payload, not a camp event). Derive `name = format!("attended/{session_id}")`, `agent = "attended"`. For `register`: query `ledger.live_sessions()`; if `name` already present, print a one-line note and return `Ok(())` (idempotent). Else append `session.woke`. For `end` with `if_registered`: if `name` not live, return `Ok(())`; else append `session.stopped` with `reason = source`.

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
// runs a hook script with CLAUDE_PLUGIN_ROOT + CAMP_DIR set, feeding stdin; returns exit status
fn run_hook(script: &str, stdin: &str, camp_dir: &std::path::Path) -> std::process::Output { /* std::process::Command sh script, env CAMP_DIR, PATH to the built camp bin, write stdin */ unimplemented!() }

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
`plugin/hooks/session-start.sh`: `INPUT=$(cat); printf '%s' "$INPUT" | camp_or_note session register --hook-stdin; camp_or_note adopt; exit 0`.
`plugin/hooks/session-end.sh`: reads `hook_event_name`; for `SubagentStop` runs `camp session end --hook-stdin --if-registered`, for `SessionEnd` runs `camp session end --hook-stdin`; always `exit 0`.
`plugin/hooks/post-tool-use.sh`: `throttle breadcrumb 5 || exit 0; ... camp_or_note event emit "tool: $tool_name"; exit 0` (unregistered; off by default).

`plugin/hooks/hooks.json`:
```json
{
  "hooks": {
    "SessionStart": [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/hooks/session-start.sh" }] }],
    "SessionEnd":   [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/hooks/session-end.sh" }] }],
    "SubagentStop": [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/hooks/session-end.sh" }] }]
  }
}
```
(NO `Stop`, NO `PostToolUse` — the breadcrumb ships unregistered.)

Fixtures under `plugin/tests/fixtures/` are the confirmed payload shapes; `README.md` there is the expectation table (event appended, exit code, throttle) per fixture.

- [ ] **Step 8: Run all hook tests, watch pass.** Run: `cargo test -p camp --test plugin_hooks && cargo test -p camp --test cli_session`. Expected: PASS. `chmod +x plugin/hooks/*.sh`.

- [ ] **Step 9: Commit.**

```bash
git add crates/camp/src/cmd/session.rs plugin/hooks plugin/tests/fixtures \
        crates/camp/tests/plugin_hooks.rs crates/camp/tests/cli_session.rs
git commit -m "feat(plugin): SessionStart/SessionEnd/SubagentStop hooks — fire-and-forget, --hook-stdin, throttle"
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
- Create: `plugin/README.md`, `packs/starter/README.md` (if not already in Task 8)
- Modify: `docs/design/2026-07-05-gas-camp-design.md` §11 (replace "Stop and SubagentStop" → "SessionEnd and SubagentStop"); `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md` Phase 12 (same one-word correction) — **only after the lead confirms D5.**

**Steps:**

- [ ] **Step 1: Write `plugin/README.md`** — what the plugin is (machinery only, zero roles; the four commands; the three hooks; the worker skill; the opt-in statusline and how to wire it into `~/.claude/settings.json`).
- [ ] **Step 2: Apply the D5 spec correction** in both docs (one line each), citing the hooks.md lifecycle finding and this plan's D5. Confirm the design-doc invariant (spec and code never diverge) holds.
- [ ] **Step 3: Commit.**
```bash
git add plugin/README.md docs/design/2026-07-05-gas-camp-design.md docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md
git commit -m "docs(plugin): README + spec §11 correction (session-end hook is SessionEnd, not Stop)"
```

---

## Task 11: Full gates, push, and CI

**Steps:**

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

- **Thin command wrappers over the CLI (§13.6):** Task 4 + parity test. ✓
- **SessionStart (register/adopt), session-end, SubagentStop hooks (§10, §16):** Task 5; Stop→SessionEnd corrected per D5. ✓
- **Fire-and-forget + throttle, verified by test (§16):** Task 5 (always exit 0 even with campd/db down; idempotent register; time-window breadcrumb throttle). ✓
- **Optional PostToolUse breadcrumb OFF by default (§10):** Task 5 (shipped unregistered). ✓
- **Worker skill = lifecycle contract:** Task 6 + verb-completeness test. ✓
- **Statusline ▲live ●ready ✖red, visible degradation (§11):** Tasks 2, 3, 7. ✓
- **Starter pack content; formula passes doctor + gc gate (corpus symlink):** Task 8 (D3). ✓
- **Zero shipped agent definitions (repo-policy test):** Task 9. ✓
- **Attended Tier-0 as teammate per §8.4/A1, no headless+attach fallback:** Task 4 `/sling` note. ✓
- **Exit criteria (drive a camp from a session end to end; zero agent defs; CI green):** Task 11. ✓

**Open decisions for plan review:** D1 (session-lifecycle CLI verbs), D2 (`red` on the status socket = stalled sessions), D5 (Stop→SessionEnd + in-PR spec edit — **needs lead sign-off**). D3, D4, D6 are settled recommendations.

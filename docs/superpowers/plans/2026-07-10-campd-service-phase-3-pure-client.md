# campd Service Management — Phase 3: The CLI Becomes a Pure Socket Client — Implementation Plan

> **Plan review: APPROVED 2026-07-10** (independent Opus reviewer, round 3).
> Round 1 REJECT — B1: the plan escalated the v1-spec correction instead of planning it, though the
> lead had already decided it, so a verbatim implementer would have left `main` carrying a spec that
> documents a deleted feature. B2: the truth sweep missed `main.rs:262` (the `camp top --help` text,
> a user-facing lie), `socket.rs:153-154`/`:448`, and `cli_sling.rs:23` — and both acceptance greps
> were unsatisfiable, demanding deletion of `poke_best_effort`'s doc, which spec §7.2 requires kept.
> Round 2 REJECT — B3: two of the plan's own four acceptance gates went RED on a verbatim execution,
> tripped by the plan's own new doc comments. The reviewer proved it by writing them into a scratch
> tree and running the gates.
> Round 3 APPROVE — the reviewer ran all four gates against a faithful post-execution tree: Gate A no
> output, Gate B exactly one line (`poke_best_effort`'s frozen §7.2 doc), Gate C no output, Gate D no
> output, the symbol grep clean. He re-derived the blast radius, both hazards, the wedge-vs-down
> split, the test-conversion list, and the minimal v1-spec correction, and found no new error.
>
> Non-blocking notes accepted at approval:
> N1 — Task 1 Step 2's expected-failure text is imprecise: `cargo test -p camp --test daemon_lifecycle`
> builds without `cfg(test)`, so `socket.rs`'s `mod tests` is not compiled and there is NO compile
> error — the two new down-campd tests compile and fail red at their assertions. That is a better TDD
> signal than a compile error; the plan just mislabels it.
> N2 — the `campd.log` tripwire depends on `camp init --no-service` not creating/touching
> `<camp>/campd.log`. Verify with one `ls` of the camp root after `init_camp` during the rebase
> re-verification.
> N3/N4 — the two-hop remedy (on a camp with no unit, `camp service status` answers "no managed unit
> — run `camp service install`") is accepted and documented; `camp daemon` works immediately either way.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Plan review round 1: REJECT → revised.** B1: the plan escalated a v1-spec correction the operator had already ruled on (recorded at PR #68 comment 4946982848) and forbade the implementer from making it — it is now **Task 5**, and the Global Constraints permit exactly that edit. B2: the truth sweep missed `main.rs`'s `camp top` `--help` text (a user-facing lie), `socket.rs:153-154`/`:448`, and `cli_sling.rs:23`; and both acceptance greps were **unsatisfiable** — they hit files the plan's own "do not touch" list protected, including `poke_best_effort`'s §7.2-frozen doc. The sweep is now complete (a full inventory of every auto-start mention in the tree is below, each with its disposition), and the gates are four per-directory greps with **stated expected output**, not one grep expected to be empty. NB1-NB4 folded in.

**Goal:** Remove the CLI's on-demand campd auto-start entirely — every verb that needs the daemon becomes a pure socket client that fails **loudly and actionably** when campd is down (naming the pid the ledger last recorded and the two real remedies), and **never spawns a daemon**.

**Architecture:** One new primitive replaces one removed one. `crates/camp/src/daemon/autostart.rs` (probe → append `campd.autostarted` → detached spawn → readiness line → retry) is **deleted**. In its place, `daemon/socket.rs` gains a typed `CampdNotRunning` error and one function, `socket::require(camp, &Request) -> Result<Response>`, built on the existing `request_if_up` so liveness is still judged on the **same connection that carries the request** (the PR #51 finding 1 law — one connect, no bare pre-probe a wedged backlog could fool). The three daemon-needing verbs — `top` (Status), `adopt` (Adopt), `sling` (Poke, both Tier-0 and formula) — call `require`. A wedged campd still surfaces as `CampdUnresponsive` (`kill -9` remedy); a *down* campd surfaces as `CampdNotRunning` (`camp service status` / `camp daemon` remedy). The two faults stay distinct because their remedies are different.

**Tech Stack:** Rust (edition 2024); `anyhow` (typed errors via `anyhow::Error::new` + `downcast_ref`, the existing `CampdUnresponsive` precedent); the existing unix-socket client. **No new crates, no new ledger event types, no new CLI flags.**

**Phasing:** Phase 3 of the campd-service-management design (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`). It implements decision **§4.3** ("CLI is a pure client; the on-demand CLI auto-start is removed. One path."), the auto-start half of the §8 migration, the §9 test obligation *"CLI-as-pure-client: a daemon-needing verb with campd down fails loudly (names the remedy) and does not spawn a daemon — assert no new process, actionable error text"*, and — per the operator's ruling — the **minimal correction of the two v1-spec lines this phase's code falsifies** (Task 5). Phase 4 keeps the full §5 rewrite, §9's orders note, §12's multi-rig recommendation, and the `contrib/launchd/` supersession.

---

## ⚠ READ FIRST — two things that will bite you

### 1. This plan is written against a main that already contains Phase 2

Phase 2 (`camp service {install,uninstall,status,restart,list,stop,start}`, the supervisor seam, environment-aware `camp init`, `camp stop` refusing on a supervised camp, and the `tests/no_bare_camp_init.rs` guard) **merges before this phase**. Every line number below was read on `main` at **b0dc950** and **WILL SHIFT**. Before you touch anything:

- [ ] Rebase this branch on merged `main` and re-verify **every** anchor by reading the file, never by trusting a line number in this plan.
- [ ] Re-run the blast-radius grep and confirm it still returns exactly the four call sites in "Verified blast radius" below:
  ```sh
  grep -rn "request_with_autostart\|autostart" crates/camp/src crates/camp/tests
  ```
  If it returns a call site this plan does not name, **stop and report it** — a new daemon-needing verb landed and the plan is incomplete.
- [ ] Re-run the **stale-prose inventory** grep (Task 4 Step 1) and reconcile it against this plan's disposition table. Phase 2 adds files; if it introduced a new auto-start mention, sweep it too.
- [ ] **Any test that calls `camp init` MUST pass `--no-service`.** Phase 2's `tests/no_bare_camp_init.rs` guard fails the build otherwise. This plan adds **no new `camp init` call site** — new tests reuse the existing `init_camp` (`daemon_lifecycle.rs`, already swept by Phase 2) and `scaffold` (`cli_sling.rs`, which never shells out to `camp init` at all: it writes `camp.toml` and opens the ledger directly). Keep it that way.
- [ ] **Confirm Phase 2's `camp init --no-service` neither creates `<camp>/campd.log` nor installs a unit.** The `campd.log` tripwire in `assert_no_campd_came_up` (Task 1) depends on it — the removed CLI-spawn path was the only thing that ever created that file, which is what makes its absence proof that no daemon was about to start. Settle it with one command during the rebase re-verification:
  ```sh
  cargo test -p camp --test daemon_lifecycle camp_top_against_a_running_campd -- --nocapture   # builds the binary
  # then, in a scratch dir:
  camp init --no-service && ls -a .camp/
  ```
  Expected: **no `campd.log`, no socket, no installed unit.** If Phase 2's init ever touches `campd.log` or starts a daemon, the tripwire AND the whole campd-down premise of Tasks 1-3 break — **stop and report**, do not weaken the assertion.

### 2. The v1-spec correction is DECIDED — you must make it (Task 5)

The operator ruled on this and recorded it (PR #68, comment 4946982848): **Phase 3 makes the minimal, surgical correction to exactly the two v1-spec passages its own code falsifies.** It is **Task 5** of this plan. Do not re-escalate it, do not skip it, and do not widen it — the full §5 rewrite, §9's orders note, §12's multi-rig recommendation, and `contrib/launchd/` all remain Phase 4's, and Appendix A lists that boundary explicitly. AGENTS.md: *"spec and code never silently diverge."* The PR cannot be opened without Task 5.

---

## Global Constraints

Copied from `AGENTS.md` and the design spec — every task implicitly includes these.

- **TDD, strictly.** Write the failing test, RUN it, watch it fail, implement, watch it pass. Never write implementation before its failing test.
- **Fail fast (invariant 5).** No fallbacks, no silenced errors, no placeholders. **A down campd is a LOUD, actionable error, never a silent respawn and never a degraded no-op.** This is the entire point of the phase.
- **Nothing hidden (invariant 3).** The ledger tells the whole story — including old stories. That is why `campd.autostarted` **stays in camp-core** (Task 3, Step 7): `EventType::parse` hard-errors on an unknown name (`crates/camp-core/src/event.rs:107-113` → `CoreError::UnknownEventType`), and the row→`Event` decoder (`ledger/mod.rs:694`) calls it on **every read path** — so deleting the variant would make every existing ledger that ever auto-started unreadable (`camp events`, the fold, `refold`) on a real user's camp. Phase 3 removes the **producer**, never the type.
- **No panics in non-test code.** Clippy denies `unwrap_used` / `expect_used` / `panic` workspace-wide; `unsafe_code` is forbidden. Unit-test modules opt out with `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` on `mod tests`; integration tests with the `#![allow(...)]` crate attribute at the top of the file. Both patterns already exist — copy them.
- **Clippy denies dead code.** `camp` is a **binary crate with no lib target**, so an unused `pub fn` IS dead code and `-D warnings` fails the gate. This dictates the task order: **the last caller of `request_with_autostart` and the deletion of `daemon/autostart.rs` land in the SAME commit (Task 3)**, and `socket::require` gains its first caller in the same commit that introduces it (Task 1). Do not "prepare" a module in one commit and wire it in the next — the gate will be red.
- **NO NEW PROSE MAY REINTRODUCE THE DEAD CONCEPT.** No comment, doc comment, assertion message, or help string you write — anywhere under `crates/`, `README.md` or `plugin/` — may contain **`auto-start` / `autostart` / `auto start` / `auto started`**, *unless the same source line also carries the literal `campd.autostarted`* (the historical event name, which Gates C and D filter on, and which the guard assertions must name in order to guard it). **Say "the removed CLI-spawn path".** The single pre-existing exception is `poke_best_effort`'s frozen §7.2 doc, which Gate B names as its one expected survivor.

  This rule binds **every task**, not just the camp-core comments in Task 3 Step 7 — it is exactly what makes Gates B/C/D in Task 4 Step 7 satisfiable. It is stated here because it has already been violated twice: round 1's review caught the plan's own assertion *messages* tripping the gates, and round 2's caught two of the plan's own *doc comments* doing the same. Every code block in this plan has since been audited against the four gates; if you write prose of your own, audit it too. **A gate that goes red because of the prose you just wrote is a bug in the prose, never a licence to loosen the gate.**

**Behavior that is frozen — and the one edit that is nevertheless permitted on it.** Several files must keep their behavior byte-for-byte, but their *doc comments* describe the removed feature and are swept in Task 4. Read this table before you touch any of them; it is the difference between a one-line comment fix and a scope violation.

| File | Behavior | Comments / docs |
|---|---|---|
| `socket::poke_best_effort` (`socket.rs`) | **FROZEN — do not touch.** Spec §7.2's one sanctioned ignore-the-error site (the post-write fire-and-forget poke behind `create`/`claim`/`close`/`rig`/`order`/`event emit`/`session`). It is a *different* path from the CLI spawn and is unaffected. | **ALSO FROZEN — do not edit its doc comment either.** Its sentence *"a poke never auto-starts the daemon"* stays **true**, and the operator has frozen it. It is the ONE permitted survivor of Gate B (Task 4 Step 7). |
| `cmd/stop.rs` | **FROZEN.** `stop` uses `socket::request` and adds its own `"campd is not running"` context — correct, because a `camp stop` against a down campd needs *no* remedy (there is nothing to stop; telling that user to run `camp daemon` would be absurd). Phase 2 also rewrites this file — re-read it after the rebase. | Doc line 7 swept (Task 4). Comment only; not one line of code changes. |
| `cmd/nudge.rs` | **FROZEN.** Uses `socket::request_if_up` — campd-down is a *designed degrade* to the resume path, not a fault. | Module doc line 10 swept (Task 4). |
| `cmd/show.rs` | **FROZEN.** `--wait` is already a pure observer. | Doc line 32 swept (Task 4). |
| `cmd/top.rs::statusline` | **FROZEN.** It uses `socket::request` (not `require`), prints nothing to stdout, writes a visible stderr note, and exits **0** (spec §11). It must NOT become a hard failure. | Docs at lines 26 and 42 swept (Task 1, which is already editing this file). |
| `daemon/event_loop.rs`, `daemon::run` | **FROZEN — Phase 1's.** Task 3 touches `daemon/mod.rs` for exactly two things: deleting `pub mod autostart;` and fixing the `READY_PREFIX` doc. Nothing else. | — |
| `crates/camp/src/service/**` | **FROZEN — Phase 2's.** | — |
| `contrib/**`, `docs/design/**` | **FROZEN, with ONE exception:** Task 5 corrects `docs/design/2026-07-05-gas-camp-design.md` **line 126 and the `**Auto-start:**` bullet at lines 170-177 — nothing else in that file, and nothing at all under `contrib/`.** | — |

- **No new ledger event types, no vocabulary changes.** Invariant 7 untouched (the `vocab.rs` edit is a trailing `//` comment; the `"campd.autostarted"` string stays in `CAMP_SPECIFIC_EVENTS`, so the `tests/fixtures/gc-vocab.json` mirror still matches).
- Branch: `phase-3-pure-client`. Never commit to `main`. **No co-author lines, no self-attribution, no AI attribution** in any commit or PR body.
- Gates green before every commit: `cargo fmt --all`, then `cargo clippy --workspace --all-targets --all-features -- -D warnings`, then `cargo test --workspace`.

---

## Verified blast radius

Re-derived from the code at `main` b0dc950 (**not** copied from the spec — the spec's claim was checked and is correct, but the evidence is below). Independently re-derived a second time by the round-1 plan reviewer and confirmed exact: four callers, three files, **no fifth caller**.

**Callers of `request_with_autostart` — exactly four, in three files:**

| File:line | Request | verb string |
|---|---|---|
| `crates/camp/src/cmd/top.rs:11` | `Request::Status` | `"top"` |
| `crates/camp/src/cmd/adopt.rs:13` | `Request::Adopt` | `"adopt"` |
| `crates/camp/src/cmd/sling.rs:64` | `Request::Poke { seq: head }` (formula) | `"sling"` |
| `crates/camp/src/cmd/sling.rs:108` | `Request::Poke { seq }` (Tier-0) | `"sling"` |

**Users of the module:** `crates/camp/src/daemon/mod.rs:6` (`pub mod autostart;`) and the three `cmd/` files above. `main.rs` never names it.

**The only emitter of `campd.autostarted`:** `autostart.rs::start_detached` (`autostart.rs:49-56`). Nothing else in the workspace appends it.

**`CampDir::log_path()` (`campdir.rs:28`) is used ONLY by `autostart.rs`** (lines 38, 63, 64, 95 — the `log_path` hits in `dispatch.rs`/`patrol.rs` are local variables and struct fields with the same name, not the `CampDir` method). Deleting `autostart.rs` alone would make it dead code and turn the clippy gate red. Task 1 gives it a real, honest new use — the `CampdNotRunning` error points the operator at `<camp>/campd.log`, which is exactly where Phase 2's launchd unit sends campd's stderr, so a supervisor stuck in a crash-restart loop is one line away in the error the operator is already reading — and it does so **before** Task 3 removes the old caller. Green at every commit.

**Tests that depend on auto-start (every one of them, exactly):**

| Test | File:line | Why it breaks / what happens |
|---|---|---|
| `a_wedged_socket_holder_fails_the_verb_loudly_and_never_autostarts` | `src/daemon/autostart.rs:120` | Deleted with the file. Coverage preserved: the wedge+pid+`kill -9` half by `socket::tests::a_wedged_campd_fails_the_request_loudly_within_its_bound` (`socket.rs:416`) **and** the new `require_tells_a_wedged_campd_apart_from_a_down_one`, which also re-pins the one-connection law **at the verb-level entry point** (NB2); the "never auto-starts" half becomes structurally impossible. |
| `start_detached_reports_a_wedged_winner_as_unresponsive` | `src/daemon/autostart.rs:187` | Deleted — `start_detached` ceases to exist. |
| `start_detached_recognizes_a_lost_race` | `src/daemon/autostart.rs:215` | Deleted — with no CLI spawn there is no CLI-vs-CLI start race. The daemon-vs-daemon bind race is still pinned by `socket::tests::concurrent_bind_or_replace_elects_exactly_one_daemon` (`socket.rs:603`, 50 rounds × 8 threads) and `daemon_lifecycle::second_daemon_refuses_to_start_while_the_first_lives` (`:283`). |
| `camp_top_autostarts_campd_with_the_event_trail` | `tests/daemon_lifecycle.rs:307` | Asserts the `campd.autostarted → campd.started` trail. **Replaced** (Task 1) by the pure-client tests; its `camp top` happy-path render is preserved by `camp_top_against_a_running_campd_renders_the_snapshot`, and its `camp stop` tail is already covered by `camp_stop_is_graceful` (`:256`). |
| `concurrent_top_autostarts_exactly_one_campd` | `tests/daemon_lifecycle.rs:358` | Eight concurrent `camp top`s racing to auto-start. **Deleted** (Task 1) — the behavior it pins no longer exists. Its `StopGuard` helper (`:142-155`, referenced ONLY at `:310` and `:361`, both inside the two deleted tests) becomes dead code and must go with it. `daemon_lifecycle.rs`'s `use std::io::{…Write}` still survives — `fn request` at `:64` calls `write_all`. |
| `sling_stamps_the_dispatch_default_agent_and_autostarts_campd` | `tests/cli_sling.rs:104` | Slings with campd **down** and asserts success + `campd.autostarted`. **Rewritten** (Task 3). |
| `rig_default_agent_outranks_the_camp_wide_default` | `tests/cli_sling.rs:128` | Slings with campd down, asserts success. **Gets a real daemon** (Task 3). |
| `explicit_agent_flag_outranks_everything` | `tests/cli_sling.rs:146` | Same. **Gets a real daemon** (Task 3). |
| `sling_formula_cooks_a_run_and_pins_it` | `tests/cli_sling.rs:167` | Same — and it has **no** `stop_campd` call today, so it leaks a detached auto-started daemon. **Gets a real daemon** with a `Drop` guard (Task 3), which fixes the leak too. |
| `sling_creates_an_open_unclaimed_bead_with_no_reservation_state` | `tests/cli_sling.rs:252` | Same. **Gets a real daemon** (Task 3). |
| `a_wedged_event_loop_fails_the_cli_loudly_within_its_bound_and_recovers` | `tests/daemon_wedge.rs:178` | **Still passes** (campd is up when it slings; `camp top` against the wedge still yields `CampdUnresponsive`). But its `campd.autostarted == 0` assertion (`:238`) becomes vacuous and its comments describe a removed path. **Truth-swept and strengthened** (Task 4): a wedge must never be reported as a *down* campd. |

`cli_sling.rs`'s `stop_campd` is called at exactly `:124, :142, :161, :263` — the four the conversions replace.

**Tests that survive untouched — verified; do not "fix" them while chasing a grep:**
- `crates/camp-core/src/ledger/mod.rs:2837` `campd_autostarted_is_validated_and_log_only` — **keep it.** It appends `EventType::CampdAutostarted` directly through the ledger API and pins the fold's payload validation. Both the type and the fold arm stay (see Global Constraints), so it passes unchanged — and it is *further evidence* for keeping the type. Deleting it would delete the only test proving old ledgers still fold.
- Every `daemon_*.rs` / `cli_nudge.rs` / `perf_daemon.rs` / `plugin_hooks.rs` / `e2e.rs` test spawns a real `camp daemon` child *before* it slings (`Daemon::spawn`), so `sling`'s poke always finds a live daemon. `daemon_patrol.rs:443`'s `camp adopt` runs with `_campd2` up. `daemon_dispatch.rs:1059`'s `camp top` runs with campd up. `cli_statusline.rs` and `plugin_hooks.rs::statusline_snippet_degrades_visibly_when_campd_is_down` assert the `--statusline` degrade, which is unchanged (its stderr note still contains `"campd"`) — **their behavior is untouched; only their comments are swept** (Task 4). `cli_sling.rs`'s four routing/validation failure tests (`:73, :93, :216, :239`) fail *before* the poke and never needed a daemon.

**Not in the blast radius (verified):** `poke_best_effort` and its 8 call sites; `cmd/stop.rs`; `cmd/nudge.rs`; `cmd/show.rs --wait`; `cmd/doctor.rs` (never touches the socket). `READY_PREFIX` survives (`daemon/mod.rs:218` writes it); `spawn_probe_guard`/`SPAWN_PROBE_LOCK` survive (used by `patrol.rs`, `dispatch.rs`, `bounded.rs`, `spawn.rs`, `socket.rs`, `event_loop.rs`).

**One production consequence, no code change needed:** `plugin/hooks/session-start.sh:9` runs `camp_or_note adopt` on every attended Claude session start. With campd down that now fails — but `camp_or_note` (`plugin/hooks/lib.sh:11-16`) prints a visible stderr note and **always exits 0**. That is exactly the designed degrade (visible, non-blocking). The hook contract already handles it; do not change the hooks.

---

## Stale-prose inventory — every auto-start mention in the tree, and its disposition

This is the completeness artifact for the truth sweep. It is the full output of

```sh
grep -rni "auto-start\|autostart\|auto started\|auto start" crates/ README.md plugin/ Makefile contrib/
```

at b0dc950, with every hit assigned. **Nothing here is left to the implementer's judgment.** (Line numbers shift after the Phase 2 rebase; the grep is the source of truth, this table is the disposition.)

| Location | Disposition |
|---|---|
| `src/cmd/top.rs:5,9,11` | **Task 1** — rewritten to `socket::require`. |
| `src/cmd/top.rs:26,42` | **Task 1** — doc/comment sweep on `statusline`. Behavior FROZEN. |
| `src/cmd/adopt.rs:4,9,13` | **Task 2** — rewritten to `socket::require`. |
| `src/cmd/sling.rs:9,14,43,64,108` | **Task 3** — rewritten to `socket::require`. |
| `src/daemon/autostart.rs` (13 hits) | **Task 3** — file deleted. |
| `src/daemon/mod.rs:6,26` | **Task 3** — `pub mod autostart;` deleted; `READY_PREFIX` doc fixed. |
| `src/daemon/socket.rs:153,154` | **Task 1** — `CampdUnresponsive`'s doc justifies its typing "so the auto-start path can tell…". Re-justified against `CampdNotRunning`. |
| `src/daemon/socket.rs:448` | **Task 1** — the same stale justification in an existing test message. |
| `src/daemon/socket.rs:285` | **FROZEN — the ONE permitted survivor.** `poke_best_effort`'s doc; still true; spec §7.2. Gate B expects exactly this line and nothing else. |
| `src/main.rs:262` | **Task 4** — the clap doc for `camp top`: *"(auto-starts the daemon)"*. **This is `camp --help` output: a user-facing lie the moment this lands.** |
| `src/main.rs:265` | **Task 4** — the clap doc for `--statusline`. |
| `src/cmd/show.rs:32` | **Task 4** — doc only; `--wait`'s behavior FROZEN. |
| `src/cmd/nudge.rs:10` | **Task 4** — module doc only; behavior FROZEN. |
| `src/cmd/stop.rs:7` | **Task 4** — doc only; behavior FROZEN (and Phase 2 rewrites this file — re-read it after the rebase). |
| `tests/daemon_lifecycle.rs:142,307-379` | **Task 1** — `StopGuard` and both auto-start tests deleted. |
| `tests/cli_sling.rs:5,23,68,104,121` | **Task 3** — module doc, `scaffold`'s doc, `stop_campd` (deleted), the test, its assertion. |
| `tests/daemon_wedge.rs:9,200,236-241` | **Task 4** — comments swept; the vacuous assertion replaced with a real one. |
| `tests/cli_statusline.rs:3,73,90` | **Task 4** — module doc, comment, assertion message. Behavior untouched. |
| `tests/plugin_hooks.rs:118,257` | **Task 4** — two comments. Behavior untouched. |
| `camp-core/src/event.rs:25,56,87` · `vocab.rs:27` · `ledger/fold.rs:26,426,430,433,434` · `ledger/mod.rs:2837,2841,2855,2866,2877` | **KEEP — the historical event type.** Task 3 Step 7 adds doc comments marking it historical; the identifiers (`CampdAutostarted` / `campd.autostarted` / `campd_autostarted`) stay. Every one of these lines carries the string `autostarted`, which is exactly what Gates C and D filter on. |
| `README.md:95,250,310,311,439` · `plugin/commands/status.md:5` · `plugin/README.md:61` · `plugin/statusline/statusline.sh:3` | **Task 4** — the user-facing docs. |
| `contrib/launchd/com.gascamp.campd.plist.example:8` | **PHASE 4 — do not touch.** Appendix A. |
| `docs/design/2026-07-05-gas-camp-design.md:126,170-177` | **Task 5** — the minimal v1-spec correction (decided). `:696` stays: `camp show --wait` "never autostarts campd" is still **true**. |

**Policy for the "never auto-starts" negatives (`show.rs:32`, `top.rs:26/42`, `nudge.rs:10`, `stop.rs:7`, `socket.rs:448`, `cli_statusline.rs:3/73/90`, `plugin_hooks.rs:118/257`, `cli_sling.rs:23`): SWEEP THEM.** They are not *false*, but they define live behavior by contrast with a concept that will no longer exist anywhere in the codebase — the same defect the v1-spec correction fixes. Sweeping is one line each, it costs nothing, and it is what makes the gates below both **complete and satisfiable**. The single exception is `poke_best_effort`'s doc, which the operator has frozen (spec §7.2) and which Gate B therefore names as its one expected survivor. **A gate is never satisfied by silencing it: every exclusion in Gates B/C/D below is a named, justified line, not a convenience filter.**

---

## The error text (the user-facing payoff of this phase)

Design §3 requires a dead socket to be *"a loud, actionable fault (naming the pid from the ledger's `campd.started`, and pointing at `camp service status`), never a silent respawn."* This is that text. Two flavors — a campd ran here and died, or none ever ran:

```
campd is not running for camp /Users/x/proj/.camp — nothing is listening on /Users/x/proj/.camp/campd.sock
  the last campd here was pid 47117 (the ledger's last campd.started); that process is gone
  the camp CLI never starts campd: it is a supervised service. Bring it up with one of:
    camp service status --camp /Users/x/proj/.camp   # the managed unit's state (`camp service restart` cycles it)
    camp daemon --camp /Users/x/proj/.camp           # run it yourself: container, CI, or a box with no service manager
  if a supervisor is restarting it in a loop, its stderr is in /Users/x/proj/.camp/campd.log
```

```
campd is not running for camp /Users/x/proj/.camp — nothing is listening on /Users/x/proj/.camp/campd.sock
  no campd has ever started in this camp (no campd.started event in the ledger)
  the camp CLI never starts campd: it is a supervised service. Bring it up with one of:
    camp service status --camp /Users/x/proj/.camp   # the managed unit's state (`camp service restart` cycles it)
    camp daemon --camp /Users/x/proj/.camp           # run it yourself: container, CI, or a box with no service manager
  if a supervisor is restarting it in a loop, its stderr is in /Users/x/proj/.camp/campd.log
```

Why each line is there:

- **Line 1** states the fault and the exact socket, and names the camp — a user with several camps needs to know *which* one is dark.
- **Line 2** is design §3's pid requirement. `kill -9` leaves a stale socket file behind, so "the socket file exists" is not evidence of life; the ledger's last `campd.started` is the only pid source that survives a crash (there are no pidfiles — spec §5). The pid-less flavor **states the absence** rather than omitting the line — the same rule `CampdUnresponsive` already follows ("pid unknown") — and it distinguishes *"your daemon died"* from *"you never had one"*.
- **Line 3** kills the expectation the removed feature created. A user who has typed `camp top` for months and had it Just Work needs to be told, in the error, that the CLI will not do that any more.
- **Line 4** is the supervised remedy (Phase 2's verb). It is first because it is the default environment.
- **Line 5** is the un-supervised remedy: containers, CI, `camp init --no-service` users, and anyone on a box with no service manager. Without this line the phase strands exactly those users — it is not optional.
- **Line 6** closes the loop for the nastiest case: the unit *is* installed, the supervisor *is* restarting campd, and campd is crash-looping. `camp service status` will show it flapping; `campd.log` says why.

Both remedies print `--camp <root>` so they are copy-pasteable regardless of how the camp was resolved (`--camp`, `$CAMP_DIR`, or walk-up). `main.rs:426`'s `eprintln!("{name}: {error:#}")` is anyhow's alternate Display, which renders the whole chain — so every needle the tests assert on reaches stderr.

**Known two-hop path, accepted:** on a camp created before Phase 2 (no unit installed), `camp service status` answers with Phase 2's own loud "this camp has no managed unit — `camp service install` first". Two hops, each loud and each actionable — and the second remedy (`camp daemon`) works immediately either way. This is the intended ordering consequence of §3/§4.3, and it is why line 5 exists.

**`sling` gets one extra sentence of context**, because `sling` is the only daemon-needing verb that has already made a **durable write** by the time it discovers campd cannot serve it. The bead is real; only the dispatch was lost (spec §7.2: campd catches up from its cursor on start). The error says exactly that, and the bead id still reaches stdout — a down campd costs the operator the dispatch, never the id. The sentence is worded to be true of a **wedged** campd too ("a healthy, running campd"), because `require` surfaces that case as well and campd *is* up when it is merely wedged (NB3).

---

## File Structure

**Deleted:**

| File | Why |
|---|---|
| `crates/camp/src/daemon/autostart.rs` | The whole auto-start path — probe, `campd.autostarted` append, detached spawn, readiness-line block, one retry — plus its three unit tests. Design §4.3. |

**Modified:**

| File | Change | Task |
|---|---|---|
| `crates/camp/src/daemon/socket.rs` | **+** `CampdNotRunning` (typed, actionable) and `require()`; three new unit tests; `CampdUnresponsive`'s doc (`:153-154`) and its test message (`:448`) re-justified against `CampdNotRunning`. **`poke_best_effort` untouched — body and doc.** | 1 |
| `crates/camp/src/campdir.rs` | One doc-comment line on `log_path()` (a *supervised* campd's stderr, not "a detached campd's"). | 1 |
| `crates/camp/src/cmd/top.rs` | `run()` calls `socket::require`; `statusline()`'s docs swept — its behavior untouched. | 1 |
| `crates/camp/src/cmd/adopt.rs` | `run()` calls `socket::require`. | 2 |
| `crates/camp/src/cmd/sling.rs` | Both poke sites call `socket::require`, each with a context line naming what IS durable; the id/run-id is printed **before** the poke. | 3 |
| `crates/camp/src/daemon/mod.rs` | Drop `pub mod autostart;`; fix the `READY_PREFIX` doc. Nothing else — signal/event-loop code is Phase 1's and stays byte-for-byte. | 3 |
| `crates/camp-core/src/event.rs`, `ledger/fold.rs`, `vocab.rs` | Doc comments only: mark `campd.autostarted` **historical — no producer, still readable**. No behavior change, no vocabulary change. | 3 |
| **`crates/camp/src/main.rs`** | **The clap docs for `camp top` (`:262`) and `--statusline` (`:265`) — this is `camp --help` output.** | 4 |
| `crates/camp/src/cmd/show.rs`, `cmd/nudge.rs`, `cmd/stop.rs` | **Doc comments only** (`:32`, `:10`, `:7`). Behavior frozen — not one line of code changes in any of them. | 4 |
| `crates/camp/tests/daemon_lifecycle.rs` | Delete the two auto-start tests + `StopGuard`; add the pure-client tests and the shared `assert_no_campd_came_up` helper. | 1, 2 |
| `crates/camp/tests/cli_sling.rs` | Add a `Daemon` harness; five tests get a real campd; add the campd-down sling test; drop `stop_campd`; sweep the module doc and `scaffold`'s doc (`:23`). | 3 |
| `crates/camp/tests/daemon_wedge.rs` | Truth sweep: drop the now-vacuous `campd.autostarted` assertion; assert instead that a wedge is never reported as a *down* campd. | 4 |
| `crates/camp/tests/cli_statusline.rs`, `tests/plugin_hooks.rs` | Comment/message sweep only. No behavior change. | 4 |
| `README.md`, `plugin/commands/status.md`, `plugin/README.md`, `plugin/statusline/statusline.sh` | Remove every auto-start claim. They are lies the moment this lands. | 4 |
| **`docs/design/2026-07-05-gas-camp-design.md`** | **The minimal v1-spec correction (operator-decided): line 126 and the `**Auto-start:**` bullet at lines 170-177. NOTHING ELSE in this file.** | 5 |

**No new files.**

---

## Task 1: `socket::require` + `CampdNotRunning`, and `camp top` becomes a pure client

The new primitive and its first caller land together — a `pub fn` with no caller is dead code in a bin crate and the clippy gate is `-D warnings`.

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs` (add `CampdNotRunning` + `require` + 3 unit tests; re-justify `CampdUnresponsive`'s doc at `:153-154` and its test message at `:448`; the file's `mod tests` already carries `#[allow(clippy::unwrap_used, …)]` and imports `UnixListener`)
- Modify: `crates/camp/src/campdir.rs:27-30` (`log_path` doc comment)
- Modify: `crates/camp/src/cmd/top.rs` (`run` at `:8-23`; the `statusline` doc at `:25-29` and its comment at `:42` — **comments only; `statusline`'s code is frozen**)
- Modify: `crates/camp/tests/daemon_lifecycle.rs` (delete `StopGuard` at `~142-155` and both auto-start tests at `~306-390`; add the helper and three tests)

**Interfaces:**
- Consumes: `crate::daemon::socket::{request_if_up, last_recorded_campd_pid, CampdUnresponsive, Request, Response, REQUEST_TIMEOUT}`; `crate::campdir::CampDir::{root, socket_path, db_path, log_path}`.
- Produces, for Tasks 2 and 3:
  - `socket::require(camp: &CampDir, request: &Request) -> anyhow::Result<Response>` — sends the request on **exactly one** connection; an absent/refusing socket becomes `Err(CampdNotRunning)`; a socket that accepts but does not answer within `REQUEST_TIMEOUT` still becomes `Err(CampdUnresponsive)`; a `Response::Error` line still bails. Never spawns anything.
  - `socket::CampdNotRunning { camp_root: PathBuf, socket: PathBuf, log: PathBuf, last_pid: Option<u32> }` — `impl Display + std::error::Error`; downcastable from `anyhow::Error`.

- [ ] **Step 1: Write the failing unit tests**

Append these three tests to the existing `mod tests` block in `crates/camp/src/daemon/socket.rs` (after `request_if_up_returns_none_when_no_daemon_listens`, before `response_wire_format_is_pinned`):

```rust
    /// Design §4.3 + §3: campd DOWN is a loud, actionable fault — never a
    /// silent respawn. The error names the camp, the socket, the pid the
    /// ledger last recorded (the only pid source that survives a crash:
    /// there are no pidfiles), BOTH remedies, and the daemon's stderr log.
    /// A `kill -9` leaves a stale socket FILE behind, so "the file exists"
    /// is not life: a refusing socket is "not running" too.
    #[test]
    fn require_reports_a_down_campd_loudly_and_actionably() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        let mut ledger = camp_core::ledger::Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::CampdStarted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({ "pid": 424242 }),
            })
            .unwrap();
        drop(ledger);
        // the kill -9 shape: the socket file is there and refuses connections
        drop(UnixListener::bind(camp.socket_path()).unwrap());

        let err = require(&camp, &Request::Status).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            err.downcast_ref::<CampdNotRunning>().is_some(),
            "typed, so a down campd is never confused with a wedged one: {msg}"
        );
        assert!(msg.contains("campd is not running"), "{msg}");
        assert!(
            msg.contains("424242"),
            "must name the last recorded campd pid: {msg}"
        );
        assert!(
            msg.contains("camp service status"),
            "must name the supervised remedy: {msg}"
        );
        assert!(
            msg.contains("camp daemon"),
            "must name the run-it-yourself remedy (containers, CI, no service manager): {msg}"
        );
        assert!(
            msg.contains(&camp.root.display().to_string()),
            "must name the camp — a user has several: {msg}"
        );
        assert!(
            msg.contains("campd.log"),
            "must point at the daemon's stderr (a crash loop shows up there): {msg}"
        );
    }

    /// The pid-unknown flavor: campd never started in this camp. The absence
    /// is STATED, never silently omitted (the CampdUnresponsive precedent) —
    /// "never had one" and "yours died" are different situations.
    #[test]
    fn require_states_a_missing_pid_rather_than_omitting_it() {
        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        drop(camp_core::ledger::Ledger::open(&camp.db_path()).unwrap()); // empty ledger
        // no socket file at all

        let err = require(&camp, &Request::Status).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no campd has ever started"),
            "the missing pid must be stated: {msg}"
        );
        assert!(msg.contains("camp service status"), "{msg}");
        assert!(msg.contains("camp daemon"), "{msg}");
    }

    /// Two laws in one test, both inherited from the path this phase deletes.
    ///
    /// (1) A WEDGED campd is not a down campd: something owns the socket, and
    /// its remedy is `kill -9`, not "start campd". `require` must not flatten
    /// the two — a second daemon would only mask the wedge, and telling the
    /// operator to start one would be wrong advice.
    ///
    /// (2) The request IS the probe (the PR #51 finding 1 law), asserted here
    /// at the VERB-LEVEL entry point — where the unit test in the module this
    /// phase deletes asserted it. A bare-connect pre-probe would open a second
    /// connection AND be fooled by the wedged daemon's kernel backlog, which
    /// accepts connections its event loop never serves. Counting accepts makes
    /// that a test failure, not a review catch, if anyone later "optimizes"
    /// `require`.
    #[test]
    fn require_tells_a_wedged_campd_apart_from_a_down_one_on_one_connection() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _no_spawns = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        let mut ledger = camp_core::ledger::Ledger::open(&camp.db_path()).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::CampdStarted,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({ "pid": 424242 }),
            })
            .unwrap();
        drop(ledger);
        // The wedge simulator: accept (and COUNT) every connection, then hold
        // it open and serve nothing — exactly a daemon stuck mid-syscall.
        let listener = UnixListener::bind(camp.socket_path()).unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepts);
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                counter.fetch_add(1, Ordering::SeqCst);
                held.push(stream); // keep it open, answer nothing
            }
        });

        let start = std::time::Instant::now();
        let err = require(&camp, &Request::Status).unwrap_err();
        assert!(
            start.elapsed() < REQUEST_TIMEOUT * 2 + Duration::from_secs(2),
            "bounded, never a hang"
        );
        let msg = format!("{err:#}");
        assert!(
            err.downcast_ref::<CampdUnresponsive>().is_some(),
            "a wedge stays a wedge: {msg}"
        );
        assert!(
            err.downcast_ref::<CampdNotRunning>().is_none(),
            "a wedged campd must never be reported as a down one: {msg}"
        );
        assert!(msg.contains("kill -9"), "the wedge remedy, unchanged: {msg}");
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "exactly ONE connection: the request IS the probe — no bare-connect \
             pre-probe, which a wedged daemon's listen backlog would fool anyway"
        );
    }
```

Then write the failing integration tests. In `crates/camp/tests/daemon_lifecycle.rs`, **delete** `struct StopGuard` + its `impl Drop` (`~142-155`), `camp_top_autostarts_campd_with_the_event_trail` (`~306-350`) and `concurrent_top_autostarts_exactly_one_campd` (`~352-390`), and add in their place:

```rust
/// The pure-client contract (design §4.3, and §9's test obligation): a
/// daemon-needing verb with campd DOWN fails loudly, names the remedy, and
/// starts NOTHING.
///
/// "Started nothing" is asserted structurally, not by scanning the process
/// table (which a parallel `cargo test` process tree would confound anyway).
/// Three independent tripwires, each of which the removed CLI-spawn path
/// would trip BEFORE the CLI could return:
///   1. `<camp>/campd.log` — `start_detached` opened it (create+append) BEFORE
///      it spawned the child, so this fires even on a regression that spawns a
///      daemon without blocking on its readiness line;
///   2. `<camp>/campd.sock` — a live campd binds it before serving anything;
///   3. a `campd.started` event — appended before the readiness line
///      (`daemon/mod.rs`), and the removed path BLOCKED on that line.
/// No sleep, no poll, no race.
///
/// `starts_before` is how many campds the TEST started by hand (a `kill -9`d
/// daemon leaves its `campd.started` in the ledger and its socket file on
/// disk — that is still "not running").
fn assert_no_campd_came_up(root: &Path, out: &std::process::Output, starts_before: usize) {
    assert!(
        !out.status.success(),
        "a daemon-needing verb must FAIL when campd is down; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !root.join("campd.log").exists(),
        "campd.log is created only by a CLI that is about to spawn a daemon: it must not exist"
    );
    let sock = root.join("campd.sock");
    assert!(
        !sock.exists() || UnixStream::connect(&sock).is_err(),
        "no campd may be listening: the CLI must never start one"
    );
    let types = event_types(root);
    assert_eq!(
        types.iter().filter(|t| t.as_str() == "campd.started").count(),
        starts_before,
        "the CLI must not have started a campd: {types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|t| t.as_str() == "campd.autostarted")
            .count(),
        0,
        "the CLI is a pure client: no campd.autostarted may ever be recorded again: {types:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in ["campd is not running", "camp service status", "camp daemon"] {
        assert!(
            stderr.contains(needle),
            "the error must name {needle:?} — the remedy IS the feature: {stderr}"
        );
    }
}

#[test]
fn camp_top_with_campd_down_fails_loudly_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());

    let out = camp_cmd(&root).arg("top").output().unwrap();

    assert_no_campd_came_up(&root, &out, 0);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no campd has ever started"),
        "a camp whose campd never ran must say so, not omit the pid: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Design §3: the down-campd error NAMES the pid from the ledger's last
/// `campd.started` — the operator's thread back to the process that died.
/// `kill -9` leaves a stale socket file; that is still "not running", never a
/// wedge, and the two errors must not be interchangeable.
#[test]
fn camp_top_after_a_kill_dash_nine_names_the_dead_campd_pid() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let mut daemon = Daemon::spawn(&root);
    let pid = daemon.child.id();
    daemon.kill_dash_nine();
    assert!(
        root.join("campd.sock").exists(),
        "kill -9 leaves the socket file behind (stale)"
    );

    let out = camp_cmd(&root).arg("top").output().unwrap();

    assert_no_campd_came_up(&root, &out, 1); // only the one WE started
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&pid.to_string()),
        "must name the dead campd's pid {pid}: {stderr}"
    );
    assert!(
        !stderr.contains("kill -9"),
        "a DEAD campd is not a wedged one: the remedies differ: {stderr}"
    );
}

/// The happy path, unchanged: against a running campd, `camp top` is one
/// status query rendered as plain text.
#[test]
fn camp_top_against_a_running_campd_renders_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let daemon = Daemon::spawn(&root);

    let out = run_ok(&root, &["top"]);

    assert!(
        out.contains(&format!("campd pid: {}", daemon.child.id())),
        "top output: {out:?}"
    );
    assert!(out.contains("ready: 0"), "top output: {out:?}");
    assert!(out.contains("open: 0"), "top output: {out:?}");
}
```

(The `campd.log` tripwire is safe in both new tests: `camp init` never creates that file, and `daemon_lifecycle.rs`'s hand-spawned `Daemon` uses `stderr(Stdio::inherit())` — only the deleted auto-start path ever wrote it.)

- [ ] **Step 2: Run the tests and watch them fail**

```sh
cargo test -p camp --bin camp -- daemon::socket::tests::require
```
Expected: **compile error** — `cannot find function 'require' in module 'socket'` / `cannot find type 'CampdNotRunning'`. That is the failing state for a new API.

```sh
cargo test -p camp --test daemon_lifecycle
```
Expected: **red assertions, NOT a compile error.** An integration test in this crate never links the binary's internals (there is no lib target — it shells out to `CARGO_BIN_EXE_camp`), so `socket.rs`'s `mod tests` is not even compiled for this target and the missing `require` cannot break the build. Instead the two new down-campd tests **run against the still-auto-starting binary and fail red at their assertions** — `camp top` spawns a campd, exits 0, and `assert_no_campd_came_up`'s `!out.status.success()` blows up first. That is a *stronger* TDD signal than a compile error: it proves the tests actually exercise the behavior being removed. (The third new test, `camp_top_against_a_running_campd_renders_the_snapshot`, **passes already** — it is a preserved happy path, not new behavior.)

- [ ] **Step 3: Implement `CampdNotRunning` + `require`**

In `crates/camp/src/daemon/socket.rs`, insert directly **after** `impl std::error::Error for CampdUnresponsive {}` (i.e. between the two error types and the `request` fn), so the two faults sit side by side:

```rust
/// campd is NOT RUNNING: nothing is listening on the socket — it is absent,
/// or it is a stale file a `kill -9` left behind. The CLI is a PURE CLIENT
/// (design §4.3: one path — campd is a supervised foreground process, run by
/// launchd / systemd --user / the container runtime / you), so a
/// daemon-needing verb turns this into a LOUD, actionable fault and stops.
/// It never spawns a daemon: a silent respawn hides the real fault (a broken
/// unit, a crash loop, a camp nobody supervised) and it is exactly the
/// behavior this phase removes.
///
/// Typed so it can be told apart from `CampdUnresponsive` — something owns
/// the socket but does not serve it. Different fault, different remedy
/// (`kill -9`, not "start campd"): flattening them would give wrong advice.
#[derive(Debug)]
pub struct CampdNotRunning {
    /// The camp this verb resolved to — a user has several; say which is dark.
    pub camp_root: std::path::PathBuf,
    pub socket: std::path::PathBuf,
    /// Where a supervised campd's stderr lands: a crash-restart loop is
    /// visible there and nowhere else.
    pub log: std::path::PathBuf,
    /// The pid from the ledger's last campd.started — the campd that WAS
    /// running here (design §3). The only pid source that survives a crash:
    /// there are no pidfiles (spec §5). None when none ever started.
    pub last_pid: Option<u32>,
}

impl std::fmt::Display for CampdNotRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let root = self.camp_root.display();
        let history = match self.last_pid {
            Some(pid) => format!(
                "the last campd here was pid {pid} (the ledger's last campd.started); \
                 that process is gone"
            ),
            None => "no campd has ever started in this camp (no campd.started event in \
                     the ledger)"
                .to_owned(),
        };
        write!(
            f,
            "campd is not running for camp {root} — nothing is listening on {socket}\n  \
             {history}\n  \
             the camp CLI never starts campd: it is a supervised service. Bring it up \
             with one of:\n    \
             camp service status --camp {root}   # the managed unit's state \
             (`camp service restart` cycles it)\n    \
             camp daemon --camp {root}           # run it yourself: container, CI, or a \
             box with no service manager\n  \
             if a supervisor is restarting it in a loop, its stderr is in {log}",
            socket = self.socket.display(),
            log = self.log.display(),
        )
    }
}

impl std::error::Error for CampdNotRunning {}

/// The daemon-needing verb's request path (design §4.3) — the ONLY way `top`,
/// `adopt` and `sling` reach campd. Send the request; a campd that is not
/// running is the loud `CampdNotRunning` error. The CLI never starts one.
///
/// Built on `request_if_up`, so liveness is judged on the SAME connection that
/// carries the request (the PR #51 finding 1 law): exactly one connect, no
/// bare pre-probe — which would both open a second connection and be fooled by
/// a wedged daemon's listen backlog. A campd that accepts and then never
/// answers therefore still surfaces as `CampdUnresponsive`: it owns the
/// socket, and its remedy is different.
pub fn require(camp: &CampDir, request: &Request) -> Result<Response> {
    match request_if_up(camp, request)? {
        Some(response) => Ok(response),
        None => Err(anyhow::Error::new(CampdNotRunning {
            camp_root: camp.root.clone(),
            socket: camp.socket_path(),
            log: camp.log_path(),
            last_pid: last_recorded_campd_pid(camp),
        })),
    }
}
```

Re-justify `CampdUnresponsive`'s typing (`socket.rs:150-154`) — it currently explains itself in terms of the path this phase deletes:

```rust
/// The wedge shape (issue #55): the kernel's listen backlog accepted the
/// connection — that happens even when the event loop never runs accept —
/// but no response line arrived within REQUEST_TIMEOUT. Typed so it is never
/// confused with `CampdNotRunning` ("nothing is listening"): something owns
/// THIS socket but does not serve it, and its remedy is `kill -9`, not
/// "start campd". Two faults, two remedies — the CLI must not flatten them.
#[derive(Debug)]
pub struct CampdUnresponsive {
```

…and the same stale justification in the existing test message (`socket.rs:448`):

```rust
        assert!(
            err.downcast_ref::<CampdUnresponsive>().is_some(),
            "typed, so a wedge is never reported as a down campd: {msg}"
        );
```

In `crates/camp/src/campdir.rs`, fix the now-stale doc on `log_path` (`~:27`):

```rust
    /// Where a supervised campd's stderr lands (never silenced, never
    /// hidden): the launchd/systemd unit points at this file, and a
    /// crash-restart loop is visible here. Named by the CampdNotRunning
    /// error so the operator reading it is one line from the reason.
    pub fn log_path(&self) -> PathBuf {
        self.root.join("campd.log")
    }
```

In `crates/camp/src/cmd/top.rs`, replace the import block and `run`, and sweep `statusline`'s prose — **`statusline`'s code is frozen; only its doc and its inline comment change**:

```rust
use anyhow::{Result, bail};
use camp_core::ledger::StatusSummary;

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp top`: ONE status query rendered as plain text — a query, not a loop
/// (spec §5); refresh is running it again. A PURE CLIENT (design §4.3): it
/// never starts campd — a campd that is down is a loud, actionable error.
pub fn run(camp: &CampDir) -> Result<()> {
    let response = socket::require(camp, &Request::Status)?;
    let Response::Status {
        summary,
        red,
        campd_pid,
        ..
    } = response
    else {
        bail!("unexpected response to status: {response:?}");
    };
    print!("{}", render(&summary, red, campd_pid));
    Ok(())
}

/// `camp top --statusline`: the compact fleet badge `▲live ●ready ✖red`, from
/// ONE read-only socket query. When campd is down it prints nothing to stdout
/// and writes a visible stderr note, exiting 0 — visible degradation, not
/// silence (spec §11). It is the one daemon-needing surface that does NOT fail
/// loudly, by design: a status line may never break the user's prompt. The
/// plugin's statusline snippet is a thin wrapper over this.
pub fn statusline(camp: &CampDir) -> Result<()> {
    match socket::request(camp, &Request::Status) {
        Ok(Response::Status { summary, red, .. }) => {
            println!(
                "▲{} ●{} ✖{}",
                summary.live_sessions.len(),
                summary.ready,
                red
            );
            Ok(())
        }
        Ok(other) => bail!("unexpected response to status: {other:?}"),
        // campd down or wedged: degrade visibly (stderr), never fail the
        // caller. The badge is empty; the note says why.
        Err(e) => {
            eprintln!("camp: campd unavailable — statusline empty ({e:#})");
            Ok(())
        }
    }
}
```

(The `use crate::daemon::autostart;` line is gone. `autostart` still exists — `adopt` and `sling` still use it — so the crate compiles.)

- [ ] **Step 4: Run the tests and watch them pass**

```sh
cargo test -p camp --bin camp -- daemon::socket::tests::require
cargo test -p camp --test daemon_lifecycle
cargo test -p camp --test cli_statusline   # statusline's behavior is untouched — prove it
```
Expected: PASS — 3 new unit tests, `daemon_lifecycle` green with its three new tests and the two auto-start tests gone, `cli_statusline` unchanged.

- [ ] **Step 5: Gates, then commit**

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
git add crates/camp/src/daemon/socket.rs crates/camp/src/campdir.rs crates/camp/src/cmd/top.rs crates/camp/tests/daemon_lifecycle.rs
git commit -m "feat(cli): socket::require — a down campd is a loud, actionable error; camp top is a pure client"
```

---

## Task 2: `camp adopt` becomes a pure client

**Files:**
- Modify: `crates/camp/src/cmd/adopt.rs` (the whole file — it is 31 lines)
- Modify: `crates/camp/tests/daemon_lifecycle.rs` (one new test, reusing Task 1's `assert_no_campd_came_up`)

**Interfaces:**
- Consumes: `socket::require` (Task 1); `socket::{Request::Adopt, Response::Adopt}`; `assert_no_campd_came_up` (Task 1, same test file).
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

Add to `crates/camp/tests/daemon_lifecycle.rs`, next to the `camp top` pure-client tests:

```rust
/// `camp adopt` is a socket op executed BY campd (the registry and the timers
/// live in its memory), so it needs the daemon. Pure client: campd down is a
/// loud, actionable error — never a fresh daemon started behind the operator's
/// back just to answer a reconciliation request.
#[test]
fn camp_adopt_with_campd_down_fails_loudly_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());

    let out = camp_cmd(&root).arg("adopt").output().unwrap();

    assert_no_campd_came_up(&root, &out, 0);
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("adopted:"),
        "nothing was adopted: the summary line must not be printed"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

```sh
cargo test -p camp --test daemon_lifecycle camp_adopt_with_campd_down
```
Expected: FAIL — `camp adopt` auto-starts a campd, exits 0, prints `adopted: 0 crashed, …`; the `!out.status.success()` assertion blows up first.

- [ ] **Step 3: Implement**

Replace `crates/camp/src/cmd/adopt.rs` in full:

```rust
use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp adopt`: reconcile the session registry against reality (spec §8.5) —
/// the routine campd runs automatically at start, on demand. A PURE CLIENT
/// (design §4.3): campd holds the registry and the timers, so this verb needs
/// it; a campd that is down is a loud, actionable error, never a spawn.
pub fn run(camp: &CampDir) -> Result<()> {
    let response = socket::require(camp, &Request::Adopt)?;
    match response {
        Response::Adopt {
            crashed,
            rearmed,
            released,
            swept,
            kept,
            ..
        } => {
            println!(
                "adopted: {crashed} crashed, {rearmed} re-armed, {released} released, \
                 {swept} worktrees swept, {kept} kept"
            );
            Ok(())
        }
        other => bail!("unexpected response to adopt: {other:?}"),
    }
}
```

- [ ] **Step 4: Run it and watch it pass**

```sh
cargo test -p camp --test daemon_lifecycle
cargo test -p camp --test daemon_patrol   # kill9_campd_then_adopt_reconciles_exactly: campd IS up there — must stay green
cargo test -p camp --test plugin_hooks    # the SessionStart hook runs `camp adopt` against a live campd
```
Expected: PASS, all three.

- [ ] **Step 5: Gates, then commit**

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
git add crates/camp/src/cmd/adopt.rs crates/camp/tests/daemon_lifecycle.rs
git commit -m "feat(cli): camp adopt is a pure client — a down campd fails loudly"
```

---

## Task 3: `camp sling` becomes a pure client, and `daemon/autostart.rs` is deleted

`sling` holds the last two callers, so its conversion and the module's deletion are **one commit** — anything else leaves `request_with_autostart` uncalled and the `-D warnings` gate red.

`sling` is the only daemon-needing verb that has already made a **durable write** when it discovers campd cannot serve it. The bead (or the cooked run) is real and campd will pick it up from its cursor whenever it next starts (spec §7.2). So: **print the id first, then poke.** A down campd costs the operator the dispatch, never the id — and the error says exactly which of the two happened.

**Files:**
- Modify: `crates/camp/src/cmd/sling.rs` (imports; the `run` doc; `sling_formula` `~44-67`; `sling_bead` `~69-111`)
- Modify: `crates/camp/src/daemon/mod.rs` (delete `pub mod autostart;` at `:6`; fix the `READY_PREFIX` doc at `~25-28`)
- Delete: `crates/camp/src/daemon/autostart.rs`
- Modify: `crates/camp-core/src/event.rs`, `crates/camp-core/src/ledger/fold.rs`, `crates/camp-core/src/vocab.rs` (doc comments only)
- Modify: `crates/camp/tests/cli_sling.rs`

**Interfaces:**
- Consumes: `socket::require` (Task 1); `socket::{Request::Poke, CampdNotRunning}`.
- Produces: nothing new. After this task the symbols `autostart`, `request_with_autostart` and `start_detached` do not exist anywhere in the workspace.

- [ ] **Step 1: Write the failing tests**

In `crates/camp/tests/cli_sling.rs`:

(a) Fix the module doc (`:2-5`) — it advertises "the auto-start poke":

```rust
//! camp sling (spec §8.1 Tier 0; master plan Phase 8). The daemon-side
//! dispatch behavior lives in daemon_dispatch.rs; this file covers the
//! CLI surface: routing resolution, fail-fast messages, assignee stamping,
//! and the poke to a running campd — sling is a PURE CLIENT (design §4.3):
//! it never starts a daemon, and a campd that is down fails it loudly.
```

(b) Fix `scaffold`'s doc (`:23-24`) — it describes a daemon the tests now spawn by hand:

```rust
/// A camp with one rig and a config we control completely. `command` is
/// `true`, so when a test spawns a real campd its dispatch spawn is harmless.
```

(c) Widen the imports (`:7-8`) and **delete** the `stop_campd` helper (`~:67-70`):

```rust
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
```

(d) Add the `Daemon` harness after `events_json` — the same shape every other integration test in this crate uses (the `camp` crate has **no lib target**, so a shared harness module is not the house pattern; duplicate it):

```rust
const READY_PREFIX: &str = "campd listening on ";

/// A real campd child. `spawn` blocks on the readiness line (deterministic —
/// no connect polling); `Drop` SIGKILLs and reaps it (crash-only: a kill -9 is
/// a supported shutdown, spec §5). `sling` is a pure client now, so every test
/// whose sling must SUCCEED needs one of these up first.
///
/// This works in a `scaffold`-built camp even though `scaffold` never shells
/// out to `camp init` (it writes `camp.toml` and opens the ledger directly):
/// `camp daemon --camp <root>` is exactly the command the removed CLI-spawn
/// path used to run, in exactly these scaffolded camps.
struct Daemon {
    child: Child,
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
        Daemon { child }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
```

(e) The new pure-client test — **this is the phase's central sling obligation**:

```rust
/// Design §4.3 + §9's test obligation. `sling` promises dispatch, and campd is
/// the only dispatcher — so a campd that is down FAILS it, loudly. It does not
/// spawn one. What it does NOT do is lose the operator's work: the bead is
/// created (the write is durable — spec §7.2: campd catches up from its cursor
/// on start), its id still reaches stdout, and the error says precisely what
/// did and did not happen.
#[test]
fn sling_with_campd_down_creates_the_bead_prints_it_and_fails_loudly_without_spawning_a_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");

    let out = camp(&root, &["sling", "no daemon here"]);

    assert!(
        !out.status.success(),
        "sling promises dispatch: a down campd must fail it"
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        "gc-1",
        "the durable bead id still reaches stdout — a down campd costs the dispatch, not the id"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in [
        "gc-1",
        "NOT dispatched",
        "campd is not running",
        "camp service status",
        "camp daemon",
    ] {
        assert!(
            stderr.contains(needle),
            "the error must name {needle:?}: {stderr}"
        );
    }
    // the write is durable and honest…
    let events = events_json(&root);
    assert!(
        events.iter().any(|e| e["type"] == "bead.created"),
        "the bead must exist: {events:?}"
    );
    // …and NO daemon came up (the same three tripwires as daemon_lifecycle's
    // assert_no_campd_came_up: the log the removed path opened BEFORE it
    // spawned, the socket a live campd binds, and the campd.started it appends).
    assert!(
        !root.join("campd.log").exists(),
        "campd.log is created only by a CLI about to spawn a daemon"
    );
    assert!(
        !root.join("campd.sock").exists(),
        "the CLI must never start campd"
    );
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "campd.started" || e["type"] == "campd.autostarted"),
        "no campd may have come up: {events:?}"
    );
}
```

(f) Rewrite `sling_stamps_the_dispatch_default_agent_and_autostarts_campd` (`~:104`) into its pure-client form, and give the four other sling-must-succeed tests a real daemon. Replace each `stop_campd(&root);` call with a `let _campd = Daemon::spawn(&root);` placed **immediately after** the agents are written and **before** the sling:

```rust
#[test]
fn sling_stamps_the_dispatch_default_agent_and_pokes_a_running_campd() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    let _campd = Daemon::spawn(&root);

    let out = camp(&root, &["sling", "add a flag"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bead = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    assert_eq!(bead, "gc-1");
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "dev");
    assert_eq!(created["data"]["title"], "add a flag");
    assert!(
        !events.iter().any(|e| e["type"] == "campd.autostarted"),
        "the CLI is a pure client: no campd.autostarted may ever be recorded: {events:?}"
    );
}

#[test]
fn rig_default_agent_outranks_the_camp_wide_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    let _campd = Daemon::spawn(&root);

    let out = camp(&root, &["sling", "review it", "--rig", "gc"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "rigger");
}

#[test]
fn explicit_agent_flag_outranks_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    write_agent(&root, "special");
    let _campd = Daemon::spawn(&root);

    let out = camp(&root, &["sling", "x", "--agent", "special"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "special");
}
```

In `sling_formula_cooks_a_run_and_pins_it` (`~:167`) add `let _campd = Daemon::spawn(&root);` after the formula file is written and before `camp(&root, &["sling", "--formula", "one-step"])`. (Today this test leaks a detached auto-started daemon — it never stops one. The `Daemon` guard fixes that too.)

In `sling_creates_an_open_unclaimed_bead_with_no_reservation_state` (`~:252`) add `let _campd = Daemon::spawn(&root);` after `write_agent`, and delete its `stop_campd(&root);` line.

Leave `sling_with_no_route_fails_naming_all_three_fixes_and_creates_nothing`, `sling_with_an_unresolvable_agent_fails_before_creating_anything`, `sling_formula_errors_name_the_formula` and `sling_rejects_formula_combined_with_a_title` **exactly as they are** — they fail before the poke and must keep passing with no daemon anywhere (the first one's `assert!(!root.join("campd.sock").exists())` is now a permanent truth, not a race).

- [ ] **Step 2: Run and watch them fail**

```sh
cargo test -p camp --test cli_sling
```
Expected: `sling_with_campd_down_…` FAILS (`sling` auto-starts a campd and exits 0 — `!out.status.success()` blows up), and `sling_stamps_the_dispatch_default_agent_and_pokes_a_running_campd` FAILS on the `campd.autostarted` assertion.

- [ ] **Step 3: Implement — `sling` calls `require`, prints first**

In `crates/camp/src/cmd/sling.rs`, change the imports (`:1-10`):

```rust
use anyhow::{Result, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::pack;

use crate::campdir::CampDir;
use crate::cmd::create::resolve_rig;
use crate::daemon::socket::{self, Request};
```

Change the `run` doc block (`:12-21`) to:

```rust
/// `camp sling "<title>" [--agent a] [--rig r]` (spec §8.1, Tier 0): one
/// `bead.created` with the routed agent stamped as assignee, then a poke to a
/// RUNNING campd — sling promises dispatch, so a fire-and-forget poke is not
/// enough (Phase 8 plan decision P). A PURE CLIENT (design §4.3): it never
/// starts campd. A campd that cannot serve the poke fails the verb loudly —
/// but the bead is already durable, so its id is printed FIRST and the error
/// says the bead exists and will dispatch once campd is up and serving (spec
/// §7.2: campd catches up from its cursor on start). campd does the spawning;
/// there is no second dispatch path (dispatch-lifecycle Phase 1, #29).
///
/// `camp sling --formula <name> [--rig r]` (spec §8.2, Phase 9 plan
/// Decision 7): cook `<camp>/formulas/<name>.toml` into `<camp>/runs/`
/// and poke — from that moment campd advances the run (spec §8.3).
```

Fix `sling_formula`'s doc (`~:42-43`) and its tail:

```rust
/// Cook a formula run (spec §8.2): pin into runs/, materialize beads, poke a
/// running campd. Prints "<run_id> root <root-bead>".
```

```rust
    drop(ledger); // campd may need the write lock immediately
    // The run is cooked and PINNED — durable before the poke, so print it
    // before we can fail on a campd that cannot serve us (spec §7.2).
    println!("{} root {}", cooked.run_id, cooked.root_bead);
    socket::require(camp, &Request::Poke { seq: head }).map_err(|e| {
        e.context(format!(
            "run {} is cooked and pinned, but NOT started — campd advances runs, and only \
             a healthy, running campd can; it starts as soon as one is (campd catches up \
             from its cursor)",
            cooked.run_id
        ))
    })?;
    Ok(())
```

And `sling_bead`'s tail:

```rust
    drop(ledger); // campd may need the write lock immediately

    // The bead is DURABLE now: print it before the poke, so a campd that
    // cannot serve us costs the operator the dispatch and never the id.
    println!("{id}");
    socket::require(camp, &Request::Poke { seq }).map_err(|e| {
        e.context(format!(
            "{id} is created and durable, but NOT dispatched — sling promises dispatch, and \
             only a healthy, running campd dispatches; it runs as soon as one is (campd \
             catches up from its cursor on start)"
        ))
    })?;
    Ok(())
```

The context is worded to be true of **both** failure modes `require` can produce: a campd that is *down* and a campd that is *wedged* (which is up, but not serving). `anyhow::Error::context` is an inherent method — no `use anyhow::Context` import is needed, exactly as in `cmd/stop.rs`. The cause line still carries the specific remedy (`camp service status` / `camp daemon`, or `kill -9`).

- [ ] **Step 4: Delete the auto-start module**

```sh
git rm crates/camp/src/daemon/autostart.rs
```

In `crates/camp/src/daemon/mod.rs`, delete line 6 (`pub mod autostart;`) and fix the `READY_PREFIX` doc (`~:25-28`), which still names auto-start:

```rust
/// The single line campd prints to stdout once the socket accepts. Anything
/// that starts campd and needs to know it is up — a supervisor, a container
/// entrypoint, the test harnesses — blocks on this line: an OS pipe read, not
/// a sleep/retry loop. stdout is never written again after this line.
pub const READY_PREFIX: &str = "campd listening on ";
```

Touch nothing else in `daemon/mod.rs` — the signal handling and event loop are Phase 1's and must stay byte-for-byte.

- [ ] **Step 5: Run and watch them pass**

```sh
cargo test -p camp --test cli_sling
cargo test -p camp --test daemon_dispatch    # 20+ slings against a live campd — must stay green
cargo test -p camp --test daemon_graph       # sling --formula against a live campd
```
Expected: PASS. The success-path stdout is byte-identical (the id is merely written a few microseconds earlier), so a failure in `daemon_dispatch`/`daemon_graph` is a real bug, not expected churn — read it, do not paper over it.

- [ ] **Step 6: Confirm the module is gone**

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
grep -rn "request_with_autostart\|autostart::\|mod autostart" crates/camp/src && echo "STILL THERE — fix it" || echo "clean"
```
Expected: clippy green (no dead code — `CampDir::log_path` gained its new caller in `CampdNotRunning` back in Task 1, and `request_with_autostart` no longer exists), and the grep prints `clean`. **This grep targets the removed SYMBOLS only** — the broader prose sweep is Task 4, and it is deliberately not run here: `cmd/show.rs`, `cmd/nudge.rs` and `poke_best_effort` still carry the word "auto-start" at this point and are not Task 3's business.

- [ ] **Step 7: Mark `campd.autostarted` historical in camp-core (doc comments only)**

The event **type stays**. `EventType::parse` returns `CoreError::UnknownEventType` for a name it does not know (`event.rs:107-113`), and the row→`Event` decoder (`ledger/mod.rs:694`) calls it on **every read path**. Deleting the variant would make every existing ledger that ever auto-started **unreadable**. Invariant 3 says the ledger tells the whole story; that includes the stories it already holds. Phase 3 removes the producer, not the record.

Word these three comments **without** the words "auto-start" / "auto start" / "auto-started" — the Global Constraints' no-new-prose rule, which binds every task. The identifiers already carry `autostarted`, and Gate D (Task 4 Step 7) filters on exactly that, so prose reintroducing the hyphenated form would trip the gate for no reason. Say "the removed CLI-spawn path".

In `crates/camp-core/src/event.rs`, add a doc comment to the variant in the `enum EventType` declaration (`~:25`):

```rust
    /// HISTORICAL — no producer since the CLI became a pure socket client
    /// (campd-service-management design §4.3): the removed CLI-spawn path
    /// recorded which verb had spawned campd. The type STAYS: `EventType::parse`
    /// rejects unknown names and every read path goes through it, so dropping
    /// this variant would make any ledger that carries one unreadable
    /// (`camp events`, the fold, `refold`). Invariant 3 — the ledger tells the
    /// whole story, old ones included.
    CampdAutostarted,
```

In `crates/camp-core/src/ledger/fold.rs`, replace the doc on `fn campd_autostarted` (`~:430-432`):

```rust
/// `campd.autostarted` is log-only, and HISTORICAL: nothing emits it since the
/// CLI became a pure socket client (design §4.3). The arm stays so that ledgers
/// written before that still fold — and it still validates the audit payload,
/// so a malformed event fails fast, then and now.
```

In `crates/camp-core/src/vocab.rs`, annotate the entry (`~:27`) — the **string stays in `CAMP_SPECIFIC_EVENTS`**, so the `gc-vocab.json` mirror still matches (invariant 7):

```rust
    "campd.autostarted", // historical: no producer since the CLI became a pure client
```

- [ ] **Step 8: Gates, then commit**

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
git add -A crates/camp/src/cmd/sling.rs crates/camp/src/daemon crates/camp-core/src crates/camp/tests/cli_sling.rs
git commit -m "feat(cli): camp sling is a pure client; remove the campd auto-start path"
```

---

## Task 4: The truth sweep — no doc, comment, help text or test may still claim auto-start

Every remaining sentence in the repo that says campd auto-starts is now a lie, and one of them is printed by `camp --help`. `-D warnings` cannot catch a lie in prose; the four gates in Step 7 are this task's real deliverable.

**Files:**
- Modify: `crates/camp/tests/daemon_wedge.rs` (module doc `~:1-17`; comments `~:199-201`; assertions `~:236-247`)
- Modify: `crates/camp/src/main.rs` (`:262`, `:265` — clap doc comments; **user-facing `--help` text**)
- Modify: `crates/camp/src/cmd/show.rs` (`:32`), `cmd/nudge.rs` (`:10`), `cmd/stop.rs` (`:7`) — **doc comments only; not one line of code**
- Modify: `crates/camp/tests/cli_statusline.rs` (`:3`, `:73`, `:90`), `tests/plugin_hooks.rs` (`:118`, `:257`) — comments and one assertion message
- Modify: `README.md` (`~:95-98`, `~:250`, `~:310-311`, `~:437-439`), `plugin/commands/status.md` (`:5`), `plugin/README.md` (`~:60-61`), `plugin/statusline/statusline.sh` (`:3`)

**Interfaces:** Consumes nothing, produces nothing. Pure truth maintenance — and the gate that keeps it true.

- [ ] **Step 1: Take the inventory (this is the failing state)**

```sh
grep -rni "auto-start\|autostart\|auto started\|auto start" crates/ README.md plugin/ Makefile
```
Reconcile every hit against the plan's **stale-prose inventory** table. At this point (Tasks 1-3 done) the survivors are exactly the rows this task owns, plus `socket.rs`'s frozen `poke_best_effort` doc and camp-core's historical-event identifiers. If a hit appears that the table does not list, Phase 2 introduced it — sweep it too and say so in the PR body.

- [ ] **Step 2: Strengthen `daemon_wedge.rs` (it is a test, so it goes first)**

The `campd.autostarted == 0` assertion (`~:238-242`) is now vacuous — the event has no producer. Replace it with the assertion that still has teeth: **a wedged campd must never be reported as a DOWN campd** (different fault, different remedy — and the two error types are the whole reason `require` is built on `request_if_up`).

Change obligation (2) in the module doc (`:9`):

```rust
//!   2. the CLI (a pure client — design §4.3) never spawns a daemon, and it
//!      never mistakes the wedge for a DOWN campd: something owns the socket,
//!      so the remedy is kill -9, not "start campd";
```

Change the comment above the `camp top` call (`~:199-201`):

```rust
    // (1) A CLI verb against the wedged daemon: loud, actionable, bounded —
    // never a hang. `camp top` needs the daemon, so this also proves (2): the
    // wedge is reported AS a wedge, and no second campd is started.
```

Replace the assertion block (`~:236-247`) with:

```rust
    // (2) continued: the wedge started no second campd, and it was never
    // reported as a down one — the remedies differ, so the errors must too.
    assert!(
        !stderr.contains("campd is not running"),
        "a WEDGED campd owns its socket: reporting it as 'not running' would send \
         the operator to `camp service status` instead of `kill -9`: {stderr}"
    );
    let events = events_json(&root);
    assert_eq!(
        count(&events, "campd.started"),
        1,
        "exactly one campd ever started — the CLI never starts one: {events:#?}"
    );
```

(`stderr` is in scope from the `camp top` call above — the second `let top = …` at `~:260` shadows it only afterwards. The `count` helper stays; it is still used here.)

- [ ] **Step 3: Run it and watch it pass**

```sh
cargo test -p camp --test daemon_wedge
```
Expected: PASS. This test was already green after Task 3 — it is being *strengthened*, not fixed. If the new `!stderr.contains("campd is not running")` assertion FAILS, that is a real bug: `require` is flattening a wedge into a down campd, which means `mark_wedge`'s timeout classification broke. **Fix the code, not the test.**

- [ ] **Step 4: Sweep the binary crate's prose — starting with the `--help` lie**

`crates/camp/src/main.rs` (`~:262-267`) — these are **clap doc comments: they are printed by `camp --help` and `camp top --help`**:

```rust
    /// One campd status snapshot as plain text (campd must be running)
    Top {
        /// Render the compact fleet badge (▲live ●ready ✖red) from a
        /// read-only socket query. Prints nothing and notes on stderr when
        /// campd is down, exiting 0 (spec §11).
        #[arg(long)]
        statusline: bool,
    },
```

`crates/camp/src/cmd/show.rs` (`~:30-32`) — **doc only; `--wait`'s behavior is frozen**:

```rust
/// `camp show <bead> [--json] [--wait [--timeout SECONDS]]`: current state
/// plus full event history (spec §7.4). Read-only: `show` never writes and
/// never starts campd — `--wait` is a pure observer (design §7).
```

`crates/camp/src/cmd/nudge.rs` (`~:9-11`) — **module doc only; behavior frozen**:

```rust
//! never a dispatch mode: this verb never dispatches, reserves, or spawns
//! workers, and it never starts campd (a fresh campd could hold no pipe for
//! the target anyway).
```

`crates/camp/src/cmd/stop.rs` (`~:6-7`) — **doc only; behavior frozen, and Phase 2 rewrites this file, so re-read it after the rebase and adapt**:

```rust
/// `camp stop`: graceful daemon shutdown over the socket. Stopping nothing is
/// an error, not a no-op — the CLI never starts campd, so it never un-stops it.
```

- [ ] **Step 5: Sweep the test-crate comments (no behavior changes)**

`crates/camp/tests/cli_statusline.rs` — module doc (`:2-5`), the comment at `:73`, the assertion message at `:90`:

```rust
//! camp top --statusline (Phase 12): the fleet badge `▲live ●ready ✖red`.
//! A read-only socket query; when campd is down it degrades to empty stdout
//! + a visible stderr note (exit 0) — visible degradation, not silence
//! (spec §11), and the one daemon-needing surface that does not fail loudly.
```
```rust
    // campd is NOT running; --statusline must still exit 0 with an empty badge.
```
```rust
        "--statusline must never start campd"
```

`crates/camp/tests/plugin_hooks.rs` — the `Daemon` doc (`~:117-119`) and the comment at `:257`:

```rust
/// A real campd child; stopped on drop. SessionStart runs `camp adopt`, which
/// is a pure client: it needs this daemon up, and fails loudly without it.
```
```rust
    // no campd running; the snippet must degrade, not start one
```

- [ ] **Step 6: Purge the auto-start claims from the user-facing docs**

⚠ **Re-read each file first.** Phase 2 adds a `camp service` subsection to `README.md` and may already have rewritten some of this prose. The line numbers are from `main` b0dc950. **The acceptance criterion is Step 7's gates, not these line numbers.**

`README.md` (`~:95-98`) — the paragraph under the install section:

```markdown
You almost never start the daemon by hand: `camp init` puts **campd under your
host's service manager** (launchd on macOS, systemd `--user` on Linux), which
starts it, keeps it alive, and restarts it after a crash. `camp service status`
shows its state and `camp service restart` cycles it after a binary upgrade.
Where there is no service manager — a container, CI, a bare box — you run
`camp daemon` yourself under your own supervisor. The CLI is a pure socket
client in every one of those environments: it never starts campd behind your
back, and a campd that is down is a loud error naming the remedy.
```

`README.md` (`~:250`):

```markdown
Then `camp sling "…"` (or `/camp:sling "…"`) creates the bead and campd
dispatches the worker. Route to a specific role with `--agent reviewer`.
Watch the fleet with `camp top` or `/camp:status`.
```

`README.md` (`~:310-311`), in the command block:

```sh
camp top                                     # one status snapshot (campd must be running)
camp top --statusline                        # compact fleet badge (▲live ●ready ✖red); empty + a stderr note when campd is down
```

`README.md` (`~:437-439`):

```markdown
The statusline is opt-in: a plugin cannot set your main status line for you, so
wire it into your own `~/.claude/settings.json`. It renders `▲live ●ready ✖red`
from a read-only socket query and degrades to empty output plus a stderr note
when `campd` is down.
```

`plugin/commands/status.md` (`:5`):

```markdown
One status query rendered as plain text (campd must be running — `camp service status` if it is not):
```

`plugin/README.md` (`~:60-61`):

```markdown
`statusline/statusline.sh` renders `▲live ●ready ✖red` from a read-only
socket query — it degrades to empty output plus a stderr note when campd is
down. It is the **main session** status line. A plugin cannot auto-set the
```

`plugin/statusline/statusline.sh` (`:3`) — **the comment only, not the code**:

```sh
# read-only campd socket query. It degrades to empty output plus a stderr
```

- [ ] **Step 7: The four gates. Each has a STATED expected output — a gate that expects "nothing" where something must legitimately survive is not a gate.**

```sh
# Gate A — user-facing prose. No claim may survive.
grep -rni "auto-start\|autostart\|auto started\|auto start" README.md plugin/ Makefile
```
**Expected: NO OUTPUT.** Any hit is a lie the user can read.

```sh
# Gate B — the camp binary crate.
grep -rni "auto-start\|autostart\|auto started\|auto start" crates/camp/src
```
**Expected: EXACTLY ONE LINE** — `poke_best_effort`'s doc, which is frozen by spec §7.2 and whose sentence is still true:
```
crates/camp/src/daemon/socket.rs:<n>:/// a poke never auto-starts the daemon.
```
Anything else is a stale claim: fix it. **Do not edit that line to make the gate quieter** — it is the one named, justified survivor, and silencing it would be exactly the kind of dishonesty this gate exists to prevent.

```sh
# Gate C — the camp test crate. The only permitted survivors are the guards that
# name the historical event in order to assert nothing ever emits it again.
grep -rni "auto-start\|autostart\|auto started\|auto start" crates/camp/tests | grep -vi "autostarted"
```
**Expected: NO OUTPUT.** (The filtered lines are `assert_no_campd_came_up`'s `campd.autostarted` guard and `cli_sling.rs`'s two — every one of them carries the literal event name on the same line, which is why this filter is exact and not a convenience.)

```sh
# Gate D — camp-core. Only the historical event identifier may survive.
grep -rni "auto-start\|autostart\|auto started\|auto start" crates/camp-core | grep -vi "autostarted"
```
**Expected: NO OUTPUT.** (Filtered: `CampdAutostarted`, `campd.autostarted`, `campd_autostarted`, `campd_autostarted_is_validated_and_log_only` — the type, the fold arm, the vocab entry, and the unit test that proves old ledgers still fold. **All four must survive; do not delete them while chasing this gate.**)

Two files are **deliberately out of every gate's path** — they are Phase 4's, and Appendix A says so: `docs/design/2026-07-05-gas-camp-design.md` (except the two passages Task 5 corrects) and `contrib/launchd/com.gascamp.campd.plist.example`. `docs/superpowers/{plans,specs}/` are historical records and are never rewritten.

- [ ] **Step 8: Full gates and commit**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
git add README.md plugin/ crates/camp/src/main.rs crates/camp/src/cmd crates/camp/tests
git commit -m "docs: campd is a supervised service — remove every auto-start claim from help text, docs and comments"
```

---

## Task 5: The minimal v1-spec correction (operator-decided — the PR cannot ship without it)

AGENTS.md: *"If implementation reality contradicts the spec, stop and update the spec via PR in the same change: spec and code never silently diverge."* Tasks 1-3 falsify exactly two passages of the authoritative v1 spec. The operator has ruled (PR #68, comment 4946982848) that **Phase 3 corrects exactly those two and nothing else** — Phase 4 keeps the full §5 rewrite, §9's orders note, §12's multi-rig recommendation, and the `contrib/launchd/` supersession (Appendix A).

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md` — **line 126 and the `**Auto-start:**` bullet at lines 170-177. Nothing else in this file. Nothing at all under `contrib/`.**

**Interfaces:** none. Documentation, in the same PR as the code that requires it.

> **Scope note the implementer must not get wrong.** The reviewer and the operator both refer to this as "line 170", because line 170 is where the bullet begins. **The bullet actually runs from 170 to 177**, and its trailing sentence — *"An optional launchd agent … starts `campd` at login for users who want orders firing **without first running a `camp` command**"* — is itself an auto-start contrast: it only makes sense against a CLI that starts the daemon on first use. It is inside the bullet being replaced and goes with it. `contrib/launchd/` itself is **not touched** (Phase 4 folds the example plist away); only this spec sentence about it, which this phase falsifies.

- [ ] **Step 1: Verify the anchors before editing (they will have shifted after the Phase 2 rebase)**

```sh
grep -n -i "auto-start\|autostart" docs/design/2026-07-05-gas-camp-design.md
```
Expected: exactly three hits — the component table row (`:126`), the `**Auto-start:**` bullet head (`:170`), and `camp show --wait`'s "never autostarts campd" (`:696`). **`:696` stays: it is still TRUE** (`--wait` is a pure observer and this phase does not touch it). If the grep shows anything else, stop and report.

- [ ] **Step 2: Correct the component table (line 126)**

Old:
```markdown
| `campd` | the same binary in daemon mode, auto-started on demand | the only standing process: watches the ledger, dispatches ready work, schedules orders, arms stall timers — purely mechanical |
```
New:
```markdown
| `campd` | the same binary in daemon mode, run by a supervisor (launchd / systemd `--user` / the container runtime / you) | the only standing process: watches the ledger, dispatches ready work, schedules orders, arms stall timers — purely mechanical |
```

- [ ] **Step 3: Replace the `**Auto-start:**` bullet (lines 170-177) with the pure-client bullet**

Old (the whole bullet — all eight lines):
```markdown
- **Auto-start:** any `camp` verb that needs the daemon sends its request
  — the request is the liveness probe, on the same connection. Only a
  refused/absent socket triggers the spawn: `campd` starts detached, the
  spawn is logged as an event, and the request retries exactly once. An
  unanswered request is a loud error, never a second daemon. `camp stop`
  shuts it down. An optional launchd agent (shipped as an example plist,
  not installed by default) starts `campd` at login for users who want
  orders firing without first running a `camp` command.
```
New:
```markdown
- **Pure client:** any `camp` verb that needs the daemon sends its request
  — the request is the liveness probe, on the same connection. The CLI
  never starts `campd`: a refused or absent socket is a loud, actionable
  error naming the pid from the ledger's last `campd.started` and the
  remedies (`camp service status`, or `camp daemon` where no service
  manager exists), never a silent respawn. An unanswered request is the
  wedge error, never a second daemon. `campd` is a supervised foreground
  process: `camp init` installs a host unit where a service manager exists,
  and `camp service {status,restart,stop,start}` controls it; under a
  container runtime, in CI, or on a bare box you run `camp daemon`
  yourself. `camp stop` shuts down an unsupervised daemon.
```

- [ ] **Step 4: Verify the spec now matches the code**

```sh
grep -n -i "auto-start\|autostart" docs/design/2026-07-05-gas-camp-design.md
```
Expected: **exactly one hit** — `:696`'s `camp show --wait` "never autostarts campd", which remains true.

```sh
git diff --stat docs/design/2026-07-05-gas-camp-design.md
```
Expected: **one file, ~1 insertion + 1 deletion for the table row, and the 8-line bullet replaced.** If the diff touches §9, §12, or any other section, you have overreached — revert and redo.

- [ ] **Step 5: Commit**

```sh
git add docs/design/2026-07-05-gas-camp-design.md
git commit -m "docs: v1 spec §5 — the CLI is a pure client (campd is supervised, never CLI-spawned)"
```

- [ ] **Step 6: Local-only gates before the PR**

```sh
make perf      # spec §14 numbers; perf_daemon.rs spawns campd explicitly — unaffected, but prove it
```

`make e2e` (real `claude -p`) is opt-in; run it if you have credentials. It spawns campd explicitly (`e2e.rs:873`) and calls no daemon-needing verb with campd down.

---

## Appendix A — the Phase 4 boundary (and what Phase 3 corrects instead)

**The v1-spec correction is DECIDED, not open.** The operator ruled on it (PR #68, comment 4946982848): Phase 3 corrects exactly the two passages its own code falsifies — **`docs/design/2026-07-05-gas-camp-design.md` line 126 and the `**Auto-start:**` bullet at lines 170-177** — and that is **Task 5** of this plan. Do not re-escalate it. Do not skip it: AGENTS.md forbids merging code that contradicts the spec, and without Task 5 this PR would leave `main` documenting a feature it had just deleted.

**Everything below stays Phase 4's. Do not touch any of it in this phase:**

| Left alone | Why |
|---|---|
| The **full §5 rewrite** of `docs/design/2026-07-05-gas-camp-design.md` (the supervised-daemon model, the environment table, the `camp service` narrative) | Phase 4. Task 5 corrects only the two passages Phase 3's code falsifies. |
| **§9's orders note** (always-on supervision removes the "no wake source, no fire" away-mode gap) | Phase 4 — it is a consequence of Phase 2's always-on supervision, not of auto-start removal. |
| **§12's multi-rig recommendation** (standalone camp + many rigs, to bound daemon count) | Phase 4 — same reason. |
| **`contrib/launchd/com.gascamp.campd.plist.example`** (including its line 8, *"Without this agent, campd auto-starts on first `camp` use"*) | Phase 4 supersedes the bare example with `camp service install`. Phase 3 corrects the *spec sentence* about it (it sits inside the bullet Task 5 replaces) but leaves the file itself alone. |
| **`docs/design/…:696`** — `camp show --wait` "never autostarts campd" | Still **TRUE**. `--wait` is a pure observer and this phase does not touch it. Phase 4's rewrite will absorb the wording. |
| **`README.md` ~306-308** — *"`campd` is the only standing process, and only while there's work"* | Falsified by **always-on supervision** (design §4.2, Phase 2), independently of auto-start removal. Flagged and relayed to Phase 2 by the lead. Not Phase 3's to fix — leave it. |

---

## Self-review

**Spec coverage.** Design §4.3 ("CLI is a pure client; the on-demand CLI auto-start is removed. One path.") → Tasks 1-3. §8's migration paragraph (`top`, `adopt`, `sling`, `daemon/autostart.rs`, its tests) → Tasks 1-3, blast radius independently re-derived from the code, not copied, and confirmed exact by the round-1 reviewer. §3's "loud, actionable fault naming the pid from the ledger's `campd.started`, pointing at `camp service status`, never a silent respawn" → the `CampdNotRunning` Display + `camp_top_after_a_kill_dash_nine_names_the_dead_campd_pid`. §9's "CLI-as-pure-client … assert no new process, actionable error text" → `assert_no_campd_came_up`'s three structural tripwires (campd.log, socket, `campd.started`), applied to `top`, `adopt`, and `sling`. §7.2's `poke_best_effort` → frozen in body AND doc, and named as Gate B's one expected survivor rather than silently filtered. AGENTS.md's spec-code coherence → Task 5. The §8 items this phase does NOT own → Appendix A's boundary table.

**Sweep completeness.** The stale-prose inventory table assigns **every** hit of `grep -rni "auto-start\|autostart\|auto started\|auto start" crates/ README.md plugin/ Makefile contrib/` at b0dc950 to a task, to "frozen", or to Phase 4. The four gates in Task 4 Step 7 partition the same ground with stated expected output — Gate A expects nothing, Gate B expects exactly the one frozen §7.2 line, Gates C and D expect nothing once the historical event identifier (which the design *requires* to survive) is filtered. Every filter names a line the plan justifies elsewhere; none of them exists to make a red gate green.

**The gates were run against this plan's OWN prescribed text.** Twice now the finding has been that the plan's own new prose would trip its own gates — round 1 caught the assertion *messages*, round 2 caught two *doc comments* (the `socket.rs` wedge test's "the dying `autostart.rs` test", and `assert_no_campd_came_up`'s "the removed auto-start path"). So the claim above is no longer an argument, it is a measurement: **every comment, doc comment, assertion message and help string this plan tells an implementer to write** was extracted and run through Gates A-D. Result: Gate A returns nothing, Gate B returns exactly the one frozen `poke_best_effort` line, Gates C and D return nothing. The only prescribed lines carrying a gate token now also carry the literal `campd.autostarted` — which the guards *must* name in order to guard it, and which is precisely what Gates C and D filter. The Global Constraints' no-new-prose rule exists so this class cannot recur a third time.

**Type consistency.** `socket::require(camp: &CampDir, request: &Request) -> Result<Response>` and `socket::CampdNotRunning { camp_root, socket, log, last_pid }` are defined in Task 1 and used with those exact names and shapes in Tasks 2, 3 and 4. `assert_no_campd_came_up(root, out, starts_before)` is defined in Task 1 (`daemon_lifecycle.rs`) and called in Task 2 from the same file. `Daemon::spawn(root)` in `cli_sling.rs` (Task 3) is a file-local harness, not a shared symbol — the `camp` crate has no lib target and every integration test file carries its own.

**Green at every commit.** Task 1 introduces `require` *and* its first caller. Task 3 removes the last caller of `request_with_autostart` *and* deletes the module, in one commit. `CampDir::log_path` never goes dead — Task 1 gives it a real new caller (`CampdNotRunning.log`) before Task 3 removes its old one.

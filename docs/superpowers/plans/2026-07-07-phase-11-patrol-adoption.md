# Gas Camp Phase 11 — Health Patrol and Adoption Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Status: APPROVED (2026-07-07, fresh review pass on commit 2a6c449, relayed by the lead).** Verification notes from the approving review: the `actor == "campd"` reset exclusion was swept against every `actor:` literal in the codebase and confirmed airtight (worker events carry `"cli"` or the session name — no legitimate reset is lost); the probe-before-kill fix's load-bearing detail (probe by uuid with pid None, kill only the re-probed pid) is pinned by both test branches; the A4-3 supersession is honest (original preserved, dated, pointed); minor 4 root-cause-fixed. One approval-pass fold-in, recorded in Task 11.1 Step 1b: the stale exit-on-EOF fact appears a SECOND time in the findings doc (A4 "Observed" prose, bullet 3) and gets a supersession pointer too. Sibling facts at execution start: phase-9 executing (its plan pins that it constructs no Workers and adds no tokens — the Token(3) reservation is safe); phase-14 PR #18 in code review; at least one rebase expected.
>
> Round-1 history: rulings recorded verbatim per the Phase 10 precedent — **Decision C ACCEPTED, Decision D ACCEPTED, Decision I ACCEPTED**; probe evidence P1/P3 judged coherent (P1 arithmetic checked; P3 internally consistent); the Task 11.16 spec sentence was **ADOPTED by the operator** (relayed 2026-07-07) — it rides this PR as planned, the only spec edit in flight, no sequencing constraints. Round-1 blockers, addressed in this revision: **(1)** Decision J now excludes patrol-authored (`actor == "campd"`) events from timer resets and makes the special-cased lifecycle kinds exclusive of the reset path — previously the just-declared `agent.stalled` self-matched J on the very next settle and rewound the ladder to Nudge forever, making Restart/Exhausted unreachable (Decision J, Task 11.10 semantics + new escalation test). **(2)** An `AdoptedPid` restart now re-probes the session uuid immediately BEFORE killing and kills the re-probed pid only — a worker dead for hours must not translate into a SIGKILL of whatever process now owns the stale pid (Task 11.11 + new probe-order test). **(3)** Task 11.1 also edits `docs/design/2026-07-06-assumption-findings.md` itself: A4-3's exit-on-EOF fact gains a supersession pointer to the P3 finding, so later phase planners cannot inherit the falsified fact. **(minor 4)** adopt skips ALL already-tracked sessions (not only campd's own children), making the second-run AdoptSummary exactly zero (Task 11.12).

**Goal:** Spec §10 and §8.5 — stall detection (one armed timer per active worker, reset by transcript-file activity and ledger events), the mechanical nudge→restart ladder with exponential backoff and a bounded budget, annotate-only handling for attended sessions, and `camp adopt` (auto at campd start, manual verb) reconciling the session registry against observed reality, including the worktree sweep.

**Architecture:** camp-core gains `patrol/{mod,timers}.rs` — pure, deterministic timer and ladder state machines that take explicit `jiff::Timestamp` inputs (the CronHeap precedent). The camp binary gains `daemon/patrol.rs` — the notify transcript watches (one watcher, per-directory, filtered by registered transcript paths, signalling the mio loop through a self-pipe exactly like the Phase 10 config watch), stall-fire declaration (durable `agent.stalled` first, action second — the `declare_cron_fires` mold), the ladder actions (nudge over held stdin / nudge via `--resume` / restart / release), and adoption. Patrol timers plug into the SAME heap-sourced poll timeout as orders (`min` of the two `poll_timeout`s); patrol event observation plugs into the SAME per-event processing path as readiness and orders (`CampdProcessor`), so timer resets from ledger events cost nothing extra and are exactly-once. campd-spawned workers move to `--input-format stream-json` with campd holding the stdin pipe — the A4-resolution live nudge path that Phase 8's spawn.rs explicitly reserved for this phase — with a mechanical release rule (bead closes → close stdin → grace timer → terminate → `session.stopped` with the reason) forced by probe P3's new finding that claude 2.1.204 does NOT exit on idle stdin EOF.

**Tech Stack:** Rust (edition 2024), jiff 0.2 (`Timestamp`, `SignedDuration` — already in camp-core), mio + notify 8 (already in camp bin), rusqlite (JSON1 for the adopt query), serde/serde_json, clap, anyhow (bin) / thiserror (core). No new dependencies. No async runtime. No `unsafe` (process probes and non-child kills go through `ps`/`pgrep`/`kill` child processes, sanctioned by the master plan's "safe `kill(pid, 0)` wrapper or `/proc`/`ps`").

## Global Constraints

Copied from AGENTS.md, the master plan, and the kickoff. Every task's requirements implicitly include this section.

- **Spec is authoritative:** `docs/design/2026-07-05-gas-camp-design.md`; §4 decisions settled. If implementation reality contradicts the spec, stop and update the spec via PR in the same change (spec edits are serialized through the lead — this plan carries exactly one sentence, Task 11.16, ADOPTED by the operator 2026-07-07).
- **Master plan contract:** `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md`, section "Phase 11 — Health Patrol and Adoption (`phase-11-patrol-adoption`)". Extra authoritative input: `docs/design/2026-07-06-assumption-findings.md` (A4 stronger; F3, F5, F6; the A4-4 two-writers caution) and spec §10 as amended (nudge live path = stdin turn for stream-json workers; resume otherwise).
- **Invariant 1 — the soul of this phase:** zero patrol code paths poll. Stall detection is armed timers (heap-sourced poll timeout) + filesystem watches (notify → self-pipe) + ledger-event observation (the post-commit processing path). No tick, no periodic scan, anywhere. Test harnesses may poll (Phase 7/8 precedent).
- **Every patrol action is an event with its cause (exit criterion):** `agent.stalled` carries session, bead, agent, action, effective threshold, restart count; a patrol kill's `session.crashed` carries `reason` and `cause_seq` (the stalled event's seq); a release's `session.stopped` carries the reason; adoption's `session.crashed` carries the probe reason; the sweep emits `bead.worktree.reaped`/`worktree.kept` per decision H.
- **Observation over state, always (spec §8.5):** adopt trusts the process table (`pgrep -f <claude-session-uuid>` — the uuid is in every worker's argv per F1, immune to pid reuse) and transcript files, never registry rows alone.
- **Respect merged interfaces — extend, don't rework:** Phase 8's registry-at-birth, capture paths (decision G), worker contract, SIGCHLD reap, and Phase 10's heap/settle/token layout are consumed. New event payloads use `#[serde(deny_unknown_fields)]` structs; keep the one-transaction event+state property, the vocab-pin partition tests, and the refold property test green.
- **Vocabulary mirror (spec §15.2):** `agent.stalled` and `patrol.degraded` are camp-specific (verified absent from `tests/fixtures/gc-vocab.json` at plan time — gc has `session.reset_stalled` but no `agent.*` or `patrol.*` names; the vocab_pin test re-enforces).
- **Zero roles in code:** the ladder is mechanical (nudge text is lifecycle machinery like `WORKER_CONTRACT`, not role content); escalation to judgment is an order matching `event:agent.stalled` — pack content, not Rust (spec §10.3).
- **Fail fast:** no silent fallbacks; no panics in library code (`clippy::unwrap_used`/`expect_used`/`panic` denied, `unsafe_code` forbidden). A degraded transcript watch is a durable `patrol.degraded` event, never just stderr (the Phase 10 LOW-8 mold).
- **TDD, strictly:** failing test → watch it fail → implement → watch it pass. Run every new or changed test.
- **Git:** never commit to main; branch `phase-11-patrol-adoption`; no co-author lines, no self-mention; conventional-commit style.
- **Gates before push:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- **Shared-file protocol (siblings phase-9-graph-execution and phase-14-export-bridge in flight):** phase-9 owns `daemon/dispatch.rs` *extensions* and `camp-core/src/formula/runtime.rs` — we are the guide's highest-conflict pair. My dispatch.rs edits (Task 11.9) are additive worker-lifecycle mechanics and are flagged to the lead via this plan. `event_loop.rs` edits stay additive and minimal (Task 11.13 lists every hunk). Shared files (`main.rs`, `event.rs`, `vocab.rs`, `config.rs`, `fold.rs`, `ledger/mod.rs`, both `Cargo.toml`s) stay minimal and additive. When the lead reports a sibling merge: rebase onto current main, resolve, re-run all gates before continuing; never open/update a PR from a branch not rebased on current main. Expect at least one real rebase against phase-9's merge (Task 11.13 is the designated rebase gate).
- **Nothing is complete until pushed, CI green (`gh pr checks --watch`), and every claim in the PR description verified.**

## Key Paths and Conventions

- Worktree: `/Users/kiener/code/gascamp/.claude/worktrees/agent-ae777f08a711a8384`, branch `phase-11-patrol-adoption` (created).
- camp-core new: `src/patrol/{mod,timers}.rs`. camp-core modified: `src/lib.rs` (one `pub mod patrol;` line), `src/config.rs` (+`[patrol]`), `src/event.rs` (+2 types), `src/vocab.rs` (+2 names), `src/ledger/fold.rs` (+2 log-only arms, +1 additive payload field), `src/ledger/mod.rs` (+`SessionRow`, +`live_sessions`), `src/pack.rs` (+`stall_after`).
- camp (bin) new: `src/daemon/patrol.rs`, `src/cmd/adopt.rs`, `tests/daemon_patrol.rs`. camp modified: `src/daemon/{mod,event_loop,orders,dispatch,spawn,socket}.rs`, `src/main.rs`, `tests/fake-agent.sh` (additive env knobs only).
- Docs new: `docs/design/2026-07-07-phase-11-probe-findings.md` (probes P1–P3, verbatim evidence). Docs modified: `docs/design/2026-07-06-assumption-findings.md` (A4-3 supersession pointer, Task 11.1); spec §10 (ONE sentence, Task 11.16 — **ADOPTED by the operator**, rides this PR).
- Actor conventions: patrol- and adopt-appended events use `actor = "campd"` (they are campd actions). `agent.stalled` puts the session in `data.session` (payload), the bead in the event's `bead` column, the rig in `rig`.
- Durations in config and event data use jiff's friendly format strings (`"10m"`, `"90s"`, `"300ms"`), parsed by `patrol::parse_duration`, which requires a strictly positive result.
- Timestamps in event data use the canonical spec §7.2 form (RFC3339 UTC whole seconds) — same as `clock.rs`.
- The pure state machines take explicit `now: jiff::Timestamp` parameters (the CronHeap precedent) — that is what makes the master plan's "with `FixedClock`" test obligation deterministic; `FixedClock` itself drives the `ts` strings of appended events in ledger-touching tests exactly as Phase 10's tests do.
- Integration tests drive the real binary via `env!("CARGO_BIN_EXE_camp")` with the `daemon_dispatch.rs` harness conventions (`scaffold`, `Daemon`, `wait_until`, `events_json` — test-harness polling is sanctioned for harnesses only).

## Plan-Time Probe Findings (2026-07-07, claude 2.1.204 — binding on this plan)

Executed at plan time in a throwaway scratch dir (never inside gascamp), per the Phase 2 method. Full transcripts land in `docs/design/2026-07-07-phase-11-probe-findings.md` (Task 11.1). The claude version moved 2.1.201 → 2.1.204 since the Phase 2 findings.

| # | Fact | Value | Consequence |
|---|---|---|---|
| P1 | **Unicode transcript munge is per-CHARACTER** — the carried PR #14 review note (spawn.rs "fidelity unverified for non-ASCII cwds") is RESOLVED: real claude maps cwd `…/p11/héllo-日本` to project dir `…-p11-h-llo---` — `é` (2 UTF-8 bytes) → ONE dash, `日` and `本` (3 bytes each) → ONE dash each. camp's `spawn::munge` (chars → dash) matches reality exactly. | Verified against a live session (sid `c7f93f71-…`), transcript found at the munge-predicted path. | Task 11.1 removes the "unverified" caveats from spawn.rs comments and the F3 pin note; NO behavior change to `munge`. The transcript watches may rely on `transcript_path_under` as-is. |
| P2 | **Stream-json flags compose**: `claude -p --input-format stream-json --output-format stream-json --session-id <uuid> --append-system-prompt <text>` is accepted (no `--verbose` needed at 2.1.204); the user-message wire shape `{"type":"user","message":{"role":"user","content":"<text>"}}` + `\n` is accepted with plain-string content; both turns answered (`S1-OK`, `S2-OK`), both result events echo the pre-assigned session id, `is_error:false`. | Probe sid `85cc86d9-…`; two `type:"result"` lines observed. | The stream spawn argv (Task 11.8) and the nudge wire write (Task 11.9) are pinned by tests to these shapes. |
| P3 | **A stream-json worker does NOT exit on idle stdin EOF** (supersedes A4-3's "exit 0 on stdin EOF" pinned at 2.1.201): with the result complete and the process idle, closing the last stdin writer left the process alive ≥45 s (probe 3; probe 2 suggests >3 min) until SIGKILLed. The process DOES stay alive between turns with stdin held (`ALIVE_BETWEEN=yes`, `ALIVE_AFTER_PAUSE=yes`) and answers a later turn — the held-pipe nudge path is real. | Probe sid `bdaf61b7-…`: `result_after=8s`, `ALIVE_PRE_EOF=yes`, `STILL_ALIVE_AFTER=45s`, killed 137. | Worker lifetime must be campd-managed: the RELEASE rule (Decision C2). Also GOOD news for adoption: workers survive a campd `kill -9` (stdin EOF does not kill them) — spec §8.5's "workers outlive campd" holds in stream mode. |

## Plan-Time Decision Log

**Decision A — pure state machines take explicit `now: Timestamp`; no new Clock surface.** `StallTimers` and `Ladder` (camp-core) are pure functions of their inputs; `fire_due(now)`, `poll_timeout(now)` mirror `CronHeap`. The master plan's "state machine with FixedClock" obligation is satisfied: tests are deterministic by construction, and `FixedClock` supplies event `ts` strings wherever a ledger is involved (Phase 10 precedent). *Fallback if overruled:* thread a `&dyn Clock` through — mechanical change, no structural impact.

**Decision B — timer store is a small map with min-scan, not a literal BinaryHeap.** Active workers are bounded by `[dispatch] max_workers` (default 10, spec §18 "ten agents"); a `HashMap<session, TimerEntry>` with O(n) `next_deadline()` is honest, simpler, and allows O(1) reset/disarm by session key (a lazy-invalidation heap would carry generation counters for no gain at n≤10s). "Heap-integrated" in the master plan names the *mechanism* — deadline-sourced poll timeout — which this satisfies identically to orders. *Fallback:* swap the store for a BinaryHeap + generation counters; the API is store-agnostic.

**Decision C — campd-spawned workers move to stream-json input mode with campd-held stdin. [ACCEPTED, plan-review round 1; the phase-9 coordination note stands]**
Basis: spec §10 as amended ("delivered live over stdin when campd spawned the worker in stream-json input mode"), F5's carve-out ("except stream-json workers, where campd deliberately holds the stdin pipe"), spawn.rs's own Phase 8 comment ("stream-json stdin-held workers are the Phase 11 nudge path"), and the A4-4 caution ("prefer the stream path for campd-owned workers" — concurrent resume makes two writers share one transcript). Probe P2 pins the mechanics. Consequences, all handled in this plan:
- **C1 — capture format:** `sessions/<name>.json` keeps its decision-G path but its content becomes stream-JSONL (one event per line) instead of a JSON array. Nothing in merged code parses this file (verified: audit-only per Phase 8; re-verified by grep at plan time). **phase-9 must be told** in case their check/retry work planned to parse the F2 array envelope — the F2 parse rule for stream mode is "the line with `type=="result"`", strictly easier.
- **C2 — the release rule (forced by P3):** a stream worker cannot exit on its own. When campd observes `bead.closed` for a tracked worker's bead, it releases the worker: drop the held stdin (EOF), mark the worker `released`, arm a release-grace timer (`[patrol] release_grace`, default `"30s"`) in the SAME timer store; if the process is still alive at fire, SIGKILL it. A released worker's reap appends `session.stopped` with `reason:"released after bead close"` (plus the observed exit code/signal) — never `session.crashed`: campd initiated the termination of a worker whose work was done, and the event says so (F4's principle: failure routing comes from close events, not exit codes).
- **C3 — fake agents are unaffected:** fake-agent.sh takes its inputs from `CAMP_*` env (decision J), ignores stdin unless a new test knob says otherwise, and exits on its own (bash does not linger on open stdin); the release path then simply finds the child already reaped or exits it cleanly.
- **C4 — campd kill -9 resilience improves:** per P3, orphaned stream workers do NOT die on EOF; they finish their current turn (closing beads via the CLI — direct SQLite, no campd needed) and linger idle. Adoption (Decision F) releases lingering live workers whose bead is closed and re-arms the genuinely working ones.
*Pre-decided fallback (NOT NEEDED — Decision C accepted at round 1; kept for the record):* keep `-p` json spawn exactly as Phase 8 built it; the nudge action becomes concurrent `--resume` for ALL workers (A4-4 verified it works; two-writers caution documented). The blast radius of that swap is Task 11.8/11.9's stream code plus the nudge dispatch in Task 11.11 — the ladder, timers, watches, adopt, and all events are identical either way.

**Decision D — exponential backoff = threshold scaling, not a dispatch delay-gate. [ACCEPTED, plan-review round 1]** After patrol's restart n of a bead, the respawned worker's effective stall threshold is `stall_after × 2^n`. Successive restarts therefore space out exponentially (the classic supervisor property) while: (a) the released bead re-dispatches immediately and VISIBLY (`session.woke` in the ledger — nothing hidden), (b) no "ready but held back" in-memory dispatch state exists (a delay-gate would contradict §7.3 readiness-on-write and invariant 3: an invisible hold), (c) zero dispatcher-converge surface changes (the phase-9 conflict area stays untouched on this axis). The backoff series test (Task 11.4) pins thresholds `[T, 2T, 4T, …]`. Ladder restart counts are per-bead, in-memory, reset by campd restart (crash-only; the `Dispatcher::failed` precedent) and forgotten when the bead closes. *Fallback if overruled:* a `held_until` set consulted by `converge` plus release timers — documented, rejected for the reasons above.

**Decision E — ladder shape and reset semantics.** The ladder is per BEAD (the bead is the work; sessions are disposable): fire sequence per generation is nudge → (still silent) → restart; the restart budget (`[patrol] restart_budget`, default 2) counts restarts per bead per campd lifetime; when a restart would exceed the budget the ladder emits `agent.stalled` with `action:"exhausted"` and STOPS — timers disarm, the worker is left running (observation: it may yet finish), and escalation is pack content (an order matching `event:agent.stalled`). Any observed activity (transcript touch or ledger event) resets the timer AND returns the ladder's next step to nudge — but keeps the restart count (a worker oscillating between stall and revival must still exhaust). A nudge whose DELIVERY fails (broken pipe, resume spawn failure) appends `agent.stalled` with `action:"nudge_failed"` + the error, advances the ladder's next step to restart, and re-arms — evented, no immediate cascade. Known and accepted (plan-review round 1 note): a worker that ANSWERS every nudge but never closes its bead oscillates nudge → activity → nudge indefinitely — mechanical patrol cannot judge progress (invariant 4: campd never reasons about work); the accumulating `agent.stalled` trail in the ledger IS the escalation surface, and a pack order matching `event:agent.stalled` (or the operator reading `camp events`) is the judgment layer.

**Decision F — adoption probes by session uuid, not stored pid.** campd workers have no pid in the registry (registry-at-birth precedes exec, F1) and stored pids can be reused after reboot. The probe is `pgrep -f <claude_session_id>` — the pre-assigned uuid is in every worker's argv (F1; true for fake-agent too, which receives claude-style argv), globally unique, and machine-reboot-safe. Rows with a recorded pid (future attended registrations) are additionally checked with `ps -p <pid>`. Both are child-process invocations — no `unsafe`, no new deps (master plan sanctions "or `/proc`/`ps`"). Adopt semantics per live registry row: process dead → `session.crashed {reason:"adopt: process not found"}` (fold releases beads, budgets intact); process alive + bead closed → RELEASE it (Decision C2 — prevents an infinite nudge loop on a finished-but-lingering worker); process alive + bead open → re-arm (fresh threshold — restart grace) with `Owned::AdoptedPid(pid)` so a later ladder restart can kill it (via `kill` child process + probe-verified death + a campd-appended `session.crashed` with cause, since no SIGCHLD comes for a non-child).

**Decision G — worktree sweep rules (mechanical, decision-H-consistent).** For each directory under `<camp>/worktrees/` (name = bead id), after the crash-marking pass: bead unknown to the ledger → report in the adopt summary and stderr, do NOT delete (never destroy what camp cannot attribute; fail loud). Bead closed pass → complete the interrupted disposition: remove + `bead.worktree.reaped`. Bead closed non-pass with no prior disposition event → `worktree.kept {reason:"adopt: found after interrupted disposition"}`. Bead open with a live session using it, or awaiting re-dispatch → leave in place (re-dispatch reuses it, Decision H). Already-disposed (a prior `worktree.kept`/`bead.worktree.reaped` exists for the bead) → leave, no event (idempotent).

**Decision H — respawn reuses the bead's existing worktree.** Phase 8 "never respawns a bead," so `create_worktree` fails fast on residue. Patrol restarts DO respawn: `ensure_worktree` (Task 11.8) reuses an existing directory iff it is a git worktree whose checked-out branch is `camp/<bead>` (partial work preserved on the branch); anything else keeps Phase 8's residue error verbatim.

**Decision I — patrol's mio token is `Token(3)`, reserved; connections start at 4. [ACCEPTED, plan-review round 1]** The settled layout (0 listener / 1 config-watch / 2 SIGCHLD / 3+ connections) grows one reserved slot: 0/1/2/3 = listener/config/SIGCHLD/patrol-watch, connections 4+. Stated here for the reviewer's collision check against phase-9: phase-9's surface (dispatch extensions, formula runtime) introduces no poll sources, so no collision. The alternative (a dynamic token from the connection range) would make the loop's match arm order-dependent; a reserved constant mirrors CONFIG_WATCH/SIGCHLD and keeps the layout auditable.

**Decision J — timer resets from ledger events match on three keys, WORKER-AUTHORED events only (amended per plan-review round 1, blocker 1).** A tracked worker's timer resets when a committed event has (a) `bead` == the worker's bead (covers `worker.milestone --bead`, `bead.updated`), or (b) `actor` == the worker's session name (covers `camp event emit --session`), or (c) `data.session` == the worker's session name (covers `bead.claimed`). Two exclusions make the rule sound: **(i) events with `actor == "campd"` NEVER reset** — campd's own appends (`agent.stalled`, `patrol.degraded`, `session.crashed`, `dispatch.failed`, `session.woke`, …) are patrol/dispatch bookkeeping, not worker activity. Without this exclusion the just-declared `agent.stalled` (which carries the worker's bead in the `bead` column and the session in `data.session` per this plan's actor conventions) would self-match keys (a)/(c) during the settle that follows every declaration, call `ladder.on_activity`, and rewind the next step to Nudge — a truly silent worker would be nudged forever and Restart/Exhausted would be unreachable. **(ii) The lifecycle kinds `observe()` special-cases (`session.woke`, `session.stopped`, `session.crashed`, `bead.closed`) are EXCLUSIVE of the reset path** — `observe` handles them and returns before reset matching, so e.g. `bead.closed` forgets the ladder without an `on_activity` resurrecting state order-dependently. The rule remains exact for the worker-contract CLI surface (verified against cmd/claim.rs, cmd/close.rs, cmd/event_emit.rs at plan time: worker-authored events carry actor `"cli"` or the session name, never `"campd"`).

**Decision K — `camp adopt` is a socket op executed by campd.** Timers and watches live in campd's memory, so the manual verb must run inside it: `Request::Adopt` → campd runs the same `adopt()` it runs at startup → `Response::Adopt {ok, crashed, rearmed, released, swept, kept}`. The CLI (`cmd/adopt.rs`) uses `request_with_autostart` (an auto-started campd adopts at startup, then answers the explicit request — the second pass is a no-op by construction, which the idempotency test pins). Attended sessions in the registry (rows whose `session.woke` actor is not `"campd"` — Phase 12's hooks will create these) are armed annotate-only: `agent.stalled` with `action:"annotate"`, re-arm, never nudge/kill (spec §10: never kill a session in the user's TUI; the statusline badge itself is Phase 12's deliverable reading these events). Expected and accepted (plan-review round 1 note): an idle-but-registered attended session annotates once per threshold — ~144 `agent.stalled`/day at the 10m default — bounded, honest ledger volume well inside spec §7.6's scale envelope; raising the agent's `stall_after` is the tuning knob if a pack finds it noisy.

**Decision L — patrol config is read at campd start; hot reload does not re-arm patrol.** `[patrol]` keys (`stall_after`, `restart_budget`, `release_grace`) apply from startup config; per-agent `stall_after` is resolved fresh at every arm (dispatch resolves the agent per spawn). Spec §13.4's hot-reload promise covers orders; extending it to patrol is deferred and documented in the module header. *Rationale:* re-arming live timers mid-flight on a config edit adds states the phase's test obligations don't cover; a campd restart (cheap, crash-only) applies new patrol config.

## What later phases rely on (interfaces Phase 11 produces)

- `camp_core::patrol::{PatrolConfig, parse_duration}` — Phase 12's hooks and Phase 13's perf suite read `[patrol]` through `CampConfig::patrol`.
- `agent.stalled` (camp-specific event): packs write orders matching `event:agent.stalled` (spec §10.3); Phase 12's statusline derives the red badge from it.
- `Ledger::live_sessions() -> Vec<SessionRow>` — Phase 12's SessionStart hook adoption path.
- `camp adopt` verb + `Request::Adopt` socket op — Phase 12's `/adopt` slash command wraps it (spec §13.6 parity).
- Stream-mode spawn (`spawn::user_message`, held-stdin Worker) — the F2 parse rule for stream captures is "the line whose `type=="result"`" (phase-9 note).

## File Structure

```
crates/camp-core/src/
  patrol/mod.rs        NEW  PatrolConfig, parse_duration, Ladder (+unit tests)
  patrol/timers.rs     NEW  StallTimers, TimerKind, StallFire (+unit tests)
  lib.rs               MOD  +pub mod patrol;
  config.rs            MOD  +PatrolSection on CampConfig (+validation +tests)
  event.rs             MOD  +AgentStalled +PatrolDegraded
  vocab.rs             MOD  +"agent.stalled" +"patrol.degraded" (camp-specific)
  ledger/fold.rs       MOD  +agent_stalled/patrol_degraded arms; SessionEnd +cause_seq
  ledger/mod.rs        MOD  +SessionRow +live_sessions()
  pack.rs              MOD  +AgentDef.stall_after (frontmatter, validated)
crates/camp/src/
  daemon/patrol.rs     NEW  PatrolRuntime: tracking, watches, declare, actions, adopt
  daemon/spawn.rs      MOD  stream-mode argv + user_message + CAMP_TRANSCRIPT + ensure_worktree
  daemon/dispatch.rs   MOD  Worker{stdin,claude_session_id,released,patrol_kill}; aux reap;
                            nudge/kill/release methods; reap overrides  [phase-9 conflict area]
  daemon/orders.rs     MOD  CampdProcessor +patrol field; settle +patrol param
  daemon/event_loop.rs MOD  Token(3), min poll timeout, PATROL_WATCH arm, stall declares,
                            settle threading  [additive hunks listed in Task 11.13]
  daemon/mod.rs        MOD  patrol watcher/pipe construction, startup adopt
  daemon/socket.rs     MOD  Request::Adopt, Response::Adopt
  cmd/adopt.rs         NEW  camp adopt (autostart + socket + summary print)
  main.rs              MOD  Command::Adopt
crates/camp/tests/
  fake-agent.sh        MOD  +FAKE_AGENT_NUDGE_CLOSE +FAKE_AGENT_IGNORE_NUDGE (additive)
  daemon_patrol.rs     NEW  integration scenarios (stall/nudge/restart/exhaust/adopt)
docs/
  design/2026-07-07-phase-11-probe-findings.md  NEW  P1–P3 verbatim
  design/2026-07-05-gas-camp-design.md          MOD  §10: ONE sentence (Task 11.16, needs ruling)
```

## Watch items

1. **phase-9 merge → Task 11.13 is the rebase gate.** dispatch.rs and the settle signature are the expected conflict surfaces; re-run all gates after rebase.
2. **jiff friendly-duration parsing:** `"10m"`/`"300ms"` are expected to parse via `SignedDuration::from_str` (friendly format). Task 11.2's first test pins it; if jiff 0.2 rejects the bare form, `parse_duration` grows a thin shim (digits+unit) — NOT a silent fallback, a parser.
3. **notify on macOS (FSEvents) latency** for transcript-dir watches: integration tests use `wait_until` horizons of 20 s and stall thresholds of 300 ms–1 s; if FSEvents coalescing proves flaky in CI, the reset assertions move to unit level (the callback filter is unit-tested regardless) — integration keeps only monotone "eventually fires/eventually closes" assertions.
4. **SQLite JSON1** (`json_extract` in the adopt query): bundled rusqlite ships it; Task 11.6's test proves it in CI.
5. **`pgrep` availability:** present on macOS and Linux (procps). The probe wrapper errors loudly if the binary is missing (fail fast, no fallback).

---

### Task 11.0: Commit this plan; verify branch and baseline

**Files:**
- Create: `docs/superpowers/plans/2026-07-07-phase-11-patrol-adoption.md` (this file)

- [ ] **Step 1: Confirm the branch and a clean baseline**

Run: `git branch --show-current && git status --porcelain && cargo test --workspace --quiet 2>&1 | tail -3`
Expected: `phase-11-patrol-adoption`, empty status (beyond this file), all tests pass.

- [ ] **Step 2: Commit the plan**

```bash
git add docs/superpowers/plans/2026-07-07-phase-11-patrol-adoption.md
git commit -m "docs: phase 11 patrol and adoption execution plan"
```

- [ ] **Step 3: Record any post-approval amendments**

If the plan review returned rulings, edit the flagged decisions to record ACCEPTED/REJECTED verbatim (the Phase 10 precedent), commit as `docs: phase 11 plan review rulings`.

---

### Task 11.1: Probe findings doc + resolve the munge caveat (P1)

**Files:**
- Create: `docs/design/2026-07-07-phase-11-probe-findings.md`
- Modify: `docs/design/2026-07-06-assumption-findings.md` (A4 section: one supersession line — plan-review round 1, blocker 3)
- Modify: `crates/camp/src/daemon/spawn.rs` (comments only — lines 33–38 munge doc, line 141–146 spawn doc)

**Interfaces:** none (docs + comments). The carried PR #14 review note (transcript-path fidelity) is resolved HERE, and the A4-3 stale-fact hazard is closed HERE.

- [ ] **Step 1: Write the findings doc**

Create `docs/design/2026-07-07-phase-11-probe-findings.md` with: a provenance table (date 2026-07-07, `claude --version` = `2.1.201`-installed / `2.1.204` reported — record the exact `claude --version` output at execution time; platform Darwin 25.5.0; method: scripted probes in the session scratchpad, never inside gascamp); then three sections P1/P2/P3 copying the probe facts and verbatim evidence from this plan's "Plan-Time Probe Findings" table, including: P1's cwd → project-dir mapping (`…/p11/héllo-日本` → `…-p11-h-llo---`, per-char confirmed, munge unchanged); P2's accepted flag set and message wire shape; P3's lifetime table (`result_after=8s`, `ALIVE_PRE_EOF=yes`, `STILL_ALIVE_AFTER=45s`, SIGKILL 137) with the explicit sentence: "A4-3's 'exit 0 on stdin EOF' (2.1.201) did not reproduce at 2.1.204 for an idle process; camp's stream-worker lifetime is therefore campd-managed (release rule, spec §10 mechanics)." Close with the design consequences (Decision C2/C4 summaries).

- [ ] **Step 1b: Mark the supersession in the AUTHORITATIVE findings doc** (round-1 blocker 3)

In `docs/design/2026-07-06-assumption-findings.md`, in the A4 section's evidence list, amend the A4-3 line in place with a bracketed supersession note so no later reader inherits the stale fact:
`A4-3 stream: two result events, both success: "FIRST-OK" then "SECOND-OK", one process, one session id, exit 0 [SUPERSEDED at claude 2.1.204: an idle stream worker does NOT exit on stdin EOF — see probe P3, docs/design/2026-07-07-phase-11-probe-findings.md; the live-input capability itself is unchanged]`
and add one sentence at the end of the A4 "Spec impact" paragraph: "Phase 11 re-probed at 2.1.204: exit-on-EOF did not reproduce (P3, findings doc above) — stream-worker lifetime is campd-managed via the release rule; the stronger verdict (live input exists) stands."

ALSO (approval-pass fold-in): the stale fact appears a SECOND time in the same doc — the A4 "Observed" prose list, bullet 3 ("Live input exists — stream-json stdin … same session id, exit 0 on stdin EOF", lines 268–269). Append a bracketed pointer there too: `[exit-on-EOF superseded at 2.1.204 — see the A4-3 supersession note below]` so NO consulted surface carries the fact unqualified.

- [ ] **Step 2: Update the spawn.rs comments**

In `crates/camp/src/daemon/spawn.rs`, replace the munge doc comment's caveat sentence ("whether real claude munges unicode cwds per byte instead is unverified (F3 verified ASCII only) and is a Phase 11 input — the path is audit-only here.") with: "Verified per-CHAR against real claude 2.1.204 (Phase 11 probe P1, docs/design/2026-07-07-phase-11-probe-findings.md): a multi-byte char maps to a single dash in the real project dir too." Update the test comment on `transcript_path_munges_every_non_alphanumeric_to_dash` the same way (it currently says "is a Phase 11 verification input").

- [ ] **Step 3: Run the spawn tests (unchanged behavior)**

Run: `cargo test -p camp munge -- --nocapture`
Expected: PASS (comments only).

- [ ] **Step 4: Commit**

```bash
git add docs/design/2026-07-07-phase-11-probe-findings.md docs/design/2026-07-06-assumption-findings.md crates/camp/src/daemon/spawn.rs
git commit -m "docs: phase 11 probe findings; munge verified per-char; A4-3 EOF fact superseded"
```

---

### Task 11.2: camp-core — `patrol::parse_duration` + `[patrol]` config section

**Files:**
- Create: `crates/camp-core/src/patrol/mod.rs` (module + `parse_duration` + `PatrolConfig`; the Ladder arrives in Task 11.4)
- Modify: `crates/camp-core/src/lib.rs` (+`pub mod patrol;`)
- Modify: `crates/camp-core/src/config.rs` (+`PatrolSection`, validation in `CampConfig::parse`)

**Interfaces:**
- Produces: `patrol::parse_duration(s: &str) -> Result<jiff::SignedDuration, CoreError>` (strictly positive, friendly format); `config::PatrolSection { stall_after: String, restart_budget: u32, release_grace: String }` with defaults `"10m"`, `2`, `"30s"`; `CampConfig.patrol: PatrolSection`; `patrol::PatrolConfig { stall_after: SignedDuration, restart_budget: u32, release_grace: SignedDuration }` + `PatrolConfig::from_section(&PatrolSection) -> Result<PatrolConfig, CoreError>`.

- [ ] **Step 1: Write the failing tests** (in `patrol/mod.rs` `#[cfg(test)]`, and in `config.rs` tests)

```rust
// patrol/mod.rs tests
#[test]
fn parse_duration_accepts_friendly_forms() {
    assert_eq!(parse_duration("10m").unwrap(), SignedDuration::from_mins(10));
    assert_eq!(parse_duration("90s").unwrap(), SignedDuration::from_secs(90));
    assert_eq!(parse_duration("300ms").unwrap(), SignedDuration::from_millis(300));
}
#[test]
fn parse_duration_rejects_zero_negative_and_junk() {
    for bad in ["0s", "-5m", "", "banana", "10"] {
        let err = parse_duration(bad).unwrap_err();
        assert!(err.to_string().contains(bad) || !err.to_string().is_empty(), "{bad}");
    }
}
// config.rs tests
#[test]
fn patrol_section_parses_with_defaults_and_overrides() {
    let cfg = CampConfig::parse("[camp]\nname=\"d\"\n").unwrap();
    assert_eq!(cfg.patrol.stall_after, "10m");
    assert_eq!(cfg.patrol.restart_budget, 2);
    assert_eq!(cfg.patrol.release_grace, "30s");
    let cfg = CampConfig::parse(
        "[camp]\nname=\"d\"\n[patrol]\nstall_after=\"90s\"\nrestart_budget=1\nrelease_grace=\"500ms\"\n",
    ).unwrap();
    assert_eq!(cfg.patrol.stall_after, "90s");
    assert_eq!(cfg.patrol.restart_budget, 1);
}
#[test]
fn bad_patrol_durations_are_rejected_at_parse() {
    for toml in [
        "[camp]\nname=\"d\"\n[patrol]\nstall_after=\"0s\"\n",
        "[camp]\nname=\"d\"\n[patrol]\nstall_after=\"nope\"\n",
        "[camp]\nname=\"d\"\n[patrol]\nrelease_grace=\"-1s\"\n",
    ] {
        let err = CampConfig::parse(toml).unwrap_err();
        assert!(err.to_string().contains("patrol"), "{err}");
    }
}
#[test]
fn unknown_patrol_key_is_rejected() {
    assert!(CampConfig::parse("[camp]\nname=\"d\"\n[patrol]\nbogus=1\n").is_err());
}
#[test]
fn patrol_defaults_do_not_pollute_serialization() {
    let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
    assert!(!toml::to_string(&cfg).unwrap().contains("patrol"));
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p camp-core patrol` / `cargo test -p camp-core config` → compile errors (module/fields absent).

- [ ] **Step 3: Implement**

`patrol/mod.rs`:
```rust
//! Health patrol state machines (spec §10): pure, deterministic, no I/O.
//! Durations are jiff friendly strings ("10m"); the pure machines take
//! explicit `now: Timestamp` (the CronHeap precedent). Patrol config is
//! read at campd start; hot reload does not re-arm patrol (plan Decision L).

pub mod timers;

use jiff::SignedDuration;
use crate::config::PatrolSection;
use crate::error::CoreError;

/// Parse a strictly positive friendly duration ("10m", "90s", "300ms").
pub fn parse_duration(s: &str) -> Result<SignedDuration, CoreError> {
    let d: SignedDuration = s.parse().map_err(|e| {
        CoreError::Config(format!("[patrol] duration {s:?} does not parse: {e}"))
    })?;
    if d.is_negative() || d.is_zero() {
        return Err(CoreError::Config(format!(
            "[patrol] duration {s:?} must be strictly positive"
        )));
    }
    Ok(d)
}

/// `[patrol]` resolved to typed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatrolConfig {
    pub stall_after: SignedDuration,
    pub restart_budget: u32,
    pub release_grace: SignedDuration,
}

impl PatrolConfig {
    pub fn from_section(section: &PatrolSection) -> Result<PatrolConfig, CoreError> {
        Ok(PatrolConfig {
            stall_after: parse_duration(&section.stall_after)?,
            restart_budget: section.restart_budget,
            release_grace: parse_duration(&section.release_grace)?,
        })
    }
}
```

`config.rs` additions (mirror `DispatchConfig` exactly):
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatrolSection {
    #[serde(default = "default_stall_after")]
    pub stall_after: String,
    #[serde(default = "default_restart_budget")]
    pub restart_budget: u32,
    #[serde(default = "default_release_grace")]
    pub release_grace: String,
}
fn default_stall_after() -> String { "10m".to_owned() }
fn default_restart_budget() -> u32 { 2 }
fn default_release_grace() -> String { "30s".to_owned() }
impl Default for PatrolSection { /* from the three defaults */ }
impl PatrolSection { fn is_default(&self) -> bool { *self == PatrolSection::default() } }
```
Add `#[serde(default, skip_serializing_if = "PatrolSection::is_default")] pub patrol: PatrolSection` to `CampConfig`, and in `CampConfig::parse`, after the max_workers check: `crate::patrol::PatrolConfig::from_section(&cfg.patrol)?;` (validation only — a typo'd threshold must not become dead config). Fix the existing `round_trips_through_toml` test constructor (`patrol: PatrolSection::default()`).

- [ ] **Step 4: Run to verify pass** — `cargo test -p camp-core` → PASS (all existing config tests included).

- [ ] **Step 5: Commit** — `git commit -m "feat(core): [patrol] config section and duration parsing"`

---

### Task 11.3: camp-core — `patrol::timers::StallTimers`

**Files:**
- Modify: `crates/camp-core/src/patrol/timers.rs` (create)

**Interfaces:**
- Produces:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind { Stall, Release }
#[derive(Debug, Clone, PartialEq)]
pub struct StallFire { pub session: String, pub kind: TimerKind, pub deadline: Timestamp, pub threshold: SignedDuration }
impl StallTimers {
    pub fn new() -> StallTimers                    // + Default
    pub fn arm(&mut self, session: &str, kind: TimerKind, threshold: SignedDuration, now: Timestamp)  // upsert
    pub fn reset(&mut self, session: &str, now: Timestamp) -> bool   // Stall entries only: deadline = now + threshold; Release entries ignore resets
    pub fn disarm(&mut self, session: &str) -> bool
    pub fn is_armed(&self, session: &str) -> bool
    pub fn next_deadline(&self) -> Option<Timestamp>
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<StallFire>     // due entries removed; sorted by (deadline, session)
    pub fn poll_timeout(&self, now: Timestamp) -> Option<std::time::Duration>  // None idle; ZERO when due; else +1ms round-up (orders shape)
    pub fn len(&self) -> usize + is_empty()
}
```

- [ ] **Step 1: Write the failing tests** (same-file `#[cfg(test)]`; `fn ts(s)` helper as in orders tests)

```rust
#[test]
fn arm_then_threshold_elapses_fires_once_and_disarms() {
    let mut t = StallTimers::new();
    t.arm("c/dev/1", TimerKind::Stall, SignedDuration::from_mins(10), ts("2026-07-07T07:00:00Z"));
    assert!(t.fire_due(ts("2026-07-07T07:09:59Z")).is_empty());
    let fires = t.fire_due(ts("2026-07-07T07:10:00Z"));
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].session, "c/dev/1");
    assert_eq!(fires[0].kind, TimerKind::Stall);
    assert_eq!(fires[0].threshold, SignedDuration::from_mins(10));
    assert!(!t.is_armed("c/dev/1"), "a fired timer is removed until re-armed");
    assert!(t.fire_due(ts("2026-07-07T09:00:00Z")).is_empty());
}
#[test]
fn reset_pushes_the_deadline_out_by_the_threshold() {
    let mut t = StallTimers::new();
    t.arm("s", TimerKind::Stall, SignedDuration::from_mins(10), ts("2026-07-07T07:00:00Z"));
    assert!(t.reset("s", ts("2026-07-07T07:09:00Z")));   // transcript touch at 07:09
    assert!(t.fire_due(ts("2026-07-07T07:10:00Z")).is_empty(), "old deadline gone");
    assert_eq!(t.fire_due(ts("2026-07-07T07:19:00Z")).len(), 1);
    assert!(!t.reset("ghost", ts("2026-07-07T07:19:00Z")), "untracked resets report false");
}
#[test]
fn release_timers_ignore_resets() {
    let mut t = StallTimers::new();
    t.arm("s", TimerKind::Release, SignedDuration::from_secs(30), ts("2026-07-07T07:00:00Z"));
    assert!(!t.reset("s", ts("2026-07-07T07:00:10Z")), "release grace is not activity-resettable");
    assert_eq!(t.fire_due(ts("2026-07-07T07:00:30Z")).len(), 1);
}
#[test]
fn poll_timeout_mirrors_the_orders_shape() {
    let mut t = StallTimers::new();
    assert_eq!(t.poll_timeout(ts("2026-07-07T07:00:00Z")), None, "idle = infinite wait");
    t.arm("s", TimerKind::Stall, SignedDuration::from_secs(60), ts("2026-07-07T07:00:00Z"));
    let to = t.poll_timeout(ts("2026-07-07T07:00:59Z")).unwrap();
    assert!(to >= std::time::Duration::from_secs(1) && to <= std::time::Duration::from_millis(1500));
    assert_eq!(t.poll_timeout(ts("2026-07-07T07:02:00Z")), Some(std::time::Duration::ZERO));
}
#[test]
fn fire_due_is_deterministically_ordered_and_disarm_works() {
    let mut t = StallTimers::new();
    t.arm("b", TimerKind::Stall, SignedDuration::from_secs(10), ts("2026-07-07T07:00:00Z"));
    t.arm("a", TimerKind::Stall, SignedDuration::from_secs(10), ts("2026-07-07T07:00:00Z"));
    t.arm("gone", TimerKind::Stall, SignedDuration::from_secs(10), ts("2026-07-07T07:00:00Z"));
    assert!(t.disarm("gone"));
    let names: Vec<String> = t.fire_due(ts("2026-07-07T07:00:10Z")).into_iter().map(|f| f.session).collect();
    assert_eq!(names, vec!["a", "b"], "equal deadlines order by session");
}
#[test]
fn rearm_overwrites_kind_and_threshold() {
    let mut t = StallTimers::new();
    t.arm("s", TimerKind::Stall, SignedDuration::from_mins(10), ts("2026-07-07T07:00:00Z"));
    t.arm("s", TimerKind::Release, SignedDuration::from_secs(30), ts("2026-07-07T07:00:00Z"));
    assert_eq!(t.len(), 1);
    let fires = t.fire_due(ts("2026-07-07T07:00:30Z"));
    assert_eq!(fires[0].kind, TimerKind::Release);
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p camp-core timers` → compile error.

- [ ] **Step 3: Implement** — `HashMap<String, Entry { kind, deadline, threshold }>` (Decision B). `fire_due` drains due entries into a Vec sorted by `(deadline, session)`. `poll_timeout` copied from `OrdersRuntime::poll_timeout`'s arithmetic verbatim (negative-or-zero → `Duration::ZERO`; else round up 1 ms).

- [ ] **Step 4: Run to verify pass** — `cargo test -p camp-core timers` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(core): patrol stall/release timer store with heap-style poll timeout"`

---

### Task 11.4: camp-core — the ladder (nudge → restart → exhausted, threshold-scaling backoff)

**Files:**
- Modify: `crates/camp-core/src/patrol/mod.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderAction { Nudge, Restart, Exhausted }
impl Ladder {
    pub fn new(restart_budget: u32) -> Ladder
    pub fn on_fire(&mut self, bead: &str) -> LadderAction  // advances state; Restart increments restarts
    pub fn on_activity(&mut self, bead: &str)              // next step back to Nudge; restarts kept
    pub fn nudge_failed(&mut self, bead: &str)             // next step -> Restart
    pub fn restarts(&self, bead: &str) -> u32
    pub fn effective_threshold(&self, bead: &str, base: SignedDuration) -> SignedDuration // base * 2^restarts, saturating
    pub fn forget(&mut self, bead: &str)                   // bead closed: drop ladder state
}
```

- [ ] **Step 1: Write the failing tests** (the master plan's ladder table, incl. the backoff series)

```rust
#[test]
fn ladder_table_nudge_restart_exhausted_with_budget_two() {
    let mut l = Ladder::new(2);
    // generation 1: nudge, then restart 1
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
    assert_eq!(l.restarts("gc-1"), 1);
    // generation 2 (respawned worker): nudge again, then restart 2
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
    assert_eq!(l.restarts("gc-1"), 2);
    // budget exhausted: the next needed restart emits-and-stops
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Exhausted);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Exhausted, "exhausted is terminal");
    assert_eq!(l.restarts("gc-1"), 2, "exhaustion does not consume budget");
}
#[test]
fn budget_zero_never_restarts() {
    let mut l = Ladder::new(0);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Exhausted);
}
#[test]
fn activity_rewinds_to_nudge_but_keeps_the_restart_count() {
    let mut l = Ladder::new(2);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
    l.on_activity("gc-1"); // the nudge revived it
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge, "a revived worker is nudged first again");
    assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
    l.on_activity("gc-1");
    assert_eq!(l.restarts("gc-1"), 1, "revival does not refund the budget");
}
#[test]
fn a_failed_nudge_advances_to_restart() {
    let mut l = Ladder::new(2);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
    l.nudge_failed("gc-1");
    assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
}
#[test]
fn backoff_series_doubles_the_threshold_per_restart() {
    let mut l = Ladder::new(3);
    let base = SignedDuration::from_mins(10);
    assert_eq!(l.effective_threshold("gc-1", base), SignedDuration::from_mins(10));
    l.on_fire("gc-1"); l.on_fire("gc-1"); // nudge, restart 1
    assert_eq!(l.effective_threshold("gc-1", base), SignedDuration::from_mins(20));
    l.on_fire("gc-1"); l.on_fire("gc-1"); // nudge, restart 2
    assert_eq!(l.effective_threshold("gc-1", base), SignedDuration::from_mins(40));
    l.on_fire("gc-1"); l.on_fire("gc-1"); // nudge, restart 3
    assert_eq!(l.effective_threshold("gc-1", base), SignedDuration::from_mins(80));
}
#[test]
fn forget_clears_bead_state() {
    let mut l = Ladder::new(1);
    l.on_fire("gc-1"); l.on_fire("gc-1");
    l.forget("gc-1");
    assert_eq!(l.restarts("gc-1"), 0);
    assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p camp-core ladder` → compile error.

- [ ] **Step 3: Implement** — `HashMap<String, LadderState { restarts: u32, next: Next }>`, `enum Next { Nudge, Restart }`. `on_fire`: Nudge → return Nudge, next=Restart; Restart → if `restarts < budget` { restarts += 1; next = Nudge; return Restart } else { return Exhausted (state unchanged) }. `effective_threshold`: `base.checked_mul(1 << restarts.min(20)).unwrap_or(SignedDuration::MAX)` (saturating, no panic).

- [ ] **Step 4: Run to verify pass**, then **Step 5: Commit** — `git commit -m "feat(core): patrol ladder with threshold-scaling backoff and bounded budget"`

---

### Task 11.5: camp-core — `agent.stalled` + `patrol.degraded` events (vocab, fold, refold)

**Files:**
- Modify: `crates/camp-core/src/event.rs`, `src/vocab.rs`, `src/ledger/fold.rs`
- Check/modify: `crates/camp-core/tests/refold_prop.rs` (generator arms if it enumerates types), `tests/vocab_pin.rs` (should pass unmodified — it derives from `vocab.rs`)

**Interfaces:**
- Produces: `EventType::AgentStalled` ("agent.stalled"), `EventType::PatrolDegraded` ("patrol.degraded") — both camp-specific, both log-only in the fold. `SessionEnd` payload gains `#[serde(default)] cause_seq: Option<i64>` (audit: a patrol-kill's `session.crashed` names the stalled event that caused it).
- `agent.stalled` payload contract (deny_unknown_fields):
```json
{ "session": "t/dev/1", "agent": "dev", "action": "nudge|nudge_failed|restart|exhausted|annotate",
  "threshold": "10m", "restarts": 0, "via": "stdin|resume"?, "error": "..."? }
```
with the bead in the event's `bead` column (required, must exist) and the rig in `rig`. Validation: `session`/`agent`/`threshold` non-empty; `action` in the set; `error` required non-empty iff `action == "nudge_failed"`; `via` allowed only for `nudge`/`nudge_failed`.
- `patrol.degraded` payload: `{ "error": "...", "session": "..."? }`, `error` non-empty; log-only; no bead required.

- [ ] **Step 1: Write the failing tests** (fold tests follow the file's existing table style)

```rust
#[test]
fn agent_stalled_validates_shape_and_is_log_only() {
    // valid nudge fires fold cleanly and mutates no bead/session state
    // invalid: missing session; action "dance"; nudge_failed without error;
    // via on a restart; unknown field "extra"; bead absent; bead unknown.
}
#[test]
fn patrol_degraded_requires_the_error() { /* empty error rejected; valid passes */ }
#[test]
fn session_crashed_accepts_an_audit_cause_seq() {
    // session.woke then session.crashed with {"name":..,"reason":"patrol restart","cause_seq":7}
    // folds; released-bead behavior unchanged.
}
```
(Write these as full ledger-append tests in fold.rs's existing test module style: `Ledger::open` on a tempdir, append, assert Ok/Err with the reason substring.)

- [ ] **Step 2: Run to verify failure** — unknown event type errors.

- [ ] **Step 3: Implement** — event.rs: add both variants to the enum, `ALL`, `as_str`, `parse`. vocab.rs: append `"agent.stalled"`, `"patrol.degraded"` to `CAMP_SPECIFIC_EVENTS`. fold.rs: `AgentStalled`/`PatrolDegraded` payload structs + validation arms per the contract above (AgentStalled: `required_bead` + `known_bead`); add `cause_seq: Option<i64>` (`#[serde(default)] #[allow(dead_code)]`) to `SessionEnd`.

- [ ] **Step 4: Check the refold property test** — read `tests/refold_prop.rs`; if its generator enumerates event types, add arms producing valid `agent.stalled`/`patrol.degraded` payloads (log-only events cannot diverge state, but the generator must not reject them). Run `cargo test -p camp-core refold_prop vocab_pin`.

- [ ] **Step 5: Run the full core suite** — `cargo test -p camp-core` → PASS. **Step 6: Commit** — `git commit -m "feat(core): agent.stalled and patrol.degraded events with fold validation"`

---

### Task 11.6: camp-core — `Ledger::live_sessions()` for adoption

**Files:**
- Modify: `crates/camp-core/src/ledger/mod.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub name: String, pub agent: String, pub rig: Option<String>,
    pub claude_session_id: Option<String>, pub transcript_path: Option<String>,
    pub pid: Option<i64>, pub bead: Option<String>, pub spawned_ts: String,
    pub woke_actor: String,          // the session.woke event's actor ("campd" = campd-spawned)
    pub worktree: Option<String>,    // from the woke event's data (audit field, Phase 8)
}
pub fn live_sessions(&self) -> Result<Vec<SessionRow>, CoreError>
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn live_sessions_returns_registry_rows_with_their_woke_provenance() {
    // append: campd session.woke (name w1, full payload incl. claude_session_id,
    // transcript_path, bead, worktree), a hook-actor session.woke (name a1, minimal),
    // a campd woke w2 then session.stopped w2.
    // live_sessions(): [a1, w1] (name-ordered); w1.woke_actor == "campd",
    // w1.claude_session_id/transcript_path/bead/worktree populated;
    // a1.woke_actor == "hook:session-start"; stopped w2 absent.
}
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement** — SQL:
```sql
SELECT s.name, s.agent, s.rig, s.claude_session_id, s.transcript_path, s.pid, s.bead, s.spawned_ts,
       (SELECT e.actor FROM events e WHERE e.type = 'session.woke'
         AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1),
       (SELECT json_extract(e.data, '$.worktree') FROM events e WHERE e.type = 'session.woke'
         AND json_extract(e.data, '$.name') = s.name ORDER BY e.seq LIMIT 1)
FROM sessions s WHERE s.status = 'live' ORDER BY s.name
```
A NULL woke actor (a sessions row without its woke event) is `CoreError::Corrupt` — fail fast, the fold guarantees the pair.

- [ ] **Step 4: Run to verify pass** (this also proves JSON1 in CI). **Step 5: Commit** — `git commit -m "feat(core): live_sessions registry query for adoption"`

---

### Task 11.7: camp-core — `AgentDef.stall_after` frontmatter override

**Files:**
- Modify: `crates/camp-core/src/pack.rs`
- Modify (constructor fixes): `crates/camp/src/daemon/spawn.rs` tests (`full_agent()`, `undeclared_agent_fields_emit_no_flags`)

**Interfaces:**
- Produces: `AgentDef.stall_after: Option<String>` — parsed from the `stall_after` frontmatter key, validated at parse time via `patrol::parse_duration` (a bad agent file fails fast naming the file and key).

- [ ] **Step 1: Write the failing tests** (pack.rs test module)

```rust
#[test]
fn stall_after_frontmatter_parses_and_validates() {
    // "---\nname: dev\nstall_after: 5m\n---\nP\n" -> Some("5m")
    // absent -> None
    // "stall_after: banana" -> Err containing "stall_after" and the file name
}
```

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement** — `let stall_after = get_str("stall_after")?; if let Some(s) = &stall_after { crate::patrol::parse_duration(s).map_err(|e| pack_err(path, format!("frontmatter key \"stall_after\": {e}")))?; }` + struct field + all constructor sites (`cargo build` finds them). **Step 4: Run** `cargo test --workspace` (spawn.rs constructors compile). **Step 5: Commit** — `git commit -m "feat(core): per-agent stall_after frontmatter override"`

---

### Task 11.8: camp — stream-mode spawn (`Decision C`) + `ensure_worktree` (`Decision H`)

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs`

**Interfaces:**
- Consumes: probe facts P2/P3; F1/F2/F5/F7 pins.
- Produces:
```rust
pub enum StdinMode { Null, HeldStream }               // on SpawnSpec: pub stdin_mode: StdinMode
pub fn user_message(text: &str) -> String              // one stream-json line + '\n' (serde-escaped)
pub fn build_spec(..., stdin_mode: StdinMode) -> SpawnSpec  // stream argv variant below
pub fn spawn(spec: &SpawnSpec) -> Result<Child>        // HeldStream: Stdio::piped() stdin; caller takes child.stdin
pub fn ensure_worktree(rig_path: &Path, worktrees_dir: &Path, bead_id: &str) -> Result<PathBuf>
```
Stream argv (pinned by test): `claude --output-format stream-json --input-format stream-json --session-id <sid> [--model M] [--permission-mode P] [--allowedTools T] --append-system-prompt <prompt> -p` — NO positional task; the task is the first `user_message(task_prompt(bead, session))` written by the caller (dispatch, Task 11.9). Json mode argv unchanged byte-for-byte from Phase 8. Env gains `("CAMP_TRANSCRIPT", <transcript_path>)` in BOTH modes (workers and tests may reference their own transcript; real claude ignores it).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn stream_argv_matches_probe_p2_and_the_fixture_facts() {
    // build_spec(..., StdinMode::HeldStream) with full_agent():
    // argv == ["claude","--output-format","stream-json","--input-format","stream-json",
    //          "--session-id",sid,"--model","sonnet","--permission-mode","acceptEdits",
    //          "--allowedTools","Read,Edit,Bash","--append-system-prompt","Implement with TDD.","-p"]
    // env contains ("CAMP_TRANSCRIPT", <transcript>); json-mode argv byte-identical to Phase 8's pin.
}
#[test]
fn user_message_is_one_escaped_stream_json_line() {
    let line = user_message("say \"hi\"\nnow");
    assert!(line.ends_with('\n'));
    let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(v["type"], "user");
    assert_eq!(v["message"]["role"], "user");
    assert_eq!(v["message"]["content"], "say \"hi\"\nnow");
}
#[test]
fn held_stream_spawn_pipes_stdin_and_null_spawn_does_not() {
    // spawn `cat` (argv override) in HeldStream: child.stdin.is_some(); write a line;
    // drop stdin; child exits 0. In Null mode: child.stdin.is_none().
}
#[test]
fn ensure_worktree_reuses_the_beads_worktree_and_rejects_impostors() {
    // (extends worktree_create_and_remove_round_trip's git fixture)
    // 1. absent -> creates (parity with create_worktree)
    // 2. existing valid worktree on camp/<bead> -> returns it, no error, work preserved
    //    (write a file, ensure again, file still there)
    // 3. plain directory (not a worktree) -> Err containing "residue"
}
```

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement.** `user_message`: `serde_json::json!({"type":"user","message":{"role":"user","content":text}})` + newline. `build_spec`: branch on mode for the three flag positions and the trailing task; keep the doc comment citing P2. `spawn`: `match spec.stdin_mode { Null => Stdio::null(), HeldStream => Stdio::piped() }` — the F5 comment updates to cite Decision C. `ensure_worktree`: if `!dir.exists()` → `create_worktree`; else if `dir.join(".git").exists()` and `git -C <dir> branch --show-current` == `camp/<bead>` → `Ok(dir)`; else → the existing residue bail.

- [ ] **Step 4: Run** `cargo test -p camp spawn` → PASS. **Step 5: Commit** — `git commit -m "feat: stream-json worker spawn mode and worktree reuse for respawns"`

---

### Task 11.9: camp — dispatcher worker-lifecycle extensions (held stdin, nudge, kill, release, aux reap)

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` **[phase-9 conflict area — every edit here is additive; announce via this plan]**

**Interfaces:**
- Consumes: Task 11.8's `StdinMode`, `user_message`, `ensure_worktree`.
- Produces (all consumed by Task 11.11's actions and Task 11.13's loop):
```rust
pub enum NudgeOutcome { Delivered, NoPipe, Failed(String) }
impl Dispatcher {
    pub fn nudge_via_stdin(&mut self, session: &str, text: &str) -> NudgeOutcome
    pub fn kill_worker(&mut self, session: &str, cause_seq: Seq) -> bool   // SIGKILL own child; marks patrol_kill; false if not our child
    pub fn release_worker(&mut self, bead: &str, reason: &str) -> Option<String> // drop stdin, mark released; returns session name if found
    pub fn kill_released(&mut self, session: &str) -> bool                 // release-grace fired: SIGKILL if still ours+alive
    pub fn spawn_aux(&mut self, session: &str, purpose: &str, cmd: Command) -> Result<()>  // resume-nudge children; reaped in reap()
    pub fn is_child(&self, session: &str) -> bool
}
```
`Worker` gains `claude_session_id: String`, `stdin: Option<std::process::ChildStdin>`, `released: Option<String>`, `patrol_kill: Option<Seq>`. `launch()` changes: agents spawn `StdinMode::HeldStream` when `config.dispatch.command` ends with `claude`… **no — mode is NOT command-sniffed** (that would be a hidden fallback): ALL campd dispatch spawns use `HeldStream` (Decision C; fake-agent tolerates it, C3). After a successful spawn, `launch` takes `child.stdin`, writes `user_message(task_prompt(bead, session))`; a failed initial write follows the existing spawn-failure path (`session.crashed {reason}` + worktree kept). `prepare()` switches `create_worktree` → `ensure_worktree` (respawn reuse) — the `Dispatcher::failed` set still prevents same-life retry loops of genuinely broken beads. `reap()` end-classification order: `released` → `SessionStopped` + `reason` (+exit/signal); else `patrol_kill` → `SessionCrashed` + `reason:"patrol restart"` + `cause_seq`; else `classify(status)` (F4). Aux children reap in the same `try_wait` sweep: exit 0 → forget; nonzero → append `patrol.degraded { error: "...", session }`.

- [ ] **Step 1: Write the failing unit tests** (dispatch.rs test module style — real exited/held children via `true`/`cat`, `spawn_probe_guard`)

```rust
#[test] fn released_worker_reaps_as_stopped_with_the_reason() { /* exited child, released=Some; reap -> session.stopped, data.reason contains "released", no crash, bead NOT released by fold */ }
#[test] fn patrol_killed_worker_reaps_as_crashed_with_cause_seq() { /* kill_worker on a `cat` child (alive), then reap -> session.crashed with signal 9, reason "patrol restart", cause_seq pinned */ }
#[test] fn nudge_via_stdin_writes_one_message_line_or_reports_no_pipe() { /* worker with a piped `cat` child: Delivered + capture file/pipe read shows the exact user_message bytes; worker with stdin None: NoPipe; dropped-reader pipe: Failed(..) */ }
#[test] fn release_worker_drops_stdin_and_names_the_session() { /* held `cat` child on bead gc-1: release_worker("gc-1","released after bead close") -> Some(session); child sees EOF and exits; stdin now None */ }
#[test] fn aux_children_reap_without_session_events_and_event_failures() { /* spawn_aux `true` -> reap appends nothing; spawn_aux `false` -> reap appends patrol.degraded naming the session */ }
```

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement** per the interface block. **Step 4: Run** `cargo test -p camp dispatch` and the full `cargo test -p camp` (Phase 8 integration tests must stay green — fake agents under HeldStream: verify `daemon_dispatch.rs` passes unchanged; the fake agent ignores stdin and exits on its own, C3). **Step 5: Commit** — `git commit -m "feat: held-stdin worker lifecycle - nudge, patrol kill, release, aux reap"`

---

### Task 11.10: camp — `daemon/patrol.rs` part 1: tracking, watches, observation, stall declaration

**Files:**
- Create: `crates/camp/src/daemon/patrol.rs`; Modify: `daemon/mod.rs` (+`pub mod patrol;`)

**Interfaces:**
- Consumes: `StallTimers`, `Ladder`, `PatrolConfig`, `EventType::{AgentStalled, PatrolDegraded}`, `pack::resolve_agent` (per-agent threshold), `spawn::munge` conventions.
- Produces:
```rust
pub struct PatrolRuntime { /* timers, ladder, config, tracked sessions, OWNED Option<notify::RecommendedWatcher> (None in unit tests), watched-dir refcounts, filter Arc, pending actions */ }
pub enum Owned { Child, AdoptedPid(i64), Annotate }
impl PatrolRuntime {
    pub fn new(config: PatrolConfig, camp_config: &CampConfig) -> PatrolRuntime
    pub fn filter_slot(&self) -> Arc<Mutex<WatchFilter>>          // for the notify callback closure
    pub fn set_watcher(&mut self, watcher: notify::RecommendedWatcher)  // daemon::run installs it; unit tests skip
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration>
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<StallFire>
    pub fn observe(&mut self, event: &Event)                      // MEMORY-ONLY; called inside the cursor txn
    pub fn apply_tracking(&mut self, ledger: &mut Ledger) -> Result<()>  // uses the owned watcher
    pub fn drain_touched(&mut self, now: Timestamp)               // watch activity -> reset timers + ladder.on_activity
    pub fn take_watch_error_events(&mut self) -> Vec<EventInput>  // patrol.degraded (LOW-8 mold)
    pub fn declare_stalls(&mut self, ledger: &mut Ledger, fires: &[StallFire]) -> Result<bool>
}
pub(super) fn on_watch_event(result: notify::Result<notify::Event>, sender: Option<&mio::unix::pipe::Sender>, filter: &Mutex<WatchFilter>)
pub struct WatchFilter { pub registered: HashSet<PathBuf>, pub touched: HashSet<PathBuf>, pub error: Option<String> }
```
Semantics:
- `observe` (inside the cursor transaction; memory-only, never I/O) processes each event through an EXCLUSIVE dispatch — special-cased lifecycle kinds return before reset matching (Decision J(ii)): `session.woke` with a `transcript_path` → queue Track (owned = Child if `event.actor == "campd"` else Annotate; threshold = agent `stall_after` override or camp default; the tracked state keeps bead, agent, rig, claude_session_id, transcript_path, and worktree from the payload — the nudge-resume and restart paths need them) and return. `session.stopped`/`session.crashed` → queue Untrack and return. `bead.closed` for a tracked worker's bead → queue Release (Decision C2) + `ladder.forget(bead)` and return. Then, ONLY for events with `actor != "campd"` (Decision J(i) — patrol's own `agent.stalled`/`patrol.degraded` and every other campd append must never read as worker activity), an event matching Decision J's three keys → timer reset + `ladder.on_activity(bead)`.
- `apply_tracking` (outside the txn): performs the queued notify `watch`/`unwatch` on transcript PARENT dirs (ref-counted; `create_dir_all` the parent first — claude uses the same dir), arms/disarms timers (`arm(session, Stall, ladder.effective_threshold(bead, threshold), now)`), maintains `registered` in the shared filter. A notify error appends `patrol.degraded` (durable — never stderr-only).
- `declare_stalls`: for each Stall fire on a tracked session: `action = if owned == Annotate { "annotate" } else { ladder.on_fire(bead) }`; append `agent.stalled` (payload per Task 11.5; `via` filled by the executor later — declared as the CHOSEN action: `"nudge"`, `"restart"`, `"exhausted"`, `"annotate"`); queue the pending action with the appended seq as `cause_seq`; re-arm for annotate/nudge (effective threshold), disarm+forget tracking for exhausted (emit-and-stop). Release fires → queue KillReleased (no event here; the reap's `session.stopped` carries the reason). Returns whether anything was appended (drives `wake_ledger_work`).
- `on_watch_event`: Ok(event) → intersect `event.paths` with `registered`; insert hits into `touched`; signal the pipe on any hit. Err → store in `error`, signal. (The `orders::on_watch_event` mold.)

- [ ] **Step 1: Write the failing unit tests**

```rust
#[test] fn observe_woke_then_apply_arms_a_timer_and_registers_the_watch() { /* tempdir ledger; woke event (campd actor, transcript under tempdir); observe+apply; poll_timeout Some; filter.registered contains the path; parent dir created */ }
#[test] fn ledger_activity_resets_the_timer_by_all_three_keys() { /* Decision J: bead-match, actor-match, data.session-match each push the deadline (fire_due empty at old deadline) */ }
#[test] fn transcript_touch_resets_via_the_filter() { /* simulate callback: on_watch_event(Ok(event with the registered path)) -> touched; drain_touched -> deadline pushed; unrelated path -> no reset */ }
#[test] fn watch_errors_become_durable_patrol_degraded() { /* on_watch_event(Err(..)) -> take_watch_error_events yields one patrol.degraded; drained on second take */ }
#[test] fn declare_stalls_appends_agent_stalled_with_the_ladder_action_and_cause() { /* tracked campd worker; synth StallFire; declare -> one agent.stalled (action nudge, threshold string, restarts 0, bead column set); second declare (still silent) -> action restart; annotate-owned session -> action annotate and timer re-armed */ }
#[test] fn patrols_own_events_do_not_rewind_the_ladder() { /* ROUND-1 BLOCKER 1 REGRESSION PIN: tracked worker; declare a stall (appends agent.stalled, action nudge); feed the JUST-APPENDED agent.stalled event through observe() exactly as the settle's catch-up will; then fire again: declare must yield action "restart", NOT "nudge" — campd-actored events never count as worker activity. Also: observe(session.crashed for the session) untracks without on_activity. */ }
#[test] fn frontmatter_stall_after_governs_the_armed_threshold() { /* round-1 note: agent file with `stall_after: 5m`; observe(session.woke for that agent) + apply_tracking at now -> fire_due(now + 4m59s) empty, fire_due(now + 5m) fires — the 5m override, not the 10m camp default, armed the timer */ }
#[test] fn session_end_untracks_and_exhaustion_stops() { /* stopped event -> disarmed; exhausted path (budget 0): declare -> action exhausted, timer NOT re-armed, tracking forgotten */ }
```

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement.** **Step 4: Run to verify pass.** **Step 5: Commit** — `git commit -m "feat: patrol runtime - tracking, transcript watches, stall declaration"`

---

### Task 11.11: camp — `daemon/patrol.rs` part 2: action execution (nudge / restart / release)

**Files:**
- Modify: `crates/camp/src/daemon/patrol.rs`

**Interfaces:**
- Consumes: Task 11.9's `Dispatcher` methods; `Ledger`; `CampConfig` (rig paths for resume cwd).
- Produces:
```rust
impl PatrolRuntime {
    pub fn execute_pending(&mut self, ledger: &mut Ledger, dispatcher: &mut Dispatcher) -> Result<()>
}
pub(super) const NUDGE_PROMPT: &str = /* mechanical status-request text, {bead}/{session}/{threshold} substituted */;
```
Semantics per pending action:
- **Nudge(session, cause_seq):** if `dispatcher.is_child(session)` → `nudge_via_stdin(session, &nudge_text)`: `Delivered` → re-arm (already re-armed at declare; update the appended event? NO — the declared `agent.stalled` already said `action:"nudge"`; the executor only handles failure); `NoPipe`/not-a-child → RESUME path: `dispatcher.spawn_aux(session, "nudge-resume", cmd)` where cmd = `claude -p --resume <claude_session_id> <nudge_text> --output-format json`, cwd = the worker's worktree (registry) else the bead's rig path, stdin null, stdout/stderr → `<camp>/sessions/<munge(session)>.nudge.log` (truncated per attempt). `Failed(e)`/spawn error → append `agent.stalled {action:"nudge_failed", error, via}` + `ladder.nudge_failed(bead)` + re-arm.
- **Restart(session, cause_seq):** child → `dispatcher.kill_worker(session, cause_seq)` (SIGCHLD path appends the caused `session.crashed`, fold releases the bead, converge respawns — each its own event). AdoptedPid → **re-probe FIRST, then kill the re-probed pid only** (round-1 blocker 2: the tracked pid was observed at adopt time, possibly hours ago; a non-child worker's death produces no SIGCHLD, so a stale pid may have been REUSED by an innocent process — `kill -9` on it would be campd killing a bystander): `probe_alive(claude_session_id, None)` immediately before the kill; probe returns None → the worker is already dead → append `session.crashed {name, reason:"patrol restart: found dead at restart", cause_seq}` directly, disarm, NO kill; probe returns Some(fresh_pid) → `/bin/kill -9 <fresh_pid>` via `Command`, verify death with a second probe, append `session.crashed {name, reason:"patrol restart", cause_seq}`, disarm. (The ms-scale window between re-probe and kill is accepted; the hours-scale window is what this closes.) A kill/verify failure → append `patrol.degraded {error, session}` and leave the timer armed (retry at next fire) — never silent.
- **Release(bead):** `dispatcher.release_worker(bead, "released after bead close")` → on Some(session): arm `TimerKind::Release` with `release_grace`; on None (already exited/not held): nothing.
- **KillReleased(session):** `dispatcher.kill_released(session)`; reap turns it into the reasoned `session.stopped`.
NUDGE_PROMPT text (machinery, not role content): `"Camp patrol status request: no activity has been observed for {threshold}. Bead {bead} is still open. If you are mid-task, continue and record a milestone: `camp event emit \"<one line>\" --bead {bead} --session {session}`. If the work is finished, close it now with `camp close {bead} --outcome <pass|fail> --reason \"<one line>\"` and exit."`

- [ ] **Step 1: Write the failing unit tests**

```rust
#[test] fn a_child_nudge_goes_over_stdin_and_a_pipeless_one_resumes() { /* held `cat` worker: execute Nudge -> Delivered, message bytes observed; stdin-None worker: aux child spawned with --resume <sid> in argv (assert via a recording fake `claude` script) */ }
#[test] fn a_failed_nudge_is_evented_and_advances_the_ladder() { /* broken pipe -> agent.stalled action nudge_failed with error; ladder next is Restart */ }
#[test] fn restart_kills_the_child_and_the_crash_carries_the_cause() { /* execute Restart on a held `cat` -> reap -> session.crashed cause_seq == stalled seq; bead released by fold */ }
#[test] fn adopted_restart_reprobes_before_killing_and_never_kills_a_stale_pid() { /* ROUND-1 BLOCKER 2 REGRESSION PIN. Case A (dead worker, stale pid): track a session as AdoptedPid(P) where P is a live INNOCENT process (a plain `sleep 30` WITHOUT the session uuid in argv — simulating pid reuse); execute Restart -> probe by uuid finds nothing -> session.crashed {reason contains "found dead", cause_seq} appended, timer disarmed, and the innocent P is STILL ALIVE (kill -0 via ps succeeds). Case B (live worker): track AdoptedPid of a uuid-bearing sleeper; execute Restart -> re-probed pid killed, death verified, session.crashed {reason:"patrol restart"} appended. */ }
#[test] fn release_arms_the_grace_and_kill_released_stops_with_reason() { /* release -> Release timer armed; fire -> kill_released -> reap -> session.stopped reason released */ }
```

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement.** **Step 4: Run to verify pass.** **Step 5: Commit** — `git commit -m "feat: patrol ladder actions - stdin/resume nudge, caused restart, release"`

---

### Task 11.12: camp — adoption (`adopt()`, process probe, worktree sweep)

**Files:**
- Modify: `crates/camp/src/daemon/patrol.rs`

**Interfaces:**
- Consumes: `Ledger::live_sessions()`, Decision F/G rules, `spawn::remove_worktree`.
- Produces:
```rust
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct AdoptSummary { pub crashed: usize, pub rearmed: usize, pub released: usize, pub swept: usize, pub kept: usize }
pub fn adopt(ledger: &mut Ledger, patrol: &mut PatrolRuntime, dispatcher: &mut Dispatcher,
             camp: &CampDir, config: &CampConfig) -> Result<AdoptSummary>   // watches via patrol's owned watcher
pub(super) fn probe_alive(claude_session_id: Option<&str>, pid: Option<i64>) -> Result<Option<i64>>  // pgrep -f uuid, else ps -p pid; Ok(Some(pid)) alive
```
Flow (Decisions F + G, in order): (1) per live registry row: **skip ALL sessions patrol already tracks** — campd's own children AND previously adopted/annotate-armed rows (round-1 minor 4: skipping only children made a second adopt re-count a live adopted worker as `rearmed`, breaking idempotency); probe the rest; dead → `session.crashed {name, reason:"adopt: process not found"}`; alive + bead closed/absent → release (for non-children: `/bin/kill` + verified + `session.stopped {name, reason:"released after bead close"}` appended directly — counted `released`); alive + bead open → track + arm fresh (`Owned::AdoptedPid`), counted `rearmed`. (2) sweep `<camp>/worktrees/*` per Decision G (`swept` = removed+reaped, `kept` = newly kept). `probe_alive` errors (pgrep/ps missing or failing to exec) are hard errors — fail fast, adopt reports it, campd startup surfaces it.

- [ ] **Step 1: Write the failing unit tests** (in-process: real `Ledger`, a real spawned `cat` child for the alive case using its actual argv trick — spawn `bash -c "sleep 30 # <uuid>"` so pgrep -f matches the uuid; `spawn_probe_guard` held)

```rust
#[test] fn adopt_marks_dead_sessions_crashed_and_releases_their_beads() { /* woke row w/ random uuid, no process -> session.crashed; claimed bead back to open */ }
#[test] fn adopt_rearms_living_sessions_and_releases_finished_ones() { /* uuid-bearing sleeper alive + open bead -> rearmed=1, timer armed; second sleeper + closed bead -> released=1, session.stopped w/ reason, process killed */ }
#[test] fn adopt_sweeps_worktrees_by_the_decision_g_table() { /* four dirs: closed-pass bead -> removed + bead.worktree.reaped; closed-fail undisposed -> worktree.kept "adopt:"; open bead -> untouched, no event; unknown-bead dir -> untouched, counted kept? NO: reported, not evented — assert dir survives and summary/kept excludes it (it lands in the returned report string/stderr) */ }
#[test] fn adopt_is_idempotent() { /* run twice WITH a still-live adopted worker in play; second AdoptSummary is ALL ZEROS (already-tracked rows are skipped — round-1 minor 4) and no duplicate events appended */ }
```

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement.** **Step 4: Run to verify pass.** **Step 5: Commit** — `git commit -m "feat: adoption - probe-based registry reconciliation and worktree sweep"`

---

### Task 11.13: camp — event-loop / settle / startup integration **[REBASE GATE: if phase-9 has merged, rebase onto main and re-run all gates BEFORE this task]**

**Files:**
- Modify: `crates/camp/src/daemon/event_loop.rs` (hunks: token const, next_token 4, min-timeout, PATROL_WATCH arm, stall-declare on wake, settle threading)
- Modify: `crates/camp/src/daemon/orders.rs` (`CampdProcessor` +`patrol` field calling `observe`; `settle` +patrol param)
- Modify: `crates/camp/src/daemon/mod.rs` (patrol watcher + pipe construction; startup adopt between the two settles)

**Interfaces:**
- Produces the final loop shape (every hunk additive):
```rust
const PATROL_WATCH: Token = Token(3);   // layout: 0 listener / 1 config / 2 SIGCHLD / 3 patrol / 4+ connections
let mut next_token = 4usize;
// wake path:
let timeout = min_timeout(runtime.poll_timeout(now), patrol.poll_timeout(now));
let stall_fires = patrol.fire_due(now);
wake_ledger_work |= patrol.declare_stalls(ledger, &stall_fires)?;
// PATROL_WATCH arm: drain pipe; patrol.drain_touched(now); for e in patrol.take_watch_error_events() { ledger.append(e)?; wake_ledger_work = true; }
// settle (event_loop::settle) becomes:
loop {
    orders::settle(ledger, processor, runtime, clock, patrol)?;   // observe() runs per event in CampdProcessor
    patrol.apply_tracking(ledger)?;                               // owned watcher
    patrol.execute_pending(ledger, dispatcher)?;
    dispatcher.converge(ledger)?;
    if !ledger.has_events_past(ledger.cursor(cursor::CAMPD_CURSOR)?)? { return Ok(()); }
}
```
`daemon::run` additions: build `PatrolConfig` from the loaded config (fail fast); create the patrol notify watcher + mio pipe (the config-watch mold, `patrol::on_watch_event` closing over `patrol.filter_slot()`), install it via `patrol.set_watcher(...)`; after the FIRST startup settle: `patrol::adopt(...)` (its events are drained by the existing second settle); pass the patrol pipe receiver + the patrol runtime into `event_loop::run`. `Request::Adopt` handling arrives in Task 11.14.

- [ ] **Step 1: Write the failing tests** — unit: `min_timeout` table (None/None→None, Some/None→Some, min of two). Loop-level: extend `daemon/mod.rs`'s `daemon_serves_status_poke_and_stop_over_the_socket` expectations ONLY if event order changed (it should not — a fresh camp has no sessions; adopt appends nothing). New unit test in event_loop.rs tests: a ledger with one live campd-woke session row and a due synthetic stall timer → one pass of the wake path declares `agent.stalled` and the settle executes the nudge against a dispatcher with no child → `nudge_failed` path evented (asserts the declare→settle wiring without a real poll).

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement** (threading `patrol` through `run`/`serve_connection`/`drain_lines`/`reap_and_refill`/`settle` — signatures grow one parameter; `#[allow(clippy::too_many_arguments)]` already present). **Step 4: Run the FULL suite** — `cargo test --workspace` (Phase 7/8/10 daemon tests must stay green: token renumbering is invisible to clients; idle poll timeout still None when both sources idle). **Step 5: Commit** — `git commit -m "feat: patrol timers and watches wired into the campd event loop (token 3)"`

---

### Task 11.14: camp — `Request::Adopt` + `camp adopt` verb

**Files:**
- Modify: `crates/camp/src/daemon/socket.rs`, `crates/camp/src/daemon/event_loop.rs` (drain_lines arm), `crates/camp/src/main.rs`
- Create: `crates/camp/src/cmd/adopt.rs`

**Interfaces:**
- Produces: `Request::Adopt` (wire `{"op":"adopt"}`); `Response::Adopt { ok: bool, crashed: usize, rearmed: usize, released: usize, swept: usize, kept: usize }` (untagged order: Status, Adopt, Error, Ok — Adopt's required fields are disjoint from Status's); `camp adopt` prints `adopted: N crashed, N re-armed, N released, N worktrees swept, N kept`.

- [ ] **Step 1: Write the failing tests** — socket.rs wire-format pins (request + response JSON exact); a daemon-level test in `daemon/mod.rs` tests: fresh camp → `{"op":"adopt"}` → `{"ok":true,"crashed":0,...}` (and the daemon stays serving). cmd-level: `tests/daemon_patrol.rs` covers the real path (Task 11.15).

- [ ] **Step 2: Run to verify failure.** **Step 3: Implement** — drain_lines arm calls `patrol::adopt(...)` and responds with the summary; `cmd/adopt.rs` uses `autostart::request_with_autostart(camp, &Request::Adopt, "adopt")`; `main.rs` gains `Command::Adopt` ("reconcile the session registry against reality"). **Step 4: Run to verify pass.** **Step 5: Commit** — `git commit -m "feat: camp adopt verb over the campd socket"`

---

### Task 11.15: fake-agent knobs + integration suite (`daemon_patrol.rs`)

**Files:**
- Modify: `crates/camp/tests/fake-agent.sh` (additive env knobs)
- Create: `crates/camp/tests/daemon_patrol.rs` (harness copied from `daemon_dispatch.rs`: `scaffold` grows a `[patrol]` section + `CLAUDE_CONFIG_DIR` env on `Daemon::spawn`)

**fake-agent.sh additions** (after the HOLD block, before the close):
```bash
if [[ -n "${FAKE_AGENT_NUDGE_CLOSE:-}" ]]; then
  # Stream-mode contract: line 1 on stdin is the task message; a later line
  # is a patrol nudge. React to the nudge by closing (the revival proof).
  read -r _task_line
  read -r _nudge_line
fi
```
plus, guarded at the top of the work phase: `if [[ -n "${FAKE_AGENT_TOUCH_TRANSCRIPT:-}" ]]; then mkdir -p "$(dirname "$CAMP_TRANSCRIPT")"; echo alive >> "$CAMP_TRANSCRIPT"; fi` (proves the watch-reset path end-to-end).

**Integration scenarios** (each a `#[test]`, `[patrol] stall_after = "400ms"`, `release_grace = "500ms"`, budgets per test; `wait_until` horizons 20 s):

- [ ] **Step 1: `silent_worker_stalls_and_a_nudge_revives_it`** — dev agent, `FAKE_AGENT_NUDGE_CLOSE=1` (no HOLD): worker claims then blocks reading stdin (silent — fake agents write no transcripts); wait for `agent.stalled` with `data.action == "nudge"`; the nudge line unblocks the read → worker closes pass → `session.stopped` (released path). Assert exactly ONE `session.woke` for the bead (no restart), the stalled event's `bead`/`session`/`threshold`/`restarts:0`, and `bead.closed` outcome pass.
- [ ] **Step 2: `an_unresponsive_worker_is_restarted_and_the_bead_rehooked`** — `FAKE_AGENT_HOLD_DIR` gate (never opened for the first instance), `restart_budget = 1`: stall → nudge (ignored) → stall → restart. Wait for `session.crashed` with `data.reason == "patrol restart"` and a `cause_seq` pointing at the second `agent.stalled`; then open the gate; the RESPAWNED worker (second `session.woke`, same bead) closes pass. Assert the event chain order and that `bead.claimed` count == 2 (re-hooked).
- [ ] **Step 3: `ladder_exhaustion_emits_and_stops`** — `restart_budget = 0`, worker holds forever: expect `agent.stalled action:"nudge"` then `action:"exhausted"`, then NO further `agent.stalled`/`session.crashed` for 2 s of quiet (bounded negative window), campd still answers `status`. Clean up via `stop` (Daemon drop kills the held worker).
- [ ] **Step 4: `kill9_campd_then_adopt_reconciles_exactly`** — the master-plan scenario: campd dispatches two held workers (worktree isolation ON for one); `kill -9` campd; SIGKILL worker A manually (dead), leave worker B alive; also pre-create the interrupted-disposition case (a bead closed pass whose worktree dir still exists — close A's bead via CLI after killing A, leaving its worktree). Restart campd (`Daemon::spawn`) → auto-adopt runs. Assert: `session.crashed {reason:"adopt: process not found"}` for A exactly once; B re-armed (observable: an `agent.stalled` for B eventually fires, then open B's gate → close pass); A's worktree removed + `bead.worktree.reaped`. Then run `camp adopt` (CLI) → summary all zeros for crash/sweep (idempotent, "reconciles exactly").
- [ ] **Step 5: `transcript_activity_resets_the_timer`** — `FAKE_AGENT_TOUCH_TRANSCRIPT=1` + `FAKE_AGENT_NUDGE_CLOSE=1`, `stall_after = "10s"`: the touch at claim time re-arms; assert NO `agent.stalled` exists at the moment `bead.claimed` + 1 s (the touch pushed the deadline to touch+10 s)… then don't wait for the stall — nudge-close via `camp adopt`? SIMPLER, monotone: assert the worker's `agent.stalled` (when it eventually fires at ~10 s) has `ts - claim ts >= 10s` measured from the TOUCH, i.e., total one stalled event and the test's own transcript touch (harness appends a second line at +5 s via the path from `session.woke` data) delays it: total elapsed claim→stalled > 15 s. Mark `#[ignore]`-free but timing-generous; if CI proves it flaky per Watch item 3, demote to the unit-level reset tests (already present) and delete — noted in the PR.
- [ ] **Step 6: Run the suite** — `cargo test -p camp --test daemon_patrol` → PASS; then `cargo test --workspace`.
- [ ] **Step 7: Commit** — `git commit -m "test: patrol integration - stall, nudge, restart, exhaustion, adopt"`

---

### Task 11.16: spec sentence **[ADOPTED by the operator, 2026-07-07 — lands in this PR]**, gates, PR, CI, exit-criteria evidence

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md` §10 (one sentence — operator-adopted, the only spec edit in flight)

- [ ] **Step 1: The spec §10 addition (operator-ADOPTED, relayed 2026-07-07)** — add after the stall mechanism paragraph: *"campd-spawned workers run in stream-json input mode with campd holding the stdin pipe (the live nudge path); when a worker's bead closes, campd releases it — stdin EOF, a bounded grace, then termination recorded as `session.stopped` with the reason — because an idle stream worker does not exit on its own (probe-verified, claude 2.1.204)."*
- [ ] **Step 2: Gates** — `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` → all green.
- [ ] **Step 3: Push + PR** — `git push -u origin phase-11-patrol-adoption`; `gh pr create` titled "Phase 11: health patrol and adoption" with: the probe findings summary (P1 resolves the carried PR #14 note; P3 supersedes A4-3 with the pointer landed in the old findings doc), the Decision C/D/I rulings as accepted, the phase-9 coordination note (capture format, settle signature), **an explicit deviation note: adopt's liveness probe is process-table-by-uuid, and the master plan's "transcript mtime" input is subsumed — a fresh re-arm plus the transcript watch supersede a point-in-time mtime read (observation the watch keeps current beats state read once)** (round-1 reviewer note: name this deviation in the PR description), and the exit-criteria evidence table.
- [ ] **Step 4: CI** — `gh pr checks --watch` → five checks green (fmt, clippy, test ×2, gc-compat).
- [ ] **Step 5: Report to the lead** — PR number, CI status, and the master-plan exit criteria quoted line by line with evidence:
  - *"every patrol action is an event with a cause"* → event-shape pins (Tasks 11.5/11.9/11.11) + integration event chains (Task 11.15).
  - *"zero patrol code paths poll (watches + timers only)"* → poll timeout stays `None` when idle (Task 11.13 test); all wakes are pipe/timer/SIGCHLD; grep evidence: no `sleep`/interval outside test harnesses.
  - *"CI green"* → `gh pr checks` output.

## Master-plan test-obligation map

| Master-plan obligation | Where |
|---|---|
| timer arm/reset/fire state machine with FixedClock (transcript touch resets; ledger event resets; threshold fires) | Tasks 11.3 (pure, explicit-now per Decision A), 11.10 (touch + three-key ledger resets), 11.15 step 5 (end-to-end) |
| ladder table (nudge → restart → budget exhausted) incl. the backoff series | Task 11.4 (`ladder_table_…`, `backoff_series_…`) |
| fake agent goes silent → stall → nudge revives it | Task 11.15 step 1 |
| nudge fails → restart re-hooks the bead | Task 11.15 step 2 |
| kill -9 campd mid-run → restart → adopt reconciles exactly (crashed marked, live re-armed, orphan worktree swept) | Task 11.15 step 4 |
| adopt: probe process + transcript mtime; dead → session.crashed; living → re-arm; worktree sweep with bead.worktree.reaped | Task 11.12 (probe is uuid-based per Decision F; transcript mtime subsumed by the fresh re-arm + watch) |
| attended teammates: annotate only, never kill | Tasks 11.10 (`declare_stalls` annotate path), Decision K |
| `[patrol] stall_after` in camp.toml + agent-frontmatter override | Tasks 11.2, 11.7, threaded in 11.10 |

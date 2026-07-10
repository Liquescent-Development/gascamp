# Operator Skill + a Quiet, Awaitable Read Surface — Design

> **Status:** design approved 2026-07-10 (brainstorming). Next step:
> implementation plan (superpowers:writing-plans). This document is the spec;
> it does not touch code. The authoritative v1 spec
> (`docs/design/2026-07-05-gas-camp-design.md`) is amended by the
> implementation PR (§9 below), per AGENTS.md — spec and code never silently
> diverge.

## 1. Problem

Driving a camp from an attended Claude Code session — the "interactive
overseer" of spec §8.4 — has no contract. The plugin ships a **worker** skill
(`plugin/skills/worker/SKILL.md`) that tells a *campd-spawned worker* how to
behave (recall → claim → work → milestones → close → exit). Nothing tells the
*operator's own session* how to drive the camp. So a session doing the most
basic thing — sling one bead, watch it, confirm the result — re-derives
everything from first principles: `which camp`, `camp --help`, re-reading the
command definitions, dumping `camp show` history, `camp events`, `campd.log`,
the `sessions/` dir, then `git ls-tree` / `git show` / a hand-built throwaway
worktree to "verify" a deliverable the camp had *already* verified.

Two distinct failures, from one observed run:

1. **No operator contract.** The session had no mental model, so it
   reconstructed campd's internals and mis-applied global rules (treated a
   `camp/<bead>` deliverable as something that must reach `main` via a PR —
   see §4, that model is wrong for camp v1).
2. **Raw-output thrash.** Every step pasted walls of raw CLI/git output into
   the conversation instead of reading it and reporting a tight summary.

A third contributor was a **stale plugin cache**, already fixed in source: the
run used an old `commands/sling.md` that still told the session to "spawn the
bead's pack agent as a teammate … do NOT fall back to headless+attach." That
second-spawner text was removed in commit `7d95b1d` (dispatch Phase 1); current
source says campd is the sole dispatcher. This design does **not** re-fix that;
it addresses the two durable gaps above.

## 2. Goals / non-goals

**Goals**

- An **operator skill**: the mirror of the worker skill, for the human's
  control-plane session. Machinery, not a role.
- A **quiet read surface** so there is less raw output to thrash in the first
  place: machine-readable `show`, promoted deliverable coordinates, and a
  blocking `--wait` so the operator can sling → await → report an outcome
  without a poll loop.

**Non-goals**

- No change to dispatch, the worker contract, or the number of spawners
  (still one: campd).
- No new event types, no ledger schema change.
- No TUI, no live-refreshing view. `camp top` stays "a query, not a loop"
  (spec §5); `--wait` blocks on a single OS event, it does not tick.
- Backwards compatibility with the old cached `sling.md` is explicitly not a
  concern.

## 3. Design overview

Two deliverables:

1. `plugin/skills/operator/SKILL.md` — behavioral contract (§4).
2. CLI read-surface changes in `crates/camp` — `camp show --json`, promoted
   deliverable coordinates, and `camp show --wait` (§§5–7), plus the spec
   amendment (§9) and tests (§10).

The two reinforce each other: the CLI changes make a concise report *cheap and
reliable* to produce; the skill makes producing one *mandatory* and forbids
pasting raw output.

## 4. The operator skill

`plugin/skills/operator/SKILL.md`. Frontmatter `name: operator`; description
triggers when a session is **driving a camp from its own session — slinging
work, watching the fleet, conversing with workers, checking results** (as
distinct from the worker skill's "you *are* a spawned worker"). Named
`operator`, deliberately not `overseer`: "overseer" denotes a persistent pack
*role* in §8.4, and the skill is machinery, not a role.

Six sections, each aimed at a specific observed failure:

1. **Mental model.** campd is the *sole* dispatcher. `sling` **enqueues** one
   bead; campd immediately spawns a headless-but-present worker (spec §8.4).
   You spawn nothing and you do not reconstruct campd's work from
   `campd.log` / `sessions/`. The local **`camp/<bead>` branch is the
   deliverable** — camp v1 has no remote, PR, or merge step (spec §8.4, §12);
   do not apply a global "code reaches main only via a PR" rule to a camp
   bead. A `shipped` close is **already mechanically verified** by camp
   (branch is real, commit reachable on it, descends from the dispatch-time
   base, is new work); you never re-verify *integration* by hand.
2. **The loop.** sling → (optionally `camp show <bead> --wait`) → read the
   result → report it concisely → `camp nudge` to converse if needed.
3. **Output discipline** (the core fix). Run camp, **read the output
   yourself, report a tight summary in prose.** Never paste raw `camp events`
   tables, full `camp show` history, `campd.log`, `sessions/`, or `git
   ls-tree`/`git show` walls into the conversation. Use `--json` when you need
   to parse a result rather than eyeball it.
4. **Verifying a deliverable.** `shipped` integration is already guaranteed
   (§4.1). If the user asks for *functional* verification (does it build / do
   the tests pass), do it **once, quietly**, and report pass/fail — do not
   paste the build log, and do not hand-build throwaway worktrees unless
   functional verification was actually requested. The commit is reachable in
   the rig via the promoted pointer (§6).
5. **Don't poll.** camp is event-driven; idle is free (invariant #1). To wait
   for a bead to finish use `camp show <bead> --wait` (it sleeps on a ledger
   watch), never a bash poll loop or a `sleep`-and-recheck. Cross-links the
   repo's `subagent-hygiene` skill (waiting on async results without polling).
6. **Verb reference.** One line each: `sling`, `show` (`--wait` / `--json`),
   `top`, `nudge`, `events`, `adopt`.

The plugin's slash commands stay thin wrappers; the skill is what makes a
session's behavior across them coherent. `commands/sling.md` gains a one-line
pointer to the operator skill, mirroring how the worker lifecycle is pointed
to from the plugin README.

## 5. CLI: `camp show --json`

Today `camp show` has **zero** flags and always prints the full history block;
`--json` already exists on `events`, `ls`, `rig ls`, `order ls`, but not on
`show` — the one command an operator reaches for after a sling.

Add `--json` to `show`. It emits a single JSON object: the bead's current
state (the fields `show` already prints) **plus** a `history` array of its
events. This is the operator's silent-read lever (§4.3): parse it, summarize,
do not paste it. Reuses the row + history `show` already loads (DRY).

## 6. Promote deliverable coordinates

`work_branch` / `work_commit` are recorded only inside the `bead.closed`
event's `data` JSON (keys `work_branch`, `work_commit`), never surfaced as
first-class fields — which is exactly why the observed run resorted to git
archaeology. `show` already loads full history, so promotion is a **pure
rendering change** (no schema change, no fold change, DRY): when
`work_outcome == "shipped"`, read the coordinates from the last `bead.closed`
event and print first-class lines plus a copy-paste pointer to see the diff.

```
status   closed   pass / shipped
branch   camp/campdemo-5
commit   b1d59a2   (see: git -C <rig-path> show b1d59a2)
```

`<rig-path>` is resolved from the bead's rig via `CampConfig` (`[[rigs]]
path`). The commit lives on branch `camp/<bead>` in the **rig repo**, not a
worktree — campd reaps the worktree on close (spec §12), so the rig repo is
the durable location. Same coordinates appear in the `--json` object.

## 7. CLI: `camp show --wait`

`camp show <bead> --wait [--json] [--timeout <dur>]` blocks until the bead
reaches a **closed** status, then renders the concise result (§6).

**Mechanism — a ledger file watch, chosen to honor invariant #1 (idle is
free; no polling anywhere):**

1. Open the ledger read-only and read the bead. **If it is already closed,
   render and return** — no watch is armed.
2. Otherwise **arm a `notify` watch on the camp directory first, then
   re-read** the bead (arm-before-check closes the lost-wakeup race where the
   close lands between the first read and the watch arming), then block on the
   watch channel — sleeping on an OS event, the same shape as campd's own
   config/patrol watches (`notify` is already a dependency;
   `crates/camp/src/daemon/patrol.rs` is the precedent).
3. On each filesystem event, re-read the bead via a **fresh** read-only
   connection (WAL-safe: writes land in `camp.db-wal`; a fresh reader sees the
   latest committed state; watching the directory catches `-wal` writes).
   Return once the bead is closed.

**Design properties:**

- **`--wait` is a pure observer.** It writes **no** ledger events and does
  **not** autostart campd. Dispatch is `sling`'s job; observation is `show`'s
  — clean separation (SOLID). `show` does not autostart today, and `--wait`
  keeps that. It therefore works whether campd is up or down: a worker's
  `camp close` writes the terminal event to the ledger directly, and the
  watch observes the ledger, which is ground truth (the observe-reality
  principle, spec §8.5).
- **Never a silent hang.** On arming the wait it prints one line to stderr —
  `waiting for <bead> to close (Ctrl-C to stop)…` — so a block is visible, not
  mistaken for a stall. `--timeout <dur>` bounds it; on expiry it prints the
  current (non-terminal) state and exits **non-zero** (fail fast, never a
  masked wait). Default is unbounded (a deliberate, interruptible foreground
  wait).
- **Terminal = the bead's status becomes `closed`.** `--wait` returns on the
  close it observes. A `fail --transient` close that campd later reopens for a
  retry is a known v1 edge: `--wait` reports the close it saw; documented, not
  silently swallowed. The 90% path (pass / shipped) is exact.

**Spelling decision (settled):** `camp show --wait`, a modifier on the single
bead-read verb, **not** a separate `camp await` verb — DRY, and it composes
with `--json` and `--timeout`.

## 8. Decision record

1. **Skill + CLI, both.** The skill alone fixes behavior; the CLI changes
   remove the raw output the behavior was thrashing. Do both.
2. **`camp show --wait` (modifier), not `camp await` (verb).** One bead-read
   verb; `--wait` composes with `--json`/`--timeout`.
3. **Ledger file watch, not a campd socket long-hold.** The socket protocol
   is strictly one-line-out/one-line-back bounded at `REQUEST_TIMEOUT` (5 s);
   a minutes-long await does not fit it, and holding a socket connection open
   would couple await to campd's liveness. A file watch observes the ledger
   (ground truth), needs no campd, and reuses the existing `notify` pattern.
4. **`--wait` is a pure observer:** no ledger writes, no campd autostart.
5. **Skill named `operator`,** not `overseer` (that word is a pack role in
   §8.4). Machinery, not a role — the mirror of the worker skill.
6. **No new event types, no schema change.** Everything here is additive
   reads; the vocabulary-mirror invariant (#7) is untouched.

## 9. Required amendments to the authoritative spec

Made by the implementation PR (`docs/design/2026-07-05-gas-camp-design.md`),
same PR as the code (AGENTS.md):

- **§13 (nothing-hidden reads):** document the quiet read surface — `show
  --json`, promoted deliverable coordinates, and `show --wait` as a
  watch-driven (non-polling) observer that emits no events.
- **§8.4 (delivery):** note that the deliverable coordinates a worker records
  at `shipped` close are surfaced first-class by `camp show` and awaitable via
  `--wait`.

If any wording in those sections would contradict the implementation, the
implementation stops and the spec is corrected in the same PR — never a silent
divergence.

## 10. Testing strategy (TDD, strict)

Failing test first for each, per AGENTS.md working rules:

- **`show --json` shape** is pinned: a JSON object with the bead's state
  fields and a `history` array; asserted against a known bead, including a
  closed-shipped bead carrying `work_branch` / `work_commit`.
- **Deliverable-coordinate promotion:** a closed-shipped bead renders
  `branch` / `commit` first-class lines and the `git -C <rig-path> show
  <commit>` pointer in the human view; the coordinates also appear in
  `--json`. A non-shipped bead renders neither.
- **`--wait` is event-driven, not polled:** with a bead open, a second thread
  closes it after a delay; `--wait` blocks until the close and returns
  promptly after it (proving watch-driven wakeup, not a fixed-interval
  recheck). An already-closed bead returns immediately with no watch armed.
  `--timeout` expiry exits non-zero with the current state.
- **`--wait` needs no campd:** the close is written directly to the ledger
  (no campd running) and `--wait` still returns.
- Gates green before push: `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets --all-features -- -D warnings`, `cargo test
  --workspace` (AGENTS.md).
- Plugin skill test: mirror `crates/camp/tests/plugin_worker_skill.rs` (which
  exists) for the operator skill — assert presence, `name: operator`
  frontmatter, and the key contract lines (sole-dispatcher model,
  branch-is-the-deliverable, output discipline, don't-poll).

## 11. Invariants respected

- **#1 Idle is free / no polling:** `--wait` sleeps on a `notify` file watch
  (an OS event), never a tick or `sleep`-recheck.
- **#3 Nothing hidden:** all additions are additive reads over the one ledger;
  `--json` widens machine-readability, it hides nothing.
- **#5 Fail fast:** `--timeout` expiry exits non-zero with state; no silenced
  errors, no fallbacks.
- **#7 Vocabulary mirror:** no new event types; `--wait` emits nothing.

## 12. Out of scope / follow-ups

- No `--wait` on `top` or a fleet-wide await (await is per-bead by design).
- No handling of the transient-retry reopen beyond the documented "reports the
  close it observed" (§7); a richer "await final disposition" is a possible
  future item, not v1.
- No change to `camp events` rendering; the operator skill governs whether it
  is ever pasted.

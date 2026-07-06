# Gas Camp — Instructions for Agents

Read `docs/design/2026-07-05-gas-camp-design.md` before changing anything.
It is the approved v1 spec and it is authoritative; its §4 decision record
is settled — do not re-litigate it. If implementation reality contradicts
the spec, stop and update the spec via PR in the same change: spec and code
never silently diverge.

## Invariants — violations are bugs, not trade-offs

1. **Idle is free.** No ticks, no polling loops, anywhere. Components sleep
   on OS events (file watches, armed timers, SIGCHLD, socket accepts).
   Idle campd: < 20 MB RSS, 0.0% CPU.
2. **Cost proportional to job.** The smallest job pays one worker spawn and
   ~3 ledger writes. Graphs, retries, fan-out are opt-in per job.
3. **Nothing hidden.** All durable truth is one SQLite ledger (camp.db) plus
   human-readable TOML and run files. Every campd action is an event with
   its cause. kill -9 anything; the ledger tells the whole story.
4. **Six primitives, zero roles in code.** Agent, Bead, Formula, Rig, Pack,
   Event. If a line of Rust contains a role name or a judgment call, it is
   a bug. campd moves work; it never reasons about it.
5. **Fail fast.** No fallbacks, no silenced errors, no placeholders. No
   panics in library code (clippy unwrap_used/expect_used/panic are denied;
   unsafe_code is forbidden). Every error surfaces to the caller or lands
   in the ledger as an event.
6. **Formula subset invariant.** Every valid camp formula is a valid Gas
   City formula-v2 file. CI validates the corpus against the real gc
   compiler pinned in ci/gc-compat/GASCITY_REF.
7. **Vocabulary mirror.** Event names and outcome metadata match Gas City
   verbatim where the concept exists (pinned in
   crates/camp-core/tests/fixtures/gc-vocab.json); camp-specific names are
   additive, never redefinitions.

## Working rules

- TDD, strictly: write the failing test, run it, watch it fail, implement,
  watch it pass. Run every new or changed test before claiming anything.
- Never commit to main. Every change lands via a PR branch
  (phase-N-<slug> during v1). No co-author lines in commits.
- Gates that must be green before push: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace`.
- Perf suite is LOCAL-ONLY by decision (2026-07-05): `make perf` asserts
  the spec §14 numbers exactly (write < 1 ms, search < 50 ms, idle 0.0%
  CPU, 1M-event volume fixture). Run it before merging perf-relevant PRs.
  `make e2e` (real claude -p) is opt-in and local-only.
- Nothing is complete until it is pushed, CI is green, and every claim in
  the PR description is verified.
- v1 phases may be dispatched by an orchestrating lead session: the
  phase-orchestration skill (.claude/skills/) is the lead's contract;
  docs/superpowers/plans/2026-07-06-v1-orchestration.md holds the
  dependency map, parallel windows, and per-phase kickoff prompts. Phase
  workers follow their kickoff prompt; parallel phases follow the guide's
  shared-file/rebase protocol.

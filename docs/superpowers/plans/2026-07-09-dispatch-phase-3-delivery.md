# Dispatch Phase 3 — Delivery via Pack + the WorkOutcome Axis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **APPROVAL NOTE:** _pending — the plan gate. Record date, verdict, non-blocking
> notes, and reviewer-accepted deviations here in the first execution commit._

**Goal:** Give dispatched work honest delivery semantics: one unified worker contract (single source), a delivery contract shipped as pack/prompt content, and Gas City's `WorkOutcome` axis (`shipped`/`no-op`/`blocked`/`abandoned`) recorded verbatim as a separate, additive axis from the control `outcome` — so `pass` can never again be reported over stranded, un-integrable work (#34).

**Architecture:** `plugin/skills/worker/SKILL.md` becomes the ONE worker-contract source; `spawn.rs` embeds it via `include_str!` (the old `WORKER_CONTRACT` const dies). Delivery *behavior* is pack content (delivery-aware `dev`, new `committer`); delivery *truth* is the `bead.closed` payload's new optional `work_outcome`/`work_commit`/`work_branch` fields, folded into new `beads` columns (schema v2). `shipped` is gated mechanically in `camp close` (CLI, not fold — git facts are not refold-stable): the commit must be reachable on the named branch AND descend from the session's dispatch-time `base`, a mechanical fact campd records in `session.woke`. Resume turns (`camp nudge`, patrol) re-apply the F7 pins recorded at spawn. Blocked/un-shipped work surfaces in `camp ls` (work_outcome on closed beads; a `dispatch_failure` marker on open beads).

**Tech Stack:** Rust (clap, serde, rusqlite via `camp-core::Ledger`), bash test agents, Claude Code plugin/pack markdown, jq/bash CI checks.

**Tracking:** Issue #48; fixes #34. PR branch `phase-3-delivery` → `main`. PR description must include "Fixes #34" and "Closes #48".

## Authoritative inputs (read before executing)

1. `docs/design/2026-07-05-gas-camp-design.md` — the spec (including the merged §8.4 and §12 amendments from Phases 1/2).
2. `docs/design/2026-07-09-dispatch-lifecycle.md` — "Final settled model", §4.3, §9 Phase 3. Q3/Q4/Q5 are RESOLVED; do not reopen.
3. Issue #48 comments — two binding review findings from Phases 1/2 (resume F7 pins; list-level failure surfacing). Both are resolved by design decisions 6 and 7 below.
4. Gas City source at the pinned ref `12410301884b51131a35e101a335dbaae16cdcb0` — verified during planning (see "Gas City ground truth" below), never from memory.

## Global Constraints

- Branch `phase-3-delivery`; never commit to main; no co-author lines in commits.
- TDD strictly: write the failing test, run it, watch it fail, implement, watch it pass.
- Gates before push: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- `make perf` locally before the PR (this plan touches the close/fold path).
- No panics/unwrap/expect in library code; `unsafe` forbidden; fail fast, no fallbacks, no silenced errors.
- Six primitives, zero roles in code; campd moves work, never reasons about it. The landability *judgment* is the worker's close plus mechanical git facts (design §4.3).
- Vocabulary mirror (invariant 7): the WorkOutcome set mirrors gc VERBATIM (values verified against pinned source, not memory); new payload fields use `deny_unknown_fields` structs; keep the one-transaction event+state property, the vocab-pin partition tests, and the refold property green.
- Remote push / PR-host / MR integration is explicitly OUT of scope (Q4). No git-remote code anywhere.
- The camp crate is BIN-ONLY: unit tests run as `cargo test -p camp --bin camp <filter>`; never `-p camp --lib`.
- git ≥ 2.42 auto-orphan fact (Phase 2 execution note 1): baseless-rig refusal is always an explicit `rev-parse --verify HEAD^{commit}` check, never delegated to git failing.
- ssh-agent signing is broken machine-wide: remote git over HTTPS with gh credentials only.

## Gas City ground truth (verified 2026-07-09 against the pinned ref)

Checked in a local gascity checkout via `git cat-file -p 12410301...:<file>` — do not re-derive from memory:

- `internal/beadmeta/values.go`: `WorkOutcomeShipped = "shipped"`, `WorkOutcomeNoOp = "no-op"`, `WorkOutcomeBlocked = "blocked"`, `WorkOutcomeAbandoned = "abandoned"` — values of key `gc.work_outcome` (ADR-0009), "deliberately disjoint from the control-plane Outcome vocabulary".
- `internal/beadmeta/keys.go`: `gc.work_branch`, `gc.work_commit`, `gc.work_dir`, `gc.work_outcome`.
- `cmd/gc/work_record_gate.go`: the close gate — `shipped` requires `gc.work_commit` + `gc.work_branch` and the commit reachable on the branch (`git merge-base --is-ancestor <commit> <branch>`, run from any worktree — "worktrees share one object store"); `no-op`/`blocked`/`abandoned` "carry their reason in the close-reason; no commit artifact is required"; the gate applies to plain task beads only. gc ships it warn-only-by-default; **camp enforces always** (invariant 5 — no warn-only mode, no env toggle).
- `internal/bootstrap/packs/core/formulas/mol-do-work.toml`: the worker itself stamps the work outcome at close (`gc.outcome=pass` stays — "it is the control-plane step result and is disjoint from gc.work_outcome") — the model for camp's pack/prompt content.

## Settled design decisions this plan implements (with rationale)

1. **One worker-contract source = `plugin/skills/worker/SKILL.md` (Q5).** `spawn.rs` embeds it (`include_str!`), strips the YAML frontmatter, substitutes the skill's own `<bead>`/`<name>` placeholders with the concrete bead id and session name, and prepends a two-line mechanical binding preamble (identity + `CAMP_DIR`). The `WORKER_CONTRACT` const is deleted. Enforcement (obligation v): a test recomputes the transform independently from the file and asserts the spawned task prompt equals preamble + transformed skill body — a divergent second copy cannot exist; a stale const would be dead code (clippy `-D warnings` fails).
2. **WorkOutcome rides the `bead.closed` payload, folded into `beads` columns; schema v1 → v2.** New OPTIONAL payload fields `work_outcome`/`work_commit`/`work_branch` (additive: old events refold clean); new `beads` columns with a CHECK mirroring the pinned set; `SCHEMA_VERSION` bumps to 2 (spec: "opening a db with a different schema version is a hard error — no auto-upgrade in v1"; pre-1.0 ledgers re-init, `camp backup`/`camp export` preserve history). The axis is NOT required on every close — it is additive (obligation iv: the control axis is unchanged) — but the unified contract instructs every worker to record one, and the coherence rules below make a dishonest pairing impossible.
3. **Coherence is fold-validated (pure); git facts are CLI-gated (not refold-stable).** Fold rules: `work_outcome` ∈ pinned set; `shipped` requires `work_commit`+`work_branch`, all other cases forbid them (gc: no commit artifact); `shipped`/`no-op` require `outcome=pass`; `blocked`/`abandoned` require `outcome=fail`. This mechanically closes #34's lie: `pass` + `blocked` is rejected in the fold. The git verification (reachable + based) runs in `camp close` BEFORE the append — it can never live in the fold because refold replays events after worktrees/repos are gone (nondeterministic), exactly why gc's gate lives in `cmd/gc`, not its store.
4. **v1 "landed" (Q4) is mechanically two git facts, verified from the rig path:** (a) *reachable* — `git -C <rig> merge-base --is-ancestor <work_commit> <work_branch>` (gc's rule verbatim; worktree branches resolve from the rig, shared object store); (b) *based* — `git -C <rig> merge-base <base> <work_commit>` succeeds, where `base` is the **dispatch-time base** recorded on the claiming session (decision 5). The rig's *current* HEAD is NOT the base reference: a live-tree worker on a baseless rig moves HEAD itself, which would self-certify a dead-end root commit as based (verified hazard; this is why obligation (i) needs the recorded base). Commit/branch values with a leading `-` are rejected outright (gc's flag-injection guard).
5. **campd records `base` in `session.woke`** — `git -C <rig> rev-parse --verify HEAD^{commit}` at prepare time; absent when unresolvable (non-repo/unborn HEAD — for worktree agents dispatch fails anyway; for `isolation="none"` the loud opt-out proceeds with no base, making `shipped` impossible on that dispatch — the #34 scenario ends `blocked`, never `shipped`). `camp session register` records the same base when given `--rig`. No sessions-table schema change: `base` (and the F7 pins) ride the woke event JSON exactly like the existing `worktree` field ("schema v1 is frozen" precedent, `fold.rs`), read back via `json_extract` in `session_rows`.
6. **Resume turns re-apply the pinned F7 config (issue #48 finding 1 — DECIDED: yes).** The pins are recorded AT SPAWN in the woke payload (`model`, `permission_mode`, `allowed_tools` — the exact argv values, tools comma-joined), and both resume sites (`camp nudge` resume, patrol nudge-resume) rebuild argv through one shared `spawn::resume_argv`, appending the recorded pins. Rationale: a session keeps its birth capability envelope — re-resolving the agent at resume time would drift when packs change, and ambient-settings resume (today's behavior) silently widens a pinned worker's tools. Sessions registered without pins (the operator's own attended session) resume bare — a recorded absence (their settings are their own), not a fallback. `--append-system-prompt` is NOT re-applied: the conversation already embodies the role prompt; re-appending duplicates it. The three flags are ordinary claude `-p` flags, valid alongside `--resume`; camp's tests pin the argv mechanically (the stub records it), and `make e2e` (local-only, opt-in) exercises real claude.
7. **List-level surfacing (issue #48 finding 2).** (a) Closed beads show the work axis: `camp ls` status cell renders `closed:blocked` / `closed:shipped` etc.; `BeadRow` gains `work_outcome`. (b) A fail-fast dispatch is no longer invisible at list level: `dispatch.failed` gains a fold effect — `beads.dispatch_failure = reason`, cleared by a later `session.woke`/`bead.claimed` for that bead — rendered as `open:dispatch-failed` (details in `camp show`). The dispatchable query is untouched (the marker is informational; campd's retry semantics are unchanged).
8. **Worktree disposition rule is UNCHANGED (reap on closed-pass), and that satisfies obligation (vi) via coherence:** `blocked`/`abandoned` require `outcome=fail` ⇒ kept-for-forensics by the existing rule (worktree + `camp/<bead>` branch both stand); `shipped` ⇒ `pass` ⇒ reaped, and the branch — the deliverable — survives reaping (`remove_worktree` leaves it standing) and stays reachable/diffable. A `no-op` reap loses nothing (the close asserts no change was needed). Pinned by new tests, no dispose logic change.
9. **Pack content (b): delivery-aware `dev` + a new `committer` agent; NO overseer agent.** The committer mirrors gc's swarm committer role as pack content. The optional Q7 away-mode overseer is SKIPPED in this PR — it is not delivery scope, Q7 is already satisfied by the human session, and it would dilute a review focused on #34. (Scope decision for the plan reviewer to veto.)
10. **Kickoff order honored with one advertised interleave:** (a)=Task 1, (b)=Task 2, (c)=Tasks 3–5, (d)=Tasks 6–8. Task 2's contract text advertises `camp close --work-outcome ...` before Task 5 implements it (design §9 lists b before c); the lockstep is pinned in Task 5 by a test asserting every flag SKILL.md advertises exists in `camp close --help`.

## File map

| File | Action | Why (task) |
|---|---|---|
| `plugin/skills/worker/SKILL.md` | modify | the single contract source; delivery semantics (T1, T2) |
| `crates/camp/src/daemon/spawn.rs` | modify | embed skill; `rig_base`; `resume_argv` (T1, T6, T9) |
| `packs/starter/agents/dev.md` | modify | delivery-aware coder prompt (T2) |
| `packs/starter/agents/committer.md` | create | pack committer role (T2) |
| `packs/starter/README.md` | modify | committer row (T2) |
| `crates/camp/tests/starter_pack.rs` | modify | pack content pins (T2) |
| `crates/camp/tests/plugin_worker_skill.rs` | modify | delivery needles (T2) |
| `crates/camp-core/src/vocab.rs` | modify | `CAMP_WORK_OUTCOMES` (T3) |
| `crates/camp-core/tests/fixtures/gc-vocab.json` | modify | pin `work_outcome` set (T3) |
| `crates/camp-core/tests/vocab_pin.rs` | modify | verbatim-mirror test (T3) |
| `ci/gc-compat/check_vocab.sh` | modify | extract `.work_outcome[]` (T3) |
| `crates/camp-core/src/ledger/schema.rs` | modify | beads columns; SCHEMA_VERSION=2 (T4) |
| `crates/camp-core/src/ledger/fold.rs` | modify | BeadClosed fields+coherence; SessionWoke fields; dispatch_failed fold (T4, T6, T10) |
| `crates/camp-core/src/ledger/refold.rs` | modify | beads cols list (T4) |
| `crates/camp-core/src/ledger/mod.rs` | modify | SessionRow base+pins via json_extract (T6) |
| `crates/camp/src/main.rs` | modify | close flags (T5) |
| `crates/camp/src/cmd/close.rs` | modify | shape checks + shipped git gate (T5, T7) |
| `crates/camp/tests/cli_claim_close.rs` | modify | gate tests, obligation (i) CLI half (T5, T7) |
| `crates/camp/src/cmd/session.rs` | modify | register records base (T6) |
| `crates/camp/src/daemon/dispatch.rs` | modify | Prep.base; woke payload base+pins (T6) |
| `crates/camp/tests/fake-agent.sh` | modify | `FAKE_AGENT_DELIVERY` modes (T8) |
| `crates/camp/tests/daemon_dispatch.rs` | modify | obligations (i)(ii)(vi) daemon tests (T8) |
| `crates/camp/src/cmd/nudge.rs` | modify | resume pins (T9) |
| `crates/camp/src/daemon/patrol.rs` | modify | Tracked pins; resume pins (T9) |
| `crates/camp/tests/cli_nudge.rs` | modify | resume argv pins (T9) |
| `crates/camp-core/src/readiness.rs` | modify | BeadRow work_outcome+dispatch_failure (T10) |
| `crates/camp/src/cmd/ls.rs`, `crates/camp/src/cmd/show.rs` | modify | list/show surfacing (T10) |
| `crates/camp/tests/cli_ls.rs`, `crates/camp/tests/cli_show.rs` | modify | surfacing pins (T10) |
| `crates/camp-core/src/export.rs` | modify | gc.work_* metadata (T11) |
| `crates/camp-core/tests/export_city.rs`, `export_golden.rs` + fixtures | modify | golden updates (T11) |
| `docs/reference/export.md` | modify | mapping-table rows (T11) |
| `docs/design/2026-07-05-gas-camp-design.md` | modify | §8.4 delivery amendment + consistency (T12) |
| `crates/camp/tests/e2e.rs` | modify | drop the Phase-2 temporary live-tree pin (T12) |

## Test-obligation → test mapping (the exit criteria)

| Obligation (design §9 Phase 3) | Test(s) |
|---|---|
| (i) dead-end commit on a no-base rig records `blocked`, never `shipped` | `cli_claim_close::shipped_is_rejected_without_a_dispatch_base_and_blocked_records` (T7); `daemon_dispatch::a_dead_end_worker_on_a_baseless_rig_records_blocked_never_shipped` (T8) |
| (ii) based `camp/<bead>` commit records `shipped`; branch reachable/diffable post-close | `daemon_dispatch::a_worktree_worker_ships_on_the_bead_branch_and_the_branch_outlives_the_reap` (T8) |
| (iii) `check_vocab.sh` green with the added set; export --city emits WorkOutcome | T3 local run at the pinned ref + CI gc-compat job; `export_city`/`export_golden` updates (T11) |
| (iv) control `outcome` axis unchanged | `vocab_pin::control_outcome_axis_is_unchanged` (T3); `cli_claim_close::a_plain_close_payload_is_byte_identical_to_v1` (T5); the whole existing suite |
| (v) unified worker contract is the single source | `spawn::the_task_prompt_is_the_worker_skill_verbatim` (T1); clippy dead-code gate; `plugin_worker_skill.rs` needles (T2) |
| (vi) worktree kept when work is not `shipped` | `daemon_dispatch::a_blocked_close_keeps_the_worktree_and_branch` (T8); fold coherence tests (T4) |

---

### Task 1: (a) Unify the worker contract — SKILL.md is the single source

**Files:**
- Modify: `plugin/skills/worker/SKILL.md` (absorb the mechanical floor's obligations; keep `<bead>`/`<name>` placeholders)
- Modify: `crates/camp/src/daemon/spawn.rs` (delete `WORKER_CONTRACT`; embed the skill)

**Interfaces:**
- Produces: `fn task_prompt(bead_id, session_name) -> String` (same signature, new body) = preamble + transformed skill body; `pub(crate) fn skill_contract_body() -> &'static str` is NOT needed — keep everything private, the test recomputes the transform from the file.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing test** in `spawn.rs`'s `mod tests`:

```rust
/// Obligation (v), dispatch-lifecycle Phase 3 (Q5): ONE worker-contract
/// source. The task prompt every campd worker receives is the worker
/// skill's body verbatim (frontmatter stripped, <bead>/<name> bound),
/// behind a two-line mechanical preamble. The transform is recomputed
/// here independently from the file, so a divergent second copy in Rust
/// cannot survive this assertion.
#[test]
fn the_task_prompt_is_the_worker_skill_verbatim() {
    let skill = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugin/skills/worker/SKILL.md"),
    )
    .unwrap();
    // frontmatter: first line is "---", body starts after the next "---" line
    let mut lines = skill.lines();
    assert_eq!(lines.next(), Some("---"), "skill must open with frontmatter");
    let body: String = lines
        .by_ref()
        .skip_while(|l| *l != "---")
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    let expected = body
        .replace("<bead>", "gc-9")
        .replace("<name>", "t/dev/9");
    let prompt = task_prompt("gc-9", "t/dev/9");
    assert!(
        prompt.ends_with(expected.trim_end()),
        "prompt must end with the transformed skill body;\nprompt tail: {}",
        &prompt[prompt.len().saturating_sub(200)..]
    );
    let preamble = prompt.strip_suffix(expected.trim_end()).unwrap();
    assert!(preamble.contains("gc-9") && preamble.contains("t/dev/9"));
    assert!(preamble.contains("CAMP_DIR"));
    assert!(
        preamble.lines().filter(|l| !l.trim().is_empty()).count() <= 2,
        "the preamble is mechanical binding only, not a second contract: {preamble:?}"
    );
}
```

- [ ] **Step 2: Run it, watch it fail**

Run: `cargo test -p camp --bin camp the_task_prompt_is_the_worker_skill_verbatim`
Expected: FAIL (prompt is the old WORKER_CONTRACT, not the skill body).

- [ ] **Step 3: Implement.** In `spawn.rs`, replace the `WORKER_CONTRACT` const and `task_prompt` with:

```rust
/// The ONE worker-contract source (dispatch-lifecycle Phase 3, Q5): the
/// worker skill shipped by the plugin. Embedded at compile time so the
/// mechanical floor campd injects and the skill a plugin user reads can
/// never drift — obligation (v) pins the equality by test.
const WORKER_SKILL: &str = include_str!("../../../../plugin/skills/worker/SKILL.md");

/// The skill body: frontmatter stripped. The skill uses `<bead>`/`<name>`
/// placeholders as documentation; task_prompt binds them per spawn.
fn skill_body() -> String {
    let mut lines = WORKER_SKILL.lines();
    // A malformed skill (no frontmatter fence) is a build defect, caught
    // by the tests below — fall through to the full text rather than
    // panicking in library code.
    if lines.next() != Some("---") {
        return WORKER_SKILL.to_owned();
    }
    lines
        .skip_while(|l| *l != "---")
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_prompt(bead_id: &str, session_name: &str) -> String {
    let bound = skill_body()
        .replace("<bead>", bead_id)
        .replace("<name>", session_name);
    format!(
        "You are Gas Camp worker session {session_name}, dispatched to work exactly one bead: {bead_id}. \
         CAMP_DIR is already set for the camp CLI; do not start unrelated work.\n\n{}",
        bound.trim_start()
    )
}
```

Then align `plugin/skills/worker/SKILL.md` so nothing from the old floor is lost — the only floor obligation the skill does not already state is the claim-failure stop rule (already there, §2) and exit (§7); verify each old `WORKER_CONTRACT` line maps to a skill section (1→§2, 2→`camp show` — ADD it, 3→§3, 4→§4, 5→§6, 6→§7 + preamble). Add to §2, after the claim block:

```markdown
Then read it: `camp show <bead>` — the title, description, and history are
the task.
```

- [ ] **Step 4: Fix the argv fixture test.** `argv_matches_the_fixture_facts_for_a_fully_pinned_agent` asserts `task.contains("camp claim gc-142 --session dev/dev/1")`, `"camp close gc-142 --outcome"`, `"camp event emit"` — these now come from the substituted skill; verify they still hold (the skill's §2/§4/§6 command blocks carry `<bead>`/`--session <name>`). Also update its `assert_eq!(argv.len(), 15)` if unchanged (it is — same argv, longer task string).

- [ ] **Step 5: Run the spawn and skill suites**

Run: `cargo test -p camp --bin camp spawn && cargo test -p camp --test plugin_worker_skill && cargo test -p camp --test daemon_dispatch`
Expected: PASS (the fake agent ignores the prompt text; daemon tests are prompt-shape-agnostic).

- [ ] **Step 6: Commit**

```bash
git add plugin/skills/worker/SKILL.md crates/camp/src/daemon/spawn.rs
git commit -m "refactor(spawn): the worker skill is the single worker-contract source (Q5, #34)"
```

---

### Task 2: (b) The delivery contract as pack/prompt content

**Files:**
- Modify: `plugin/skills/worker/SKILL.md` (§3 delivery paragraph; §6 both-axes close)
- Modify: `packs/starter/agents/dev.md`
- Create: `packs/starter/agents/committer.md`
- Modify: `packs/starter/README.md` (agents table row)
- Modify: `crates/camp/tests/plugin_worker_skill.rs`, `crates/camp/tests/starter_pack.rs`

**Interfaces:**
- Produces: the contract text that Task 5's `--help` lockstep test pins; flags advertised: `--work-outcome`, `--work-commit`, `--work-branch` (implemented in Tasks 4–5, same PR — decision 10).

- [ ] **Step 1: Write the failing tests.** Append to `plugin_worker_skill.rs`:

```rust
/// Dispatch-lifecycle Phase 3 (#34): the unified contract carries the
/// delivery semantics — commit to the bead branch, record the WorkOutcome
/// axis (gc vocabulary verbatim), no remote in v1.
#[test]
fn worker_skill_carries_the_delivery_contract() {
    let s = worker_skill();
    for needle in [
        "camp/<bead>",
        "--work-outcome",
        "--work-commit",
        "--work-branch",
        "shipped",
        "no-op",
        "blocked",
        "abandoned",
        "never push",
    ] {
        assert!(s.contains(needle), "worker skill must state `{needle}`");
    }
}
```

Append to `starter_pack.rs`:

```rust
#[test]
fn starter_dev_agent_carries_the_delivery_contract() {
    let dev = std::fs::read_to_string(repo_root().join("packs/starter/agents/dev.md")).unwrap();
    for needle in ["camp/", "work outcome", "shipped", "blocked", "never push"] {
        assert!(dev.contains(needle), "dev agent must state `{needle}`");
    }
}

#[test]
fn starter_pack_ships_a_committer_role_and_the_plugin_still_ships_none() {
    let committer =
        std::fs::read_to_string(repo_root().join("packs/starter/agents/committer.md")).unwrap();
    assert!(committer.contains("name: committer"));
    assert!(committer.contains("git"));
    // the role-free-plugin policy is enforced by plugin_policy.rs; this is
    // the positive control that the new role landed in the PACK.
}
```

- [ ] **Step 2: Run them, watch them fail**

Run: `cargo test -p camp --test plugin_worker_skill worker_skill_carries_the_delivery_contract && cargo test -p camp --test starter_pack`
Expected: FAIL (needles absent; committer.md missing).

- [ ] **Step 3: Amend `plugin/skills/worker/SKILL.md`.** In §3 ("work — do the task"), after the existing paragraph, add:

```markdown
**Delivery — work in a git rig ships as a commit, not loose edits.** A
campd-dispatched autonomous worker runs in a camp-managed worktree on the
bead branch `camp/<bead>` (spec §12): commit your finished work to that
branch — the local branch, reachable and diffable, IS the deliverable. Do
not invent branching policy from unrelated global rules, do not create
other branches, and never push: v1 has no remote, PR, or merge step. If you
were dispatched onto the rig's live tree instead (the agent's explicit
`isolation = "none"` opt-out), commit to the branch checked out for you —
the operator supervising that tree owns integration.
```

Replace §6 entirely with:

```markdown
## 6. close — record the outcome on both axes

Close the bead with its **control outcome** — `pass` on success, `fail`
(add `--transient` for a retryable/flaky failure) otherwise — and, whenever
your task was concrete work, the **work outcome**: what became of the work
itself, Gas City's WorkOutcome vocabulary verbatim.

- `shipped` — you committed a change that satisfies the bead. Name the
  commit and its branch; camp verifies mechanically that the commit is
  reachable on that branch and descends from the base you were dispatched
  on. An unverifiable `shipped` is rejected, never recorded.
- `no-op` — the bead needed no change (already satisfied, duplicate). No
  commit is named.
- `blocked` — you could not deliver: the change cannot land (no base, no
  integration path, a missing permission). Close `fail` and say why in
  `--reason`. Anything you committed stays safe: the worktree and the bead
  branch are kept.
- `abandoned` — the work should not proceed (obsolete, superseded). Close
  `fail` with the reason.

`shipped`/`no-op` ride a `pass`; `blocked`/`abandoned` ride a `fail` —
camp rejects incoherent pairings. Attach structured step output with
`--output-json -` when a downstream check needs it.

```
camp close <bead> --outcome pass --reason "<what you did>" \
  --work-outcome shipped \
  --work-commit "$(git rev-parse HEAD)" \
  --work-branch "$(git rev-parse --abbrev-ref HEAD)"

camp close <bead> --outcome pass --reason "<why no change was needed>" --work-outcome no-op
camp close <bead> --outcome fail --reason "<what blocks landing>" --work-outcome blocked
camp close <bead> --outcome fail --reason "<why>" [--transient]
```

Closing is what dispatches dependents (spec §7.3) — do it as your last act.
```

(Note: the `camp close <bead>` occurrences bind to the real bead id via Task 1's substitution — deliberate.)

- [ ] **Step 4: Rewrite `packs/starter/agents/dev.md`** — keep frontmatter and the existing three paragraphs, append:

```markdown
Delivery: if the bead changes files in the rig, your work ships as a commit
on the branch you were dispatched on (`camp/<bead>` in a camp worktree, or
the checked-out branch on an `isolation = "none"` live tree). Commit with a
clear message once the change is verified; never push and never open a PR —
the local bead branch is the deliverable. Close on both axes exactly as the
worker skill describes: record the work outcome — `shipped` with the commit
and branch when you committed, `no-op` when no change was needed, `blocked`
(with `--outcome fail`) when the change cannot land, `abandoned` (fail)
when the work should stop.
```

- [ ] **Step 5: Create `packs/starter/agents/committer.md`:**

```markdown
---
name: committer
description: Owns git for a camp that separates coding from committing — turns verified work in a bead's worktree into a clean commit on the bead branch, mirroring Gas City's swarm committer.
model: sonnet
tools: Read, Bash, Grep, Glob
---
You are the committer for this camp — the only agent in this pack whose job
is version control (Gas City swarm's committer role). You do not write or
rewrite code.

Follow the `worker` skill lifecycle contract. Your bead names work already
done in a worktree on a `camp/<bead>` branch. Claim it, inspect the tree
(`git status`, `git diff`), verify the stated checks were run, then commit
the work to that branch with a clear, factual message — no co-authors, no
tool attributions. Never push, never merge, never touch any other branch:
the local bead branch is the deliverable.

Close on both axes: `--outcome pass --work-outcome shipped` with the commit
and branch you produced; if the tree cannot be committed cleanly (conflicts,
unverified work, a broken base), close `--outcome fail --work-outcome
blocked` with the reason — never force it.
```

- [ ] **Step 6: Add the row to `packs/starter/README.md`'s agents list** (match the file's existing table/list style; one line: committer — owns git; turns verified worktree work into a commit on the bead branch).

- [ ] **Step 7: Run the tests**

Run: `cargo test -p camp --test plugin_worker_skill && cargo test -p camp --test starter_pack && cargo test -p camp --test plugin_policy && cargo test -p camp --bin camp the_task_prompt_is_the_worker_skill_verbatim`
Expected: PASS — including `plugin_policy` (roles stayed out of the plugin) and Task 1's single-source test (the transform tracks the edited file automatically).

- [ ] **Step 8: Commit**

```bash
git add plugin/skills/worker/SKILL.md packs/starter/agents/dev.md packs/starter/agents/committer.md packs/starter/README.md crates/camp/tests/plugin_worker_skill.rs crates/camp/tests/starter_pack.rs
git commit -m "feat(pack): delivery contract as pack/prompt content — bead-branch commits + WorkOutcome close (#34)"
```

---

### Task 3: (c) Pin the WorkOutcome vocabulary — mirrored verbatim from gc

**Files:**
- Modify: `crates/camp-core/src/vocab.rs`
- Modify: `crates/camp-core/tests/fixtures/gc-vocab.json`
- Modify: `crates/camp-core/tests/vocab_pin.rs`
- Modify: `ci/gc-compat/check_vocab.sh`

**Interfaces:**
- Produces: `pub const CAMP_WORK_OUTCOMES: &[&str] = &["shipped", "no-op", "blocked", "abandoned"];` — consumed by Tasks 4, 5.

- [ ] **Step 1: Write the failing tests.** In `vocab_pin.rs`, add `work_outcome: Vec<String>` to the `GcVocab` struct and append:

```rust
/// Q3 (REVISED, SETTLED): camp adopts Gas City's WorkOutcome axis VERBATIM
/// — the full set, exact spelling and order, mirrored (not a subset, not a
/// superset). Values verified against gascity internal/beadmeta/values.go
/// at the pinned ref (gc.work_outcome, ADR-0009).
#[test]
fn work_outcome_axis_mirrors_gc_verbatim() {
    let gc_work: Vec<&str> = gc().work_outcome.iter().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(vocab::CAMP_WORK_OUTCOMES, gc_work.as_slice());
}

/// Obligation (iv): adopting the WorkOutcome axis changes NOTHING on the
/// control axis — the exact v1 sets, pinned.
#[test]
fn control_outcome_axis_is_unchanged() {
    assert_eq!(vocab::CAMP_OUTCOMES, ["pass", "fail", "skipped"]);
    assert_eq!(vocab::CAMP_FINAL_DISPOSITIONS, ["hard_fail", "soft_fail"]);
    assert_eq!(vocab::CAMP_RUN_DISPOSITIONS, ["pass", "hard_fail", "soft_fail"]);
}
```

- [ ] **Step 2: Run, watch it fail**

Run: `cargo test -p camp-core --test vocab_pin`
Expected: FAIL to compile (`work_outcome` field missing from the fixture / `CAMP_WORK_OUTCOMES` undefined).

- [ ] **Step 3: Implement.** In `gc-vocab.json`, after the `"outcome"` line, add (and append `internal/beadmeta/values.go` is already in `_provenance.source_files` — no provenance change):

```json
  "work_outcome": ["shipped", "no-op", "blocked", "abandoned"],
```

In `vocab.rs`, after `CAMP_OUTCOMES`:

```rust
/// Values `bead.closed` accepts for `work_outcome` — Gas City's WorkOutcome
/// axis (`gc.work_outcome`, ADR-0009 at the pinned ref), mirrored VERBATIM
/// as a SEPARATE, additive axis from the control `outcome` (dispatch-
/// lifecycle Q3, REVISED & SETTLED 2026-07-09). Un-integrable work is
/// `blocked` here, never a new control-outcome value. Only `shipped`
/// carries an artifact (a commit on the work branch); the "shipped requires
/// a reachable, based commit" rule is owned by the `camp close` gate, not
/// declared here — exactly gc's division (values.go vs cmd/gc).
pub const CAMP_WORK_OUTCOMES: &[&str] = &["shipped", "no-op", "blocked", "abandoned"];
```

In `check_vocab.sh` line 61, extend the extraction:

```bash
pin_names="$(jq -r '.events[], .outcome[], .work_outcome[], .final_disposition[], .on_exhausted[]' "$pin_file" | sort -u)"
```

- [ ] **Step 4: Run the pin suite, then check_vocab.sh against the REAL pinned source**

Run: `cargo test -p camp-core --test vocab_pin`
Expected: PASS.

Then verify obligation (iii)'s vocab half locally, against the pinned ref (never assume):

```bash
scratch=/private/tmp/claude-501/-Users-kiener-code-gascamp/f32159b8-b147-4d82-9e81-1ebec5b9a66b/scratchpad
git clone --no-checkout /Users/kiener/code/gascity "$scratch/gascity-pin" 2>/dev/null || true
git -C "$scratch/gascity-pin" checkout 12410301884b51131a35e101a335dbaae16cdcb0
ci/gc-compat/check_vocab.sh "$scratch/gascity-pin" "$(pwd)"
```

Expected: `check_vocab: OK — <N> pinned names present in gc source; <M> camp-specific names absent (ref 12410301884b51131a35e101a335dbaae16cdcb0)` with N grown by 4. (If the local gascity clone lacks the pinned ref, `git -C /Users/kiener/code/gascity fetch --all` first; CI re-verifies authoritatively.)

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/vocab.rs crates/camp-core/tests/fixtures/gc-vocab.json crates/camp-core/tests/vocab_pin.rs ci/gc-compat/check_vocab.sh
git commit -m "feat(vocab): pin gc's WorkOutcome axis verbatim (shipped/no-op/blocked/abandoned) (Q3, #34)"
```

---

### Task 4: (c) Fold the WorkOutcome axis — schema v2, coherence rules

**Files:**
- Modify: `crates/camp-core/src/ledger/schema.rs`
- Modify: `crates/camp-core/src/ledger/fold.rs`
- Modify: `crates/camp-core/src/ledger/refold.rs`

**Interfaces:**
- Consumes: `vocab::CAMP_WORK_OUTCOMES` (T3).
- Produces: `bead.closed` payload accepts optional `work_outcome`, `work_commit`, `work_branch` (strings); `beads` columns of the same names; SCHEMA_VERSION = 2. Consumed by Tasks 5, 10, 11.

- [ ] **Step 1: Write the failing fold tests** in `fold.rs`'s tests module (copy the sibling fixture style — build a ledger, append `bead.created`, then assert accept/reject on `bead.closed`):

```rust
/// Dispatch-lifecycle Phase 3 (#34, Q3): the WorkOutcome axis on
/// bead.closed — additive, separate from the control outcome, coherence
/// fold-enforced. Pure shape rules only: the git facts (reachable, based)
/// are gated in `camp close`, never here — refold replays events after
/// worktrees are gone, so a fold that shelled to git would be
/// nondeterministic.
#[test]
fn bead_closed_records_the_work_outcome_axis_with_coherence() {
    let (mut ledger, _dir) = test_ledger(); // adapt to the module's fixture name
    let create = |l: &mut Ledger, id: &str| {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"title": "t"}),
        })
        .unwrap();
    };
    let close = |l: &mut Ledger, id: &str, data: serde_json::Value| {
        l.append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data,
        })
    };
    let cols = |l: &Ledger, id: &str| -> (Option<String>, Option<String>, Option<String>) {
        l.conn // adapt: use whatever raw-SQL access the sibling tests use
            .query_row(
                "SELECT work_outcome, work_commit, work_branch FROM beads WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };

    // ACCEPTED shapes, one bead each — and the columns fold through:
    create(&mut ledger, "gc-1");
    close(&mut ledger, "gc-1", serde_json::json!({
        "outcome": "pass", "work_outcome": "shipped",
        "work_commit": "c0ffee", "work_branch": "camp/gc-1"})).unwrap();
    assert_eq!(cols(&ledger, "gc-1"),
        (Some("shipped".into()), Some("c0ffee".into()), Some("camp/gc-1".into())));
    create(&mut ledger, "gc-2");
    close(&mut ledger, "gc-2",
        serde_json::json!({"outcome": "pass", "work_outcome": "no-op"})).unwrap();
    assert_eq!(cols(&ledger, "gc-2"), (Some("no-op".into()), None, None));
    create(&mut ledger, "gc-3");
    close(&mut ledger, "gc-3",
        serde_json::json!({"outcome": "fail", "work_outcome": "blocked", "reason": "no base"})).unwrap();
    assert_eq!(cols(&ledger, "gc-3"), (Some("blocked".into()), None, None));
    create(&mut ledger, "gc-4");
    close(&mut ledger, "gc-4",
        serde_json::json!({"outcome": "fail", "work_outcome": "abandoned", "reason": "obsolete"})).unwrap();
    create(&mut ledger, "gc-5");
    close(&mut ledger, "gc-5", serde_json::json!({"outcome": "pass"})).unwrap(); // the v1 shape
    assert_eq!(cols(&ledger, "gc-5"), (None, None, None));

    // REJECTED shapes — each on a fresh OPEN bead so the rejection is the
    // payload's, not a double-close:
    let rejected: &[serde_json::Value] = &[
        serde_json::json!({"outcome": "pass", "work_outcome": "blocked"}), // the #34 lie
        serde_json::json!({"outcome": "fail", "work_outcome": "shipped",
                           "work_commit": "c", "work_branch": "b"}),
        serde_json::json!({"outcome": "pass", "work_outcome": "shipped"}),
        serde_json::json!({"outcome": "pass", "work_outcome": "shipped", "work_commit": "c"}),
        serde_json::json!({"outcome": "pass", "work_outcome": "no-op",
                           "work_commit": "c", "work_branch": "b"}),
        serde_json::json!({"outcome": "fail", "work_outcome": "blocked",
                           "work_commit": "c", "work_branch": "b"}),
        serde_json::json!({"outcome": "pass", "work_outcome": "delivered"}), // not pinned
        serde_json::json!({"outcome": "pass", "work_commit": "c"}), // artifact without axis
    ];
    for (i, data) in rejected.iter().enumerate() {
        let id = format!("gc-9{i}");
        create(&mut ledger, &id);
        assert!(
            close(&mut ledger, &id, data.clone()).is_err(),
            "must reject: {data}"
        );
    }
}
```

(Adapt the fixture/raw-SQL access to the module's existing test helpers — if `ledger.conn` is not reachable from tests, use the same query seam the neighboring fold tests use to inspect bead state.)

- [ ] **Step 2: Run, watch it fail**

Run: `cargo test -p camp-core bead_closed_records_the_work_outcome_axis`
Expected: FAIL (deny_unknown_fields rejects the new keys — the accepted cases error).

- [ ] **Step 3: Implement.** `schema.rs`: bump `SCHEMA_VERSION` to 2, the `INSERT INTO meta` literal to `'2'`, and extend the beads DDL after `close_reason`:

```sql
  work_outcome     TEXT CHECK (work_outcome IN ('shipped','no-op','blocked','abandoned')),
  work_commit      TEXT,
  work_branch      TEXT,
  dispatch_failure TEXT,
```

(`dispatch_failure` lands here too so v2 is bumped ONCE; its fold arrives in Task 10.) Update the schema module doc comment: schema v2; opening a v1 db is a hard error (re-init; `camp backup`/`camp export` preserve history).

`refold.rs`: extend the beads `TableSpec.cols` with `, work_outcome, work_commit, work_branch, dispatch_failure`.

`fold.rs` — extend `BeadClosed` and `bead_closed`:

```rust
    /// Phase 3 (#34, Q3): Gas City's WorkOutcome axis, mirrored verbatim —
    /// a SEPARATE additive axis from the control `outcome`.
    #[serde(default)]
    work_outcome: Option<String>,
    #[serde(default)]
    work_commit: Option<String>,
    #[serde(default)]
    work_branch: Option<String>,
```

In `bead_closed`, after the `final_disposition` block:

```rust
    match p.work_outcome.as_deref() {
        None => {
            if p.work_commit.is_some() || p.work_branch.is_some() {
                return Err(bad(
                    "work_commit/work_branch require work_outcome \"shipped\"".to_owned(),
                ));
            }
        }
        Some(wo) => {
            if !crate::vocab::CAMP_WORK_OUTCOMES.contains(&wo) {
                return Err(bad(format!(
                    "work_outcome {wo:?} is not in camp's vocabulary {:?}",
                    crate::vocab::CAMP_WORK_OUTCOMES
                )));
            }
            // Coherence (the #34 gate): shipped/no-op assert success,
            // blocked/abandoned assert the work did NOT land — `pass` over
            // un-integrable work is exactly the lie this rejects.
            let wants_pass = matches!(wo, "shipped" | "no-op");
            if wants_pass && p.outcome != "pass" {
                return Err(bad(format!(
                    "work_outcome {wo:?} requires outcome \"pass\", got {:?}",
                    p.outcome
                )));
            }
            if !wants_pass && p.outcome != "fail" {
                return Err(bad(format!(
                    "work_outcome {wo:?} requires outcome \"fail\", got {:?}",
                    p.outcome
                )));
            }
            // Only shipped carries an artifact (gc values.go, verbatim).
            let has_artifact = p.work_commit.is_some() && p.work_branch.is_some();
            if wo == "shipped" && !has_artifact {
                return Err(bad(
                    "work_outcome \"shipped\" requires work_commit and work_branch".to_owned(),
                ));
            }
            if wo != "shipped" && (p.work_commit.is_some() || p.work_branch.is_some()) {
                return Err(bad(format!(
                    "work_outcome {wo:?} must not carry work_commit/work_branch (only shipped has an artifact)"
                )));
            }
        }
    }
```

And extend the UPDATE:

```rust
            conn.execute(
                "UPDATE beads SET status = 'closed', outcome = ?1, close_reason = ?2,
                                  work_outcome = ?3, work_commit = ?4, work_branch = ?5,
                                  closed_ts = ?6, updated_ts = ?6
                 WHERE id = ?7",
                params![p.outcome, p.reason, p.work_outcome, p.work_commit, p.work_branch, event.ts, id],
            )?;
```

- [ ] **Step 4: Run the full core suite** (refold property, one-transaction property, doctor, perf-adjacent unit tests all touch this path):

Run: `cargo test -p camp-core`
Expected: PASS. The `unsupported_schema_version_is_a_hard_error` test still passes (999 ≠ 2). If any test embeds the literal schema version `1`, update it to `2` — search first: `grep -rn "schema_version" crates/ | grep -v "'2'"`.

- [ ] **Step 5: Run the camp-bin suite** (fold consumers): `cargo test -p camp`
Expected: PASS — existing closes carry no work fields (additive; obligation iv).

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/ledger/schema.rs crates/camp-core/src/ledger/fold.rs crates/camp-core/src/ledger/refold.rs
git commit -m "feat(core): bead.closed records the WorkOutcome axis; schema v2 (Q3, #34)"
```

---

### Task 5: (c) `camp close` grows the work-outcome flags

**Files:**
- Modify: `crates/camp/src/main.rs` (Close variant + match arm)
- Modify: `crates/camp/src/cmd/close.rs` (signature + shape checks; the git gate is Task 7)
- Modify: `crates/camp/tests/cli_claim_close.rs`

**Interfaces:**
- Consumes: fold contract from T4.
- Produces: `cmd::close::run(camp, bead, outcome, reason, transient, output_json, work_outcome: Option<String>, work_commit: Option<String>, work_branch: Option<String>)`; flags `--work-outcome <shipped|no-op|blocked|abandoned>`, `--work-commit <SHA>`, `--work-branch <BRANCH>`. Consumed by T7, T8.

- [ ] **Step 1: Write the failing tests** in `cli_claim_close.rs`:

```rust
/// Phase 3 (#34): the WorkOutcome axis at the CLI. no-op/blocked/abandoned
/// need no git facts — accepted here; shipped is gated (Task 7 tests).
#[test]
fn close_records_a_no_op_work_outcome() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--work-outcome", "no-op",
               "--reason", "already satisfied"])
        .assert()
        .success()
        .stdout(predicates::str::contains("closed gc-1 (pass, no-op)"));
}

#[test]
fn close_rejects_incoherent_axis_pairings_at_the_prompt() {
    let dir = camp_with_bead();
    // the #34 lie: pass over blocked work — rejected before any append
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--work-outcome", "blocked"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("requires --outcome fail"));
    // artifact flags without the axis
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--work-commit", "deadbeef"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--work-outcome shipped"));
    // clap vocabulary: an unknown work outcome is a usage error
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--work-outcome", "delivered"])
        .assert()
        .failure()
        .code(2);
}

/// Obligation (iv): a close WITHOUT the new flags appends a payload with
/// exactly the v1 keys — the control axis and its event shape are
/// unchanged, byte for byte.
#[test]
fn a_plain_close_payload_is_byte_identical_to_v1() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "done"])
        .assert()
        .success();
    let out = camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .output()
        .unwrap();
    let events: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).unwrap();
    let closed = events.iter().find(|e| e["type"] == "bead.closed").unwrap();
    let mut keys: Vec<&str> = closed["data"].as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["outcome", "reason"]);
}

/// Decision 10 lockstep: every close flag the worker skill advertises is a
/// real flag — the contract text and the CLI cannot drift.
#[test]
fn close_help_documents_every_flag_the_worker_skill_advertises() {
    let out = camp().args(["close", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout).to_string();
    let skill = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugin/skills/worker/SKILL.md"),
    )
    .unwrap();
    for flag in ["--work-outcome", "--work-commit", "--work-branch", "--transient", "--output-json"] {
        assert!(skill.contains(flag), "worker skill should advertise {flag}");
        assert!(help.contains(flag), "camp close --help must document {flag}");
    }
}
```

(If `camp events --json` flags differ, copy the accessor an existing test in `cli_events.rs` uses.)

- [ ] **Step 2: Run, watch them fail**

Run: `cargo test -p camp --test cli_claim_close`
Expected: the new tests FAIL (unknown `--work-outcome` argument), existing ones PASS.

- [ ] **Step 3: Implement.** `main.rs` `Close` variant gains:

```rust
        /// Work outcome (gc's WorkOutcome axis, verbatim): what became of
        /// the work itself — separate from the control outcome
        #[arg(long, value_parser = ["shipped", "no-op", "blocked", "abandoned"])]
        work_outcome: Option<String>,
        /// The commit that satisfies the bead (required with --work-outcome shipped)
        #[arg(long, value_name = "SHA")]
        work_commit: Option<String>,
        /// The branch the commit lives on (required with --work-outcome shipped)
        #[arg(long, value_name = "BRANCH")]
        work_branch: Option<String>,
```

and the match arm passes them through. `cmd/close.rs::run` gains the three parameters and, after the `--transient` check:

```rust
    match work_outcome.as_deref() {
        None => {
            if work_commit.is_some() || work_branch.is_some() {
                bail!("--work-commit/--work-branch require --work-outcome shipped");
            }
        }
        Some(wo @ ("shipped" | "no-op")) if outcome != "pass" => {
            bail!("--work-outcome {wo} requires --outcome pass");
        }
        Some(wo @ ("blocked" | "abandoned")) if outcome != "fail" => {
            bail!("--work-outcome {wo} requires --outcome fail (the work did not land)");
        }
        Some("shipped") => {
            let commit = work_commit.as_deref().ok_or_else(|| {
                anyhow!("--work-outcome shipped requires --work-commit (the commit that satisfies the bead)")
            })?;
            let branch = work_branch.as_deref().ok_or_else(|| {
                anyhow!("--work-outcome shipped requires --work-branch (the branch the commit lives on)")
            })?;
            verify_shipped(camp, &bead, commit, branch)?; // Task 7 (stub: Ok(()) in this task)
        }
        Some(_) => {
            if work_commit.is_some() || work_branch.is_some() {
                bail!("only --work-outcome shipped carries --work-commit/--work-branch");
            }
        }
    }
```

Add the fields to the event data only when present (obligation iv):

```rust
    if let Some(wo) = &work_outcome {
        data["work_outcome"] = serde_json::json!(wo);
    }
    if let Some(c) = &work_commit {
        data["work_commit"] = serde_json::json!(c);
    }
    if let Some(b) = &work_branch {
        data["work_branch"] = serde_json::json!(b);
    }
```

and extend the final println: `println!("closed {bead} ({outcome}{})", work_outcome.as_deref().map(|wo| format!(", {wo}")).unwrap_or_default());` — match the exact string the Step-1 test asserts. In this task `verify_shipped` is a stub returning `Ok(())` with a `// Task 7` comment (the shipped CLI test lands there; keep the stub private).

- [ ] **Step 4: Run the suite**

Run: `cargo test -p camp --test cli_claim_close`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/main.rs crates/camp/src/cmd/close.rs crates/camp/tests/cli_claim_close.rs
git commit -m "feat(cli): camp close records the WorkOutcome axis (#34)"
```

---

### Task 6: (d) Record the dispatch-time base + F7 pins in session.woke

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs` (`rig_base`)
- Modify: `crates/camp/src/daemon/dispatch.rs` (`Prep.base`; woke payload)
- Modify: `crates/camp/src/cmd/session.rs` (register records base; `WokeData` fields)
- Modify: `crates/camp-core/src/ledger/fold.rs` (`SessionWoke` optional fields)
- Modify: `crates/camp-core/src/ledger/mod.rs` (`SessionRow` + `session_rows` json_extract)

**Interfaces:**
- Produces: `pub fn rig_base(rig_path: &Path) -> Option<String>` (spawn.rs); woke payload optional fields `base`, `model`, `permission_mode`, `allowed_tools`; `SessionRow` gains `pub base: Option<String>, pub model: Option<String>, pub permission_mode: Option<String>, pub allowed_tools: Option<String>`. Consumed by T7 (base) and T9 (pins).

- [ ] **Step 1: Write the failing tests.** In `spawn.rs` tests:

```rust
/// The dispatch-time base (Phase 3, Q4): the mechanical fact "what commit
/// was this rig on when the work was dispatched" — the reference the
/// shipped gate verifies descent from. None on an unborn HEAD or a
/// non-repo (the same shapes ensure_worktree_base refuses).
#[test]
fn rig_base_resolves_head_and_is_none_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let rig = git_rig(dir.path());
    let base = rig_base(&rig).expect("a committed rig has a base");
    assert_eq!(base.len(), 40, "full sha: {base}");

    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    assert!(rig_base(&bare).is_none(), "not a repo");
    Command::new("git").arg("-C").arg(&bare).args(["init", "-b", "main"]).output().unwrap();
    assert!(rig_base(&bare).is_none(), "unborn HEAD");
}
```

In `ledger/mod.rs` tests, extend `live_sessions_returns_registry_rows_with_their_woke_provenance`'s w1 woke payload with `"base": "aaaa...", "model": "sonnet", "permission_mode": "acceptEdits", "allowed_tools": "Read,Edit,Bash"` and assert the four new `SessionRow` fields round-trip (and are `None` for the minimal a1 row).

- [ ] **Step 2: Run, watch them fail** (`rig_base` undefined; SessionRow fields missing):

Run: `cargo test -p camp --bin camp rig_base_resolves && cargo test -p camp-core live_sessions_returns_registry_rows`

- [ ] **Step 3: Implement.**

`spawn.rs` (next to `ensure_worktree_base`, sharing its rationale comment):

```rust
/// The rig's base commit at this moment — `git rev-parse --verify
/// HEAD^{commit}` — or None when the rig has none (non-repo / unborn
/// HEAD). Recorded in session.woke as the dispatch-time `base`: the
/// mechanical reference the `camp close` shipped gate verifies descent
/// from (using the rig's LATER HEAD would let a live-tree worker on a
/// baseless rig self-certify its own dead-end commit as based).
pub fn rig_base(rig_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(rig_path)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}
```

`dispatch.rs`: `Prep` gains two fields:

```rust
    /// The rig's base commit at dispatch time (None: non-repo/unborn HEAD)
    /// — recorded in session.woke; the shipped gate's descent reference.
    base: Option<String>,
    /// The F7 pins as spawned (model, permission_mode, comma-joined
    /// allowedTools) — recorded in session.woke; re-applied on resume.
    pins: (Option<String>, Option<String>, Option<String>),
```

In `prepare()`, after the rig-dir check and BEFORE `build_spec` consumes `agent`:

```rust
        let base = spawn::rig_base(&rig.path);
        let pins = (
            agent.model.clone(),
            agent.permission_mode.clone(),
            agent.tools.as_ref().map(|t| t.join(",")),
        );
```

and include both in the returned `Prep`. In `launch()`, right after `woke` is built (next to the existing `worktree` enrichment):

```rust
        if let Some(base) = &prep.base {
            woke["base"] = serde_json::json!(base);
        }
        let (model, permission_mode, allowed_tools) = &prep.pins;
        if let Some(m) = model {
            woke["model"] = serde_json::json!(m);
        }
        if let Some(p) = permission_mode {
            woke["permission_mode"] = serde_json::json!(p);
        }
        if let Some(t) = allowed_tools {
            woke["allowed_tools"] = serde_json::json!(t);
        }
```

`fold.rs` `SessionWoke` gains (audit-only, like `worktree`):

```rust
    /// Phase 3: dispatch-time facts, audit-only in the fold — read back via
    /// the woke-JSON join in session_rows (sessions DDL unchanged): the
    /// rig's base commit at dispatch (the shipped gate's reference) and the
    /// F7 pins (re-applied on resume turns).
    #[serde(default)]
    #[allow(dead_code)]
    base: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    model: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    permission_mode: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    allowed_tools: Option<String>,
```

`ledger/mod.rs`: `SessionRow` gains the four `Option<String>` fields; `session_rows`' SELECT gains four more `(SELECT json_extract(e.data, '$.base') ...)` subselects following the `worktree` subselect verbatim (one per field), and the row mapping fills them.

`cmd/session.rs`: `WokeData` gains the optional `base` field (same serde-skip style as its siblings); `register` computes it when `--rig` names a configured rig:

```rust
    let base = match rig.as_deref() {
        Some(r) => {
            let config = camp_core::config::CampConfig::load(&camp.config_path())?;
            crate::daemon::spawn::rig_base(&config.rig(r)?.path)
        }
        None => None,
    };
```

(An unconfigured `--rig` name already errors through `config.rig` — fail fast, keep it.)

- [ ] **Step 4: Run the affected suites**

Run: `cargo test -p camp-core && cargo test -p camp --bin camp && cargo test -p camp --test cli_session && cargo test -p camp --test daemon_dispatch`
Expected: PASS (fields are additive; existing payload tests unaffected).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/spawn.rs crates/camp/src/daemon/dispatch.rs crates/camp/src/cmd/session.rs crates/camp-core/src/ledger/fold.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat(campd): session.woke records the dispatch-time base and F7 pins"
```

---

### Task 7: (d) The shipped gate — mechanical git verification in `camp close`

**Files:**
- Modify: `crates/camp/src/cmd/close.rs` (replace the Task-5 stub)
- Modify: `crates/camp/tests/cli_claim_close.rs`

**Interfaces:**
- Consumes: `SessionRow.base` (T6), `Ledger::{get_bead, session_by_name}`.
- Produces: `verify_shipped` — the obligation-(i)/(ii) gate.

- [ ] **Step 1: Write the failing tests** in `cli_claim_close.rs`. First the shared fixtures (refactor `camp_with_bead` to delegate, rather than duplicating its body):

```rust
/// Run git in `repo` with hermetic identity/signing (a global
/// commit.gpgsign=true must not stall tests — spawn.rs::git_rig precedent).
fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C").arg(repo)
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// camp init + one rig + one bead (gc-1), with the rig prepared by
/// `prepare` (git init / commits) BEFORE any session registers against it.
fn camp_with_bead_in(prepare: impl Fn(&std::path::Path)) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    prepare(&rig_dir);
    camp().current_dir(dir.path())
        .args(["rig", "add"]).arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert().success();
    camp().current_dir(dir.path())
        .args(["create", "do the thing", "--rig", "gascity"])
        .assert().success();
    dir
}

fn based_rig(repo: &std::path::Path) {
    git(repo, &["init", "-b", "main"]);
    git(repo, &["commit", "--allow-empty", "-m", "init"]);
}

fn baseless_rig(repo: &std::path::Path) {
    git(repo, &["init", "-b", "main"]); // unborn HEAD: no commit
}

/// Register + claim gc-1 for `camp/dev/1` against rig `gascity` — the
/// woke's `base` is whatever the rig had at this moment.
fn register_and_claim(dir: &std::path::Path) {
    camp().current_dir(dir)
        .args(["session", "register", "--name", "camp/dev/1", "--agent", "dev",
               "--rig", "gascity",
               "--session-id", "7bd2befc-b018-4080-8738-429d541b3646"])
        .assert().success();
    camp().current_dir(dir)
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert().success();
}

```

(The `work_outcome` assertions below read the `bead.closed` payload via
`camp events --json` — the row-level `ls --json` shape lands in Task 10 and
is pinned there.)

Then the tests:

```rust
/// Obligation (i), CLI half (#34's exact scenario): a dead-end root commit
/// on a baseless rig can NEVER close shipped — there was no dispatch-time
/// base, so nothing can have landed. The honest close is fail+blocked, and
/// that is what the ledger records.
#[test]
fn shipped_is_rejected_without_a_dispatch_base_and_blocked_records() {
    let dir = camp_with_bead_in(baseless_rig);
    register_and_claim(dir.path());
    let rig = dir.path().join("repo");
    // the stray dead-end commit (what #34's worker did)
    git(&rig, &["checkout", "-b", "add-readme"]);
    std::fs::write(rig.join("README.md"), "readme\n").unwrap();
    git(&rig, &["add", "README.md"]);
    git(&rig, &["commit", "-m", "dead-end readme"]);
    let sha = git(&rig, &["rev-parse", "HEAD"]);

    camp().current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--work-outcome", "shipped",
               "--work-commit", &sha, "--work-branch", "add-readme"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no dispatch-time base"));
    // the rejected close appended NOTHING
    let out = camp().current_dir(dir.path()).args(["events", "--json"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("bead.closed"));

    camp().current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "fail", "--work-outcome", "blocked",
               "--reason", "no base; the branch cannot land"])
        .assert().success();
    let events = String::from_utf8_lossy(
        &camp().current_dir(dir.path()).args(["events", "--json"]).output().unwrap().stdout,
    ).into_owned();
    assert!(events.contains(r#""work_outcome":"blocked""#), "{events}");
    assert!(!events.contains(r#""work_outcome":"shipped""#), "never shipped: {events}");
}

/// Obligation (ii), CLI half: on a based rig, a commit that descends from
/// the dispatch-time base and is reachable on its branch closes shipped.
#[test]
fn shipped_verifies_reachable_and_based_then_records() {
    let dir = camp_with_bead_in(based_rig);
    register_and_claim(dir.path());
    let rig = dir.path().join("repo");
    git(&rig, &["checkout", "-b", "camp/gc-1"]);
    std::fs::write(rig.join("work.txt"), "the change\n").unwrap();
    git(&rig, &["add", "work.txt"]);
    git(&rig, &["commit", "-m", "the work"]);
    let sha = git(&rig, &["rev-parse", "HEAD"]);

    camp().current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "done",
               "--work-outcome", "shipped", "--work-commit", &sha,
               "--work-branch", "camp/gc-1"])
        .assert().success()
        .stdout(predicates::str::contains("closed gc-1 (pass, shipped)"));
    let events = String::from_utf8_lossy(
        &camp().current_dir(dir.path()).args(["events", "--json"]).output().unwrap().stdout,
    ).into_owned();
    assert!(events.contains(r#""work_outcome":"shipped""#), "{events}");
    assert!(events.contains(&sha), "{events}");
}

/// The gate is fact-checking, not vibes: a wrong branch, an unbased orphan
/// commit, a flag-shaped value, and an unclaimed bead each fail with a
/// message naming the failed fact — and nothing is appended.
#[test]
fn shipped_rejects_unreachable_unbased_flag_shaped_and_unclaimed_facts() {
    let dir = camp_with_bead_in(based_rig);
    register_and_claim(dir.path());
    let rig = dir.path().join("repo");
    let head = git(&rig, &["rev-parse", "HEAD"]);
    let close_shipped = |commit: &str, branch: &str| {
        camp().current_dir(dir.path())
            .args(["close", "gc-1", "--outcome", "pass", "--work-outcome", "shipped",
                   "--work-commit", commit, "--work-branch", branch])
            .output().unwrap()
    };
    // unreachable: a branch that does not exist
    let out = close_shipped(&head, "no-such-branch");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not reachable on"));
    // unbased: an orphan branch on a based rig shares no history
    git(&rig, &["checkout", "--orphan", "lone"]);
    git(&rig, &["commit", "--allow-empty", "-m", "orphan"]);
    let orphan = git(&rig, &["rev-parse", "HEAD"]);
    git(&rig, &["checkout", "main"]);
    let out = close_shipped(&orphan, "lone");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not descend from"));
    // flag-shaped values are rejected outright (gc's injection guard)
    let out = close_shipped("-x", "main");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("must not begin with '-'"));
    // an unclaimed bead has no session, hence no base to verify against
    camp().current_dir(dir.path())
        .args(["create", "second", "--rig", "gascity"])
        .assert().success();
    let out = camp().current_dir(dir.path())
        .args(["close", "gc-2", "--outcome", "pass", "--work-outcome", "shipped",
               "--work-commit", &head, "--work-branch", "main"])
        .output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no claiming session"));
}
```

(Adjust `camp events --json` / `camp ls --json` accessors to the flags those commands actually take — copy from `cli_events.rs`/`cli_ls.rs`. Refactor the existing `camp_with_bead()` to call `camp_with_bead_in(|_| ())` so there is one fixture body.)

- [ ] **Step 2: Run, watch them fail** (the stub accepts everything):

Run: `cargo test -p camp --test cli_claim_close shipped`
Expected: FAIL on the rejection assertions.

- [ ] **Step 3: Implement `verify_shipped`** in `cmd/close.rs`:

```rust
/// The shipped gate (dispatch-lifecycle Phase 3, Q4 — #34): "landed" in v1
/// is a LOCAL fact, mechanically checkable — the commit is reachable on
/// its branch (gc's work-record gate rule, verbatim) AND descends from the
/// dispatch-time base recorded on the claiming session's woke event. All
/// git runs against the rig path: worktrees share the object store, so
/// bead-branch refs resolve from the rig. gc ships this gate warn-only by
/// default; camp enforces always (invariant 5 — an unverifiable `shipped`
/// is rejected, never recorded).
fn verify_shipped(camp: &CampDir, bead: &str, commit: &str, branch: &str) -> Result<()> {
    for (flag, value) in [("--work-commit", commit), ("--work-branch", branch)] {
        if value.starts_with('-') {
            bail!("{flag} value {value:?} must not begin with '-'");
        }
    }
    let ledger = Ledger::open(&camp.db_path())?;
    let row = ledger
        .get_bead(bead)?
        .ok_or_else(|| anyhow!("no such bead: {bead}"))?;
    let session = row.claimed_by.as_deref().ok_or_else(|| {
        anyhow!(
            "shipped requires a claiming session with a recorded dispatch-time base; \
             bead {bead} has no claiming session — record --work-outcome blocked (or no-op) instead"
        )
    })?;
    let base = ledger
        .session_by_name(session)?
        .and_then(|s| s.base)
        .ok_or_else(|| {
            anyhow!(
                "session {session:?} has no dispatch-time base recorded (the rig had no base \
                 commit when this work was dispatched) — the work cannot have landed; close it \
                 --work-outcome blocked with the reason"
            )
        })?;
    let config = CampConfig::load(&camp.config_path())?;
    let rig_path = &config.rig(&row.rig)?.path;
    drop(ledger); // reopened by the caller for the append
    let git = |args: &[&str]| -> Result<bool> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(args)
            .output()
            .with_context(|| format!("running git {args:?}"))?;
        Ok(out.status.success())
    };
    if !git(&["merge-base", "--is-ancestor", commit, branch])? {
        bail!(
            "work_commit {commit} is not reachable on work_branch {branch:?} in rig {} — \
             shipped must name the commit as it exists on its branch",
            row.rig
        );
    }
    if !git(&["merge-base", &base, commit])? {
        bail!(
            "work_commit {commit} does not descend from the dispatch-time base {base} — \
             the branch has no path to the rig's integration branch; close it \
             --work-outcome blocked instead"
        );
    }
    Ok(())
}
```

(Adjust `close.rs` imports: `anyhow::anyhow`, `camp_core::config::CampConfig`. The caller (`run`) already opens its own ledger afterward — keep the existing structure; the doc comment on `run` gains one line naming the gate.)

- [ ] **Step 4: Run the suite**

Run: `cargo test -p camp --test cli_claim_close`
Expected: PASS, all of it.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/close.rs crates/camp/tests/cli_claim_close.rs
git commit -m "feat(cli): shipped is gated on mechanical git facts — reachable + based (Q4, #34)"
```

---

### Task 8: Daemon end-to-end — obligations (i), (ii), (vi) through the full dispatch path

**Files:**
- Modify: `crates/camp/tests/fake-agent.sh`
- Modify: `crates/camp/tests/daemon_dispatch.rs`

**Interfaces:**
- Consumes: everything above; the fake-agent env contract.
- Produces: `FAKE_AGENT_DELIVERY` = `ship` | `deadend` | `blocked`.

- [ ] **Step 1: Extend `fake-agent.sh`.** Document the new var in the header comment, and insert BEFORE the close block (after the HOLD/NUDGE blocks):

```bash
# Phase 3 delivery modes (dispatch-lifecycle §9 obligations i/ii/vi).
# GITC pins identity/hermeticity for commits made by the fake worker.
GITC=(-c user.email=fake@agent -c user.name=fake-agent -c commit.gpgsign=false)
if [[ "${FAKE_AGENT_DELIVERY:-}" = "ship" ]]; then
  # Obligation (ii): commit on the branch campd dispatched us onto
  # (camp/<bead> in a worktree) and close shipped with the real facts.
  git "${GITC[@]}" commit --allow-empty -m "fake ship for $CAMP_BEAD"
  ship_commit="$(git rev-parse HEAD)"
  ship_branch="$(git rev-parse --abbrev-ref HEAD)"
  "$CAMP_BIN" close "$CAMP_BEAD" --outcome pass --reason "shipped by fake agent" \
    --work-outcome shipped --work-commit "$ship_commit" --work-branch "$ship_branch"
  exit 0
fi
if [[ "${FAKE_AGENT_DELIVERY:-}" = "deadend" ]]; then
  # Obligation (i): the #34 scenario — a root commit on a stray branch of
  # a baseless rig. The shipped close MUST be rejected by the gate; the
  # honest record is fail+blocked. If the gate ever accepts, exit 96 so
  # the test fails loudly (never silence the hole).
  git "${GITC[@]}" checkout -b add-readme
  echo "readme" > README.md
  git "${GITC[@]}" add README.md
  git "${GITC[@]}" commit -m "dead-end readme"
  dead_commit="$(git rev-parse HEAD)"
  if "$CAMP_BIN" close "$CAMP_BEAD" --outcome pass --reason "should be rejected" \
       --work-outcome shipped --work-commit "$dead_commit" --work-branch add-readme; then
    echo "fake-agent: THE SHIPPED GATE ACCEPTED A DEAD-END COMMIT" >&2
    exit 96
  fi
  "$CAMP_BIN" close "$CAMP_BEAD" --outcome fail \
    --reason "no base: the branch cannot land" --work-outcome blocked
  exit 0
fi
if [[ "${FAKE_AGENT_DELIVERY:-}" = "blocked" ]]; then
  # Obligation (vi): committed-but-unlandable work closes blocked; the
  # worktree and bead branch must survive for forensics.
  git "${GITC[@]}" commit --allow-empty -m "half-done work for $CAMP_BEAD"
  "$CAMP_BIN" close "$CAMP_BEAD" --outcome fail \
    --reason "cannot land: blocked by fake scenario" --work-outcome blocked
  exit 0
fi
```

- [ ] **Step 2: Write the three failing tests** in `daemon_dispatch.rs` (reuse its helpers: `scaffold`, `write_agent`, `camp_ok`, `events_json`, `wait_until`, `count`, `Daemon::spawn`; copy the exact idioms of the Phase 2 isolation tests in this file, including the git-rig scaffolding those tests use):

The code below names the harness helpers by their roles — before writing,
read the Phase 2 isolation tests in this file and copy their EXACT helper
signatures (`scaffold`, `write_agent`, `camp_ok`, `events_json`,
`wait_until`, `count`, `Daemon::spawn`, and whatever git-rig fixture they
use); a `git(rig, args)` helper like Task 7's may need adding here too.

```rust
/// Obligation (ii) + the (vi)-complement, dispatch-lifecycle Phase 3
/// (#34, Q4): through the REAL dispatch path — worktree default,
/// camp/<bead> branch — a shipping worker records work_outcome=shipped,
/// the worktree is reaped (clean pass), and the bead branch OUTLIVES the
/// reap: reachable and diffable from the rig. The branch is the
/// deliverable.
#[test]
fn a_worktree_worker_ships_on_the_bead_branch_and_the_branch_outlives_the_reap() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 4, ""); // rig must be a COMMITTED git repo — copy the Phase 2 test's rig-git setup
    write_agent(&root, "dev", ""); // default isolation = worktree (Phase 2)
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT_DELIVERY", "ship")]);

    let bead = camp_ok(&root, &["sling", "ship it", "--agent", "dev"]);
    wait_until(&root, "closed and reaped", |e| {
        count(e, "bead.closed") == 1 && count(e, "bead.worktree.reaped") == 1
    });

    let events = events_json(&root);
    let closed = events.iter().find(|e| e["type"] == "bead.closed").unwrap();
    assert_eq!(closed["data"]["work_outcome"], "shipped");
    assert_eq!(closed["data"]["work_branch"], format!("camp/{bead}"));
    let branch_tip = git(&rig, &["rev-parse", &format!("camp/{bead}")]);
    assert_eq!(closed["data"]["work_commit"], branch_tip.as_str(),
        "the recorded commit IS the bead branch's tip");

    // the worktree is gone (clean pass), the branch is not:
    let wt = root.join("worktrees").join(&bead);
    assert!(!wt.exists(), "reaped worktree must be removed");
    // reachable + diffable FROM THE RIG (shared object store):
    let diff = std::process::Command::new("git")
        .arg("-C").arg(&rig)
        .args(["diff", "--stat", &format!("HEAD...camp/{bead}")])
        .output().unwrap();
    assert!(diff.status.success(),
        "the bead branch must be diffable post-reap: {}",
        String::from_utf8_lossy(&diff.stderr));
}

/// Obligation (i), dispatch-lifecycle Phase 3 (#34): the original defect,
/// end to end — a baseless rig (isolation="none" is the only way a worker
/// reaches one; the loud opt-out), a dead-end root commit — and the ledger
/// records blocked. `shipped` appears NOWHERE; the gate held (a gate hole
/// makes the fake agent exit 96, which would surface as a crashed session
/// before the blocked close ever lands).
#[test]
fn a_dead_end_worker_on_a_baseless_rig_records_blocked_never_shipped() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 4, ""); // then make the rig BASELESS:
    // if scaffold pre-commits the rig, use the baseless variant the Phase 2
    // dispatch-failed test uses (git init -b main, NO commit) instead.
    write_agent(&root, "dev", "isolation: none\n"); // the loud opt-out — copy Phase 2's opt-out agent idiom
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT_DELIVERY", "deadend")]);

    let _bead = camp_ok(&root, &["sling", "give this repo a README", "--agent", "dev"]);
    wait_until(&root, "blocked close", |e| count(e, "bead.closed") == 1);

    let events = events_json(&root);
    let closed = events.iter().find(|e| e["type"] == "bead.closed").unwrap();
    assert_eq!(closed["data"]["outcome"], "fail");
    assert_eq!(closed["data"]["work_outcome"], "blocked");
    let all = serde_json::to_string(&events).unwrap();
    assert!(!all.contains(r#""work_outcome":"shipped""#), "never shipped: {all}");
    assert_eq!(count(&events, "dispatch.live_tree"), 1, "the opt-out was loud");
    let _ = rig; // rig asserted only through the worker's own commits
}

/// Obligation (vi): work that is not shipped loses nothing — a blocked
/// close keeps the worktree (worktree.kept via the existing not-pass rule,
/// which the fold's coherence gate guarantees for blocked/abandoned) AND
/// the camp/<bead> branch with the worker's commit stays reachable.
#[test]
fn a_blocked_close_keeps_the_worktree_and_branch() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 4, ""); // committed git rig, as in the ship test
    write_agent(&root, "dev", "");
    let _daemon = Daemon::spawn(&root, &[("FAKE_AGENT_DELIVERY", "blocked")]);

    let bead = camp_ok(&root, &["sling", "doomed work", "--agent", "dev"]);
    wait_until(&root, "blocked close kept the tree", |e| {
        count(e, "bead.closed") == 1 && count(e, "worktree.kept") == 1
    });

    let events = events_json(&root);
    assert_eq!(count(&events, "bead.worktree.reaped"), 0, "must NOT reap");
    let wt = root.join("worktrees").join(&bead);
    assert!(wt.exists(), "worktree kept for forensics");
    let subject = git(&rig, &["log", "-1", "--format=%s", &format!("camp/{bead}")]);
    assert_eq!(subject, format!("half-done work for {bead}"),
        "the worker's commit survives on the kept branch");
}
```

(Where the plan's helper roles and the file's real helpers differ — e.g.
`scaffold`'s worktree-dir location, whether `wait_until` takes the events or
the root — follow the file; the ASSERTIONS above are the binding content.)

- [ ] **Step 3: Run, watch them fail, then pass**

Run: `cargo test -p camp --test daemon_dispatch -- a_worktree_worker_ships a_dead_end_worker a_blocked_close`
Expected first: FAIL only if earlier tasks left gaps (the mechanics all exist by now — these are the binding end-to-end pins; a failure here is a real bug: stop and debug with the systematic-debugging skill, never adjust the assertion). Then: PASS.

- [ ] **Step 4: Run the full workspace test suite** (the fake agent changed — every daemon test rides it):

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/tests/fake-agent.sh crates/camp/tests/daemon_dispatch.rs
git commit -m "test(daemon): delivery obligations i/ii/vi end to end — blocked never shipped; branch outlives reap (#34)"
```

---

### Task 9: Resume turns re-apply the pinned F7 config (#48 finding 1)

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs` (`resume_argv`)
- Modify: `crates/camp/src/cmd/nudge.rs`
- Modify: `crates/camp/src/daemon/patrol.rs` (Tracked pins; both build sites)
- Modify: `crates/camp/tests/cli_nudge.rs`

**Interfaces:**
- Consumes: `SessionRow.{model, permission_mode, allowed_tools}` (T6).
- Produces: `pub struct ResumePins { pub model: Option<String>, pub permission_mode: Option<String>, pub allowed_tools: Option<String> }` and `pub fn resume_argv(sid: &str, text: &str, pins: &ResumePins) -> Vec<std::ffi::OsString>` in spawn.rs — the ONE resume argv vocabulary.

- [ ] **Step 1: Write the failing tests.** In `spawn.rs` tests:

```rust
/// Issue #48 finding 1 (DECIDED, Phase 3): a resume turn re-applies the F7
/// pins recorded at spawn — a session keeps its birth capability envelope;
/// resuming under ambient user settings would silently widen a pinned
/// worker's tools. Pins absent (the operator's own registered session) =
/// a bare resume: a recorded absence, not a fallback. The role prompt
/// (--append-system-prompt) is NOT re-applied — the conversation already
/// embodies it.
#[test]
fn resume_argv_reapplies_recorded_pins_and_only_those() {
    let pins = ResumePins {
        model: Some("sonnet".into()),
        permission_mode: Some("acceptEdits".into()),
        allowed_tools: Some("Read,Edit,Bash".into()),
    };
    let argv: Vec<String> = resume_argv("sid-1", "status?", &pins)
        .iter().map(|s| s.to_string_lossy().into_owned()).collect();
    assert_eq!(
        argv,
        vec![
            "-p", "--resume", "sid-1", "status?", "--output-format", "json",
            "--model", "sonnet", "--permission-mode", "acceptEdits",
            "--allowedTools", "Read,Edit,Bash",
        ]
    );
    let bare: Vec<String> = resume_argv("sid-1", "status?", &ResumePins::default())
        .iter().map(|s| s.to_string_lossy().into_owned()).collect();
    assert_eq!(bare, vec!["-p", "--resume", "sid-1", "status?", "--output-format", "json"]);
}
```

In `cli_nudge.rs`, extend `nudge_resumes_an_exited_worker_and_prints_the_reply`: the scaffolded `dev` agent must be written WITH pins (model/permission-mode/tools — copy `write_agent`'s extra-frontmatter parameter idiom), and after the existing `--resume <sid>` assertion add:

```rust
    assert!(logged.contains("--permission-mode"), "resume must re-apply the recorded pins: {logged}");
    assert!(logged.contains("--allowedTools"), "log: {logged}");
```

And pin the attended half in `nudge_reaches_a_live_attended_session_via_resume`:

```rust
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        !logged.contains("--allowedTools") && !logged.contains("--permission-mode"),
        "a session registered without pins resumes bare (its settings are its own): {logged}"
    );
```

- [ ] **Step 2: Run, watch them fail**

Run: `cargo test -p camp --bin camp resume_argv && cargo test -p camp --test cli_nudge`
Expected: FAIL (`ResumePins`/`resume_argv` undefined; stub log lacks pin flags).

- [ ] **Step 3: Implement.** `spawn.rs`:

```rust
/// F7 pins as recorded on the session's woke event — the values the resume
/// paths re-apply (issue #48 finding 1, resolved in dispatch-lifecycle
/// Phase 3; the decision record is the plan doc + spec §8.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumePins {
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<String>,
}

/// The ONE resume argv vocabulary (`camp nudge` resume + patrol
/// nudge-resume): `-p --resume <sid> <text> --output-format json` plus the
/// recorded F7 pins. NOT --append-system-prompt: the conversation already
/// embodies the role prompt.
pub fn resume_argv(sid: &str, text: &str, pins: &ResumePins) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["-p", "--resume", sid, text, "--output-format", "json"]
        .iter()
        .map(OsString::from)
        .collect();
    let mut push = |flag: &str, value: &Option<String>| {
        if let Some(v) = value {
            argv.push(OsString::from(flag));
            argv.push(OsString::from(v));
        }
    };
    push("--model", &pins.model);
    push("--permission-mode", &pins.permission_mode);
    push("--allowedTools", &pins.allowed_tools);
    argv
}
```

`cmd/nudge.rs::resume`: replace the six `.arg(...)` calls with:

```rust
    let pins = spawn_pins(row);
    let out = std::process::Command::new(&config.dispatch.command)
        .args(crate::daemon::spawn::resume_argv(sid, text, &pins))
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {} --resume", config.dispatch.command.display()))?;
```

with a tiny mapper in nudge.rs (patrol maps from its own `Tracked` fields with the same three lines):

```rust
fn spawn_pins(row: &SessionRow) -> crate::daemon::spawn::ResumePins {
    crate::daemon::spawn::ResumePins {
        model: row.model.clone(),
        permission_mode: row.permission_mode.clone(),
        allowed_tools: row.allowed_tools.clone(),
    }
}
```

`patrol.rs`: `Tracked` gains `model: Option<String>, permission_mode: Option<String>, allowed_tools: Option<String>`; populate at BOTH build sites — the woke-JSON site (~line 340: `data["model"].as_str().map(str::to_owned)` etc.) and the SessionRow/adopt site (~line 378: `row.model.clone()` etc.). The resume `Command` build replaces its six `.arg(...)` calls with `.args(crate::daemon::spawn::resume_argv(sid, &text, &pins))` where `pins` maps from `tracked`. The existing patrol test `a_child_nudge_goes_over_stdin_and_a_pipeless_one_resumes` keeps passing (its recording stub captures argv; its `--resume` assertion is unchanged) — extend it with one line asserting the recorded args for a pinned agent include `--permission-mode` if its fixture agent declares one; if the fixture agent has no pins, assert the args do NOT contain `--allowedTools` (the bare shape).

- [ ] **Step 4: Run the suites**

Run: `cargo test -p camp --bin camp && cargo test -p camp --test cli_nudge && cargo test -p camp --test daemon_patrol`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/spawn.rs crates/camp/src/cmd/nudge.rs crates/camp/src/daemon/patrol.rs crates/camp/tests/cli_nudge.rs
git commit -m "fix(resume): nudge/patrol resume turns re-apply the session's recorded F7 pins (#48)"
```

---

### Task 10: List-level surfacing — WorkOutcome + dispatch-failure marker (#48 finding 2)

**Files:**
- Modify: `crates/camp-core/src/ledger/fold.rs` (dispatch.failed fold; clears)
- Modify: `crates/camp-core/src/readiness.rs` (BeadRow + SELECTs)
- Modify: `crates/camp/src/cmd/ls.rs`, `crates/camp/src/cmd/show.rs`
- Modify: `crates/camp/tests/cli_ls.rs`, `crates/camp/tests/cli_show.rs`

**Interfaces:**
- Consumes: T4's beads columns (incl. `dispatch_failure`).
- Produces: `BeadRow` gains `pub work_outcome: Option<String>, pub dispatch_failure: Option<String>`; `camp ls` status cell renders `closed:blocked` / `open:dispatch-failed`; `camp show` prints the work axis and the failure reason.

- [ ] **Step 1: Write the failing fold tests** in `fold.rs` tests:

```rust
/// Issue #48 finding 2: a fail-fast dispatch is a bead-level fact the list
/// can show — dispatch.failed folds into beads.dispatch_failure (the
/// reason), cleared by a later session.woke or claim for that bead. The
/// dispatchable query is untouched: the marker informs, it never gates.
#[test]
fn dispatch_failed_marks_the_bead_and_dispatch_or_claim_clears_it() {
    // bead.created gc-1; append dispatch.failed {reason:"rig ... cannot host a worktree"}
    //   -> beads.dispatch_failure == that reason
    // append session.woke with bead gc-1 -> dispatch_failure IS NULL again
    // (second bead) dispatch.failed then bead.claimed -> cleared too
    // dispatch.failed on an unknown bead -> Err (fail fast, like milestones)
    // dispatch.failed with an extra payload key -> Err (deny_unknown_fields)
}
```

Write it out fully in the module's idiom.

- [ ] **Step 2: Run, watch it fail** (`dispatch.failed` currently has no fold arm — the column stays NULL):

Run: `cargo test -p camp-core dispatch_failed_marks_the_bead`

- [ ] **Step 3: Implement in `fold.rs`.** Add the match arm `EventType::DispatchFailed => dispatch_failed(conn, event),` plus:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchFailed {
    reason: String,
}

/// dispatch.failed (Phase 3, #48 finding 2): fold the fail-fast reason
/// onto the bead so `camp ls` can mark work that looks ready but will not
/// dispatch (e.g. a baseless rig). Cleared by a later session.woke/claim.
fn dispatch_failed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let id = required_bead(event)?;
    let p: DispatchFailed = payload(event)?;
    if bead_status(conn, id)?.is_none() {
        return Err(CoreError::UnknownBead(id.to_owned()));
    }
    conn.execute(
        "UPDATE beads SET dispatch_failure = ?1, updated_ts = ?2 WHERE id = ?3",
        params![p.reason, event.ts, id],
    )?;
    Ok(())
}
```

In `session_woke`, after the sessions upsert, add (the woke's bead means a dispatch succeeded):

```rust
    if let Some(bead) = &p.bead {
        conn.execute(
            "UPDATE beads SET dispatch_failure = NULL WHERE id = ?1 AND dispatch_failure IS NOT NULL",
            [bead],
        )?;
    }
```

and the same two-liner in `bead_claimed` (using the claimed bead id). Note: this makes `SessionWoke.bead` no longer `#[allow(dead_code)]` if it was.

- [ ] **Step 4: Surface it.** `readiness.rs`: add the two fields to `BeadRow`, extend every SELECT that builds it (`ready_beads`, `list_beads`, `get_bead`, the `mine` query — grep for the column list) with `work_outcome, dispatch_failure`. `ls.rs` render:

```rust
        for b in &beads {
            let status = match (&b.work_outcome, &b.dispatch_failure) {
                (Some(wo), _) => format!("{}:{}", b.status, wo),
                (None, Some(_)) if b.status != "closed" => format!("{}:dispatch-failed", b.status),
                _ => b.status.clone(),
            };
            println!("{}\t{}\t{}\t{}", b.id, status, b.rig, b.title);
        }
```

`show.rs`: after the `outcome` line add:

```rust
    if let Some(wo) = &row.work_outcome {
        println!("work     {wo}");
    }
    if let Some(df) = &row.dispatch_failure {
        println!("dispatch-failed  {df}");
    }
```

(`work_commit`/`work_branch` appear in the bead's close event history `camp show` already prints — no extra lines needed; if `show` prints event data, verify one manual run.)

- [ ] **Step 5: Write the failing CLI tests.** In `cli_ls.rs` (follow its scaffold idiom):

```rust
/// #48 finding 2 + the obligation surface: blocked/un-shipped work is
/// visible at the LIST level — closed beads show their work outcome; an
/// open bead whose dispatch failed fast shows the marker instead of a
/// clean `open`. The dispatch.failed fixture is appended straight through
/// camp-core's Ledger (the camp crate depends on camp-core; campd is the
/// only real writer of this event and needs a baseless rig to produce it).
#[test]
fn ls_surfaces_work_outcomes_and_dispatch_failures() {
    let dir = /* this file's scaffold: camp init + rig + three beads gc-1..3 */;
    // gc-1: blocked
    camp().current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "fail", "--work-outcome", "blocked",
               "--reason", "cannot land"])
        .assert().success();
    // gc-2: the v1 shape — unchanged rendering
    camp().current_dir(dir.path())
        .args(["close", "gc-2", "--outcome", "pass"])
        .assert().success();
    // gc-3: a fail-fast dispatch record
    {
        let mut ledger =
            camp_core::ledger::Ledger::open(&dir.path().join(".camp/camp.db")).unwrap();
        // adapt the db path to this file's scaffold layout (CAMP_DIR root)
        ledger.append(camp_core::event::EventInput {
            kind: camp_core::event::EventType::DispatchFailed,
            rig: Some("gascity".into()),
            actor: "campd".into(),
            bead: Some("gc-3".into()),
            data: serde_json::json!({"reason": "rig cannot host a worktree (no base commit)"}),
        }).unwrap();
    }
    let out = camp().current_dir(dir.path()).args(["ls"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("gc-1\tclosed:blocked\t"), "{text}");
    assert!(text.contains("gc-2\tclosed\t"), "{text}");
    assert!(text.contains("gc-3\topen:dispatch-failed\t"), "{text}");

    let out = camp().current_dir(dir.path()).args(["ls", "--json"]).output().unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    let row = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(row("gc-1")["work_outcome"], "blocked");
    assert!(row("gc-3")["dispatch_failure"].as_str().unwrap().contains("worktree"));
}
```

(Adapt the scaffold and db path to this file's existing helpers; if the
camp dev-dependencies do not already expose `camp_core` to integration
tests, it is a direct dependency of the crate and available as
`camp_core::` — verify with an existing test import before assuming.)

In `cli_show.rs`: close a bead `fail`+`blocked` (same argv as above) and assert `camp show <bead>` stdout contains a line `work     blocked`.

- [ ] **Step 6: Run everything touched**

Run: `cargo test -p camp-core && cargo test -p camp --test cli_ls && cargo test -p camp --test cli_show && cargo test -p camp --test cli_lifecycle`
Expected: PASS (if any existing ls/top/statusline test pins the old 4-column line for a bead that now renders identically, nothing changes — only beads WITH an axis or marker render differently; fix any that seeded those states deliberately).

- [ ] **Step 7: Commit**

```bash
git add crates/camp-core/src/ledger/fold.rs crates/camp-core/src/readiness.rs crates/camp/src/cmd/ls.rs crates/camp/src/cmd/show.rs crates/camp/tests/cli_ls.rs crates/camp/tests/cli_show.rs
git commit -m "feat(ls): surface WorkOutcome and fail-fast dispatch at the list level (#48)"
```

---

### Task 11: `camp export --city` carries the WorkOutcome axis (obligation iii)

**Files:**
- Modify: `crates/camp-core/src/export.rs`
- Modify: `crates/camp-core/tests/export_city.rs`, `crates/camp-core/tests/export_golden.rs` + golden fixture files under `crates/camp-core/tests/fixtures/` (locate with `grep -rn "gc.outcome" crates/camp-core/tests/`)
- Modify: `docs/reference/export.md`

**Interfaces:**
- Consumes: T4's beads columns.
- Produces: `ExportBead` gains `work_outcome`, `work_commit`, `work_branch`; `beads.jsonl` metadata gains `gc.work_outcome`/`gc.work_commit`/`gc.work_branch` (gc's exact key spellings, `internal/beadmeta/keys.go`).

- [ ] **Step 1: Write the failing unit test** in `export.rs` tests (extend `seed` so gc-1 closes shipped):

In `seed`, change gc-1's close payload to

```rust
            serde_json::json!({"outcome": "pass", "reason": "shipped the widget",
                "work_outcome": "shipped",
                "work_commit": "1111111111111111111111111111111111111111",
                "work_branch": "camp/gc-1"}),
```

and add:

```rust
    /// Obligation (iii), Phase 3: the export is city-NATIVE for the work
    /// axis — gc's own metadata keys, verbatim (beadmeta/keys.go at the
    /// pinned ref): gc.work_outcome / gc.work_commit / gc.work_branch.
    #[test]
    fn work_outcome_axis_exports_as_gc_native_metadata() {
        let mut bead = full_bead();
        bead.work_outcome = Some("shipped".into());
        bead.work_commit = Some("1111111111111111111111111111111111111111".into());
        bead.work_branch = Some("camp/gc-1".into());
        let line = jsonl_line(&bd_record(&bead).unwrap()).unwrap();
        for needle in [
            r#""gc.work_outcome":"shipped""#,
            r#""gc.work_commit":"1111111111111111111111111111111111111111""#,
            r#""gc.work_branch":"camp/gc-1""#,
        ] {
            assert!(line.contains(needle), "{line}");
        }
        // a bead without the axis emits NONE of the keys (additive)
        let plain = jsonl_line(&bd_record(&minimal_bead()).unwrap()).unwrap();
        assert!(!plain.contains("gc.work_"), "{plain}");
    }
```

(`full_bead()`/`minimal_bead()` gain the three new `ExportBead` fields — `Some(...)`/`None`.)

- [ ] **Step 2: Run, watch it fail to compile** (fields missing):

Run: `cargo test -p camp-core export`

- [ ] **Step 3: Implement.** `ExportBead` gains the three `Option<String>` fields; `export_beads`'s SELECT adds `work_outcome, work_commit, work_branch` (keep column-index literals consistent — they shift; adjust the `labels_json: row.get(10)?` index comment if positions move, or append the new columns LAST to keep existing indices stable — do that: append last, `row.get(16/17/18)?`). `bd_record`, after the `gc.outcome` insert:

```rust
    if let Some(wo) = &bead.work_outcome {
        metadata.insert("gc.work_outcome".into(), wo.clone().into());
    }
    if let Some(c) = &bead.work_commit {
        metadata.insert("gc.work_commit".into(), c.clone().into());
    }
    if let Some(b) = &bead.work_branch {
        metadata.insert("gc.work_branch".into(), b.clone().into());
    }
```

(BTreeMap keeps alphabetical key order — `gc.outcome` < `gc.work_branch` < `gc.work_commit` < `gc.work_outcome`; the goldens encode this.)

- [ ] **Step 4: Update the goldens and integration tests.** Run `cargo test -p camp-core --test export_golden --test export_city` — the seed change makes them fail with exact expected-vs-actual lines; update the golden fixture lines/assertions to the NEW correct output (each updated line must show the three `gc.work_*` keys in alphabetical position). Update `closed_task_maps_to_a_bd_issue_line_exactly` similarly if `full_bead()` gained Some-values (it did — its expected string gains the three keys). Update the two `docs/reference/export.md` mapping-table rows (beads → metadata): add `work_outcome → gc.work_outcome`, `work_commit → gc.work_commit`, `work_branch → gc.work_branch`.

- [ ] **Step 5: Run the export suites + the CLI export test**

Run: `cargo test -p camp-core export && cargo test -p camp --test cli_export`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/export.rs crates/camp-core/tests/ docs/reference/export.md
git commit -m "feat(export): city export carries the WorkOutcome axis as gc-native metadata (obligation iii)"
```

---

### Task 12: Spec §8.4 delivery amendment + consistency edits + e2e un-pin

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md` (§8.4 worker-lifecycle-contract paragraph; §12 one line; §15.2/vocabulary section one line; §7.1 schema-version line if it names v1 — grep first)
- Modify: `crates/camp/tests/e2e.rs`

- [ ] **Step 1: Amend spec §8.4.** Replace the closing **Worker lifecycle contract** paragraph with:

```markdown
**Worker lifecycle contract** (the worker skill, shipped by the camp
plugin — the ONE contract source; campd's spawn prompt embeds the same
file): claim → work → **deliver** → emit milestones (`camp event emit`) →
close on both axes → exit. *Deliver:* work that changes a rig ships as a
commit on the branch the worker was dispatched onto — `camp/<bead>` in a
camp worktree (§12); the local bead branch, reachable and diffable, is the
deliverable. v1 has no remote push, PR/MR, or merge step. *Close on both
axes:* the control `outcome` (`pass`/`fail`/`skipped`) plus, for concrete
work, the **WorkOutcome axis** — `shipped`/`no-op`/`blocked`/`abandoned`,
Gas City's `gc.work_outcome` vocabulary mirrored verbatim (§15.2) as a
separate, additive axis. `shipped` is mechanically gated by `camp close`:
the named commit must be reachable on the named branch and descend from
the session's dispatch-time base (recorded in `session.woke`); an
unverifiable `shipped` is rejected, never recorded. Un-integrable work
closes `fail` + `blocked` — its worktree and bead branch are kept, so
nothing is lost. Workers run under the permission mode and tool allowlist
their agent definition declares — **including resume turns**: `camp nudge`
and patrol resume re-apply the model/permission-mode/allowedTools pins
recorded at spawn (a session keeps its birth capability envelope; sessions
registered without pins — the operator's own — resume under their own
settings). `campd`-spawned workers run non-interactively: anything the
agent definition has not pre-allowed fails fast (and lands in the ledger)
rather than hanging on a prompt no one will answer.
```

- [ ] **Step 2: Consistency edits.**
- §12, the worktree-contract line: extend "autonomous work happens on `camp/<bead>`, reaped on clean pass, kept on failure" with "; the bead branch is the deliverable and outlives the reap (§8.4 delivery)".
- The vocabulary-mirror section (§15.2 or wherever `outcome` subsets are enumerated — grep `"skipped"` in the spec): add one line: "`work_outcome` mirrors gc's WorkOutcome set verbatim: `shipped`/`no-op`/`blocked`/`abandoned` (pinned in gc-vocab.json, CI-checked)."
- §7.1 (ledger schema): if it pins "schema v1"/version 1, amend to v2 with one clause: "(v2 adds the WorkOutcome/delivery columns; opening an older db is a hard error — no auto-upgrade)". If it names no version, add nothing.

- [ ] **Step 3: Un-pin e2e.** In `crates/camp/tests/e2e.rs` (~lines 335–351): delete the two `isolation: none\n` lines and the "Until Phase 3 defines 'landed'…" comment; replace the comment with: `// Phase 3 defined "landed" (worker-contract delivery, spec §8.4): e2e\n// runs the DEFAULT worktree isolation — the shipped path a real worker sees.` Adjust any e2e assertion that expected rig-tree artifacts to accept the worktree/bead-branch location (`git -C rig rev-parse camp/<bead>` style — mirror Task 8's assertions). Real-claude closes may or may not record a work outcome (the contract instructs it; the model decides): assert the bead CLOSES and, IF `work_outcome` is present and `shipped`, that the branch resolves — never assert the model's judgment.

- [ ] **Step 4: Run e2e locally if a real `claude` is available** (`make e2e`; it is opt-in and LOCAL-ONLY by decision — CI never runs it). If the environment has no authenticated claude, run `cargo test -p camp --test e2e` (which must skip/pass without it, as today) and state exactly that in the PR: e2e updated per the Phase-2 execution note, not exercised against real claude in this environment.

- [ ] **Step 5: Run the doc-adjacent suites** (`plugin_parity` greps spec-referencing docs? No — just the full gate): `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add docs/design/2026-07-05-gas-camp-design.md crates/camp/tests/e2e.rs
git commit -m "docs(spec): §8.4 delivery amendment — bead-branch delivery, WorkOutcome axis, resume pins (#34)"
```

---

### Task 13: Gates, perf, PR, CI to terminal green

- [ ] **Step 1: The three gates**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all green. Fix anything; re-run until clean.

- [ ] **Step 2: Perf (the close/fold path changed — kickoff-required)**

Run: `make perf`
Expected: the spec §14 assertions hold (write < 1 ms, search < 50 ms, idle 0.0% CPU, 1M-event volume). A perf regression here is a blocker — investigate (the fold added one UPDATE-set clause and columns; the volume fixture must still build v2 schema cleanly).

- [ ] **Step 3: Re-run the local check_vocab** (Task 3 Step 4 command) — proof for obligation (iii) in the PR body.

- [ ] **Step 4: Record the approval note** at the top of this plan doc (date, verdict, non-blocking notes, reviewer-accepted deviations) if not already done in the first execution commit; append an "Execution notes" section documenting any deviations found while executing (Phase 1/2 precedent).

- [ ] **Step 5: Push and open the PR** (HTTPS remote; gh credentials):

```bash
git push -u origin phase-3-delivery
gh pr create --title "Dispatch Phase 3: delivery contract + WorkOutcome axis" --body "$(cat <<'EOF'
Fixes #34
Closes #48

<summary per the exit criteria: each of obligations (i)-(vi) with its test name and result; the spec §8.4 amendment; the two #48 findings' resolutions (resume pins; ls surfacing); schema v2 note; check_vocab + perf evidence>
EOF
)"
```

- [ ] **Step 6: Watch CI to a TERMINAL result** — stay in-turn and poll:

```bash
gh pr checks --watch
```

If the harness demotes it to background, poll `gh pr checks` with cheap foreground calls in a loop until every check is terminal. Green = done; red = fix, push, re-watch. Never end the turn with CI outstanding.

---

## Known risks / notes for the plan reviewer

1. **Schema v2 (decision 2)** hard-errors existing camp.db files at open ("no auto-upgrade in v1" is the spec's own rule). Pre-1.0, single-operator; `camp backup`/`camp export` preserve history. Flagging for explicit reviewer sign-off since it touches every existing camp.
2. **No overseer agent (decision 9)** — the kickoff's "optionally" is exercised as skip, with rationale. Veto = one added pack file, no code.
3. **`--model/--permission-mode/--allowedTools` alongside `--resume`** (decision 6): standard `-p` flags; mechanically pinned by stub-argv tests; real-claude behavior is `make e2e` territory (local-only by decision). If the operator wants a live probe before merge, it is one `claude -p --resume <sid> --allowedTools Read "hi"` against any session.
4. **Coherence rule strictness** (blocked/abandoned ⇒ `fail` exactly): campd's finalization `skipped` closes never carry a work outcome, so `skipped` needs no pairing rule. If a future control flow wants `skipped`+axis, that is a one-line fold change with a vocab test.
5. **Obligation-(vi) reading** (decision 8): "kept when not shipped" is satisfied via coherence (blocked/abandoned ⇒ fail ⇒ kept) with the dispose rule unchanged; a `no-op` pass still reaps (nothing to lose; spec §12 "reaped on clean pass" unchanged). Pinned by the Task 8 blocked-keeps test.
6. **Task 2 advertises flags Task 5 implements** (decision 10, kickoff order b-before-c honored): drift-proofed by Task 5's `--help` lockstep test; both land in this one PR.

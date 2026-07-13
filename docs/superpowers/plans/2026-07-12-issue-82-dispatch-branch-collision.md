# Issue #82 — Dispatch Branch Collision (Actionable Named-Recovery Error) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task in a fresh session on branch `fix-82-dispatch-branch-collision`. Steps use checkbox (`- [ ]`) syntax for tracking. This is a PLANNING-ONLY document; execution is a separate session per the fix-82 kickoff amendment.

> **APPROVAL NOTE:** Plan review: APPROVE, 2026-07-13 (Opus 4.8 plan gate). Remedy choice (loud named-recovery error, branch never deleted) explicitly judged sound; alternatives' rejections verified substantive. Non-blocking notes: N1 the reason-reaches-dispatch.failed property rests on verified code inspection of dispatch.rs:615 (fix-83's file — do not add a dispatch.rs test here); N2 reproduction at the spawn/mechanical layer is the appropriate fidelity for this stream; N3 bail! String semantics and line-join behavior to be confirmed by the Step 2/5 test runs as planned; N4 the foreign-live-worktree edge stays YAGNI-deferred. No deviations accepted.
>
> Deviation accepted 2026-07-13 by the plan reviewer: check order swapped — directory residue precedes branch probe; create_dir_all moved after both checks. Reason: pre-existing round_trip regression + wrong -D advice in the both-exist state.

**Goal:** A camp whose rig carries a predecessor ledger's leftover `camp/<bead>` branch no longer dies on a raw `git worktree add` error; instead `create_worktree` refuses with a loud, self-explaining, named-recovery error that identifies the branch, states whether it holds commits beyond the rig's base, and gives the exact command to clear it — never deleting the branch itself.

**Architecture:** The fix is a single, self-contained change to `crates/camp/src/daemon/spawn.rs` (the `create_worktree` area — this work stream's owned region). Before `git worktree add -b camp/<bead>` runs, `create_worktree` explicitly probes for a pre-existing `camp/<bead>` branch (the same "never delegate detection to git failing" philosophy already used by `ensure_worktree_base`). On a hit it returns an actionable error string; this error flows unchanged into the existing `dispatch.failed` event via `launch()`'s `format!("{e:#}")` — no change to `dispatch.rs`, no new event, no vocab/fold/schema change. `ensure_worktree` (which `dispatch::launch` actually calls) reaches `create_worktree` on exactly the reset scenario (worktree directory absent, branch present), so the fix covers the real dispatch path without touching a sibling's file.

**Tech Stack:** Rust (`anyhow`, `std::process::Command` via the existing `super::bounded::output_bounded` deadline wrapper), real `git` subprocesses in tempdir fixtures (no network, no `claude`), `cargo test`.

## Global Constraints

- **Owned file only:** modify **`crates/camp/src/daemon/spawn.rs`** and its in-file `#[cfg(test)]` module. Do NOT touch `dispatch.rs` (fix-83), `patrol.rs`/`event_loop.rs` (fix-81), the HeldStream argv block of `spawn.rs` (fix-86), or any shared file (`main.rs`, `event.rs`, `vocab.rs`, `fold.rs`, `Cargo.toml`, `Cargo.lock`). This change needs none of them.
- **No new event / no vocab / no fold / no schema change.** The improved reason string rides the existing `dispatch.failed` payload untouched.
- **Fail fast, no fallbacks, no silenced errors** (invariant 5). No `unwrap`/`expect`/`panic` in library code (clippy `unwrap_used`/`expect_used`/`panic` denied). `unsafe_code` forbidden.
- **Silent branch deletion is forbidden** (kickoff; `remove_worktree`'s own contract: a `camp/<bead>` branch "may hold unpushed work"). The fix inspects and reports; it MUST NOT delete or modify any branch.
- **TDD strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new/changed test before claiming anything.
- **Gates before push:** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`. Work is not complete until pushed and CI is green (foreground-watch `gh pr checks --watch`).
- **No co-author lines in commits; never mention the assistant in commits.**
- **Test fixture hygiene:** git fixtures must set `commit.gpgsign false` (a global signing config would stall the fixture). Reuse the existing `git_rig` test helper in `spawn.rs`, which already does this.

---

## Authoritative inputs (read before executing)

1. **This plan** — self-sufficient; it carries every command, path, and code block you need.
2. **Issue #82** (`gh issue view 82 --comments`) — the bug, the observed `dispatch.failed`, the structural analysis, and the "Directions (not a decision)" list. The fix-82 kickoff comment + its amendment are the binding contract.
3. **`AGENTS.md`** — repository invariants (esp. invariant 3 "Nothing hidden", invariant 5 "Fail fast").
4. **`crates/camp/src/daemon/spawn.rs`** — the file you edit. Study `ensure_worktree_base` (lines ~292–310), `rig_base` (~321–336), `create_worktree` (~342–379), `ensure_worktree` (~385–420), and `remove_worktree` (~424–442). Study the existing worktree tests (`git_rig`, `worktree_create_and_remove_round_trip`, `create_worktree_refuses_a_rig_without_a_base_commit`) for the fixture patterns you will mirror.
5. **`crates/camp/src/daemon/bounded.rs`** — `output_bounded(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output>` returns `Ok(Output)` even when git exits nonzero (callers check `out.status.success()` themselves); it errors only on spawn/timeout failure. This is the wrapper every git call in `spawn.rs` uses.

---

## Root-cause investigation (systematic-debugging — completed during planning)

**Phase 1 — Root cause (confirmed, not assumed):**

- `create_worktree` runs `git worktree add -b camp/<bead> <dir>`. `git worktree add -b` **fails hard** when a branch named `camp/<bead>` already exists: `fatal: a branch named 'camp/<bead>' already exists` (reproduced live, git 2.55.0).
- Bead ids come from the `counters` table **inside** `camp.db` (`camp_core::id::next_bead_id`), starting at 1 per rig prefix. A ledger reset (delete `.camp`, `camp init` again) restarts the counter at 1.
- `camp/<bead>` branches are **repo-permanent**: `remove_worktree` deliberately leaves the branch standing ("it may hold unpushed work; sweeping is Phase 11 policy"). `worktree.kept` after a spawn failure (worker never ran) makes a leftover branch at the rig's base especially likely.
- So on the first dispatch of a reset camp, bead `<prefix>-1` collides with the predecessor's `camp/<prefix>-1` branch. `create_worktree` bails with the **raw git stderr**; `dispatch::dispatch_one`/`launch` records it as `dispatch.failed` and inserts the bead into the in-memory `failed` set, which `converge` skips (`dispatch.rs:432`) for the rest of campd's life — **permanent** death, still showing `ready: 1` / `open: 1`.
- The invariant `create_worktree` documents ("bead ids are unique and Phase 8 never respawns a bead") holds only **within one ledger**; the branch namespace outlives the ledger, so it is false across a reset.

**The precise hole:** `ensure_worktree` already reuses the *directory* case (Phase 11 Decision H: an existing `worktrees/<bead>` that is a git worktree checked out on `camp/<bead>`). But when the **directory is absent and only the branch survives** (exactly the reset scenario — `.camp/worktrees` was deleted with the ledger), `ensure_worktree` falls straight through to `create_worktree`, which hits the branch collision. This branch-without-directory case is unhandled.

**Phase 2/3 — reproduction + mechanism validation (run live during planning, git 2.55.0):**

```
# collision reproduces exactly:
git worktree add -b camp/campdemo-1 ../worktrees/campdemo-1
  -> fatal: a branch named 'camp/campdemo-1' already exists

# explicit existence probe (version-stable, mirrors ensure_worktree_base's philosophy):
git rev-parse --verify --quiet refs/heads/camp/campdemo-1
  -> prints the sha, exit 0 when the branch exists
  -> empty output, exit 1 when it does not

# "commits beyond the rig's base" count:
git rev-list --count refs/heads/camp/campdemo-1 --not <base-sha>
  -> 0   for a branch sitting at/behind the base (no unpushed work)
  -> N>0 when the branch holds commits not reachable from the base (possible unpushed work)
```

All three commands behave as the fix relies on. Root cause is confirmed at the mechanism level.

---

## Decision: the chosen remedy (and why the alternatives were rejected)

Issue #82 offers four non-equivalent directions. **This plan implements Direction 1 — the loud, actionable, named-recovery error** — as the remedy, done thoroughly (it also computes the "commits beyond base" fact Direction 1 asks for). Rationale and rejected alternatives are recorded here so the plan reviewer can judge the choice.

**Chosen — actionable named-recovery error in `create_worktree`:**
- Directly satisfies the acceptance criterion ("... or fails with an actionable named-recovery error").
- Unambiguously invariant-aligned: fail fast (invariant 5), nothing hidden (invariant 3), no silent mutation, no branch deletion.
- Smallest, safest blast radius — one file, this stream's owned region, no shared/sibling files, no new event, no fold/schema/vocab change, minimal rebase surface against fix-86.
- The issue itself calls this direction "**strictly an improvement regardless of what else is chosen**."

**Rejected — Direction 2 (reuse the branch when it has no unique commits):** Reusing an existing branch means `git worktree add <dir> camp/<bead>` (checkout of the existing ref) rather than `-b` (branch from current HEAD). These differ semantically: if the leftover branch tip is not **exactly** the rig's current base, reuse would silently start the worker on a **stale base**, and the `session.woke` `base` (computed from the rig's *current* `HEAD^{commit}` by `rig_base`, independent of the checkout) would then disagree with the worktree's actual starting commit — breaking the `camp close` shipped-gate descent check. It also conflates two logically distinct beads' branch identities across a reset. The only provably-safe subset (branch tip == current base, zero commits beyond base) is exactly the case where a loud error plus a one-command `branch -D` is trivial for the operator — so the extra reuse machinery buys little and adds a real correctness hazard. Not chosen.

**Rejected — Direction 3 (`camp adopt`/`camp sweep` operator verb):** The adopt/retry operator surface is **fix-83's** owned scope (`dispatch.rs` failed-set/ready-scan + the adopt/retry surface). Building a re-dispatch or sweep verb here would cross a sibling's boundary. Out of scope by the parallel-stream contract.

**Rejected — Direction 4 (per-generation unique branch names):** Changing the `camp/<bead>` naming scheme has a wide blast radius (embedded across the codebase, tests, and the shipped-gate `work_branch` semantics), sacrifices the human-readable convention, and needs a repo-permanent uniqueness source (the ledger-scoped counter resets too). Disproportionate to the defect. Not chosen.

**Consequence acknowledged:** with Direction 1, the bead still lands in campd's in-memory `failed` set and is not auto-retried within one campd lifetime; recovery is: operator runs the named command, then re-dispatches. The re-dispatch/retry surface is **fix-83's** deliverable. Direction 1's job is to make the failure *actionable* instead of raw git stderr — which is precisely the acceptance contract for fix-82.

---

## File structure

- **Modify:** `crates/camp/src/daemon/spawn.rs`
  - Add a private helper `branch_collision_error(rig_path, bead_id, timeout) -> Result<Option<String>>` (near `rig_base`/`create_worktree`).
  - Insert one guard call into `create_worktree`, immediately after `ensure_worktree_base(...)?` and before any side effect (before `create_dir_all`).
  - Update `create_worktree`'s doc comment to record the collision behavior and reference issue #82.
- **Modify (tests):** `crates/camp/src/daemon/spawn.rs` `#[cfg(test)]` module — add three tests using the existing `git_rig` helper.

No other files change.

---

## Task 1: Actionable branch-collision error in `create_worktree`

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs` (add `branch_collision_error`; guard inside `create_worktree`; update the doc comment)
- Test: `crates/camp/src/daemon/spawn.rs` `#[cfg(test)]` module (three new tests)

**Interfaces:**
- Consumes: `super::bounded::output_bounded(&mut Command, Duration) -> std::io::Result<Output>`; `rig_base(rig_path, timeout) -> Result<Option<String>>` (existing, in this file); `anyhow::{Context, Result, bail}` (already imported at the top of the file).
- Produces: `fn branch_collision_error(rig_path: &Path, bead_id: &str, timeout: Duration) -> Result<Option<String>>` — `Ok(None)` when no `camp/<bead>` branch exists; `Ok(Some(message))` when it does (the message is the actionable, named-recovery error, ready to `bail!`); `Err` only when a git probe cannot run / times out / returns unparseable output. `create_worktree`'s external signature is unchanged.

---

- [ ] **Step 1: Write the failing tests**

Add these three tests inside the existing `#[cfg(test)] mod tests` block in `crates/camp/src/daemon/spawn.rs` (the `git_rig`, `TEST_EXEC_TIMEOUT`, and `crate::daemon::spawn_probe_guard()` helpers already exist there). They reproduce issue #82's exact scenario at the mechanical layer: a rig carrying a predecessor's `camp/<bead>` branch with the worktree directory gone (ledger reset), dispatched onto bead #1.

```rust
    /// Issue #82: a leftover `camp/<bead>` branch (predecessor ledger's
    /// residue — the worktrees dir was deleted with the ledger, the
    /// repo-permanent branch survived) must NOT die on raw git stderr. It
    /// fails with a loud, self-explaining, named-recovery error and the
    /// branch is left untouched (silent deletion is forbidden — it may hold
    /// unpushed work).
    #[test]
    fn create_worktree_refuses_a_stale_predecessor_branch_with_an_actionable_error() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");

        // Predecessor residue: the branch exists at the rig's base, no
        // worktree directory (the reset scenario). git branch <name> makes
        // a branch at HEAD without checking it out or creating a worktree.
        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["branch", "camp/gc-1"])
            .output()
            .unwrap();
        assert!(out.status.success(), "seeding the stale branch");

        let err = create_worktree(&rig, &worktrees, "gc-1", TEST_EXEC_TIMEOUT).unwrap_err();
        let msg = format!("{err:#}");

        // names the branch, states it is safe (no commits beyond base), and
        // gives the exact recovery command
        assert!(msg.contains("camp/gc-1"), "must name the branch: {msg}");
        assert!(msg.contains("already exists"), "must state the collision: {msg}");
        assert!(msg.contains("safe to delete"), "no-unique-commits branch is safe: {msg}");
        assert!(
            msg.contains(&format!("git -C {} branch -D camp/gc-1", rig.display())),
            "must name the exact recovery command: {msg}"
        );
        // explains WHY (ledger-scoped ids vs repo-permanent branches)
        assert!(
            msg.contains("ledger") && msg.contains("repo-permanent"),
            "must explain the structural cause: {msg}"
        );

        // NO silent deletion: the branch still exists after the refusal
        let still = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/camp/gc-1"])
            .output()
            .unwrap();
        assert!(still.status.success(), "the branch must NOT be deleted");
        // no worktree residue created on the refusal
        assert!(!worktrees.join("gc-1").exists(), "no worktree dir on refusal");
    }

    /// Issue #82: when the stale branch holds commits NOT on the rig's base,
    /// it may carry unpushed work — the error says so, gives the inspect
    /// command, and still names the delete command; the branch is preserved.
    #[test]
    fn create_worktree_flags_a_stale_branch_that_holds_unpushed_work() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");

        // A stale branch with a unique commit beyond the base: add the
        // branch via a throwaway worktree, commit onto it, then remove the
        // worktree (leaving the branch — the repo-permanent residue).
        for args in [
            vec!["worktree", "add", "-b", "camp/gc-1"],
        ] {
            let mut c = Command::new("git");
            c.arg("-C").arg(&rig).args(&args).arg(dir.path().join("stale"));
            assert!(c.output().unwrap().status.success(), "git {args:?}");
        }
        let stale = dir.path().join("stale");
        for args in [
            vec!["commit", "--allow-empty", "-m", "unpushed work"],
        ] {
            let out = Command::new("git").arg("-C").arg(&stale).args(&args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        // remove the throwaway worktree; the branch (with its commit) remains
        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["worktree", "remove", "--force"])
            .arg(&stale)
            .output()
            .unwrap();
        assert!(out.status.success(), "removing the throwaway worktree");

        let err = create_worktree(&rig, &worktrees, "gc-1", TEST_EXEC_TIMEOUT).unwrap_err();
        let msg = format!("{err:#}");

        assert!(msg.contains("camp/gc-1"), "must name the branch: {msg}");
        assert!(msg.contains("unpushed work"), "must warn about unpushed work: {msg}");
        assert!(
            msg.contains(&format!("git -C {} log", rig.display())) && msg.contains("..camp/gc-1"),
            "must give the inspect command: {msg}"
        );
        assert!(
            msg.contains(&format!("git -C {} branch -D camp/gc-1", rig.display())),
            "must still name the delete command: {msg}"
        );

        // branch preserved (its commit is not lost)
        let still = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/camp/gc-1"])
            .output()
            .unwrap();
        assert!(still.status.success(), "the branch must NOT be deleted");
        assert!(!worktrees.join("gc-1").exists(), "no worktree dir on refusal");
    }

    /// Issue #82 via the real dispatch entry point: dispatch::launch calls
    /// ensure_worktree, which on the reset scenario (worktrees dir absent,
    /// branch present) delegates to create_worktree — so the actionable
    /// error surfaces there too, and flows into dispatch.failed unchanged.
    #[test]
    fn ensure_worktree_surfaces_the_branch_collision_on_the_create_path() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");

        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["branch", "camp/gc-1"])
            .output()
            .unwrap();
        assert!(out.status.success());

        let err = ensure_worktree(&rig, &worktrees, "gc-1", TEST_EXEC_TIMEOUT).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("camp/gc-1") && msg.contains("already exists"), "got: {msg}");
        assert!(
            msg.contains(&format!("git -C {} branch -D camp/gc-1", rig.display())),
            "must name the recovery command: {msg}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they FAIL**

Run:
```bash
cargo test -p camp --lib daemon::spawn::tests::create_worktree_refuses_a_stale_predecessor_branch_with_an_actionable_error \
  daemon::spawn::tests::create_worktree_flags_a_stale_branch_that_holds_unpushed_work \
  daemon::spawn::tests::ensure_worktree_surfaces_the_branch_collision_on_the_create_path
```
Expected: all three FAIL. The current `create_worktree` reaches `git worktree add -b` and bails with the raw `fatal: a branch named 'camp/gc-1' already exists` message, which contains neither `safe to delete`, `unpushed work`, the `branch -D` recovery command, nor the `ledger`/`repo-permanent` explanation. (The assertions on those substrings are what fail.)

- [ ] **Step 3: Implement `branch_collision_error`**

Add this helper to `crates/camp/src/daemon/spawn.rs`, immediately after `rig_base` (so it sits with the other git-probe helpers, above `create_worktree`). `anyhow::{Context, Result, bail}`, `std::path::Path`, `std::process::Command`, `std::time::Duration`, and `super::bounded` are already in scope at the top of the file.

```rust
/// Detect a pre-existing `camp/<bead>` branch on `rig_path` and, if one is
/// there, build the loud named-recovery error dispatch needs (issue #82).
/// Bead ids are ledger-scoped but `camp/<bead>` branches are repo-permanent,
/// so a ledger reset restarts the bead counter onto a predecessor's branch;
/// `git worktree add -b` then dies on a raw "a branch named ... already
/// exists". The probe is EXPLICIT — the same discipline as
/// `ensure_worktree_base`: never delegate detection to `git worktree add`
/// failing. Returns `Ok(None)` when there is no such branch (the common,
/// happy path — one extra bounded rev-parse, "noise" per the module).
///
/// The message states whether the branch holds commits NOT reachable from
/// the rig's base (possible unpushed work) and names the exact recovery
/// command. It NEVER deletes the branch: a leftover branch may hold unpushed
/// work (`remove_worktree`'s own contract), so clearing it is the operator's
/// deliberate, informed act.
fn branch_collision_error(
    rig_path: &Path,
    bead_id: &str,
    timeout: Duration,
) -> Result<Option<String>> {
    let branch = format!("camp/{bead_id}");
    let refname = format!("refs/heads/{branch}");

    // Explicit existence probe: exit 0 + sha when present, nonzero when not.
    let exists = bounded::output_bounded(
        Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(&refname),
        timeout,
    )
    .context("running git rev-parse for the branch-collision probe")?;
    if !exists.status.success() {
        return Ok(None); // no camp/<bead> branch: no collision
    }

    // The branch exists. The base is guaranteed present here (create_worktree
    // runs ensure_worktree_base first); a None now means the repo was gutted
    // between the two probes — fail loud rather than guess (invariant 5).
    let base = rig_base(rig_path, timeout)?.context(
        "rig carries a camp/<bead> branch but has no base commit to compare it against",
    )?;

    // Commits on the branch not reachable from the base: >0 means possible
    // unpushed work the operator must not lose.
    let counted = bounded::output_bounded(
        Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(["rev-list", "--count"])
            .arg(&refname)
            .arg("--not")
            .arg(&base),
        timeout,
    )
    .context("running git rev-list for the branch-collision probe")?;
    if !counted.status.success() {
        bail!(
            "git rev-list failed inspecting branch {branch}: {}",
            String::from_utf8_lossy(&counted.stderr).trim()
        );
    }
    let ahead: u64 = String::from_utf8_lossy(&counted.stdout)
        .trim()
        .parse()
        .context("parsing git rev-list --count output")?;

    let rig = rig_path.display();
    let delete = format!("git -C {rig} branch -D {branch}");
    let message = if ahead == 0 {
        format!(
            "cannot dispatch bead {bead_id}: git branch {branch} already exists on rig {rig} \
             and holds no commits beyond the rig's base — it is leftover residue. Bead ids are \
             ledger-scoped but camp/<bead> branches are repo-permanent, so a ledger reset \
             restarts the bead counter onto a predecessor's branch. It is safe to delete: \
             {delete}, then re-dispatch."
        )
    } else {
        let inspect = format!("git -C {rig} log {base}..{branch}");
        format!(
            "cannot dispatch bead {bead_id}: git branch {branch} already exists on rig {rig} \
             and holds {ahead} commit(s) not on the rig's base {base} — it may contain unpushed \
             work. Bead ids are ledger-scoped but camp/<bead> branches are repo-permanent, so a \
             ledger reset restarts the bead counter onto a predecessor's branch. Inspect it \
             ({inspect}); once anything you need is preserved, delete it ({delete}) before \
             re-dispatching."
        )
    };
    Ok(Some(message))
}
```

- [ ] **Step 4: Wire the guard into `create_worktree`**

In `create_worktree`, insert the collision guard immediately after the base check and before any side effect. Change:

```rust
    ensure_worktree_base(rig_path, timeout)?;
    std::fs::create_dir_all(worktrees_dir)
        .with_context(|| format!("creating {}", worktrees_dir.display()))?;
```

to:

```rust
    ensure_worktree_base(rig_path, timeout)?;
    // Issue #82: refuse a repo-permanent camp/<bead> branch left by a
    // previous ledger's life BEFORE any side effect, with an actionable
    // named-recovery error instead of the raw `git worktree add` stderr
    // that used to permanently wedge dispatch on a reset camp's bead #1.
    if let Some(message) = branch_collision_error(rig_path, bead_id, timeout)? {
        bail!(message);
    }
    std::fs::create_dir_all(worktrees_dir)
        .with_context(|| format!("creating {}", worktrees_dir.display()))?;
```

Then update `create_worktree`'s doc comment so the contract is honest. Change the existing line:

```rust
/// A pre-existing directory or branch fails fast — bead ids are unique and
/// Phase 8 never respawns a bead. A rig with no base commit is refused
/// before any side effect (spec §12 fail-fast).
```

to:

```rust
/// A pre-existing directory fails fast (residue hint below). A pre-existing
/// `camp/<bead>` branch fails fast too, but with an actionable
/// named-recovery error (issue #82): bead ids are ledger-scoped while
/// branches are repo-permanent, so "bead ids are unique" is false across a
/// ledger reset — the error names the branch, says whether it holds commits
/// beyond the base, and gives the exact command to clear it (the branch is
/// never deleted here — it may hold unpushed work). A rig with no base
/// commit is refused before any side effect (spec §12 fail-fast).
```

- [ ] **Step 5: Run the new tests to verify they PASS**

Run:
```bash
cargo test -p camp --lib daemon::spawn::tests::create_worktree_refuses_a_stale_predecessor_branch_with_an_actionable_error \
  daemon::spawn::tests::create_worktree_flags_a_stale_branch_that_holds_unpushed_work \
  daemon::spawn::tests::ensure_worktree_surfaces_the_branch_collision_on_the_create_path
```
Expected: all three PASS.

- [ ] **Step 6: Run the full spawn test module to confirm no regression**

_(Amended during execution, deviation accepted 2026-07-13 — see the approval note.)_ The original rationale here claimed the pre-existing tests were unaffected because they "create `camp/<bead>` branches fresh with no pre-existing branch"; that was wrong for `worktree_create_and_remove_round_trip`, whose SECOND `create_worktree` call runs with both the branch (from the first call) and the live worktree directory present, expecting the directory-residue error. With Step 4's original ordering (branch probe before the directory check) the collision error fired first — and its `branch -D` advice would be wrong there, since git refuses `-D` on a branch checked out in a live worktree. Final ordering therefore: the directory-residue check runs FIRST, then the branch-collision probe, and `create_dir_all` runs after both so every refusal is side-effect-free. On the real dispatch path this is behavior-identical (`ensure_worktree` only reaches `create_worktree` when the directory is absent), and the three new tests are order-insensitive.

Run:
```bash
cargo test -p camp --lib daemon::spawn::tests
```
Expected: PASS (all pre-existing spawn tests plus the three new ones).

- [ ] **Step 7: Run the gates**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: all green. (If `cargo fmt --all --check` reports diffs, run `cargo fmt --all` and re-run the gate.)

- [ ] **Step 8: Commit**

```bash
git add crates/camp/src/daemon/spawn.rs
git commit -m "fix(dispatch): actionable error on a stale camp/<bead> branch (#82)

A ledger reset restarts the bead counter at 1 while the rig still carries
a predecessor ledger's repo-permanent camp/<bead> branch. create_worktree
now probes for the branch explicitly and refuses with a loud, named-recovery
error (branch name, whether it holds commits beyond the base, and the exact
git command to clear it) instead of the raw git stderr that permanently
wedged dispatch on bead #1. The branch is never deleted."
```

---

## Task 2: Push and settle CI

- [ ] **Step 1: Push the branch**

```bash
git push -u origin fix-82-dispatch-branch-collision
```

- [ ] **Step 2: Open the PR** (only after the plan-review APPROVE is recorded per the kickoff; the PR body must reference issue #82 — e.g. "Fixes #82")

```bash
gh pr create --base main --head fix-82-dispatch-branch-collision \
  --title "fix(dispatch): actionable error on a stale camp/<bead> branch (#82)" \
  --body "Fixes #82. ..."
```

- [ ] **Step 3: Foreground-watch CI to the settled result**

```bash
gh pr checks --watch
```
Expected: all checks green before reporting completion. Never report "CI is running."

---

## Acceptance criteria → evidence map

Quote each acceptance line and map it to the evidence the implementer must produce:

1. *"a camp whose rig carries a predecessor's camp/<bead> branch ... fails with an actionable named-recovery error"* → `create_worktree_refuses_a_stale_predecessor_branch_with_an_actionable_error` (safe case) and `create_worktree_flags_a_stale_branch_that_holds_unpushed_work` (unpushed-work case) assert the message names the branch, states the beyond-base commit fact, and gives the exact `branch -D` (and `log`) recovery command.
2. *"covered by a test reproducing the issue's exact scenario (ledger reset, stale branch, bead #1)"* → all three tests seed a `camp/gc-1` branch with the worktrees dir absent (the reset shape) and dispatch onto bead `gc-1`; `ensure_worktree_surfaces_the_branch_collision_on_the_create_path` exercises the exact function `dispatch::launch` calls.
3. *silent branch deletion is forbidden* → both `create_worktree_*` tests assert the branch still resolves via `git rev-parse --verify` after the refusal.
4. *CI green* → Task 2 Step 3 (`gh pr checks --watch` settled green).

---

## Self-review (writing-plans checklist — completed)

- **Spec coverage:** the acceptance criterion's two halves (actionable-error path + issue-scenario test) both map to Task 1 tests; the "silent deletion forbidden" invariant is asserted by test; CI-green maps to Task 2. No uncovered requirement.
- **Placeholder scan:** every code and test block is complete and literal; no TBD/TODO/"handle edge cases." The only intentional blank is the APPROVAL NOTE, which the implementer fills from the relayed verdict (per the kickoff amendment).
- **Type consistency:** `branch_collision_error(rig_path: &Path, bead_id: &str, timeout: Duration) -> Result<Option<String>>` is used exactly as defined in the `create_worktree` guard (`if let Some(message) = branch_collision_error(...)? { bail!(message); }`). `rig_base` and `output_bounded` are used with their existing signatures. `create_worktree`'s public signature is unchanged, so all existing call sites (`ensure_worktree`, tests) compile untouched.

---

## Risks / notes for the reviewer and implementer

- **Rebase with fix-86:** fix-86 edits the HeldStream/argv block of `spawn.rs` (`build_spec`), which is disjoint from the worktree functions this change touches. Expect at most a trivial context rebase; if fix-86 merges first, rebase onto main and re-run all gates before pushing.
- **Rare edge — a foreign live worktree on the branch:** if `camp/<bead>` is currently checked out in some *other* worktree (not `worktrees/<bead>`, which is absent in the reset case), `branch -D` would itself be refused by git ("used by worktree"). The reported repro is a *dangling* branch with no worktree, for which the named command works; the message still correctly names the branch so the operator can `git worktree list`. Handling that edge with a bespoke message is deliberately out of scope (YAGNI) — it does not occur in the issue's scenario and would add branching for a case fix-83's operator surface is better placed to address.
- **Out of scope (fix-83):** the bead remaining in campd's in-memory `failed` set (no auto-retry within one campd lifetime) and the absence of a re-dispatch verb are fix-83's deliverables (adopt/retry surface). This change makes the failure *actionable*; it does not add a retry path, and deliberately touches no `dispatch.rs`.
- **No new event/vocab/fold/schema:** the improved reason string rides the existing `dispatch.failed` payload; no shared file changes, keeping the partition/refold/vocab-pin properties untouched by construction.

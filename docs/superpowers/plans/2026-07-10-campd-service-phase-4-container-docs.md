# campd Service Management — Phase 4: the reference container + the spec reconciliation

> **Plan review: APPROVED 2026-07-10** (independent Opus reviewer, round 2).
> Round 1 REJECT — B1: the reconciliation sweep's expected output was false ("exactly two" hits; the
> real number is ~20), it condemned six benign true statements, and — the dangerous one — it condemned
> `daemon/socket.rs`'s `poke_best_effort` doc, which spec §7.2 FREEZES; executed verbatim, an
> implementer would have edited frozen code. B2: two genuinely stale claims fell through every phase's
> net and no task owned them (`cmd/nudge.rs`'s "on-demand daemon" comment, and
> `docs/design/2026-07-09-dispatch-lifecycle.md`'s reference to `request_with_autostart`, a function
> Phase 3 deletes).
> Round 2 APPROVE — the reviewer ran the sweep against a faithful post-execution tree and it matched
> the plan's disposition table exactly; the frozen `poke_best_effort` line is now a named, justified
> survivor rather than a condemned one; both stale claims are owned; and `contrib/launchd/` deletion
> is planned per the lead's ruling.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Plan review round 1: REJECT → revised.** **B1:** Task 4's reconciliation sweep was one grep with a false expected output ("exactly two" hits, where a faithful post-execution tree yields **34**) — and, followed verbatim, it condemned `daemon/socket.rs`'s `poke_best_effort` doc, which the operator **froze** (spec §7.2; Phase 3 names it "the ONE permitted survivor"). It is now **five per-scope gates, each with its literal expected output, run by the author against a simulated post-Phase-2/3/4 tree** (Task 4, Step 10) plus a full disposition table naming every legitimate survivor. **B2:** two genuinely stale claims that fall through every other phase's net — `crates/camp/src/cmd/nudge.rs`'s "(on-demand daemon)" comment and `docs/design/2026-07-09-dispatch-lifecycle.md`'s `request_with_autostart(..., Poke)` (a function Phase 3 deletes) — are now owned by Task 4, with steps. N1-N5, N7 folded in. **N6 is ruled by the operator: `contrib/launchd/` is deleted, with a pointer at `camp service install`** — no longer an open question.

**Goal:** Ship the reference container setup (`contrib/docker/`) that makes the container runtime a first-class campd supervisor, prove it with an opt-in local-only smoke test, and finish the reconciliation the first three phases started — the v1 spec's §5/§9/§12 amendments, the deletion of the superseded `contrib/launchd/`, and every remaining sentence **in prose or in code comments** that still describes a campd that starts itself.

**Architecture:** Three deliverables, one PR. (1) One tiny Rust change — `camp init --exists-ok` — because the entrypoint must be re-runnable on a restart and today's `camp init` **hard-fails** on an existing camp (verified: `crates/camp/src/cmd/init.rs:14-16`, `bail!("a camp already exists at …")`, pinned by `tests/cli_init.rs::reinit_fails_fast`). (2) `contrib/docker/`: a multi-stage `Dockerfile`, a 6-line POSIX-sh entrypoint that ensures the camp then `exec`s `camp daemon` (so campd — not a shell — is what the runtime signals), a `compose.yaml` with `restart: unless-stopped`, and a README that is honest about what does not work. (3) Docs: the v1 spec §5 supervised-process model, §9's away-mode limit narrowed to the unsupervised case, §12's one-camp-many-rigs recommendation, `contrib/launchd/` deleted as superseded, and the last stale README/pack sentences fixed.

**Tech Stack:** Rust (edition 2024, one clap flag), Docker + Compose, POSIX sh, Debian bookworm, tini. No new crate dependencies.

---

## Global Constraints

- **Phases 1-3 merge before this one.** Phase 1 (`b0dc950`, merged): campd handles SIGTERM/SIGINT → graceful shutdown (`campd.stopped`, socket unlinked, exit 0) — the reason `docker stop` is clean. Phase 2: `camp service {install,uninstall,status,restart,list,stop,start}`, environment-aware `camp init` with `--service` / `--no-service`, `camp stop` refusing on a supervised camp, the `tests/no_bare_camp_init.rs` guard, a `README.md` `camp service` subsection, a `Makefile` `service-e2e` target. Phase 3: the CLI is a pure socket client (the CLI-spawn path is gone), plus a **minimal** correction to `docs/design/2026-07-05-gas-camp-design.md` (the component-table row and §5's `**Auto-start:**` bullet → a `**Pure client:**` bullet). **Task 0 re-verifies all of this against merged `main` before anything else.**
- **EVERY LINE NUMBER IN THIS PLAN IS FROM `main` @ `b0dc950` AND WILL HAVE SHIFTED.** Three phases land first. **Anchor on the quoted text and the section heading, never on the line number.** Task 0 is not optional.
- **TDD, strictly** (AGENTS.md): write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything.
- **Gates green before every commit:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`.
- **No CI job may build or run Docker.** The container smoke test is `#[ignore]`d **and** gated on `CAMP_CONTAINER_E2E=1`, run only via `make container-smoke` — exactly how `make e2e` (`CAMP_E2E=1`) and Phase 2's `make service-e2e` (`CAMP_SERVICE_E2E=1`) are gated. `.github/workflows/ci.yml` is **not modified by this phase**.
- **No test may run a bare `camp init`.** Phase 2's `tests/no_bare_camp_init.rs` fails the build if a line contains both `"init"` and `.arg(`/`.args(` without `--no-service` (or a `// not-camp:` / `// real-manager:` marker). This plan's new test calls the camp binary through a helper (`docker_ok(&[…])`), so no call site carries `.args(` and `"init"` together — **and it passes `--no-service` anyway**, because that is the correct flag, not because a guard asks for it.
- **Do not reintroduce the dead concept.** Phase 3 removed the CLI-spawn path; no prose this phase writes may say campd "auto-starts" / "starts on demand" anywhere. Say *"the removed CLI-spawn path"* when the history must be named at all. Task 4 Step 10's gates are run against every line this phase writes, so a slip is caught, not shipped.
- **Three things in this tree are FROZEN. Task 4's gates match them on purpose and name them as survivors; if you "fix" one you have broken an invariant:**
  1. `crates/camp/src/daemon/socket.rs` — `/// a poke never auto-starts the daemon.` (`poke_best_effort`'s doc). Spec §7.2's one sanctioned ignore-the-error site; the operator froze the function **and its doc comment**, and Phase 3's Gate B names it "the ONE permitted survivor". It is still true. **Do not touch it.**
  2. The `campd.autostarted` **event vocabulary** in `crates/camp-core` (`EventType::CampdAutostarted`, the `vocab.rs` entry, the `fold.rs` arm, the ledger test) and the guard assertions in `crates/camp/tests` that name it. `EventType::parse` hard-errors on an unknown event name, so **deleting the variant would make every ledger that ever recorded one unreadable.** Deleting it is never the fix.
  3. `docs/superpowers/{plans,specs}/` — historical records. Never rewritten (this plan's own spec amendment to the *feature design* doc is the one deliberate exception, in Task 1).
- **Out of scope (feature design §11) — do not plan or build:** ephemeral one-shot container usage (`docker run camp sling …` that exits), a network/remote socket transport, auto-migrating existing camps to managed units.
- Branch: `phase-4-container-docs`. Never commit to `main`. No co-author lines, no self-attribution in any commit or PR body.

---

## Task 0: Rebase, then re-verify every anchor (no commit)

Three phases landed after this plan was written. **In this feature's plan reviews, an unverified anchor has been the blocking finding five times out of six.** Every claim below was read on `main` @ `b0dc950`; your job is to re-establish each one against reality before you touch a file. If any expectation below is violated, **stop and report** — do not adapt silently.

**Files:** none modified.

- [ ] **Step 1: Rebase onto merged main**

```bash
cd /Users/kiener/code/gascamp-phase-4-container
git fetch origin
git rebase origin/main
git log --oneline -6
```
Expected: Phase 2's and Phase 3's merge commits are in the log. If either is missing, **stop** — this phase depends on both.

- [ ] **Step 2: Re-verify the `camp init` anchor (Task 1 depends on it)**

```bash
grep -n "already exists\|exists_ok\|pub fn run" crates/camp/src/cmd/init.rs
grep -n "reinit_fails_fast" -A 14 crates/camp/tests/cli_init.rs
```
Expected: `cmd/init.rs` still contains `bail!("a camp already exists at {}", root.display());` guarded by `if root.join("camp.toml").exists() || root.join("camp.db").exists()`, its `run` now takes a `ServiceChoice` (Phase 2), and there is **no** `exists_ok` anywhere. `reinit_fails_fast` still asserts `.failure().code(1)` with stderr containing `"already"`.
**If `camp init` has somehow become idempotent, stop and report** — Task 1 is then wrong and the entrypoint needs no flag.

- [ ] **Step 3: Re-verify the Phase 2 surface the container and the docs rely on**

```bash
grep -rn "no-service\|exists" crates/camp/src/cmd/init.rs | head
grep -n "no_bare_camp_init" -r crates/camp/tests/ | head -3
grep -n "e2e\|perf\|service-e2e\|PHONY" Makefile
grep -rn "camp service" README.md | head -3
```
Expected: `camp init --no-service` exists and, per Phase 2, "constructs no supervisor and touches no unit directory"; `tests/no_bare_camp_init.rs` exists; the `Makefile` has `.PHONY: install uninstall perf e2e service-e2e` (or similar) plus `perf`, `e2e` and `service-e2e` targets; `README.md` has a `camp service` subsection.

- [ ] **Step 4: Re-verify the design-spec anchors (Tasks 3 and 4 rewrite these)**

```bash
grep -n "^## 5\.\|^### campd lifecycle\|^## 9\. Orders\|^## 12\." docs/design/2026-07-05-gas-camp-design.md
grep -n -i "auto-start\|autostart\|on demand\|on-demand" docs/design/2026-07-05-gas-camp-design.md
```
Expected after Phase 3: **exactly two** hits from the second grep — §9's `with the default on-demand daemon` (inside the "Away-mode is the same code path" bullet — **Task 3 kills it**) and `camp show --wait`'s `never autostarts campd` (**still true; leave it**). The component-table row and the `**Auto-start:**` bullet are already gone (Phase 3 replaced them with a `**Pure client:**` bullet — **do not re-edit that bullet; Task 3 inserts a new bullet ABOVE the liveness bullet and leaves Phase 3's alone**). If the second grep still shows `auto-started on demand` in the component table, Phase 3 did not land; **stop and report**.

- [ ] **Step 5: Re-verify the stale-doc anchors (Task 4 fixes these)**

```bash
grep -rn -i "on demand\|on-demand\|only while there's work\|fire-at-login\|launchd" README.md contrib/ packs/
grep -n "on-demand daemon" crates/camp/src/cmd/nudge.rs
grep -n "request_with_autostart" docs/design/2026-07-09-dispatch-lifecycle.md
```
Expected (each is Task 4's; each is anchored by its **quoted sentence**, not its line number):
| Anchor text | File (line on `b0dc950`) | Owner |
|---|---|---|
| `while you're away, with an optional launchd agent for fire-at-login.` | `README.md:49` | **Task 4** |
| `` `campd` is the only standing process, and only while there's work `` | `README.md:305` | **Task 4** |
| `Honest limits: with the default on-demand daemon, orders` … `([contrib/launchd/](contrib/launchd/README.md))` | `README.md:404-406` | **Task 4** |
| `Install the launchd agent for fire-at-login.` | `packs/starter/README.md:40` | **Task 4** |
| `Deliberately NO KeepAlive: `camp stop` must stay stopped.` | `contrib/launchd/com.gascamp.campd.plist.example:21-22` | **Task 4 (deleted)** |
| the whole `contrib/launchd/README.md` | `contrib/launchd/README.md` | **Task 4 (deleted)** |
| `// campd-not-listening is a normal state (on-demand daemon), and a` — **a live code comment asserting a concept the codebase no longer contains.** Phase 3's sweep grep (`auto-start\|autostart\|auto started\|auto start`) cannot match "on-demand daemon", so it survives Phase 3 by omission. The *behavior* is right (`nudge` uses `request_if_up` and a down campd genuinely is normal for it) — only the parenthetical justification is dead. | `crates/camp/src/cmd/nudge.rs:37` | **Task 4** |
| `` `request_with_autostart(..., Poke)`. On that poke, `campd`'s dispatcher `` — **a design doc naming a function Phase 3 deletes** (`daemon/autostart.rs` is gone). Phase 3 freezes `docs/design/**` except the two v1-spec lines, so nobody else owns it. AGENTS.md: spec and code never silently diverge. | `docs/design/2026-07-09-dispatch-lifecycle.md:287` | **Task 4** |

> **The kickoff for this phase said Phase 2 owns the `README.md:305-308` fix. It does not.** Phase 2's own plan states: *"`contrib/launchd/`'s superseded example, the `README.md` quickstart/orders text that still describes on-demand campd, and the `docs/design/…` §5/§9/§12 amendments are **Phase 4**"*. If the grep above shows those sentences already fixed, skip them and say so; if it shows them present (expected), **Phase 4 fixes them**.

- [ ] **Step 6: Re-verify the runtime facts the container design is built on**

Each of these was read in the source; re-confirm nothing moved:

```bash
grep -n "global = true" crates/camp/src/main.rs                       # --camp is a GLOBAL flag → `camp daemon --camp <dir>` parses
grep -n "CAMP_DIR" crates/camp/src/campdir.rs | head -2               # $CAMP_DIR resolves the camp → `docker exec <c> camp sling` needs no flags
grep -n "READY_PREFIX" crates/camp/src/daemon/mod.rs                  # "campd listening on " — printed to STDOUT → visible in `docker logs`
grep -n "pub fn rig_base" -A 6 crates/camp/src/daemon/spawn.rs        # runs `git` on EVERY dispatch → the image MUST contain git
grep -n "pub fn claude_config_root" -A 6 crates/camp/src/daemon/spawn.rs  # HOME unset = hard error → the image MUST set HOME
grep -n "create_dir_all(&parent)" crates/camp/src/daemon/patrol.rs    # patrol mkdir -p's $HOME/.claude/projects/… → HOME must be WRITABLE
grep -n "bundled" crates/camp-core/Cargo.toml                         # rusqlite bundled → the builder needs a C toolchain; the runtime needs no libsqlite3
grep -n "fn resolve_rig" -A 9 crates/camp/src/cmd/create.rs           # a single-rig camp needs no --rig on `camp sling`
git ls-files Cargo.lock                                               # committed → `cargo build --locked` works in the image
```
Expected: every one confirms. **Any miss changes the Dockerfile — stop and report rather than patching around it.**

---

## File Structure

**New:**

| File | Responsibility |
|---|---|
| `contrib/docker/Dockerfile` | Multi-stage: build `camp` from the workspace, then a slim runtime with `git` + `tini` + a non-root `camp` user. The image's contract: campd is the main process. |
| `contrib/docker/entrypoint.sh` | Six lines of POSIX sh: ensure the camp exists (`camp init --no-service --exists-ok`), then `exec camp daemon --camp "$CAMP_DIR"`. `exec` is the whole point — campd, not a shell, receives SIGTERM. |
| `contrib/docker/compose.yaml` | The container runtime AS the supervisor: `restart: unless-stopped`, a named volume for the camp dir, an explicit `stop_grace_period`. |
| `contrib/docker/README.md` | How to run it, how to drive it (`docker exec … camp sling`), and — honestly — what does **not** work (host-side socket access off Linux; cross-host is out of scope). |
| `.dockerignore` (repo root) | Keeps `target/` and `.git/` out of the build context. The build context is the repo root (the Dockerfile compiles the workspace). |
| `crates/camp/tests/container_smoke.rs` | The opt-in, local-only smoke: build → run → `docker exec camp sling` → bead dispatched and closed by an in-container worker → `docker stop` is a fast, graceful, exit-0 SIGTERM. |

**Modified:**

| File | Change |
|---|---|
| `crates/camp/src/cmd/init.rs` | `run` takes `exists_ok: bool`; an existing camp + `--exists-ok` is a **no-op success**, not a `bail!`. Without the flag, byte-for-byte today's hard error. |
| `crates/camp/src/main.rs` | `Init` gains `#[arg(long = "exists-ok")] exists_ok: bool`; the dispatch arm passes it through. |
| `crates/camp/tests/cli_init.rs` | Three new tests around `--exists-ok` (`reinit_fails_fast` stays, untouched — **without `--exists-ok`** the behavior is byte-for-byte what it was; note that after Phase 2's sweep that test already carries `--no-service`). |
| `Makefile` | A `container-smoke` target next to `perf` / `e2e` / `service-e2e`, and `.PHONY`. |
| `docs/superpowers/specs/2026-07-10-campd-service-management-design.md` | §7's container bullets corrected to the truth this phase discovered (init is not idempotent without a flag; the image needs git). AGENTS.md: spec and code never silently diverge. |
| `docs/design/2026-07-05-gas-camp-design.md` | **§5**: a new **supervised-process** bullet + the supervision table (Phase 3's `**Pure client:**` bullet is left exactly as it is). **§9**: the away-mode limit narrowed to the unsupervised case. **§12**: the one-camp-many-rigs recommendation. |
| `docs/design/2026-07-09-dispatch-lifecycle.md` | One clause: `request_with_autostart(..., Poke)` → the pure-client poke Phase 3 replaced it with. The doc currently describes a call into a module that no longer exists. |
| `README.md` | The three surviving on-demand/launchd sentences; a new "in a container" pointer under the daemon section. |
| `packs/starter/README.md` | `Install the launchd agent for fire-at-login.` → the supervised truth. |
| `crates/camp/src/cmd/nudge.rs` | One comment: `(on-demand daemon)` → the true reason a down campd is normal for `nudge`. Behavior untouched. |

**Deleted:**

| File | Why |
|---|---|
| `contrib/launchd/README.md`, `contrib/launchd/com.gascamp.campd.plist.example` | Superseded by `camp service install`, which generates a **KeepAlive** unit. The example's central claim — *"Deliberately NO KeepAlive: `camp stop` must stay stopped"* — is the exact opposite of what camp now ships, and a hand-edited plist that fights the supervisor is a trap. Feature design §8: *"fold it into the new `camp service` docs / reference."* Git history keeps it; the docs point at `camp service install`.

---

## Task 1: `camp init --exists-ok` — the restartable entrypoint's one prerequisite

The feature design §7 says the container entrypoint runs "`camp init --no-service` (idempotent)". **It is not idempotent.** `cmd/init.rs` bails on an existing camp, and `tests/cli_init.rs::reinit_fails_fast` pins that. On a container restart with a persistent camp volume the camp already exists, so a bare `camp init` in the entrypoint would exit 1 and the container would crash-loop.

Two ways out, and only one of them is honest:
- **Test for the camp in the shell** (`[ -e "$CAMP_DIR/camp.toml" ] || camp init …`). This duplicates `init`'s own existence predicate in a second language, in a file no unit test ever runs. When the predicate changes, the entrypoint drifts silently. Rejected.
- **Give `init` the semantics the entrypoint actually needs** — "ensure this camp exists" — as an explicit, tested flag. One owner of the predicate; testable in `cargo test` in milliseconds instead of only inside an opt-in Docker run. **This is what we do.**

`--exists-ok` short-circuits *before* the service decision: an already-existing camp is not re-examined for a unit. That is deliberate and it matches feature design §11 ("auto-migrating existing camps to managed units" is explicitly out of scope) — `camp service install` is how an existing camp gets a unit.

**Files:**
- Modify: `crates/camp/src/cmd/init.rs` (the `run` signature and the existence guard — the `bail!` at `:14-16` on `b0dc950`, inside Phase 2's rewritten `run`)
- Modify: `crates/camp/src/main.rs` (the `Init` variant — `:58` on `b0dc950`, given `--service` / `--no-service` by Phase 2 — and the `Command::Init` dispatch arm)
- Modify: `crates/camp/tests/cli_init.rs` (three new tests)
- Modify: `docs/superpowers/specs/2026-07-10-campd-service-management-design.md` (§7's first container bullet)

**Interfaces:**
- Consumes: Phase 2's `crate::service::ServiceChoice` (`Auto` / `Force` / `Skip`) and its `cmd::init::run(camp_flag: Option<&Path>, choice: ServiceChoice) -> Result<()>`.
- Produces, for Task 2: the CLI contract `camp init --camp <DIR> --no-service --exists-ok` → **exit 0** whether or not the camp exists; on an existing camp it prints `camp already exists at <dir> (--exists-ok)` on stdout and changes nothing on disk.

- [ ] **Step 1: Write the failing tests**

Append to `crates/camp/tests/cli_init.rs` (below `reinit_fails_fast`, which stays exactly as it is — **without `--exists-ok`**, re-init behavior does not change. That test already carries `--no-service` after Phase 2's sweep; leave it alone):

```rust
/// The container entrypoint (contrib/docker/) re-runs `camp init` on every
/// start, and on a restart the camp already exists on the mounted volume.
/// `--exists-ok` makes that a no-op SUCCESS — and a no-op means it touches
/// NOTHING: the marker we wrote into camp.toml must survive it.
#[test]
fn init_exists_ok_is_a_no_op_on_an_existing_camp() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .args(["init", "--no-service"])
        .assert()
        .success();

    let config = dir.path().join(".camp/camp.toml");
    let before = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{before}\n# operator's own edit\n")).unwrap();
    let marked = std::fs::read_to_string(&config).unwrap();

    camp()
        .current_dir(dir.path())
        .args(["init", "--no-service", "--exists-ok"])
        .assert()
        .success()
        .stdout(predicates::str::contains("already exists"));

    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        marked,
        "--exists-ok must not rewrite an existing camp.toml"
    );
}

/// On a FRESH directory --exists-ok changes nothing: it still creates the camp.
#[test]
fn init_exists_ok_on_a_fresh_dir_still_creates_the_camp() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .args(["init", "--no-service", "--exists-ok"])
        .assert()
        .success()
        .stdout(predicates::str::contains("initialized camp"));

    assert!(dir.path().join(".camp/camp.toml").exists());
    assert!(dir.path().join(".camp/camp.db").exists());
}

/// A camp with a ledger but no camp.toml is still "already there" — the flag
/// reuses init's OWN existence predicate rather than inventing a second one,
/// so `--exists-ok` never half-repairs a camp behind your back. (It hands off
/// to campd, which opens the ledger; it does not rebuild what it did not make.)
#[test]
fn init_exists_ok_uses_inits_own_existence_predicate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("camp.db"), b"").unwrap();

    camp()
        .current_dir(dir.path())
        .args(["init", "--no-service", "--exists-ok"])
        .assert()
        .success()
        .stdout(predicates::str::contains("already exists"));

    assert!(
        !root.join("camp.toml").exists(),
        "--exists-ok is a no-op, not a repair: it must not write into a camp it did not create"
    );
}
```

- [ ] **Step 2: Run them to watch them fail**

Run: `cargo test -p camp --test cli_init`
Expected: FAIL — `error: unexpected argument '--exists-ok' found` on all three new tests. (`reinit_fails_fast` and the older tests still pass.)

- [ ] **Step 3: Add the flag in `main.rs`**

In the `Init` variant of `enum Command` (Phase 2 gave it `service` / `no_service`), add the third flag:

```rust
    /// Create a new camp (./.camp by default; --camp DIR to choose the place)
    Init {
        /// Install and start a host service unit; a hard error when no host
        /// service manager is available (container/CI)
        #[arg(long, conflicts_with = "no_service")]
        service: bool,
        /// Do not install a host service unit (containers, CI, or by choice)
        #[arg(long = "no-service")]
        no_service: bool,
        /// An existing camp is a no-op success, not an error — for entrypoints
        /// and units that re-run `camp init` on every start (contrib/docker/)
        #[arg(long = "exists-ok")]
        exists_ok: bool,
    },
```

And in the `Command::Init` dispatch arm, pass it through (Phase 2's arm builds the `ServiceChoice`; keep that exactly, add the argument):

```rust
        Command::Init {
            service,
            no_service,
            exists_ok,
        } => {
            // Two bools at the CLI edge; ONE tri-state inside (clap already
            // rejected the contradictory pair).
            let choice = if service {
                ServiceChoice::Force
            } else if no_service {
                ServiceChoice::Skip
            } else {
                ServiceChoice::Auto
            };
            cmd::init::run(cli.camp.as_deref(), choice, exists_ok)
        }
```

- [ ] **Step 4: Implement it in `cmd/init.rs`**

Change the signature and the existence guard — **one** predicate, two outcomes:

```rust
/// Create a new camp: `<cwd>/.camp` by default, `--camp DIR` to choose. Then
/// (design §6) put its campd under the host's service manager where one
/// exists — `--service` forces it, `--no-service` skips it.
///
/// `exists_ok` turns the "already a camp here" case from a hard error into a
/// no-op success. It exists for supervised entrypoints that re-run init on
/// every start (contrib/docker/): a restarted container with a persistent camp
/// volume MUST come back up, and a crash-loop over an error that says "yes,
/// the camp you asked for is right there" would be a lie about a failure.
/// It is a no-op, never a repair: an existing camp is returned as it is, and
/// no unit is installed for it (a camp created before this had a service
/// manager gets one from `camp service install` — an explicit act).
pub fn run(camp_flag: Option<&Path>, choice: ServiceChoice, exists_ok: bool) -> Result<()> {
    let root = match camp_flag {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()
            .context("cannot determine current directory")?
            .join(".camp"),
    };
    if root.join("camp.toml").exists() || root.join("camp.db").exists() {
        if exists_ok {
            println!("camp already exists at {} (--exists-ok)", root.display());
            return Ok(());
        }
        bail!("a camp already exists at {}", root.display());
    }
```

(The rest of `run` — `create_dir_all`, `camp.toml`, `Ledger::open`, the gitignore call, the `println!`, and Phase 2's `match service::decide(...)` block — is unchanged.)

- [ ] **Step 5: Run the tests to watch them pass**

Run: `cargo test -p camp --test cli_init`
Expected: PASS — all tests, including the untouched `reinit_fails_fast` (a re-init **without `--exists-ok`** is still exit 1, stderr `already`).

- [ ] **Step 6: Correct the feature spec's §7 (it says "idempotent" and it is wrong)**

In `docs/superpowers/specs/2026-07-10-campd-service-management-design.md`, §7 "Reference container setup", replace the first bullet:

Old:
```markdown
- `Dockerfile` — build/copy the `camp` binary; a small entrypoint runs
  `camp init --no-service` (idempotent) then `exec camp daemon --camp <dir>`.
```
New:
```markdown
- `Dockerfile` — build the `camp` binary; a small entrypoint runs
  `camp init --no-service --exists-ok` then `exec camp daemon --camp <dir>`.
  (`camp init` is NOT idempotent on its own — it hard-errors on an existing
  camp, and a restarted container's camp volume always has one. Phase 4 adds
  `--exists-ok`: an existing camp is a no-op success. The flag, not a shell
  test, so one predicate owns "is there a camp here".)
- The image must contain **`git`**: campd shells out to `git rev-parse --verify
  HEAD^{commit}` on every dispatch to read the rig's base commit
  (`daemon/spawn.rs::rig_base`), and to `git worktree add` for the default
  isolation. It must also set a writable **`HOME`**: campd computes the worker
  transcript path under `$HOME/.claude` (a hard error if `HOME` is unset) and
  patrol creates that directory.
```

- [ ] **Step 7: Gates**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: clean; all tests pass (including `no_bare_camp_init` — every new call site above passes `--no-service`).

- [ ] **Step 8: Commit**

```bash
git add crates/camp/src/cmd/init.rs crates/camp/src/main.rs crates/camp/tests/cli_init.rs \
        docs/superpowers/specs/2026-07-10-campd-service-management-design.md
git commit -m "feat(init): --exists-ok — an existing camp is a no-op success, for supervised entrypoints"
```

---

## Task 2: The reference container, driven by its smoke test

The container runtime is a supervisor, so it slots into the same seat as launchd and systemd (feature design §3): `camp daemon` **is** the container's main process, `restart: unless-stopped` is its KeepAlive, and `docker stop` is a SIGTERM that Phase 1 already made graceful.

The test comes first and it drives everything: it fails because there is no `contrib/docker/Dockerfile`, and it goes green only when a real image really builds, really serves the socket, really dispatches, and really stops clean.

**Files:**
- Create: `crates/camp/tests/container_smoke.rs`
- Create: `contrib/docker/Dockerfile`, `contrib/docker/entrypoint.sh`, `contrib/docker/compose.yaml`, `contrib/docker/README.md`
- Create: `.dockerignore` (repo root — the build context is the workspace)
- Modify: `Makefile` (the `container-smoke` target + `.PHONY`)

**Interfaces:**
- Consumes: `camp init --camp <DIR> --no-service --exists-ok` (Task 1); `camp daemon` with the **global** `--camp` flag; `$CAMP_DIR` camp resolution; the readiness line `campd listening on <socket>` on **stdout**; `[dispatch] command` as the worker executable (`crates/camp-core/src/config.rs`).
- Produces: nothing consumed by later tasks (Tasks 3 and 4 are documentation and reference `contrib/docker/` by path only).

- [ ] **Step 1: Write the failing test**

Create `crates/camp/tests/container_smoke.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The reference container, end to end (feature design §7/§9; v1 spec §5).
//!
//! OPT-IN and LOCAL-ONLY, exactly like `make e2e` and `make service-e2e`: the
//! measured test is `#[ignore]`d AND requires CAMP_CONTAINER_E2E=1, so
//! `cargo test --workspace` and CI never build or run Docker. Run it with
//! `make container-smoke`. It needs a working `docker` on PATH.
//!
//! What it proves about contrib/docker/:
//!   1. the image builds, and the entrypoint's `camp init --no-service
//!      --exists-ok` is a NO-OP on an already-initialized camp — the restart
//!      path, which a bare `camp init` would crash-loop on;
//!   2. campd is the container's main process and answers on the in-container
//!      socket: `docker exec <c> camp sling "…"` creates a bead, campd
//!      dispatches a worker, and the worker claims and closes it — the whole
//!      round trip, inside the container;
//!   3. `docker stop` is graceful: SIGTERM reaches campd (that is what `exec`
//!      in the entrypoint buys), the ledger gets `campd.stopped`, the socket is
//!      unlinked, and the container exits 0 — FAST, not after the 10 s SIGKILL
//!      grace. This is Phase 1's payoff, measured.
//!
//! The worker is a four-line POSIX-sh fake wired in through `[dispatch]
//! command` — visible config, not a fallback — so the image needs no `claude`
//! and this test spends no API money.
//!
//! Two environment assumptions, stated rather than assumed silently:
//!   - The fixture is handed to the prep container as a read-only bind mount of
//!     a `tempfile::tempdir()` path (`/tmp/…` on Linux, `/var/folders/…` on
//!     macOS). Both are inside Docker Desktop's default shared paths. If you
//!     have narrowed file sharing, docker will fail this mount with its own
//!     error — set `TMPDIR` to a shared path and rerun.
//!   - The `Drop` guard removes the container, the volume AND the image, so a
//!     run leaves nothing behind.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const IMAGE: &str = "gascamp-container-smoke:test";
const CONTAINER: &str = "gascamp-container-smoke";
const VOLUME: &str = "gascamp-container-smoke-camp";
/// campd must answer `docker stop`'s SIGTERM well inside the 10 s grace a plain
/// `docker stop` gives it; an ignored SIGTERM shows up as ~10 s + exit 137,
/// which is the failure this bound catches. (Unrelated to compose.yaml's
/// `stop_grace_period: 30s`, which is a *ceiling* for a slow real shutdown, not
/// a target — this test does not use compose.)
const GRACEFUL_STOP_MAX: Duration = Duration::from_secs(5);

fn repo_root() -> PathBuf {
    // crates/camp/ -> crates/ -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap()
}

fn require_opt_in() {
    assert_eq!(
        std::env::var("CAMP_CONTAINER_E2E").as_deref(),
        Ok("1"),
        "the container smoke test is opt-in and LOCAL-ONLY: set CAMP_CONTAINER_E2E=1 \
         (use `make container-smoke`). It builds a Docker image and runs a container."
    );
}

/// Run docker and return the outcome. Failure is the CALLER's to judge.
fn docker(args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("docker {args:?} could not be run ({e}) — is docker on PATH?"))
}

/// Run docker and fail loudly on a non-zero exit. Returns stdout.
fn docker_ok(args: &[&str]) -> String {
    let out = docker(args);
    assert!(
        out.status.success(),
        "docker {args:?} failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Pre-run cleanup ONLY: a leftover container/volume from an aborted run must
/// not fail the next one, and "there was nothing to remove" is a fine outcome.
/// Every assertion in the test proper goes through docker_ok.
fn docker_cleanup(args: &[&str]) {
    let _ = docker(args);
}

/// Removes the container, the volume AND the image even when the test panics —
/// a test suite that silts up the developer's Docker with a stray image per run
/// is a test suite people stop running.
struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        docker_cleanup(&["rm", "-f", CONTAINER]);
        docker_cleanup(&["volume", "rm", "-f", VOLUME]);
        docker_cleanup(&["image", "rm", "-f", IMAGE]);
    }
}

/// The camp the container will serve, written on the host and copied into the
/// volume by a prep container. One rig (a plain directory — the `dev` agent
/// pins `isolation: none`, so no worktree and no git repo is needed), one
/// agent, and a worker script that speaks the camp worker contract.
fn write_fixture(dir: &Path) {
    std::fs::write(
        dir.join("camp.toml"),
        "[camp]\nname = \"smoke\"\n\n\
         [[rigs]]\nname = \"demo\"\npath = \"/camp/rig\"\nprefix = \"d\"\n\n\
         [dispatch]\nmax_workers = 1\ncommand = \"/camp/worker.sh\"\ndefault_agent = \"dev\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dev.md"),
        "---\nname: dev\nisolation: none\n---\nDo the work.\n",
    )
    .unwrap();
    // campd sets CAMP_DIR/CAMP_BEAD/CAMP_SESSION in the worker's env and passes
    // claude-style argv, which a fake worker ignores (same contract as
    // tests/fake-agent.sh). claim -> close is the whole worker.
    std::fs::write(
        dir.join("worker.sh"),
        "#!/bin/sh\nset -eu\n\
         /usr/local/bin/camp claim \"$CAMP_BEAD\" --session \"$CAMP_SESSION\"\n\
         /usr/local/bin/camp close \"$CAMP_BEAD\" --outcome pass --reason \"container smoke\"\n",
    )
    .unwrap();
}

fn events(container: &str) -> Vec<serde_json::Value> {
    docker_ok(&["exec", container, "camp", "events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Harness-side waiting (camp itself never polls; tests may). Panics with the
/// container's logs so a failure is diagnosable without a rerun.
fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if done() {
            return;
        }
        if Instant::now() > deadline {
            let logs = docker(&["logs", CONTAINER]);
            panic!(
                "timed out after {timeout:?} waiting for {what}\n--- container stdout ---\n{}\n\
                 --- container stderr ---\n{}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr),
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "opt-in, local-only: builds and runs Docker (make container-smoke)"]
fn reference_container_serves_a_camp_and_stops_gracefully() {
    require_opt_in();
    let root = repo_root();
    let fixture = tempfile::tempdir().unwrap();
    write_fixture(fixture.path());

    docker_cleanup(&["rm", "-f", CONTAINER]);
    docker_cleanup(&["volume", "rm", "-f", VOLUME]);
    let _cleanup = Cleanup;

    // 1. The image builds from the repo root (the Dockerfile compiles the
    //    workspace; the build context is therefore the repo, not contrib/).
    docker_ok(&[
        "build",
        "-f",
        "contrib/docker/Dockerfile",
        "-t",
        IMAGE,
        root.to_str().unwrap(),
    ]);

    // 2. Prepare the camp ON the volume, before the container ever starts, so
    //    that the entrypoint meets an EXISTING camp — the restart path.
    docker_ok(&["volume", "create", VOLUME]);
    let vol_mount = format!("{VOLUME}:/camp");
    let fixture_mount = format!("{}:/fixture:ro", fixture.path().to_str().unwrap());
    docker_ok(&[
        "run", "--rm", "-v", &vol_mount, "--entrypoint", "camp", IMAGE,
        "init", "--camp", "/camp", "--no-service",
    ]);
    docker_ok(&[
        "run", "--rm", "-v", &vol_mount, "-v", &fixture_mount, "--entrypoint", "sh", IMAGE, "-c",
        "cp /fixture/camp.toml /camp/camp.toml && mkdir -p /camp/agents /camp/rig && \
         cp /fixture/dev.md /camp/agents/dev.md && cp /fixture/worker.sh /camp/worker.sh && \
         chmod +x /camp/worker.sh",
    ]);

    // 3. Start the real thing: the image's own entrypoint, nothing overridden.
    docker_ok(&["run", "-d", "--name", CONTAINER, "-v", &vol_mount, IMAGE]);

    // 4. The entrypoint's init found the camp and said so instead of dying,
    //    and campd came up and announced its socket — both on stdout.
    wait_for("campd to announce its socket", Duration::from_secs(60), || {
        let logs = docker(&["logs", CONTAINER]);
        String::from_utf8_lossy(&logs.stdout).contains("campd listening on /camp/campd.sock")
    });
    let logs = docker(&["logs", CONTAINER]);
    let stdout = String::from_utf8_lossy(&logs.stdout).into_owned();
    assert!(
        stdout.contains("already exists"),
        "the entrypoint's `camp init --exists-ok` must be a no-op success on the existing \
         camp (a bare `camp init` would exit 1 and crash-loop the container); logs were:\n{stdout}"
    );

    // 5. Drive it the documented way: the CLI is a pure socket client, and
    //    `docker exec` puts it on the same side of the socket as campd.
    //    ($CAMP_DIR is set in the image, so no --camp is needed.)
    docker_ok(&["exec", CONTAINER, "camp", "sling", "smoke: dispatch a bead"]);

    // 6. campd dispatched it and the in-container worker closed it.
    wait_for("the bead to be dispatched and closed", Duration::from_secs(60), || {
        let evs = events(CONTAINER);
        evs.iter().any(|e| e["type"] == "bead.claimed")
            && evs.iter().any(|e| e["type"] == "bead.closed")
    });
    let evs = events(CONTAINER);
    let closed = evs.iter().find(|e| e["type"] == "bead.closed").unwrap();
    assert_eq!(
        closed["data"]["outcome"], "pass",
        "the worker closed the bead pass; events were: {evs:#?}"
    );

    // 7. `docker stop` = SIGTERM to campd (tini forwards it to its only child,
    //    which the entrypoint exec'd). Graceful means: quick, exit 0, and the
    //    shutdown is IN THE LEDGER.
    let stop_started = Instant::now();
    docker_ok(&["stop", CONTAINER]);
    let stop_took = stop_started.elapsed();
    assert!(
        stop_took < GRACEFUL_STOP_MAX,
        "docker stop took {stop_took:?} — campd did not answer SIGTERM promptly (an ignored \
         SIGTERM shows up as the full 10 s grace, then SIGKILL)"
    );

    let code = docker_ok(&["inspect", "-f", "{{.State.ExitCode}}", CONTAINER]);
    assert_eq!(
        code.trim(),
        "0",
        "the container must exit 0 on SIGTERM (137 = SIGKILL after the grace period)"
    );

    // The ledger and the socket, read from the volume after the fact.
    let after = docker_ok(&[
        "run", "--rm", "-v", &vol_mount, "--entrypoint", "camp", IMAGE,
        "events", "--camp", "/camp", "--json",
    ]);
    assert!(
        after.lines().any(|l| l.contains("\"campd.stopped\"")),
        "SIGTERM must append campd.stopped to the ledger; events were:\n{after}"
    );
    docker_ok(&[
        "run", "--rm", "-v", &vol_mount, "--entrypoint", "sh", IMAGE, "-c",
        "test ! -e /camp/campd.sock",
    ]);
}
```

- [ ] **Step 2: Add the Makefile target, then run the test to watch it fail**

In `Makefile`, add `container-smoke` to the `.PHONY` line (Phase 2 made it `.PHONY: install uninstall perf e2e service-e2e` — re-read it) and append the target below `e2e`:

```makefile
# Opt-in reference-container smoke (design §9). LOCAL-ONLY and never in CI: the
# test is #[ignore]d AND gated on CAMP_CONTAINER_E2E=1. It builds
# contrib/docker/Dockerfile, runs the image, slings a bead over the
# in-container socket, and asserts `docker stop` is a graceful SIGTERM (exit 0,
# campd.stopped in the ledger). Requires `docker` on PATH.
container-smoke:
	CAMP_CONTAINER_E2E=1 cargo test -p camp --test container_smoke -- --ignored --nocapture --test-threads=1
```

Run: `make container-smoke`
Expected: **FAIL** — `docker build … failed`, with docker's own stderr in the panic message: `failed to read dockerfile: open contrib/docker/Dockerfile: no such file or directory`. That is the red state; the image does not exist yet.

- [ ] **Step 3: Keep `target/` out of the build context**

Create `.dockerignore` at the **repo root** (the build context is the workspace, because the Dockerfile compiles it):

```
# The build context for contrib/docker/Dockerfile is the repo root. Keep the
# host's build artifacts and history out of it — they are megabytes of noise
# the image never uses, and target/ would bust the layer cache on every build.
target/
.git/
.github/
docs/
packs/
plugin/
```

- [ ] **Step 4: Write the Dockerfile**

Create `contrib/docker/Dockerfile`:

```dockerfile
# Gas Camp — the reference container (v1 spec §5; campd-service design §3, §7).
#
# The container runtime IS the supervisor. campd is the container's main
# process: it gets `docker stop`'s SIGTERM directly and shuts down gracefully
# (appends campd.stopped, unlinks the socket, exits 0), and compose's
# `restart: unless-stopped` is this environment's KeepAlive. There is no
# service manager inside the container and there is nothing to install.
#
# Build from the REPO ROOT — the build context is the whole workspace:
#   docker build -f contrib/docker/Dockerfile -t gascamp:latest .
# See contrib/docker/README.md.

# ---- build ----------------------------------------------------------------
FROM rust:1-slim-bookworm AS build

# rusqlite is `bundled` (crates/camp-core/Cargo.toml): SQLite is compiled from
# C, so the builder needs a C toolchain. Named explicitly rather than assumed
# from the base image.
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
# rust-toolchain.toml comes along on purpose: the image builds camp with the
# toolchain the repo pins, not whatever the base image happened to ship.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p camp

# ---- runtime --------------------------------------------------------------
FROM debian:bookworm-slim

# git             campd shells out to `git rev-parse --verify HEAD^{commit}` on
#                 EVERY dispatch to read the rig's base commit
#                 (daemon/spawn.rs::rig_base), and to `git worktree add` for the
#                 default isolation (spec §12). No git, no dispatch.
# tini            a real init as PID 1. Belt and braces: campd reaps its own
#                 workers (SIGCHLD self-pipe) and handles SIGTERM (Phase 1), so
#                 it is PID-1-safe on its own — tini just means you never have
#                 to think about it. `docker run --init` / compose `init: true`
#                 is the same thing by another route.
# ca-certificates for the worker you wire into [dispatch].command (a real
#                 Claude Code worker talks TLS). camp itself opens no sockets
#                 but the unix one.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/camp /usr/local/bin/camp
# The same `campd` argv0 alias `make install` ships: main.rs keys the daemon
# path off a "campd" file stem, so `ps` shows what is actually running.
RUN ln -sf camp /usr/local/bin/campd

COPY contrib/docker/entrypoint.sh /usr/local/bin/camp-entrypoint
RUN chmod 0755 /usr/local/bin/camp-entrypoint

# A non-root camp user owns /camp. A NAMED VOLUME mounted there inherits this
# directory's ownership when Docker first initializes it, so campd can write the
# ledger, the socket, runs/ and sessions/ with no chown dance. (A BIND mount
# does not: the host's ownership wins — see README.md.)
RUN useradd --create-home --uid 10001 camp \
 && mkdir -p /camp \
 && chown camp:camp /camp
USER camp

# HOME must exist and be writable: campd computes the worker transcript path
# under $HOME/.claude (daemon/spawn.rs::claude_config_root — a HARD ERROR when
# HOME is unset) and health patrol creates that directory (daemon/patrol.rs).
# CAMP_DIR is why `docker exec <c> camp sling "…"` needs no --camp flag.
ENV HOME=/home/camp \
    CAMP_DIR=/camp

VOLUME ["/camp"]

# tini forwards SIGTERM to its only child — the campd the entrypoint exec'd —
# and exits with campd's status, so `docker stop` yields exit code 0.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/camp-entrypoint"]
```

- [ ] **Step 5: Write the entrypoint**

Create `contrib/docker/entrypoint.sh`:

```sh
#!/bin/sh
# The camp container's entrypoint (v1 spec §5; campd-service design §7).
#
# Two lines of work, and the second one is the important one:
#
#   1. Make sure the camp exists. `--exists-ok` is what makes a RESTART work:
#      the camp dir is a volume, so on the second start the camp is already
#      there, and a bare `camp init` would exit 1 and crash-loop the container.
#      `--no-service` because there is no host service manager in here and none
#      is wanted — the container runtime is the supervisor.
#   2. BECOME campd. `exec` matters: campd must be tini's direct child, so that
#      `docker stop`'s SIGTERM lands on campd itself (graceful shutdown, spec
#      §5) instead of on a shell that would ignore it and get SIGKILLed.
#
# Anything you need before campd starts (a rig checkout, credentials for the
# worker in [dispatch].command) belongs in front of the exec, or in an image
# built FROM this one.
set -eu

: "${CAMP_DIR:?CAMP_DIR must name the camp directory (the image sets it to /camp)}"

camp init --camp "$CAMP_DIR" --no-service --exists-ok

exec camp daemon --camp "$CAMP_DIR"
```

Make it executable in git:

```bash
chmod +x contrib/docker/entrypoint.sh
```

- [ ] **Step 6: Run the smoke test — this is the real red-to-green**

Run: `make container-smoke`
Expected: PASS, in a few minutes (the first build compiles the workspace inside the image). The `--nocapture` output ends with the test name and `ok`.

If it fails, the panic message carries docker's stderr and the container's logs. **Do not weaken an assertion to get green.** The likely honest failures and what they mean:
- `docker: command not found` → docker is not installed/running. That is your environment, not the code.
- campd never announces on the socket → read `docker logs`; the entrypoint or the camp dir permissions are wrong.
- `bead.claimed` never appears → read `camp events --json` in the container for a `dispatch.failed` event; its `data.reason` names the cause (a missing `git`, an unwritable `$HOME`, a bad `[dispatch] command`) — fix the image, not the test.
- `docker stop` slow / exit 137 → SIGTERM did not reach campd. Check that the entrypoint `exec`s and that `ENTRYPOINT` is the exec-form JSON array (a shell-form `ENTRYPOINT` would wrap it in `/bin/sh -c` and swallow the signal).

- [ ] **Step 7: Write `compose.yaml`**

Create `contrib/docker/compose.yaml`:

```yaml
# Gas Camp — the reference compose file (campd-service design §3, §4.7, §7).
#
#   docker compose -f contrib/docker/compose.yaml up -d --build
#   docker compose -f contrib/docker/compose.yaml exec campd camp sling "fix the flaky test"
#   docker compose -f contrib/docker/compose.yaml logs -f campd
#   docker compose -f contrib/docker/compose.yaml down          # SIGTERM: graceful
#
# `restart: unless-stopped` is this environment's KeepAlive — the container
# runtime is the supervisor, and campd, the container's main process, is what it
# keeps alive. Nothing here installs a service unit; there is no service manager
# inside the container and camp does not want one (`camp init --no-service`, in
# the entrypoint).
services:
  campd:
    build:
      context: ../..
      dockerfile: contrib/docker/Dockerfile
    image: gascamp:latest
    container_name: gascamp
    restart: unless-stopped
    # The defaults, named because they are the contract: campd handles SIGTERM
    # (v1 spec §5) — it appends campd.stopped, unlinks the socket, and exits 0.
    # The 30 s is a CEILING for a shutdown that has real work to flush, not a
    # target: a healthy campd exits in well under a second (the smoke test
    # asserts under 5 s against a plain `docker stop`'s 10 s default). If you
    # ever see a 137 exit, SIGTERM is not reaching campd and something has
    # broken the entrypoint's `exec`.
    stop_signal: SIGTERM
    stop_grace_period: 30s
    volumes:
      # The camp: the SQLite ledger, campd.sock, runs/, sessions/, worktrees/.
      # A NAMED volume, so it survives `down`/`up` and a host reboot, and so the
      # container user owns it (see README.md before you swap in a bind mount).
      - camp:/camp

volumes:
  camp:
```

- [ ] **Step 8: Verify compose the same way you verified the image**

```bash
docker compose -f contrib/docker/compose.yaml up -d --build
docker compose -f contrib/docker/compose.yaml logs campd
```
Expected: the logs show `campd listening on /camp/campd.sock`.

```bash
docker compose -f contrib/docker/compose.yaml exec campd camp top
```
Expected: a `campd pid: …` status snapshot — the CLI, as a pure socket client, got an answer from campd over the in-container socket.

```bash
time docker compose -f contrib/docker/compose.yaml down
docker compose -f contrib/docker/compose.yaml ps -a
```
Expected: `down` returns in about a second (not ~10), and the container is gone. Then clean up the volume:
```bash
docker volume rm docker_camp || docker volume ls   # the project-prefixed volume name
```

- [ ] **Step 9: Write `contrib/docker/README.md` — including what does NOT work**

Create `contrib/docker/README.md`:

```markdown
# camp in a container — the reference setup

The container runtime is a **supervisor**, exactly like launchd or systemd
`--user` (v1 spec §5): `camp daemon` is the container's main process, the
runtime restarts it if it dies, and `docker stop` is a SIGTERM campd answers by
appending `campd.stopped`, unlinking its socket, and exiting 0. There is no
service manager inside the container and camp does not install one — the
entrypoint runs `camp init --no-service --exists-ok`, then `exec`s campd.

## Run it

```sh
docker compose -f contrib/docker/compose.yaml up -d --build
docker compose -f contrib/docker/compose.yaml logs -f campd     # "campd listening on /camp/campd.sock"
```

Or without compose:

```sh
docker build -f contrib/docker/Dockerfile -t gascamp:latest .   # from the repo root
docker volume create camp
docker run -d --name gascamp --restart unless-stopped -v camp:/camp gascamp:latest
```

## Drive it

The camp CLI is a **pure socket client**: it talks to campd over
`<camp>/campd.sock` and never starts it. Inside the container the socket is
right there, so `docker exec` is the way in (the image sets `CAMP_DIR=/camp`, so
no `--camp` flag is needed):

```sh
docker exec gascamp camp sling "fix the flaky auth test"
docker exec gascamp camp top
docker exec gascamp camp ls --ready
docker exec gascamp camp events --json | tail -5
```

## Stop it

```sh
docker stop gascamp        # SIGTERM -> graceful: campd.stopped in the ledger, exit 0
```

`camp stop` inside the container also works (this camp is unsupervised as far as
camp is concerned — the supervisor is outside it), but the runtime will restart
campd if you asked for `restart: unless-stopped`. **Stop the container, not the
daemon.**

## Make it useful: a rig and a worker

The image ships `camp` and `git` and nothing else. A camp that dispatches real
work needs three things in `/camp/camp.toml`, all of which you can put on the
volume before the first start (or edit live — campd hot-reloads `camp.toml`):

```toml
[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/rigs/gascity"       # mount your repo here
prefix = "gc"

[dispatch]
command = "claude"           # the worker executable (the default)
default_agent = "dev"
```

...plus an agent definition in `/camp/agents/dev.md`. `command = "claude"` means
the image needs the Claude Code CLI and its credentials: build an image `FROM
gascamp:latest` that installs it, and mount the credentials in. The reference
image deliberately stops short of that — it is the supervision reference, not a
worker-provisioning one — and campd will tell you the truth if the worker is
missing: a `dispatch.failed` event whose `reason` names the failure, in the
ledger, where every camp failure goes.

## What does NOT work — read this before you mount the camp dir on the host

- **Reaching the socket from the host is a Linux-only trick.** Bind-mount the
  camp dir (`-v /srv/camp:/camp`) and, on a **native Linux** host, a host-side
  `camp --camp /srv/camp top` can connect to `/srv/camp/campd.sock` — same
  kernel, same socket. On **Docker Desktop (macOS/Windows)** it cannot: the
  container's filesystem is shared into a VM, a unix socket created in there is
  not a socket the host can connect to, and SQLite's WAL locking is not safe
  across that share. Even on Linux, the host `camp` must be a build whose ledger
  `schema_version` matches the container's — opening a camp with a different
  schema version is a hard error, never an auto-upgrade (v1 spec §7.1). Use
  `docker exec` — it is the supported path everywhere, and it is by definition
  the same binary that wrote the ledger.
- **The container user owns the camp.** The image runs as uid 10001 (`camp`). A
  *named* volume inherits that ownership automatically. A *bind* mount does not
  — the host directory's ownership wins, so either `chown 10001` it or run with
  `--user "$(id -u):$(id -g)"` (and make `$HOME` writable for that uid, because
  campd puts worker transcripts under it).
- **Cross-host is out of scope.** campd serves a unix-domain socket, full stop;
  there is no network transport, so a CLI on another machine cannot reach it
  (campd-service design §11).
- **This is not a one-shot runner.** `docker run gascamp camp sling "…"` and exit
  is not a supported shape: camp is durable async work, and the bead only moves
  while campd runs (design §11). Keep the container up.

## Why tini

campd reaps its own worker children (a SIGCHLD self-pipe) and handles SIGTERM,
so it is PID-1-safe on its own. `tini` is belt and braces — it also means an
adopted orphan from a worker's own subprocess tree can never accumulate. If you
would rather not have it, `docker run --init` (or compose's `init: true`) does
the same job with the runtime's own init.

## The smoke test

`make container-smoke` builds this image, runs it, slings a bead over the
in-container socket, asserts the bead is dispatched and closed by a worker
inside the container, and asserts `docker stop` is fast, graceful, and exit 0.
It is opt-in and local-only (`CAMP_CONTAINER_E2E=1` + `#[ignore]`) — CI never
builds or runs Docker.
```

- [ ] **Step 10: Gates**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: clean. `cargo test --workspace` **compiles** `container_smoke.rs` but does not run its `#[ignore]`d test — that is the point.

Then prove the gating claim itself, rather than asserting it:
```bash
cargo test -p camp --test container_smoke 2>&1 | tail -3
```
Expected: `test result: ok. 0 passed; 0 failed; 1 ignored` — Docker is never touched without `make container-smoke`.

- [ ] **Step 11: Re-run the smoke test end to end and commit**

```bash
make container-smoke
```
Expected: PASS.

```bash
git add contrib/docker .dockerignore crates/camp/tests/container_smoke.rs Makefile
git commit -m "feat(contrib): reference container — campd as the container's main process, with an opt-in smoke test"
```

---

## Task 3: The v1 spec amendments — §5, §9, §12

AGENTS.md: *"If implementation reality contradicts the spec, stop and update the spec via PR in the same change: spec and code never silently diverge."* Phase 3 made the **minimal** correction (it deleted the two falsified passages). This task states the **positive** model the four phases actually built.

**Read `docs/design/2026-07-05-gas-camp-design.md` §5, §9 and §12 in full before editing.** Phase 3 has already rewritten the component-table `campd` row and replaced §5's `**Auto-start:**` bullet with a `**Pure client:**` bullet. **Do not re-edit either. Do not revert either.** This task *adds* to §5 and rewrites one bullet each in §9 and §12.

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md` (§5's `### campd lifecycle`, §9's away-mode bullet, §12's bullet list)

**Interfaces:** none. Documentation, in the same PR as the container it describes.

- [ ] **Step 1: Insert the supervised-process bullet at the top of §5's `### campd lifecycle`**

Find the heading `### campd lifecycle` and insert this **as the first bullet, immediately above the `- **Liveness is an answered request…` bullet**:

```markdown
- **campd is a supervised foreground process.** `camp daemon` — foreground,
  long-lived, socket-serving — is the one primitive, and *who supervises it* is
  environment-provided:

  | Environment | Supervisor | How campd runs |
  |---|---|---|
  | macOS desktop | launchd — a `KeepAlive` LaunchAgent | `camp init`, or `camp service install` |
  | Linux desktop/server | systemd `--user` — `Restart=always` | `camp init`, or `camp service install` |
  | Container | the container runtime (`restart: unless-stopped`, K8s) | `camp daemon` **is** the container's main process (`contrib/docker/`) |
  | CI / bare box | you | `camp daemon`, run in the foreground |

  `camp init` detects a usable host service manager and installs + starts the
  unit; where there is none (a container, CI), it says so on stderr and hands
  off — a different supervisor, not a failure. `camp service
  {install,uninstall,status,restart,list,stop,start}` is the control surface,
  and the installed units are its registry (no status files, §13).
  **Always-on is not idle cost.** A supervised campd runs continuously, and
  invariant 1 measures what that costs: it sleeps on OS events, never ticks, and
  idles at < 20 MB RSS and 0.0% CPU. What always-on buys is that orders fire
  (§9) and that the daemon can be managed, upgraded, and restarted like any
  other service. What it costs is one process per camp — which is why one
  standalone camp with many rigs is the recommended shape (§12).
```

- [ ] **Step 2: Verify §5 now reads as one story**

```bash
sed -n '/^### campd lifecycle/,/^## 6\./p' docs/design/2026-07-05-gas-camp-design.md
```
Expected, in order: the new **supervised foreground process** bullet, then **Liveness is an answered request** (untouched), then Phase 3's **Pure client** (untouched), then **Crash-only design** (untouched). No sentence anywhere in there says the CLI starts campd.

- [ ] **Step 3: Rewrite §9's away-mode bullet**

In `## 9. Orders`, replace the whole `- **Away-mode is the same code path.**` bullet (it ends with "…a powered-off laptop fires nothing until wake (catch-up policy applies)."):

Old:
```markdown
- **Away-mode is the same code path.** An order fires, `campd` cooks and
  dispatches headless workers, everything lands in the ledger. You come
  back, `/status` shows what happened, and every worker it spawned is
  resumable. Limits stated honestly: with the default on-demand daemon,
  orders fire only while `campd` is running (from first `camp` use until
  `camp stop`/reboot); install the optional launchd agent for
  fire-at-login coverage; a powered-off laptop fires nothing until wake
  (catch-up policy applies).
```
New:
```markdown
- **Away-mode is the same code path.** An order fires, `campd` cooks and
  dispatches headless workers, everything lands in the ledger. You come
  back, `/status` shows what happened, and every worker it spawned is
  resumable. Limits stated honestly, and supervision (§5) removes the worst
  one: a **supervised** campd is kept alive by launchd, systemd `--user`, or
  the container runtime, so it is up at login, up again after a crash, and up
  after a reboot — scheduled orders fire without anyone running a `camp`
  command first. Where there is **no supervisor** — a bare box, CI, a container
  you did not keep running — the old limit stands unchanged: orders fire only
  while a `camp daemon` you started is alive, and no wake source means no fire.
  In every case a powered-off or sleeping machine fires nothing until wake, and
  what it then catches up on is the `catch_up_window` policy above.
```

- [ ] **Step 4: Add §12's daemon-count bullet**

In `## 12. Multi-rig and worktrees`, insert this **immediately after the first bullet** (`- A camp dir stands alone (~/camps/dev/ + camp rig add ~/code/gascity) or lives repo-local (.camp/, rig = self).`):

```markdown
- **One standalone camp with many rigs is the recommended shape**, and
  supervision (§5) is why: a supervised campd is always on, so the number of
  standing daemons is exactly the number of camps. One camp
  (`~/camps/dev/` + a `camp rig add` per repo) is one supervised campd across
  every repo you work in — one ledger, one place to look, scoped queries by rig
  (below). Repo-local `.camp/` still works and is still right for a repo you
  want self-contained; it just costs one supervised daemon each, and
  `camp service list` is where you will notice that count growing.
```

- [ ] **Step 5: Verify the diff touched only what it should**

```bash
git diff --stat docs/design/2026-07-05-gas-camp-design.md
grep -n -i "auto-start\|autostart\|on demand\|on-demand" docs/design/2026-07-05-gas-camp-design.md
```
Expected: one file changed; **exactly one** grep hit — `camp show --wait`'s `never autostarts campd`, which is still true. If §9's `with the default on-demand daemon` survives, Step 3 did not land.

```bash
git diff docs/design/2026-07-05-gas-camp-design.md | grep '^-' | grep -v '^---'
```
Expected: the removed lines are **only** the old §9 away-mode bullet. §5 and §12 are pure insertions; nothing Phase 3 wrote is removed.

- [ ] **Step 6: Commit**

```bash
git add docs/design/2026-07-05-gas-camp-design.md
git commit -m "docs: v1 spec — campd is supervised (§5), orders fire under supervision (§9), one camp many rigs (§12)"
```

---

## Task 4: Supersede `contrib/launchd/`, and finish the reconciliation

`contrib/launchd/` ships a hand-editable LaunchAgent whose defining comment is
*"Deliberately NO KeepAlive: `camp stop` must stay stopped. A campd that exits is restarted on demand by the next camp verb (spec §5)."*
Both halves of that are now false: `camp service install` generates a **KeepAlive** unit (Phase 2), and there is no next-camp-verb restart (Phase 3). A shipped example that fights the supervisor is a trap, and the feature design (§8) says to fold it into the `camp service` docs. So: it goes, and `camp service install` — which does the same job correctly, for launchd *and* systemd, with a stable per-camp id — takes its place.

Then every remaining sentence that still describes a campd that starts itself — **including the two that fall through every other phase's net**: a live code comment in `cmd/nudge.rs` and a `docs/design/` file that names a function Phase 3 deletes. Neither is reachable by Phase 3's sweep grep (`auto-start|autostart|auto started|auto start`), because neither uses those words.

**Files:**
- Delete: `contrib/launchd/README.md`, `contrib/launchd/com.gascamp.campd.plist.example`
- Modify: `README.md` (three passages, anchored by text — **their line numbers have moved**; plus one new pointer)
- Modify: `packs/starter/README.md` (one sentence)
- Modify: `crates/camp/src/cmd/nudge.rs` (one comment — the `(on-demand daemon)` justification; the behavior is correct and stays)
- Modify: `docs/design/2026-07-09-dispatch-lifecycle.md` (one clause — `request_with_autostart(..., Poke)`, a function Phase 3 deletes)

**Interfaces:** none.

- [ ] **Step 1: Confirm what still needs fixing (Phase 2/3 may have taken some of it)**

```bash
grep -rn -i "on demand\|on-demand\|only while there's work\|fire-at-login\|launchd" README.md packs/ contrib/
grep -n "on-demand daemon" crates/camp/src/cmd/nudge.rs
grep -n "request_with_autostart" docs/design/2026-07-09-dispatch-lifecycle.md
```
Expected: the four README/packs sentences from Task 0's table, the two `contrib/launchd/` files, `nudge.rs`'s comment, and `dispatch-lifecycle.md`'s clause. **Fix exactly what the greps show.** If a sentence is already correct, skip it and say so in the PR body.

- [ ] **Step 2: Delete the superseded example**

```bash
git rm -r contrib/launchd
```

Expected: both files staged for deletion; `contrib/` now holds only `docker/`.

- [ ] **Step 3: `README.md` — the feature bullet**

Find: `` while you're away, with an optional launchd agent for fire-at-login. ``
Replace that line (it is the tail of the "Cron & event orders" bullet) with:

```markdown
  while you're away — campd is supervised (launchd, systemd `--user`, or your
  container runtime), so orders fire whether or not you ran a `camp` command.
```

- [ ] **Step 4: `README.md` — the daemon-model paragraph**

Find: `` `campd` is the only standing process, and only while there's work: it watches ``
Replace the whole three-line paragraph:

Old:
```markdown
`campd` is the only standing process, and only while there's work: it watches
the ledger, dispatches ready work, schedules orders, and arms stall timers —
all event-driven, never on a tick.
```
New:
```markdown
`campd` is the only standing process: it watches the ledger, dispatches ready
work, schedules orders, and arms stall timers — all event-driven, never on a
tick. It is **supervised** — by launchd, by systemd `--user`, or by your
container runtime — so it is up whenever the machine is, and it costs nothing
to leave up: an idle campd sleeps on OS events at 0.0% CPU and under 20 MB.
```

- [ ] **Step 5: `README.md` — the orders "honest limits" paragraph**

Find: `` Honest limits: with the default on-demand daemon, orders `` (in the Orders section, after "**Away-mode is the same code path**").
Replace from `Honest limits:` to the end of that paragraph:

Old:
```markdown
lands in the ledger. Honest limits: with the default on-demand daemon, orders
fire only while `campd` is running; install the optional launchd agent
([contrib/launchd/](contrib/launchd/README.md)) for fire-at-login coverage; a
powered-off laptop fires nothing until wake.
```
New:
```markdown
lands in the ledger. Honest limits: a supervised campd (`camp service install`,
or a container with `restart: unless-stopped`) is kept alive by its supervisor,
so orders fire at login, after a crash, and after a reboot without you running
anything. Where there is no supervisor — CI, a bare box, a container you did not
keep running — orders fire only while a `camp daemon` you started is alive. And
a powered-off or sleeping machine fires nothing until wake, when the catch-up
window applies.
```

- [ ] **Step 6: `README.md` — point at the container**

Under `### campd & the daemon model`, **after** the `#### Supervised campd — camp service` subsection Phase 2 added (re-read the section first; put this immediately below it), add:

```markdown
#### In a container

The container runtime is just another supervisor: campd is the container's main
process, `restart: unless-stopped` is its KeepAlive, and `docker stop` is a
SIGTERM campd answers gracefully. A reference `Dockerfile`, entrypoint and
`compose.yaml` ship in [contrib/docker/](contrib/docker/README.md):

    docker compose -f contrib/docker/compose.yaml up -d --build
    docker exec gascamp camp sling "fix the flaky auth test"
    docker stop gascamp     # graceful: campd.stopped in the ledger, exit 0

The CLI is a pure socket client, so drive the camp with `docker exec` — that
puts the CLI on the same side of `<camp>/campd.sock` as campd. Reaching the
socket from the host means bind-mounting the camp dir and works only on a native
Linux host; cross-host access is out of scope (there is no network transport).
```

- [ ] **Step 7: `packs/starter/README.md`**

Find: `` Install the launchd agent for fire-at-login. ``
Replace the bullet's tail:

Old:
```markdown
- `orders.toml` is an example; a powered-off or logged-out machine fires no
  orders until wake (spec §9). Install the launchd agent for fire-at-login.
```
New:
```markdown
- `orders.toml` is an example; a powered-off or sleeping machine fires no
  orders until wake (spec §9). A supervised campd (`camp init`, or `camp
  service install` on an existing camp) fires them from login onward — no
  `camp` command needed first.
```

- [ ] **Step 8: `crates/camp/src/cmd/nudge.rs` — the comment that outlived the concept**

This is a **live code comment in shipped source** asserting a thing the codebase no longer contains. Phase 3's sweep grep cannot see it (it says "on-demand daemon", not "auto-start"), and Phase 3's file list names `nudge.rs` only at its module doc. So it is ours.

The behavior is **correct and does not change**: `nudge` calls `socket::request_if_up` (not Phase 3's `socket::require`), and a campd that is down genuinely *is* a normal state for this verb — the live path needs campd's held stdin pipe, and no campd means no pipe, so `Ok(None)` correctly routes to the resume path. Only the *reason given* is dead.

Find the comment inside `run`'s `if row.status == "live" {` block:

Old:
```rust
        // campd-not-listening is a normal state (on-demand daemon), and a
        // fresh campd holds no pipes — Ok(None) routes to resume (A4).
```
New:
```rust
        // A down campd is a normal state for THIS verb — it never requires the
        // daemon — and a fresh campd holds no pipes anyway: Ok(None) routes to
        // resume (A4).
```

Run: `cargo test -p camp --test cli_nudge`
Expected: PASS (a comment change; the test proves it stayed a comment change).

- [ ] **Step 9: `docs/design/2026-07-09-dispatch-lifecycle.md` — the design doc that names a deleted function**

The doc's double-dispatch analysis says `camp sling` *"immediately **pokes campd** via `request_with_autostart(..., Poke)`"*. Phase 3 **deletes** `daemon/autostart.rs`; both sling poke sites now call `socket::require(camp, &Request::Poke { seq })`. AGENTS.md: *spec and code never silently diverge* — so we correct the clause. **Only the clause**: the doc's argument (the plugin path and campd's dispatcher race) is unchanged and still true, and this is not a licence to rewrite a dated decision record.

Old:
```markdown
(with the routed agent as `assignee`) and immediately **pokes campd** via
`request_with_autostart(..., Poke)`. On that poke, `campd`'s dispatcher
```
New:
```markdown
(with the routed agent as `assignee`) and immediately **pokes campd** via
`socket::require(..., Poke)` — the pure-client poke (campd down is a loud
error, never a spawn). On that poke, `campd`'s dispatcher
```

(The `crates/camp/src/cmd/sling.rs:98-107` line reference in the sentence above it is a dated citation in a dated document; leave it.)

- [ ] **Step 10: The reconciliation gates — five scopes, each with its literal expected output**

A gate that expects "nothing" where something must legitimately survive is not a gate — it is a trap that trains the next implementer to delete load-bearing code. **These five were run by the plan's author against a simulated post-Phase-2/3/4 tree; the outputs below are measured, not guessed.** Every surviving line is named in the disposition table under Gate E. **If a gate prints a line the table does not name, that line is a stale claim: fix it or report it. If a gate prints FEWER lines than the table names, something load-bearing was deleted: put it back.**

```bash
TOK="auto-start\|autostart\|auto started\|auto start\|on demand\|on-demand\|only while there's work\|fire-at-login"

# Gate A — user-facing prose. Everything a user can read.
grep -rni "$TOK" README.md packs/ plugin/ Makefile contrib/
```
**Expected: EXACTLY ONE LINE.**
```
plugin/commands/adopt.md:6:(spec §8.5) — the routine campd runs at startup, on demand:
```
That is the **adoption** routine (spec §8.5) — a different, still-true sense of the phrase: campd runs adoption at startup *and* on request. It is not about starting the daemon.

```bash
# Gate B — the camp binary crate.
grep -rni "auto-start\|autostart\|auto started\|auto start\|on demand\|on-demand" crates/camp/src
```
**Expected: EXACTLY FIVE LINES** — the four adopt-sense comments and **the one frozen line**:
```
crates/camp/src/cmd/adopt.rs:8:/// §8.5) — the routine campd runs automatically at start, on demand.
crates/camp/src/daemon/mod.rs:305:        // Phase 11: adopt on demand — a fresh camp reconciles to zeros
crates/camp/src/daemon/event_loop.rs:541:                // The startup routine, on demand (spec §8.5). Its events
crates/camp/src/daemon/socket.rs:37:    /// same routine campd runs at startup, on demand (Phase 11).
crates/camp/src/daemon/socket.rs:285:/// a poke never auto-starts the daemon.
```
**`socket.rs:285` is FROZEN — `poke_best_effort`'s doc, spec §7.2's one sanctioned ignore-the-error site, which the operator froze *including its doc comment* and which Phase 3's own Gate B names as "the ONE permitted survivor". It is still true. DO NOT EDIT IT TO QUIET THIS GATE.** And `cmd/nudge.rs` must **not** appear — Step 8 fixed it.

```bash
# Gate C — the test crate and camp-core. Only the historical EVENT NAME survives.
grep -rni "auto-start\|autostart\|auto started\|auto start\|on demand\|on-demand" crates/camp/tests crates/camp-core | grep -vi "autostarted"
```
**Expected: NO OUTPUT.**
What the filter removes, and why every one of them must stay: `EventType::CampdAutostarted`, the `vocab.rs` entry, the `fold.rs` arm and its `campd_autostarted` fn, `ledger/mod.rs`'s `campd_autostarted_is_validated_and_log_only` test, and Phase 3's guard assertions that name `campd.autostarted` in order to assert nothing ever emits it again. **`EventType::parse` hard-errors on an unknown event name: delete the variant and every ledger that ever recorded one becomes unreadable. Deleting them is never the fix — that is why the filter is exact and not a convenience.**

```bash
# Gate D — the design docs.
grep -rni "auto-start\|autostart\|auto started\|auto start\|on demand\|on-demand" docs/design/
```
**Expected: EXACTLY FIVE LINES** (line numbers will differ — anchor on the text):
```
docs/design/2026-07-05-gas-camp-design.md:  …never autostarts campd — a pure observer of ground truth…
docs/design/2026-07-09-dispatch-lifecycle.md:  An **optional on-demand pack overseer agent** covers away-mode.          (×1)
docs/design/2026-07-09-dispatch-lifecycle.md:  …with an **optional on-demand …**                                        (×3 more)
```
1. `2026-07-05-…`: `camp show --wait` "never autostarts campd" — Phase 3 kept it deliberately and it is still **true** (`--wait` is a pure observer).
2. `2026-07-09-…` ×4 (its §"north star", the Q7 decision row, and two restatements): an **on-demand pack overseer *agent*** — a different sense entirely (an agent slung when needed), nothing to do with the daemon's lifecycle. True, and not ours to touch.
And `request_with_autostart` must **not** appear — Step 9 fixed it.

```bash
# Gate E — the launchd example is gone, and nothing points at its corpse.
test ! -e contrib/launchd && echo "contrib/launchd: gone"
grep -rn "contrib/launchd\|Deliberately NO KeepAlive" README.md packs/ plugin/ contrib/ crates/ docs/design/ Makefile
```
**Expected:** `contrib/launchd: gone`, and **NO hits** from the grep. (`docs/superpowers/{plans,specs}/` are historical records — excluded on purpose; they may still name it and must not be rewritten.)

**The disposition table — every line the five gates are allowed to print:**

| Survivor | Gate | Why it is legitimate |
|---|---|---|
| `plugin/commands/adopt.md` — "the routine campd runs at startup, on demand" | A | The **adoption** routine (§8.5), not daemon start-up. A different sense of the phrase; true. |
| `crates/camp/src/cmd/adopt.rs` — "the routine campd runs automatically at start, on demand" | B | Same. Phase 3 rewrites this file to `socket::require` and **keeps this clause**. |
| `crates/camp/src/daemon/mod.rs` — "Phase 11: adopt on demand" | B | Same. |
| `crates/camp/src/daemon/event_loop.rs` — "The startup routine, on demand (spec §8.5)" | B | Same. |
| `crates/camp/src/daemon/socket.rs` — "same routine campd runs at startup, on demand (Phase 11)" | B | Same (the `Request::Adopt` doc). |
| **`crates/camp/src/daemon/socket.rs` — "a poke never auto-starts the daemon"** | B | **OPERATOR-FROZEN.** `poke_best_effort`'s doc; spec §7.2's one sanctioned ignore-the-error site; Phase 3's Gate B names it "the ONE permitted survivor". Still true. **Do not touch.** |
| `crates/camp-core` — `CampdAutostarted` / `campd.autostarted` / `campd_autostarted` (event type, vocab entry, fold arm, ledger test) | C (filtered) | The **historical event name**. `EventType::parse` hard-errors on unknown names, so deleting it makes old ledgers unreadable. Load-bearing. |
| `crates/camp/tests` — the guard assertions naming `campd.autostarted` | C (filtered) | They name the event **in order to assert nothing emits it any more**. Deleting them deletes the guard. |
| `docs/design/2026-07-05-gas-camp-design.md` — `camp show --wait` "never autostarts campd" | D | Still true; Phase 3 deliberately kept it. |
| `docs/design/2026-07-09-dispatch-lifecycle.md` ×4 — "optional **on-demand** pack overseer agent" | D | An on-demand **agent**, not an on-demand **daemon**. A different sense; true; a dated decision record. |

- [ ] **Step 11: Gates and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: clean. (This task is documentation plus one comment; the gates prove it broke nothing.)

```bash
git add -A README.md packs/starter/README.md contrib/ crates/camp/src/cmd/nudge.rs \
          docs/design/2026-07-09-dispatch-lifecycle.md
git commit -m "docs: camp service install supersedes the launchd example; no doc or comment still promises a self-starting campd"
```

---

## Task 5: The gates, the local-only suites, and the PR

**Files:** none (verification + the PR).

- [ ] **Step 1: The full unit gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: all green.

- [ ] **Step 2: The local-only suites (AGENTS.md: run them before merging)**

```bash
make container-smoke     # this phase's own suite — MUST pass
make perf                # spec §14 numbers; this phase adds no daemon code, but prove it
```
Expected: both pass. `make e2e` (real `claude`, real money) is opt-in — run it if you have credentials; nothing in this phase touches the dispatch path it exercises.

- [ ] **Step 3: Confirm CI does not touch Docker**

```bash
git diff origin/main --stat -- .github/
grep -rn -i "docker" .github/workflows/ci.yml
```
Expected: **no diff under `.github/`**, and **no hits** for docker in the workflow. The container suite is opt-in and local-only, like `make e2e`.

- [ ] **Step 4: Re-run Task 4's five reconciliation gates on the final tree**

Everything has moved since Task 4 (Task 5 changed nothing, but a rebase might have). Re-run Gates A-E from Task 4 Step 10 and check each output against the disposition table there.
Expected: **A → 1 line; B → 5 lines (one of them the frozen `socket.rs` poke doc); C → no output; D → 5 lines; E → `contrib/launchd: gone` + no dangling references.** A line the table does not name is a stale claim; a missing line means something load-bearing was deleted. Either way: **stop, do not open the PR.**

- [ ] **Step 5: Push and open the PR**

```bash
git push -u origin phase-4-container-docs
gh pr create --title "campd service management, phase 4: the reference container + the spec reconciliation" --body "$(cat <<'EOF'
The last of four phases. The container runtime becomes a first-class campd
supervisor, and the docs stop describing a daemon that starts itself.

**`camp init --exists-ok`** — the container entrypoint re-runs `camp init` on
every start, and on a restart the camp already exists on the volume. `camp init`
hard-errors on that (it always has), so a bare init in the entrypoint would
crash-loop the container. `--exists-ok` makes an existing camp a no-op success:
one flag, one owner of the "is there a camp here" predicate, tested in
milliseconds instead of only inside a Docker run. Without the flag, the hard
error is byte-for-byte what it was.

**`contrib/docker/`** — a multi-stage `Dockerfile` (git and a writable `$HOME`
are not optional: campd runs `git rev-parse` on every dispatch and computes
worker transcript paths under `$HOME`), a 6-line entrypoint that `exec`s
`camp daemon` so campd — not a shell — receives SIGTERM, a `compose.yaml` with
`restart: unless-stopped`, and a README that is honest about the limits
(host-side socket access is a native-Linux-only trick; cross-host is out of
scope; this is not a one-shot runner).

**`make container-smoke`** — opt-in and local-only (`#[ignore]` +
`CAMP_CONTAINER_E2E=1`), exactly like `make e2e`: it builds the image, slings a
bead over the in-container socket, watches a worker inside the container claim
and close it, and asserts `docker stop` is fast, graceful, and exit 0 — Phase
1's SIGTERM handling, measured. **CI never builds or runs Docker.**

**The spec reconciliation** — v1 spec §5 gains the supervised-process model and
the supervision table (and says plainly that always-on is not idle cost); §9's
away-mode gap is narrowed to the unsupervised case, where it still holds
honestly; §12 recommends one standalone camp with many rigs, because a
supervised campd means the daemon count is the camp count. `contrib/launchd/`
is deleted: its central claim ("Deliberately NO KeepAlive: `camp stop` must stay
stopped") is the opposite of what `camp service install` now ships. The last
README and starter-pack sentences that promised a self-starting daemon are gone
— and so are the two that fell through every other phase's net: a live comment
in `cmd/nudge.rs` justifying itself with an "on-demand daemon" that no longer
exists, and a `docs/design/` file describing a poke through
`request_with_autostart(...)`, a function phase 3 deleted.

Five scoped reconciliation gates (`README`/`packs`/`plugin`/`Makefile`/`contrib`;
`crates/camp/src`; `crates/camp/tests` + `camp-core`; `docs/design`; the deleted
launchd example) each have a stated expected output and a disposition table
naming every legitimate survivor — including the operator-frozen
`poke_best_effort` doc and the `campd.autostarted` event vocabulary, which is
load-bearing (deleting it would make every ledger that ever recorded one
unreadable).
EOF
)"
```

- [ ] **Step 5: Watch CI**

```bash
gh pr checks --watch
```
Expected: green. Nothing is complete until it is pushed, CI is green, and every claim in the PR description is verified.

---

## Verified facts this plan is built on

Every claim below was read in the source on `main` @ `b0dc950`. Task 0 re-verifies each. **If one of them is false at execution time, the design that depends on it changes — stop and report.**

| Fact | Evidence |
|---|---|
| `camp init` **hard-errors** on an existing camp; it is not idempotent | `crates/camp/src/cmd/init.rs:14-16` — `if root.join("camp.toml").exists() \|\| root.join("camp.db").exists() { bail!("a camp already exists at {}", …) }`; pinned by `crates/camp/tests/cli_init.rs:189-203` (`reinit_fails_fast`: `.failure().code(1)`, stderr contains `"already"`). **Phase 2 keeps that `bail!` verbatim** (its plan, Task 5 Step 4). This is why Task 1 exists. |
| `--camp` is a **global** clap arg → `camp daemon --camp <dir>` parses | `crates/camp/src/main.rs:48` — `#[arg(long, global = true, value_name = "DIR")] camp: Option<PathBuf>` |
| `camp daemon` is a real subcommand (not only the `campd` argv0 alias) | `crates/camp/src/main.rs:248-251` — `#[command(visible_alias = "campd")] Daemon`, dispatched at `:578` to `run_daemon(cli.camp.as_deref())` |
| `$CAMP_DIR` resolves the camp → `docker exec <c> camp sling "…"` needs no flag | `crates/camp/src/campdir.rs:1-2, 47` — "`--camp` flag, then `$CAMP_DIR`, then walking up" |
| campd's readiness line goes to **stdout** → `docker logs` sees it | `crates/camp/src/daemon/mod.rs:28` `READY_PREFIX = "campd listening on "`; `:217-219` `writeln!(stdout, "{READY_PREFIX}{}", socket_path.display())` |
| The image **must have git** | `crates/camp/src/daemon/spawn.rs:321-330` — `rig_base` runs `git -C <rig> rev-parse --verify HEAD^{commit}` and is called from `Dispatcher::prepare` on **every** dispatch; a git that cannot be run is an `Err` → `dispatch.failed`, not a silent "no base" |
| The image **must set a writable `HOME`** | `crates/camp/src/daemon/spawn.rs:73-79` — `claude_config_root()`: `HOME` unset is a hard error ("cannot compute the worker transcript path"); `crates/camp/src/daemon/patrol.rs:484` — patrol `create_dir_all`s the transcript's parent under it |
| A single-rig camp needs no `--rig` on `camp sling` | `crates/camp/src/cmd/create.rs:58-67` — `resolve_rig`: `[only] => Ok(only)` |
| Dispatch needs a configured rig, an agent, and `[dispatch] command` | `crates/camp/src/daemon/dispatch.rs:513-524` (`route` → `resolve_agent` → `config.rig(&bead.rig)` → `rig.path.is_dir()`); `crates/camp-core/src/config.rs:47-48` (`command`, default `claude`) |
| `isolation: none` skips the worktree → the smoke's rig can be a plain dir | `crates/camp/src/daemon/dispatch.rs:546` — `let make_worktree = agent.isolation == Isolation::Worktree;` |
| The worker inherits campd's env, plus `CAMP_DIR` / `CAMP_BEAD` / `CAMP_SESSION` / `CAMP_TRANSCRIPT` | `crates/camp/src/daemon/spawn.rs:229-241`; no `env_clear` anywhere in `spawn.rs`. (`CAMP_BIN` is **not** set by campd — the test harnesses inject it; the smoke's worker calls `/usr/local/bin/camp` by absolute path and needs nothing.) |
| Event names the smoke asserts on | `crates/camp-core/src/event.rs:78-80, and the EventType list` — `bead.claimed`, `bead.closed`, `campd.stopped` |
| `rusqlite` is `bundled` → the builder needs a C toolchain; the runtime needs no libsqlite3 | `crates/camp-core/Cargo.toml` — `rusqlite = { version = "0.40.1", features = ["bundled"] }` |
| `Cargo.lock` is committed → `cargo build --locked` works in the image | `git ls-files Cargo.lock` → `Cargo.lock` |
| Opt-in suites are gated by `#[ignore]` + an env var + a `make` target | `Makefile:36-42` (`e2e`: `CAMP_E2E=1 cargo test … -- --ignored`); `crates/camp/tests/e2e.rs:1-7` ("LOCAL-ONLY and OPERATOR-GATED … CI compiles this file and runs ONLY the non-ignored tests"); Phase 2 adds `service-e2e` / `CAMP_SERVICE_E2E=1` the same way |
| CI runs no Docker today, and this phase adds none | `.github/workflows/ci.yml` — jobs `fmt`, `clippy`, `test`, `gc-compat`; no `docker` anywhere |
| The launchd example contradicts what camp now ships | `contrib/launchd/com.gascamp.campd.plist.example:21-22` — "Deliberately NO KeepAlive: `camp stop` must stay stopped. A campd that exits is restarted on demand by the next camp verb (spec §5)." Phase 2 ships `KeepAlive`; Phase 3 removes the CLI-spawn restart. |
| The README/pack stale sentences are **Phase 4's**, not Phase 2's | Phase 2's plan, "Open questions" §: *"`contrib/launchd/`'s superseded example, the `README.md` quickstart/orders text that still describes on-demand campd, and the `docs/design/…` §5/§9/§12 amendments are **Phase 4**."* |
| Phase 3 already corrected the design doc's component row + `**Auto-start:**` bullet, and **freezes** `contrib/` | Phase 3's plan, Task 5: *"line 126 and the `**Auto-start:**` bullet … Nothing else in this file. Nothing at all under `contrib/`."* |
| `poke_best_effort`'s doc is **operator-frozen**, and Phase 3's Gate B names it the one permitted survivor | Phase 3's plan, disposition table: *"`src/daemon/socket.rs:285` — **FROZEN — the ONE permitted survivor.** `poke_best_effort`'s doc; still true; spec §7.2. Gate B expects exactly this line and nothing else."* |
| Phase 3 **keeps** `cmd/adopt.rs`'s "on demand" clause (the adopt sense) | Phase 3's plan, Task 2's new `adopt.rs` doc: *"the routine campd runs automatically at start, on demand. A PURE CLIENT (design §4.3)…"* — so Gate B expects it. |
| `nudge` is a **pure observer of campd's liveness** — the stale comment is only its justification | `crates/camp/src/cmd/nudge.rs:39` — `socket::request_if_up(...)`, whose `Ok(None)` routes to the resume path; Phase 3's plan lists `cmd/nudge.rs` under *"Not in the blast radius (verified)"*. The behavior is right; the `(on-demand daemon)` reason is dead. |
| `campd.autostarted` is **load-bearing in camp-core** | `crates/camp-core/src/event.rs:25,56,87` (the variant + `parse` table + `as_str`), `vocab.rs:27`, `ledger/fold.rs:26,426-434`, `ledger/mod.rs:2837-2877` (the test proving old ledgers still fold). Phase 3's Gate D: *"All four must survive; do not delete them while chasing this gate."* |
| **The five reconciliation gates' expected output is measured, not guessed** | The plan's author built a post-Phase-2/3/4 tree (Phase 3's Gates A-D pin its auto-start state exactly; Phase 4's own edits applied on top) and ran all five. Results: **A → 1 line; B → 5 lines; C → no output; D → 5 lines; E → gone + no dangling refs.** The single grep the round-1 plan proposed returned **34** lines on that tree — including the frozen `socket.rs` doc, which is precisely why it is now five scoped gates with a disposition table. |

---

## Flagged for the operator

1. **The feature spec said `camp init --no-service` is "idempotent" (§7). It is not, and never was.** Task 1 makes it true with an explicit `--exists-ok` flag rather than papering over it with a shell existence test in the entrypoint (which would duplicate `init`'s own predicate in a file no unit test runs). The spec is amended in the same PR. If the operator would rather the entrypoint shell out to `[ -e "$CAMP_DIR/camp.toml" ]`, Task 1 disappears and Task 2's entrypoint changes by one line — but the predicate then lives in two places and only one of them is tested.
2. **The reference image ships `camp` + `git`, not `claude`.** A camp that dispatches real Claude Code workers needs the worker binary and credentials in the image; that is a build-your-own-FROM decision (and a credentials-in-containers decision) the reference deliberately does not make for you. The README says so plainly, and the smoke test proves dispatch works with a four-line fake worker wired in through `[dispatch] command`.
3. **Host-side `camp` against a container's socket works only on native Linux**, and only with a schema-compatible binary. Documented as such rather than promised. `docker exec` is the supported path everywhere. (Cross-host is already out of scope per design §11.)

**Settled, not flagged (round-1 rulings — do not re-open):**
- **`contrib/launchd/` is DELETED**, with a pointer at `camp service install` in the `camp service` docs. Operator's ruling, 2026-07-10: git history preserves the example, and keeping a directory whose README says *"Deliberately NO KeepAlive: `camp stop` must stay stopped"* would leave a live contradiction in the tree. Task 4 Step 2. Gate E proves it.
- **The reconciliation sweep is five scoped gates with a disposition table, not one grep expecting silence.** A gate that condemns a frozen line teaches the next implementer to delete load-bearing code; a gate whose expected output is fiction teaches them to ignore gates. Both were true of the round-1 plan. Task 4 Step 10's outputs are measured against a simulated post-Phase-2/3/4 tree.

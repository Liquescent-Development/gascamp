# Compat Phase 3 — The gc Worker Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A downloaded Gas City pack worker closes a Gas City bead end-to-end inside camp — running its own unmodified 140-line bash fragment against `gc`/`bd` shims that campd puts on the worker's PATH — proven by executing the REAL `gc-role-worker` fragment from the corpus at `GCPACKS_REF` and asserting it claims, closes, drain-acks, and exits under a deadline.

**Architecture:** campd installs two `#!/bin/sh` argv-translator shims (`.camp/bin/gc`, `.camp/bin/bd`) that `exec` camp's own absolute binary as `camp gc-shim …` / `camp bd-shim …`, and prepends `.camp/bin` to the worker child's PATH only. The shims are the sole new ledger-touching surface; `camp` stays the one process that writes `camp.db`. The claim invariant lives on the **bead row** (§6.1): one ledger transaction stamps `claimed_by` (the session), `assignee` (the qualified route, projected as `gc.routed_to`), and `work_branch` (the dispatch branch, projected as `gc.work_branch`); the hook JSON, `bd show --json`, and the worker's environment are three byte-identical projections of that one row. `runtime drain-ack` becomes campd's prompt release signal, with the existing bead-close grace timer as the backstop.

**Tech Stack:** Rust (workspace crates `camp`, `camp-core`), clap subcommands, rusqlite/SQLite ledger, `serde`/`serde_json`, POSIX `sh` shims, a `ci/gc-compat` Python gate driving the real `camp` binary against the corpus fetched at `GCPACKS_REF`, and `contrib/docker`.

## Global Constraints

Every task's requirements implicitly include this section. Values are copied verbatim from the spec and AGENTS.md; do not paraphrase them into code.

- **Fail fast, no fallbacks, no silenced errors.** An unknown shim verb/flag, an unresolved binding, an unspawnable agent, a missing `python3` — each surfaces to the caller AND lands in the ledger. Never a no-op (spec §6: "a silently-ignored `bd update --set-metadata gc.outcome=pass` is a corrupted ledger").
- **No panics in library code.** `clippy::unwrap_used`/`expect_used`/`panic` are denied outside `#[cfg(test)]`; `unsafe_code` forbidden.
- **New event payload structs use `#[serde(deny_unknown_fields)]`.** Extend an existing payload only by adding `#[serde(default)]` fields (backward-readable ledgers — invariant 3).
- **One transaction for event + state.** Every state change is one appended event whose fold is one SQLite transaction; `append` rolls back entirely on `Err` ("rejections appended nothing"); `build_shadow`/`refold` replays the accepted prefix and must reproduce the state byte-for-byte.
- **Vocabulary mirror (invariant 7).** Every new event name is declared in `crates/camp-core/src/vocab.rs` as `CAMP_SPECIFIC_EVENTS` (camp has no gc counterpart for a shim) and MUST NOT appear in gc's registry (`crates/camp-core/tests/fixtures/gc-vocab.json`). The `tests/vocab_pin.rs` partition tests enforce both directions.
- **The shim embeds camp's ABSOLUTE path** (`std::env::current_exe()`), never `exec camp` by bare name (§6.3 — campd's PATH snapshot is not guaranteed to contain camp's bindir).
- **`.camp/bin` is gitignored** (§6.3): `gitignore::RUNTIME_DIRS` gains `bin`.
- **Attended sessions get no shims** (§6.3): gc pack agents are campd-dispatch-only; never wire `.camp/bin` into an operator's shell.
- **Tests use no network and spend no API.** Git-backed imports run against local `file://` repos in a temp dir; workers are `#!/bin/sh` fakes; never a real `claude`.
- **`python3` is a hard runtime dependency** of the gc worker contract (§6.1) and must be declared and added to `contrib/docker/` (which today installs only `ca-certificates git tini`).
- **Every new test must die against a mutation of the code it guards** (§14). Where a test is the only guard for a fact, the step says which mutation it must catch.
- **Branch:** `compat-3-worker-contract`. Never commit to `main`. No co-author lines.
- **Already merged, do NOT re-do:** #86 (`--verbose` worker argv + the `$0` real-claude gate) landed in fix-86 (#88); `spawn.rs` already emits `--verbose` for `HeldStream` (spawn.rs:199). `PROJECTED_METADATA` already maps `gc.routed_to → assignee` and `gc.work_branch → work_branch` (readiness.rs:71-73); the `beads.work_branch` column already exists (schema.rs:32). Cook already stamps `assignee = <qualified route>` on a routed step bead (cook.rs:224-225) and carries step `metadata` onto the bead (cook.rs:412-414). The binding namespace + `resolve_agent(cfg, "<binding>.<agent>")` are merged (pack.rs:251).

## Operator ruling — MEASURE gc, do not infer it

Spec §6.1 quotes a five-line excerpt of the `gc-role-worker` fragment; that excerpt is **not** the contract. The operator ruling (kickoff) is binding: **build the shim first and measure the real fragment's behavior; where the plan leans on a gc behavior, it must say so and cite how it is measured.** Task 1 is therefore a pure measurement task that fetches the fragment at `GCPACKS_REF`, runs it under `sh` with a recording stub, and commits the observed contract (verb set, JSON field names, exit-code expectations) as the fixture every later task asserts against. No task below may hard-code a fragment fact that Task 1 did not observe; where a step names a field (e.g. `route`, `assignee`, `action`), it is the spec's claim to be **confirmed** by the Task 1 recording, and the step says so.

---

## File Structure

**New files (this phase creates them):**

- `crates/camp/src/cmd/shim/mod.rs` — the shim entry points (`gc-shim`, `bd-shim` dispatch), the `shim.refused` emitter, the shared refusal helper. One module owns the wire, mirroring cp-1's "one module owns the wire" discipline.
- `crates/camp/src/cmd/shim/install.rs` — `.camp/bin` generation (absolute-path `sh` scripts), PATH-prepend helper. Pure/near-pure; asserted byte-for-byte.
- `crates/camp/src/cmd/shim/project.rs` — the ONE bead→projection function `claim_projection(bead_row) -> ClaimProjection { assignee, route, work_branch }`, shared by `hook`, `bd show`, and (via a mirror assertion) the worker env. No second formatter (§6.1).
- `crates/camp/src/cmd/shim/hook.rs` — `gc hook --claim --json` (discovery + claim flip + drain).
- `crates/camp/src/cmd/shim/bd.rs` — `bd show/update/close/list/ready/create` data-plane translation.
- `crates/camp/src/cmd/shim/runtime.rs` — `runtime drain-ack` (release signal) + `convoy status --json`.
- `crates/camp/tests/worker_contract.rs` — the hermetic Rust integration test: real campd, a fake `claude` that `exec`s the fragment under `sh`, real ledger, real shims; claim → close → drain-ack → exit under a deadline. Plus the byte-projection equality test.
- `crates/camp/tests/fixtures/gc-fragment.sh` — a FAITHFUL synthetic fragment built from Task 1's recording (the hermetic stand-in for the corpus fragment; the CI gate uses the real one).
- `ci/gc-compat/worker_contract.py` — THE §14 gate: fetches nothing itself (CI passes it `gcpacks-src` + the camp binary), renders the REAL corpus `gc-role-worker` fragment, drives real campd + a fake claude that runs it under `sh`, asserts claim → close → drain-ack → exit under a wall deadline (a hang IS the failing signal).
- `ci/gc-compat/fixtures/gc-role-worker.observed.json` — Task 1's committed measurement: the verb/flag set, JSON field names, and exit-code contract the real fragment depends on, plus the fragment's corpus path and the `GCPACKS_REF` sha it was observed at (our derived facts — NOT the fragment's copyrighted source, which is never vendored, §10).

**Modified files:**

- `crates/camp/src/main.rs` — wire two subcommands `GcShim` and `BdShim` (raw trailing args). **Guaranteed-contention file: keep the change ADDITIVE** (two new `Command` variants + two match arms).
- `crates/camp/src/daemon/spawn.rs` — `build_spec` sets the gc worker env vars and the PATH prepend; shim install is invoked at dispatch.
- `crates/camp/src/daemon/dispatch.rs` — install shims at dispatch (before spawn); observe `session.drain_acked` to release promptly.
- `crates/camp/src/daemon/patrol.rs` — drain-ack → prompt `kill_released`; bead-close release stays as the grace backstop.
- `crates/camp/src/gitignore.rs` — `RUNTIME_DIRS += "bin"`.
- `crates/camp-core/src/event.rs` — `ShimRefused`, `SessionDrainAcked` variants (+ `ALL`, `as_str`, `parse`). **Guaranteed-contention file: additive only.**
- `crates/camp-core/src/vocab.rs` — the two names in `CAMP_SPECIFIC_EVENTS`. **Additive only.**
- `crates/camp-core/src/ledger/fold.rs` — `BeadClaimed` gains `route`/`work_branch`; `bead_claimed` stamps them in the claim transaction. **Additive only.**
- `contrib/docker/Dockerfile` — add `python3` to the runtime apt install, with the § reference.
- `.github/workflows/ci.yml` — one new step in the `gc-compat` job running `worker_contract.py`.

---

## Task 1: Measure the real fragment (SHIM FIRST — no Rust yet)

**Files:**
- Create: `ci/gc-compat/fixtures/gc-role-worker.observed.json`
- Create (scratch, not committed): a recording stub `gc`/`bd`
- Test: the measurement IS the deliverable; a follow-up assertion in `ci/gc-compat/worker_contract.py` (Task 11) re-derives it from the live corpus and fails on drift.

**Interfaces:**
- Produces: `gc-role-worker.observed.json` with keys `{ "gcpacks_ref", "fragment_path", "verbs": {"gc": [...], "bd": [...]}, "hook_json_fields": [...], "drain_ack": {"argv": [...], "exit_expected": 0}, "env_read": [...], "exit_contract": {...} }`. Every later task that names a fragment fact cites this file.

- [ ] **Step 1: Fetch the corpus at the pinned ref and locate the fragment**

Run (local, one-time; the corpus is NEVER committed — §10):

```bash
REF=$(cat ci/gc-compat/GCPACKS_REF)
git clone https://github.com/gastownhall/gascity-packs.git /tmp/gcpacks
git -C /tmp/gcpacks checkout "$REF"
# The roles pack ships its own copy of the fragment (§7.3, verified byte-identical to gascity's):
find /tmp/gcpacks -name '*gc-role-worker*' -o -name '*role-worker*' | sort
grep -rl 'runtime drain-ack' /tmp/gcpacks/gascity /tmp/gcpacks/gascity/roles 2>/dev/null
```

Expected: exactly one `gc-role-worker` template fragment under the `gascity/roles` (and `gascity`) `template-fragments/` directory. Record its repo-relative path.

- [ ] **Step 2: Run the fragment under `sh` with a RECORDING stub and capture every call**

Write a scratch recording `gc` and `bd` (each appends its full argv + reads a canned response to a log), set the §6.1 env (`BEADS_ACTOR`, `GC_SESSION_NAME`, `GC_SESSION_ID`, `GC_AGENT`, `GC_TEMPLATE`, plus whatever the fragment reads), put the stubs on PATH, and execute the rendered fragment under `sh` with a short deadline. The fragment's `set +e … sleep 2; continue` loop means you must feed the stub `hook` a "work" response once then a "drain" response, so the loop terminates.

Capture and record into the fixture:
1. The exact `gc` verbs and flags invoked (confirm the spec's `hook --claim --json`, `runtime drain-ack`; note any `prime`/`mail`/`convoy status`).
2. The exact `bd` subcommands and flags (`show`/`update`/`close`/`list`/`ready`/`create`, `--json`, `--set-metadata k=v`).
3. Which JSON fields the fragment PARSES out of `hook --claim --json` and `bd show --json` (grep the fragment's inline `python3` parsers). This CONFIRMS OR CORRECTS spec §6's `{schema_version, ok, action, reason, bead_id, assignee, route}` and §6.1's "`bd show`'s assignee/route overwrite hook's before comparing".
4. The exit-code contract: `hook` exit 0 on work, exit 1 on drain, and the `drain-ack` path's expected exit.
5. Every environment variable the fragment reads (confirm `BEADS_ACTOR`/`GC_SESSION_NAME`/`GC_SESSION_ID`/`GC_AGENT`/`GC_TEMPLATE`, `python3`).

- [ ] **Step 3: Commit the measurement**

Write `ci/gc-compat/fixtures/gc-role-worker.observed.json` from Step 2. Do NOT commit the fragment text (licensing, §10) — only our derived facts + `fragment_path` + `gcpacks_ref`.

```bash
git add ci/gc-compat/fixtures/gc-role-worker.observed.json
git commit -m "compat(worker): measure the real gc-role-worker fragment's shim contract"
```

**If the recording contradicts the spec** (e.g. the fragment needs a verb §12.3 puts in phase 4, like `prime` or `mail send`, on its REQUIRED claim→close→drain path): STOP and report to the lead. Refusing a required verb would hang the §14 test; that is a scope decision, not a guess.

---

## Task 2: The two new ledger events (`shim.refused`, `session.drain_acked`)

**Files:**
- Modify: `crates/camp-core/src/event.rs` (add variants to `EventType`, `ALL`, `as_str`, `parse`)
- Modify: `crates/camp-core/src/vocab.rs` (`CAMP_SPECIFIC_EVENTS`)
- Test: `crates/camp-core/src/event.rs` (inline `#[cfg(test)]`), `crates/camp-core/tests/vocab_pin.rs` (already enforces the partition)

**Interfaces:**
- Produces: `EventType::ShimRefused` (`"shim.refused"`), `EventType::SessionDrainAcked` (`"session.drain_acked"`). Both are audit-only (no fold arm in `fold.rs::process`). Consumed by Task 3 (drain-ack fold-visibility), Tasks 5–8 (refusal emission), Task 10 (release trigger).

- [ ] **Step 1: Write the failing test** (append to the `#[cfg(test)] mod tests` in `event.rs`)

```rust
#[test]
fn shim_and_drain_ack_events_roundtrip_and_are_camp_specific() {
    for (variant, name) in [
        (EventType::ShimRefused, "shim.refused"),
        (EventType::SessionDrainAcked, "session.drain_acked"),
    ] {
        assert_eq!(variant.as_str(), name);
        assert_eq!(EventType::parse(name).unwrap(), variant);
        assert!(EventType::ALL.contains(&variant));
        assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&name));
        assert!(!crate::vocab::GC_MIRRORED_EVENTS.contains(&name));
    }
}
```

- [ ] **Step 2: Run it, watch it fail**

Run: `cargo test -p camp-core --lib event::tests::shim_and_drain_ack_events_roundtrip -- --nocapture`
Expected: FAIL — `no variant named ShimRefused`.

- [ ] **Step 3: Implement** — add both variants to `EventType` (with doc comments naming the §), to `EventType::ALL`, to the `as_str` match, and the two names to `CAMP_SPECIFIC_EVENTS`. Follow the `FormulaRefused`/`ImportRefused` precedent verbatim (they are audit-only, no fold arm). Doc comment for `ShimRefused`: `compat §6 — a gc/bd shim refused an unknown verb/flag; the caller swallows failures (set +e; sleep 2; continue), so the refusal is evented, naming binding/agent/verb/flag. Audit-only, no state fold.` For `SessionDrainAcked`: `compat §6.2 — a gc worker ran runtime drain-ack; campd's prompt release signal. Audit-only, no state fold.`

- [ ] **Step 4: Run the event test AND the vocab partition gate**

Run: `cargo test -p camp-core --lib event:: && cargo test -p camp-core --test vocab_pin`
Expected: PASS. `vocab_pin.rs` proves neither name exists in gc's registry (invariant 7). Mutation this catches: dropping either name from `CAMP_SPECIFIC_EVENTS` (partition test fails) or misspelling `as_str` (roundtrip fails).

- [ ] **Step 5: Confirm the refold property still holds** (audit-only events must not perturb state)

Run: `cargo test -p camp-core --test refold` (and the ledger refold property test)
Expected: PASS — an event with no fold arm folds to a no-op and refolds identically.

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs
git commit -m "compat(worker): shim.refused + session.drain_acked audit events"
```

---

## Task 3: The claim invariant on the bead — `BeadClaimed` stamps route + work_branch

**Files:**
- Modify: `crates/camp-core/src/ledger/fold.rs` (`BeadClaimed` struct ~269, `bead_claimed` ~273)
- Test: `crates/camp-core/src/ledger/fold.rs` (inline tests) + `crates/camp-core/tests/` refold property

**Interfaces:**
- Consumes: nothing new.
- Produces: `BeadClaimed { session, route: Option<String>, work_branch: Option<String> }`. When `route`/`work_branch` are `Some`, the SAME `UPDATE` that sets `claimed_by`/`status='in_progress'` sets `beads.assignee = route` and `beads.work_branch = work_branch`. Consumed by Task 6 (hook emits this) and Task 9 (the projection reads these columns). `camp claim` (claim.rs) keeps emitting `{session}` only (both `None` → columns untouched).

- [ ] **Step 1: Write the failing test** (append to `fold.rs` tests)

```rust
#[test]
fn bead_claimed_stamps_route_and_work_branch_in_one_transaction() {
    let led = /* open an in-memory ledger with a cooked, open step bead "gc-2"
                 whose assignee column is already the qualified route from cook */;
    // Claim with the gc worker's three facts:
    led.append(EventInput {
        kind: EventType::BeadClaimed,
        rig: None,
        actor: "gc-shim".into(),
        bead: Some("gc-2".into()),
        data: serde_json::json!({
            "session": "t/gc.run-operator/1",
            "route": "gc.run-operator",
            "work_branch": "camp/gc-2",
        }),
    }).unwrap();
    let row = /* SELECT status, claimed_by, assignee, work_branch FROM beads WHERE id='gc-2' */;
    assert_eq!(row.status, "in_progress");
    assert_eq!(row.claimed_by.as_deref(), Some("t/gc.run-operator/1"));
    assert_eq!(row.assignee.as_deref(), Some("gc.run-operator"));       // → gc.routed_to
    assert_eq!(row.work_branch.as_deref(), Some("camp/gc-2"));          // → gc.work_branch
}

#[test]
fn bead_claimed_without_route_leaves_columns_untouched() {
    // camp's own `camp claim {session}` path: no route/work_branch → assignee/work_branch unchanged.
    // (Guards against a mutation that always overwrites assignee to NULL.)
}

#[test]
fn bead_claimed_rejects_unknown_fields() {
    // deny_unknown_fields is preserved: {session, bogus:1} must fail the fold, appending nothing.
}
```

- [ ] **Step 2: Run, watch it fail**

Run: `cargo test -p camp-core --lib fold::tests::bead_claimed_stamps_route`
Expected: FAIL — `unknown field 'route'` (the struct still `deny_unknown_fields` without the field).

- [ ] **Step 3: Implement** — extend the struct and the fold arm:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadClaimed {
    session: String,
    /// compat §6.1 — the qualified route, projected as gc.routed_to (beads.assignee).
    #[serde(default)]
    route: Option<String>,
    /// compat §6.1 — the dispatch branch, projected as gc.work_branch (beads.work_branch).
    #[serde(default)]
    work_branch: Option<String>,
}
```

In `bead_claimed`, replace the `Some("open")` UPDATE so the three columns are stamped together (COALESCE keeps existing values when the field is `None`):

```rust
conn.execute(
    "UPDATE beads SET status = 'in_progress', claimed_by = ?1,
                      assignee = COALESCE(?2, assignee),
                      work_branch = COALESCE(?3, work_branch),
                      updated_ts = ?4
     WHERE id = ?5",
    params![p.session, p.route, p.work_branch, event.ts, id],
)?;
```

Keep the existing `dispatch_failure = NULL` clear that follows.

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p camp-core --lib fold::tests::bead_claimed`
Expected: PASS all three.

- [ ] **Step 5: Refold property**

Run: `cargo test -p camp-core --test refold`
Expected: PASS — the CAS/claim replays deterministically from the accepted prefix.

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/ledger/fold.rs
git commit -m "compat(worker): the claim invariant — one txn stamps session, route, work_branch"
```

---

## Task 4: Shim scaffolding — subcommands, absolute-path install, gitignore

**Files:**
- Create: `crates/camp/src/cmd/shim/mod.rs`, `crates/camp/src/cmd/shim/install.rs`
- Modify: `crates/camp/src/main.rs` (two `Command` variants + arms), `crates/camp/src/gitignore.rs`, `crates/camp/src/cmd/mod.rs` (declare `pub mod shim;`)
- Test: `crates/camp/src/cmd/shim/install.rs` (inline), `crates/camp/src/gitignore.rs` (inline)

**Interfaces:**
- Produces:
  - `shim::install::write_shims(camp_root: &Path, camp_exe: &Path) -> Result<()>` — writes `<camp_root>/bin/gc` and `<camp_root>/bin/bd`, each `#!/bin/sh\nexec <camp_exe> gc-shim "$@"\n` (resp. `bd-shim`), mode `0755`. `camp_exe` is ABSOLUTE.
  - `shim::install::prepend_bin_path(camp_root: &Path, existing_path: Option<&str>) -> String` — `<camp_root>/bin:<existing PATH>`.
  - CLI: `camp gc-shim [ARGS…]`, `camp bd-shim [ARGS…]` (raw trailing args; see Step 3).

- [ ] **Step 1: Write the failing tests** (`install.rs` inline)

```rust
#[test]
fn shims_embed_the_absolute_camp_path_not_a_bare_name() {
    let dir = tempfile::tempdir().unwrap();
    write_shims(dir.path(), Path::new("/opt/camp/bin/camp")).unwrap();
    let gc = std::fs::read_to_string(dir.path().join("bin/gc")).unwrap();
    assert_eq!(gc, "#!/bin/sh\nexec /opt/camp/bin/camp gc-shim \"$@\"\n");
    let bd = std::fs::read_to_string(dir.path().join("bin/bd")).unwrap();
    assert_eq!(bd, "#!/bin/sh\nexec /opt/camp/bin/camp bd-shim \"$@\"\n");
    assert!(!gc.contains("exec camp "), "never a bare-name lookup (§6.3)");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.path().join("bin/gc")).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable");
    }
}

#[test]
fn prepend_bin_path_puts_camp_bin_first_and_preserves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let p = prepend_bin_path(dir.path(), Some("/usr/bin:/bin"));
    assert!(p.starts_with(&format!("{}/bin:", dir.path().display())));
    assert!(p.ends_with("/usr/bin:/bin"));
}
```

And in `gitignore.rs`:

```rust
#[test]
fn bin_is_a_runtime_dir() {
    assert!(RUNTIME_DIRS.contains(&"bin"), "the shim bindir must be gitignored (§6.3)");
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p camp --lib cmd::shim::install:: ; cargo test -p camp --lib gitignore::tests::bin_is_a_runtime_dir`
Expected: FAIL — module/`write_shims` do not exist; `RUNTIME_DIRS` lacks `bin`.

- [ ] **Step 3: Implement** —
  - `gitignore.rs`: `const RUNTIME_DIRS: &[&str] = &["runs", "sessions", "worktrees", "imports", "bin"];`
  - `shim/install.rs`: `write_shims` (create `<root>/bin`, write both scripts with the exact bytes above, set mode 0755 on unix) and `prepend_bin_path`.
  - `shim/mod.rs`: `pub mod install;` plus `pub fn gc_shim(camp: &CampDir, args: Vec<String>) -> Result<()>` and `pub fn bd_shim(camp: &CampDir, args: Vec<String>) -> Result<()>` that (for now) route to the refusal path from Task 5. For this task they may `bail!("unimplemented")` — later tasks fill them.
  - `main.rs`: add to `enum Command`:

```rust
/// gc pack worker shim (spec §6). NOT for humans; installed into .camp/bin.
#[command(hide = true)]
GcShim {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
},
#[command(hide = true)]
BdShim {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
},
```

and arms: `Command::GcShim { args } => cmd::shim::gc_shim(&camp, args),` `Command::BdShim { args } => cmd::shim::bd_shim(&camp, args),`. (`trailing_var_arg + allow_hyphen_values` is why these are raw `Vec<String>`: gc/bd own their arg grammar; clap must not interpret `--claim`/`--set-metadata` as camp flags.)

- [ ] **Step 4: Run, watch pass** — `cargo test -p camp --lib cmd::shim::install:: gitignore::tests::bin_is_a_runtime_dir` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/shim/ crates/camp/src/main.rs crates/camp/src/gitignore.rs crates/camp/src/cmd/mod.rs
git commit -m "compat(worker): shim scaffolding — absolute-path .camp/bin, gitignored, two subcommands"
```

---

## Task 5: The refusal path — unknown verbs/flags fail fast AND event `shim.refused`

**Files:**
- Modify: `crates/camp/src/cmd/shim/mod.rs`
- Test: `crates/camp/src/cmd/shim/mod.rs` (inline)

**Interfaces:**
- Produces: `shim::refuse(camp, binding, agent, verb, detail) -> Result<()>` — appends `EventType::ShimRefused { binding, agent, verb, detail }` (best-effort poke), prints a named error to stderr, and returns `Err` so the process exits nonzero. `binding`/`agent` come from `$GC_TEMPLATE`/`$GC_AGENT`/`$CAMP_SESSION` env when present (naming which pack asked — §6). Consumed by hook/bd/runtime dispatch (Tasks 6–8) for every unhandled verb/flag.

- [ ] **Step 1: Write the failing test** (`mod.rs` inline — drive through the top-level dispatch with a real temp camp)

```rust
#[test]
fn unknown_gc_verb_fails_fast_and_events_shim_refused() {
    let camp = /* a temp CampDir with an initialized ledger */;
    let err = gc_shim(&camp, vec!["mol".into(), "list".into()]).unwrap_err();
    let m = format!("{err:#}");
    assert!(m.contains("gc mol") || m.contains("\"mol\""), "names the refused verb: {m}");
    // the ledger carries the audit event even though the caller's `set +e` would eat the stderr
    let events = /* read events */;
    assert!(events.iter().any(|e| e.kind == EventType::ShimRefused
        && e.data["verb"] == "mol"));
}

#[test]
fn unknown_bd_flag_is_refused_not_silently_ignored() {
    // `bd update gc-1 --set-metadata gc.outcome=pass --frobnicate` → refuse, naming --frobnicate.
    // Mutation this catches: a dispatch that falls through unknown flags to a no-op (a corrupted ledger, §6).
}
```

- [ ] **Step 2: Run, watch fail** — `cargo test -p camp --lib cmd::shim` → FAIL (`gc_shim` still bails "unimplemented").

- [ ] **Step 3: Implement** the top-of-dispatch in `gc_shim`/`bd_shim`: match the first arg (the verb); a verb not in the served set routes to `refuse(...)`. Implement `refuse` to append `ShimRefused` and `bail!` a named message. A verb that IS served but carries an unknown flag routes to `refuse` from inside that verb's handler (Tasks 6–8). Read `binding`/`agent` from env for the event payload.

- [ ] **Step 4: Run, watch pass** — PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): shim refusals are loud AND evented (shim.refused)"
```

---

## Task 6: `gc hook --claim --json` — discovery, claim flip, drain

**Files:**
- Create: `crates/camp/src/cmd/shim/hook.rs`, `crates/camp/src/cmd/shim/project.rs`
- Modify: `crates/camp/src/cmd/shim/mod.rs` (route `hook` here)
- Test: `crates/camp/src/cmd/shim/hook.rs` (inline)

**Interfaces:**
- Consumes: env `CAMP_BEAD`, `CAMP_SESSION`, `GC_AGENT` (qualified route); Task 3's `BeadClaimed{session,route,work_branch}`; `claim_projection` (below).
- Produces: `shim::project::claim_projection(row: &BeadRow) -> ClaimProjection { assignee: String, route: String, work_branch: String }` where `assignee = row.claimed_by`, `route = row.assignee` (the qualified route → gc.routed_to), `work_branch = row.work_branch`. `hook --claim --json` prints, on WORK: `{"schema_version":1,"ok":true,"action":"work","reason":null,"bead_id":"…","assignee":"<session>","route":"<qualified>"}` and exits 0; on DRAIN: `{"schema_version":1,"ok":true,"action":"drain","reason":"<why>","bead_id":"…","assignee":null,"route":null}` and exits 1 (0 with `--drain-ack`). **Field names/shape are the spec's claim (§6) to be CONFIRMED against Task 1's `hook_json_fields` recording before implementing — adjust to what the fragment actually parses.**

- [ ] **Step 1: Confirm the shape against Task 1** — open `gc-role-worker.observed.json`; verify `hook_json_fields` matches `{schema_version, ok, action, reason, bead_id, assignee, route}` and the exit-code contract (work=0, drain=1). If the recording differs, the JSON below and the assertions change to match it. Note the confirmation in a code comment citing the fixture.

- [ ] **Step 2: Write the failing tests** (`hook.rs` inline)

```rust
#[test]
fn hook_claim_on_an_open_dispatched_bead_returns_work_exit_0() {
    // ledger: cooked open bead "gc-2", assignee column = "gc.run-operator" (from cook).
    // env: CAMP_BEAD=gc-2, CAMP_SESSION="t/gc.run-operator/1", GC_AGENT="gc.run-operator".
    let out = run_hook(&camp, &["--claim","--json"]);
    assert_eq!(out.exit, 0);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["action"], "work");
    assert_eq!(v["bead_id"], "gc-2");
    assert_eq!(v["assignee"], "t/gc.run-operator/1");     // the session
    assert_eq!(v["route"], "gc.run-operator");            // the qualified name = bead gc.routed_to
    // the flip happened, atomically, with work_branch stamped:
    let row = /* SELECT */;
    assert_eq!(row.status, "in_progress");
    assert_eq!(row.work_branch.as_deref(), Some("camp/gc-2"));
}

#[test]
fn hook_claim_on_a_closed_bead_returns_drain_exit_1() {
    // same session, bead already closed → the one-bead-per-session drain path (§6.2).
    let out = run_hook(&camp, &["--claim","--json"]);
    assert_eq!(out.exit, 1);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&out.stdout).unwrap()["action"], "drain");
}

#[test]
fn hook_claim_drain_with_drain_ack_flag_exits_0() {
    // `--drain-ack` turns the drain exit 1 into exit 0 (§6 hook row).
}

#[test]
fn hook_route_equals_the_bead_gc_routed_to_byte_for_byte() {
    // hook.route == projection of bead gc.routed_to (§6.1). Mutation caught:
    // hook re-deriving the route from GC_AGENT env instead of the bead row.
}
```

- [ ] **Step 3: Run, watch fail** — FAIL (`hook` still refused/unimplemented).

- [ ] **Step 4: Implement** — `project.rs`: `claim_projection`. `hook.rs`: parse `--claim`/`--json`/`--drain-ack` from the raw args (an unrecognised flag → `refuse`); read `CAMP_BEAD`/`CAMP_SESSION`/`GC_AGENT`; load the bead row.
  - If the bead is closed (or not `open`/`in_progress`-by-this-session) → print the drain JSON; exit 1, unless `--drain-ack` → 0.
  - If `open` → append `BeadClaimed{ session: CAMP_SESSION, route: GC_AGENT, work_branch: format!("camp/{bead}") }`, poke campd, print the work JSON from `claim_projection`, exit 0.
  - If already `in_progress` by THIS session (idempotent re-hook mid-continuation before close) → print work again from the row.
  Exit codes: return a typed result the `main` arm converts to `std::process::exit` (hook needs exit 1 on drain WITHOUT an error print — it is a normal outcome, not a failure; do not route it through `bail!`).

- [ ] **Step 5: Run, watch pass** — PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/shim/hook.rs crates/camp/src/cmd/shim/project.rs crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): gc hook --claim --json — discovery, claim flip, drain (§6.1/§6.2)"
```

---

## Task 7: `bd` data plane — show/update/close/list/ready/create, projecting the claimed row

**Files:**
- Create: `crates/camp/src/cmd/shim/bd.rs`
- Modify: `crates/camp/src/cmd/shim/mod.rs` (route `bd` verbs)
- Test: `crates/camp/src/cmd/shim/bd.rs` (inline)

**Interfaces:**
- Consumes: `claim_projection` (Task 6), camp's existing `cmd::close`/`cmd::create`/`cmd::show`/readiness reads, `BeadUpdated{metadata}` (fold.rs).
- Produces: `bd show <bead> --json` → JSON whose `assignee` = the SESSION (camp `claimed_by`) and `metadata."gc.routed_to"` / `metadata."gc.work_branch"` = the projected columns (§6.1 — bd's assignee overwrites hook's before the fragment compares); `bd update <bead> --set-metadata k=v …` → `BeadUpdated`; `bd close <bead> --status … [--set-metadata gc.outcome=…]` → camp's close vocabulary (`pass`/`fail`; `shipped`/`no-op`/`blocked`/`abandoned`, already gc-verbatim — vocab.rs:58,68); `bd create`/`list`/`ready` → camp equivalents. **The exact `bd` flags served are Task 1's `verbs.bd` recording; anything outside it → `refuse`.**

- [ ] **Step 1: Confirm against Task 1** — verify the `bd` subcommand+flag set and the field names `bd show --json` must emit (especially `assignee` and the `gc.routed_to` metadata key the fragment reads at lines 127-133). Cite the fixture in a comment.

- [ ] **Step 2: Write the failing tests**

```rust
#[test]
fn bd_show_json_projects_the_session_as_assignee_and_route_as_gc_routed_to() {
    // after a hook claim: claimed_by="t/gc.run-operator/1", assignee col="gc.run-operator".
    let v = run_bd_json(&camp, &["show","gc-2","--json"]);
    assert_eq!(v["assignee"], "t/gc.run-operator/1");                 // gc's assignee = the session
    assert_eq!(v["metadata"]["gc.routed_to"], "gc.run-operator");     // the qualified name
    assert_eq!(v["metadata"]["gc.work_branch"], "camp/gc-2");
}

#[test]
fn bd_update_set_metadata_writes_through_bead_updated() {
    run_bd(&camp, &["update","gc-2","--set-metadata","gc.custom=x"]).unwrap();
    // bead_meta now carries gc.custom=x (a non-projected key).
}

#[test]
fn bd_close_maps_to_camps_close_and_records_the_outcome() {
    run_bd(&camp, &["close","gc-2","--status","pass"]).unwrap();
    // bead gc-2 is closed with outcome pass; the projected assignee/route survive close.
}

#[test]
fn bd_unknown_subcommand_is_refused() {
    // `bd mol` (a prohibition-only verb in v1) → refuse + shim.refused (Task 5 path).
}
```

- [ ] **Step 3: Run, watch fail** — FAIL.

- [ ] **Step 4: Implement** `bd.rs`: a match over the first arg → `show`/`update`/`close`/`list`/`ready`/`create`; each parses only Task-1-observed flags (unknown → `refuse`); `show --json` builds its JSON via `claim_projection` + the bead's other `bead_meta`; `update`/`close`/`create` translate to the existing `cmd::*`/`EventInput` paths (reuse — do not re-implement close's shipped-commit gate). Map gc's `--status`/`--set-metadata gc.outcome=…` to camp's `--outcome`/`--work-outcome` per Task 1's recording.

- [ ] **Step 5: Run, watch pass** — PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/shim/bd.rs crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): bd data plane — show/update/close/list/ready/create, projecting the claim"
```

---

## Task 8: `runtime drain-ack` (release signal) + `convoy status --json`

**Files:**
- Create: `crates/camp/src/cmd/shim/runtime.rs`
- Modify: `crates/camp/src/cmd/shim/mod.rs`
- Test: `crates/camp/src/cmd/shim/runtime.rs` (inline)

**Interfaces:**
- Consumes: env `CAMP_SESSION`; `EventType::SessionDrainAcked` (Task 2).
- Produces: `runtime drain-ack` → append `SessionDrainAcked{session}`, poke campd, exit 0; `convoy status --json` → a worker-facing read of the session's bead/status (minimal shape confirmed against Task 1). Any other `runtime`/`convoy` verb → `refuse`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn runtime_drain_ack_appends_session_drain_acked_and_exits_0() {
    run_gc(&camp, &["runtime","drain-ack"]).unwrap();
    let events = /* read */;
    assert!(events.iter().any(|e| e.kind == EventType::SessionDrainAcked
        && e.data["session"] == "t/gc.run-operator/1"));
}

#[test]
fn convoy_status_json_reports_the_sessions_bead() {
    let v = run_gc_json(&camp, &["convoy","status","--json"]);
    assert_eq!(v["bead_id"], "gc-2");   // fields per Task 1's recording
}

#[test]
fn runtime_unknown_subcommand_is_refused() { /* `runtime foo` → shim.refused */ }
```

- [ ] **Step 2: Run, watch fail** — FAIL.

- [ ] **Step 3: Implement** `runtime.rs`: `drain-ack` appends `SessionDrainAcked` + poke + exit 0; `convoy status --json` reads the session's bead via the ledger; unknown verbs → `refuse`.

- [ ] **Step 4: Run, watch pass** — PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/shim/runtime.rs crates/camp/src/cmd/shim/mod.rs
git commit -m "compat(worker): runtime drain-ack (release signal) + convoy status --json"
```

---

## Task 9: The worker environment + shim install at dispatch (§6.1 projection #3)

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs` (`build_spec` env; PATH), `crates/camp/src/daemon/dispatch.rs` (call `write_shims` in `launch`, before spawn)
- Test: `crates/camp/src/daemon/spawn.rs` (inline — extend the argv/env fixture tests)

**Interfaces:**
- Consumes: `AgentDef.name` (qualified), `session_name`, `camp_root`, `shim::install`.
- Produces: `build_spec`'s `env` additionally carries `BEADS_ACTOR`, `GC_SESSION_NAME`, `GC_SESSION_ID` = `session_name`; `GC_AGENT`, `GC_TEMPLATE` = `agent.name`; and `PATH` = `prepend_bin_path(camp_root, inherited PATH)`. These are projection #3 of the claim invariant (§6.1). `launch` writes the shims once per dispatch (idempotent; `write_shims` overwrites).

- [ ] **Step 1: Write the failing test** (add a new env test in `spawn.rs`; set the fixture agent's `name` to the qualified form)

```rust
#[test]
fn build_spec_exports_the_gc_worker_environment() {
    // full_agent() with name = "gc.run-operator"
    let spec = build_spec(Path::new("claude"), &full_agent(), Path::new("/camps/dev"),
        "gc-142", "dev/gc.run-operator/1", "sid",
        Path::new("/h/.claude/x.jsonl"), Path::new("/code/gc"), StdinMode::HeldStream);
    let env: std::collections::BTreeMap<_,_> = spec.env.iter().cloned().collect();
    // session identity, three spellings (§6.1):
    for k in ["BEADS_ACTOR","GC_SESSION_NAME","GC_SESSION_ID"] {
        assert_eq!(env.get(k).map(String::as_str), Some("dev/gc.run-operator/1"), "{k}");
    }
    // qualified route, two spellings:
    for k in ["GC_AGENT","GC_TEMPLATE"] {
        assert_eq!(env.get(k).map(String::as_str), Some("gc.run-operator"), "{k}");
    }
    assert!(env.get("PATH").unwrap().starts_with("/camps/dev/bin:"));
    // the four CAMP_* vars still present (unchanged):
    assert_eq!(env.get("CAMP_BEAD").map(String::as_str), Some("gc-142"));
}
```

- [ ] **Step 2: Run, watch fail** — FAIL (env lacks the gc vars / PATH). NOTE: the existing `argv_matches_the_fixture_facts_for_a_fully_pinned_agent` test asserts `spec.env == vec![…4 CAMP_*…]` exactly — it will now fail too; UPDATE that assertion to include the five gc vars + PATH (they are appended after the four `CAMP_*`).

- [ ] **Step 3: Implement** — in `build_spec`, extend the `env` vec (after the four `CAMP_*` entries) with the five gc vars from `session_name`/`agent.name` and `PATH` = `prepend_bin_path(camp_root, std::env::var("PATH").ok().as_deref())`. In `dispatch.rs::launch`, before `spawn`, call `shim::install::write_shims(&self.camp.root, &std::env::current_exe()?)`; on error append `dispatch.failed` and return `Ok(())` (never a silent skip — no shims, no worker contract). **`current_exe()` is the §6.3 absolute path; do not read `[dispatch].command`, which is `claude`, not `camp`.**

- [ ] **Step 4: Run, watch pass** — `cargo test -p camp --lib daemon::spawn::` → PASS (both the updated fixture test and the new env test).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/spawn.rs crates/camp/src/daemon/dispatch.rs
git commit -m "compat(worker): the worker env (BEADS_ACTOR/GC_*) + shim install on dispatch"
```

---

## Task 10: Session lifecycle §6.2 — drain-ack is the prompt release; grace is the backstop

**Files:**
- Modify: `crates/camp/src/daemon/dispatch.rs` (observe `SessionDrainAcked`), `crates/camp/src/daemon/patrol.rs` (prompt `kill_released` on drain-ack)
- Test: `crates/camp/src/daemon/patrol.rs` (inline)

**Interfaces:**
- Consumes: `EventType::SessionDrainAcked`, existing `Dispatcher::release_worker`/`kill_released`, the `TimerKind::Release` grace.
- Produces: on a `session.drain_acked` for a live worker, campd releases it (drop stdin) and reaps promptly, classified as a clean drain release; the bead-close `TimerKind::Release` grace remains as the backstop for a worker that never drain-acks.

**Decision (record, do not re-litigate):** §6.2 says "release on drain-ack, with the existing grace timer as a backstop, not bead-close." The implementation is **additive** to the merged release path, because camp's NATIVE (non-gc) workers never drain-ack and must not regress: bead-close keeps arming the grace (the backstop, unchanged); drain-ack ADDS a prompt clean release. Dropping the held stdin at bead-close is harmless (P3: a stream worker ignores EOF), and the gc fragment reads no stdin — so its post-close continuation loop (`hook → drain → drain-ack → exit`) is unaffected. This satisfies §6.2's intent (drain-ack IS the release signal; the grace is the backstop) without reworking a merged interface (kickoff: "extend, don't rework"). **If the implementer finds §6.2 demands REMOVING the bead-close release entirely and that regresses native-worker reap classification, STOP and escalate — that is a spec-vs-reality conflict (AGENTS.md), not a judgment call.**

- [ ] **Step 1: Write the failing tests** (`patrol.rs` inline, following `release_arms_the_grace_and_kill_released_stops_with_reason`)

```rust
#[test]
fn drain_ack_promptly_releases_the_worker_with_a_clean_reason() {
    // a live registered worker for gc-2; feed campd a session.drain_acked{session};
    // assert the worker is released+reaped promptly (before the 30s grace) and the
    // session.stopped reason names the drain, not a grace timeout.
}

#[test]
fn a_worker_that_never_drain_acks_is_killed_by_the_grace_backstop() {
    // bead closes, no drain-ack; the TimerKind::Release grace still fires → kill_released.
    // Mutation caught: removing the bead-close grace arm (a gc worker that dies before
    // drain-ack would then linger forever).
}
```

- [ ] **Step 2: Run, watch fail** — FAIL.

- [ ] **Step 3: Implement** — in the settle path (event_loop → dispatcher), on observing a `SessionDrainAcked` event, look up the worker by session and, if live, `release_worker` + arm a near-zero `TimerKind::Release` (or `kill_released` directly after release) so the reap classifies as a clean drain stop. Keep `on_bead_closed`'s existing `PendingAction::Release` (the backstop). Ensure the drain-ack release and the bead-close backstop are idempotent (a session released by drain-ack must no-op the later grace fire).

- [ ] **Step 4: Run, watch pass** — `cargo test -p camp --lib daemon::patrol::` → PASS (existing release tests included — confirm none regressed).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/daemon/dispatch.rs crates/camp/src/daemon/patrol.rs
git commit -m "compat(worker): drain-ack is campd's prompt release signal; grace is the backstop (§6.2)"
```

---

## Task 11: THE UNSKIPPABLE §14 real-fragment test + the projection property

**Files:**
- Create: `crates/camp/tests/worker_contract.rs`, `crates/camp/tests/fixtures/gc-fragment.sh`
- Create: `ci/gc-compat/worker_contract.py`
- Modify: `.github/workflows/ci.yml` (one step in the `gc-compat` job), `ci/gc-compat/README.md` (drift procedure)
- Test: the files above ARE the tests.

**Interfaces:**
- Consumes: the whole stack (Tasks 2–10), the corpus at `GCPACKS_REF` (CI passes `gcpacks-src`), the real `camp` binary.

- [ ] **Step 1: The byte-projection property (hermetic Rust)** — write, in `worker_contract.rs`:

```rust
#[test]
fn hook_bd_show_and_env_project_the_same_row_byte_for_byte() {
    // 1. dispatch a gc bead → env (BEADS_ACTOR=…=session, GC_AGENT=GC_TEMPLATE=qualified);
    // 2. run `gc hook --claim --json` → {assignee, route};
    // 3. run `bd show --json`          → {assignee, metadata.gc.routed_to};
    // assert env session == hook.assignee == bd.assignee, and
    //        env GC_AGENT == hook.route == bd.metadata.gc.routed_to,
    // all byte-for-byte (§6.1: one row, three projections, no second formatter).
    // Mutation caught: any projection deriving a value independently (rev 3's bug).
}
```

Run: `cargo test -p camp --test worker_contract hook_bd_show_and_env_project` → FAIL first, then PASS after wiring.

- [ ] **Step 2: The hermetic loop test (Rust, no corpus)** — write `crates/camp/tests/fixtures/gc-fragment.sh`: a FAITHFUL synthetic fragment built from Task 1's `gc-role-worker.observed.json` — the `EXPECTED_ASSIGNEE`/`python3` guard, the `set +e … while true` claim loop calling `gc hook --claim --json`, `bd show`, `bd close`, then `gc runtime drain-ack; exit 0` on drain. In `worker_contract.rs`, drive REAL campd with `[dispatch].command` = a fake `claude` that `exec`s `sh gc-fragment.sh`, real ledger, real shims installed; assert the bead closes AND the process exits 0 within a wall deadline (a hang = the failing signal). This guards the class in plain `cargo test`.

```rust
#[test]
fn a_gc_worker_closes_a_gc_bead_end_to_end_via_a_faithful_fragment() {
    // deadline: e.g. 20s. On timeout, FAIL with "fragment hung in sleep 2; continue".
}
```

- [ ] **Step 3: Run the hermetic tests, watch pass** — `cargo test -p camp --test worker_contract` → PASS.

- [ ] **Step 4: The §14 CI gate (real corpus fragment)** — write `ci/gc-compat/worker_contract.py <corpus-checkout> <camp-binary>`, mirroring `e2e_corpus.py`'s structure:
  1. `camp init`; `camp import add <corpus>/gascity/roles --name gc` (local path — the real deployment recipe, §3/§7.3); set `[agent_defaults].tools` so gc agents resolve (§5.2).
  2. Create + route a bead to a real `gc.<agent>` from the roles pack (so `gc.routed_to` is stamped from cook), OR `camp sling` a corpus formula that routes to one.
  3. Run real campd with `[dispatch].command` = a fake claude that `exec`s `sh` on the REAL rendered `gc-role-worker` fragment (path from `gc-role-worker.observed.json`; render its Go-template via the pack's own fragment resolution / camp's prime-equivalent).
  4. Assert, under a wall deadline: the bead reaches `closed`, a `session.drain_acked` appears, and the worker process exits — a hang fails the gate.
  5. Re-derive Task 1's `observed.json` from the live fragment and fail on drift (the measurement stays honest as `GCPACKS_REF` moves — add this to the README "Moving GCPACKS_REF" procedure).

- [ ] **Step 5: Wire the gate into CI** — add to the `gc-compat` job in `.github/workflows/ci.yml`, after the `e2e_corpus.py` step:

```yaml
      - name: "phase-3 WORKER CONTRACT gate — the real gc-role-worker fragment closes a gc bead (§14)"
        run: python3 ci/gc-compat/worker_contract.py gcpacks-src target/debug/camp
```

- [ ] **Step 6: Run the gate locally against a fetched corpus** (the fixture from Task 1's clone):

```bash
python3 ci/gc-compat/worker_contract.py /tmp/gcpacks target/debug/camp
```

Expected: PASS — "a gc worker claimed, closed, drain-acked, and exited in N.NNs".

- [ ] **Step 7: Commit**

```bash
git add crates/camp/tests/worker_contract.rs crates/camp/tests/fixtures/gc-fragment.sh \
        ci/gc-compat/worker_contract.py ci/gc-compat/README.md .github/workflows/ci.yml
git commit -m "compat(worker): THE §14 gate — the real gc-role-worker fragment closes a gc bead"
```

---

## Task 12: `python3` in the container

**Files:**
- Modify: `contrib/docker/Dockerfile`
- Test: an assertion in `ci/gc-compat/worker_contract.py` (docker is not unit-testable in `cargo`)

**Interfaces:**
- Produces: the runtime image installs `python3` alongside `ca-certificates git tini`.

- [ ] **Step 1: Write the failing guard** — add to `worker_contract.py`:

```python
dockerfile = open("contrib/docker/Dockerfile").read()
assert "python3" in dockerfile, "python3 is a hard gc-worker dependency (§6.1) and must be in the runtime image"
```

Run it → FAIL.

- [ ] **Step 2: Implement** — in the runtime stage's `apt-get install`, add `python3`, with a comment:

```dockerfile
# python3         a HARD runtime dependency of the gc worker contract (compat §6.1):
#                 every gc pack agent's shared fragment parses `hook --claim --json`
#                 with an inline `python3`, and refuses (CONFIG_REJECTED, exit 0
#                 doing nothing) if it is absent. No python3, no gc worker.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini python3 \
 && rm -rf /var/lib/apt/lists/*
```

- [ ] **Step 3: Run the guard, watch pass** → PASS.

- [ ] **Step 4: Commit**

```bash
git add contrib/docker/Dockerfile ci/gc-compat/worker_contract.py
git commit -m "compat(worker): python3 in the reference container (§6.1 gc-worker dependency)"
```

---

## Task 13: Full-gate green + self-review

**Files:** none (verification only).

- [ ] **Step 1: The three merge gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Expected: all green. Any `unwrap_used`/`expect_used`/`panic` outside `#[cfg(test)]` is a clippy failure — fix at the source, never `#[allow]`.

- [ ] **Step 2: The compat gates against a fetched corpus**

```bash
python3 ci/gc-compat/load_corpus_packs.py /tmp/gcpacks target/debug/camp
python3 ci/gc-compat/formula_gate.py    /tmp/gcpacks target/debug/camp
python3 ci/gc-compat/e2e_corpus.py      /tmp/gcpacks target/debug/camp
python3 ci/gc-compat/worker_contract.py /tmp/gcpacks target/debug/camp
```

Expected: all pass (the phase-3 gate closes a real gc bead; phase-1/2 gates unregressed).

- [ ] **Step 3: Exit-criteria checklist** (from the phase block)
  - A gc worker closes a gc bead end-to-end via the REAL fragment — Task 11 §14 gate.
  - Every §6 verb served or refused loudly — `hook`/`bd`/`runtime drain-ack`/`convoy status` served (Tasks 6–8); every other verb/flag → `shim.refused` (Task 5); confirm the served set covers Task 1's recording.
  - `.camp/bin` absolute-path, gitignored, dispatch-only — Task 4/9.
  - The bead-side claim invariant + byte-projection equality — Tasks 3/6/9/11.
  - `python3` declared + in the container — Task 12.
  - CI green — Task 13.

- [ ] **Step 4: Rebase discipline** — if `main` advanced during the phase, rebase onto it and re-run Steps 1–2 before opening the PR (kickoff: the guaranteed-contention files `main.rs`, `event.rs`, `vocab.rs`, `fold.rs`, `Cargo.toml`, `Cargo.lock` are additive here, so a rebase should be clean).

---

## Self-Review

**Spec coverage (§6 in full, §12.3, §14):**

- §6 shims (argv translators, `camp` sole ledger writer) → Tasks 4, 5–8.
- §6 verb table (`hook --claim --json`, `bd …`, `runtime drain-ack`, `convoy status --json`) → Tasks 6, 7, 8; `prime`/`mail` are phase 4 (§12.4) and are REFUSED loudly in phase 3 (Task 5) — Task 1 confirms the real fragment does not need them on its claim→close→drain path (else escalate).
- §6 "unknown subcommands/flags FAIL FAST" + "every refusal also appends `shim.refused`" → Task 5.
- §6.1 claim invariant on the bead (one row, three projections) → Task 3 (the row), Task 6 (hook projection), Task 7 (bd-show projection), Task 9 (env projection), Task 11 (byte-for-byte equality). `python3` hard dependency → Task 12.
- §6.2 session lifecycle (drain post-close at the hook; drain-ack as release signal; grace backstop) → Task 6 (hook drain), Task 10.
- §6.3 shims (absolute path, gitignored `.camp/bin`, dispatch-only) → Task 4 (absolute path, gitignore), Task 9 (dispatch-only install + PATH), Global Constraints.
- §12.3 exit criteria + §14 unskippable real-fragment test + byte-projection test → Task 11.
- §14 supporting tests (routing/collision) are already covered by compat-1's `pack.rs` tests and the phase-3 hook route test (Task 6); the skills-gitignore and tool-allowlist refusals are compat-1's (`resolve_agent_def`), unchanged here.
- #86 (`--verbose`) — already merged (fix-86 #88); NOT re-done.

**Placeholder scan:** every code step shows real code or the exact assertion; every step that names a fragment fact (JSON fields, exit codes, verb set) cites Task 1's `gc-role-worker.observed.json` as the measured source and says "confirm against the recording" rather than hard-coding an inferred value — this is the operator's measure-don't-infer ruling made executable.

**Type consistency:** `BeadClaimed{session, route, work_branch}` (Task 3) is the exact shape the hook emits (Task 6). `claim_projection(row) -> {assignee, route, work_branch}` (Task 6) is the single projection used by hook (Task 6), bd-show (Task 7), and asserted against env (Task 9) in Task 11. `EventType::ShimRefused`/`SessionDrainAcked` (Task 2) are emitted by Tasks 5/8 and consumed by Task 10. `write_shims`/`prepend_bin_path` (Task 4) are called by `launch`/`build_spec` (Task 9).

**Known lean-on-gc points, each measured not inferred:** the fragment's exact verb/flag set, the `hook`/`bd show` JSON field names, and the exit-code contract — ALL from Task 1's live recording, re-derived by the Task 11 gate on every `GCPACKS_REF` move. The one camp-owned lifecycle decision (§6.2 additive release) is recorded in Task 10 with an explicit escalation trigger.

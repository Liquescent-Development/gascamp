# Gas Camp v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Gas Camp v1 exactly as specified in `docs/design/2026-07-05-gas-camp-design.md` — one Rust binary (`camp`/`campd`), one WAL-mode SQLite ledger with an append-only events table, Claude Code as the only agent runtime, zero polling anywhere — delivered as a sequence of reviewable PRs, each with tests, in strict TDD.

**Architecture:** A Cargo workspace with two crates: `camp-core` (pure library: ledger, fold/refold, readiness, formula subset compiler, orders, patrol logic — no process spawning, heavily unit-tested) and `camp` (the binary: CLI verbs, daemon event loop, dispatch, plugin-facing surfaces; integration-tested with a fake agent). All durable truth lives in `camp.db`; every mutation is one WAL transaction that writes the event row and its state effect together; `campd` is push-driven end to end (socket pokes, SIGCHLD, filesystem watches, armed timers — no ticks).

**Tech Stack:** Rust (edition 2024, pinned stable toolchain), rusqlite (bundled SQLite, WAL + FTS5), clap, serde/toml/serde_json, jiff (time), notify (FSEvents/inotify), polling (socket + timer event loop), signal-hook (SIGCHLD), thiserror (core) / anyhow (bin), proptest + assert_cmd + predicates + tempfile (tests). No async runtime (spec §15.1).

## Global Constraints

Copied from the spec and the operator's standing rules. Every task's requirements implicitly include this section.

- **Spec is authoritative:** `docs/design/2026-07-05-gas-camp-design.md`; its §4 decision record is settled. If implementation reality contradicts the spec, stop and update the spec via PR in the same change — spec and code never silently diverge.
- **Idle is free:** no ticks, no polling loops anywhere. `campd` idle target: < 20 MB RSS, 0.0% CPU, zero wakeups except armed timers (spec §2.1).
- **One SQLite ledger file** (`camp.db`, WAL): append-only `events` table is both history and bus. No separate journal file. JSONL is an export format only (`camp events --json`).
- **A write is one transaction:** event row + state effect commit together (spec §7.2). State tables ≡ fold of the event log, verified by `camp doctor --refold`.
- **Zero role names in machinery code** (spec §2.4, §8.3). `campd` executes structure only: edges, budgets, caps, cron expressions, timer thresholds.
- **Every camp formula is a valid Gas City formula-v2 file** (spec §8.2, §15.2), CI-enforced against the real `gc` compiler at a pinned ref.
- **Vocabulary mirror** (spec §15.2): event names and outcome metadata match Gas City verbatim where the concept exists; camp-specific names are additive, never redefinitions. Camp v1 outcome vocabulary: `outcome ∈ {pass, fail}`, `final_disposition ∈ {hard_fail, soft_fail}` (strict subsets of gc's).
- **Fail fast:** no silent fallbacks, no silenced errors, no placeholders. No panics in library code (`clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic` denied; `#![forbid(unsafe_code)]`). Every error path surfaces to the caller or lands in the ledger as an event.
- **TDD:** test first, run it, watch it fail, make it pass. Unit tests live next to code (spec §16).
- **Git:** never commit to main — every phase lands via its own PR branch. No co-author lines, no self-mention in commit messages. Conventional-commit style (`feat:`, `test:`, `ci:`, `docs:`, `chore:`).
- **Warnings are errors:** CI runs `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- **SQLite pragmas (decided 2026-07-05):** `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`; transactions use `BEGIN IMMEDIATE`; all tables `STRICT`. NORMAL is kill-9-safe and meets the <1 ms write target; the power-loss tail-drop window is documented in the schema docs.
- **Perf policy (decided 2026-07-05):** the §14 cost-budget assertions and the 30-day/1M-event volume fixture are **local-only** (`make perf`), asserting the exact §14 numbers. CI carries no perf job for now; revisit if outside contributors arrive. The opt-in e2e suite (`make e2e`, real `claude -p`) also asserts §14 latencies.
- **CI platforms:** ubuntu-latest + macos-latest matrix for tests (notify/polling abstract kqueue↔inotify); fmt/clippy on ubuntu.

## Plan-Time Decision Log

Decisions made while writing this plan, so execution sessions do not re-derive them:

1. **Crate layout:** two crates only (`camp-core` lib, `camp` bin). Split further only if a real boundary or compile-time problem appears.
2. **`campd` invocation:** `[[bin]] camp` plus a `campd` symlink created on install; `main()` dispatches to daemon mode when invoked as `campd` or as `camp daemon`.
3. **gc compiler gate mechanism (§15.2):** Gas City has **no** `gc formula validate` CLI. The real compiler is the Go API `formula.CompileWithoutRuntimeVarValidation(ctx, name, searchPaths, vars)` in `internal/formula` (an internal package — not importable from outside the gascity module). CI therefore checks out `gastownhall/gascity` (public) at the SHA pinned in `ci/gc-compat/GASCITY_REF`, copies camp-owned `ci/gc-compat/camp_corpus_validate.go` into `<checkout>/cmd/camp-corpus-validate/main.go`, and runs it over camp's valid-formula corpus. A deliberately broken self-test fixture proves the shim actually fails on bad input.
4. **Vocabulary pin:** `crates/camp-core/tests/fixtures/gc-vocab.json` pins gc's event names and outcome/disposition values (provenance: gascity @ `1241030188…`, `internal/events/events.go`, `internal/beadmeta/values.go`). A fast unit test checks camp's registry against the pin; the CI compat job re-extracts the vocabulary from gascity source at the pinned ref and cross-checks the pin itself, so bumping `GASCITY_REF` cannot silently drift.
5. **Camp is stricter than gc:** gc silently ignores unknown step keys and still accepts legacy v1 constructs (`gate`, `loop`, `expand`, `children`). Camp's parser uses `deny_unknown_fields` and rejects every non-subset construct with an error naming the construct and pointing to the city. Rejecting more preserves the subset invariant and adds the fail-fast gc lacks.
6. **Readiness rule (v1):** ready = `status='open'` ∧ every `needs` target exists, is `closed`, **and** has `outcome='pass'`. A failed dependency never unblocks dependents; formula runs route failure via run finalization (Phase 9), and manual beads stay visibly blocked.
7. **Worker-contract verbs** (`camp claim`, `camp close`, `camp event emit`) are additive CLI surface implied by spec §8.1/§8.4; the §5 verb list is the human-facing surface. Not a spec divergence.
8. **camp order-TOML ≠ gc order-TOML:** camp's `on = "cron:…" / "event:type[label=x]"` differs from gc's `trigger`/`schedule`/`on` fields. Compatibility only requires formulas + vocabulary to mirror; orders are translated at export time via an explicit mapping table (Phase 14), failing fast on untranslatable orders (e.g. camp's `[label=…]` filter has no gc equivalent).
9. **Timestamps:** RFC3339 UTC strings via a `Clock` trait (`jiff` for formatting); `seq` is the authoritative order, `ts` is informational.
10. **Master plan + per-phase expansion:** Phases 0–1 below are execution-ready (bite-sized TDD steps with code). Phases 2–15 are pinned here at contract level (files, exact interfaces, test lists, exit criteria); the first step of each is to expand it into its own execution-ready plan document via the writing-plans skill, in `docs/superpowers/plans/`. This is the skill's own scope-check rule applied to a 16-PR system.
11. **Phase sequencing protocol:** a phase starts only after the previous phase's PR is merged (the review checkpoint). Branches are named `phase-N-<slug>`.
    **Amended 2026-07-06:** sequencing is by *dependency*, not strict numeric order — a phase may start once every phase in its "Depends on" column (Phase Map) has merged, and phases whose dependencies are all merged may run **in parallel**, each in its own session (and its own git worktree when siblings are in flight), per `docs/superpowers/plans/2026-07-06-v1-orchestration.md` (parallel windows, shared-file conflict protocol, per-phase kickoff prompts). Two authorities are unchanged: the operator approves each phase's execution plan before it runs (decision 10), and the operator reviews and merges every PR — the review checkpoint is per-PR, not global. Orchestration may be driven by a lead session under the repo's `phase-orchestration` skill; the lead dispatches and verifies but never writes code and never merges.

## Phase Map

| Phase | PR branch | Delivers | Depends on |
|---|---|---|---|
| 0 | `phase-0-bootstrap` | workspace, CI, lint gates, AGENTS.md, `camp --version` | — |
| 1 | `phase-1-ledger-core` | schema v1, append txn path, fold, refold, `init`/`doctor --refold`/`events --json`, vocab pin | 0 |
| 2 | `phase-2-assumptions` | §17 A1–A4 verified, findings doc, spec PR if divergent | 0 |
| 3 | `phase-3-beads-readiness` | rigs, bead IDs, claim/close verbs, readiness, `ls`/`show` | 1 |
| 4 | `phase-4-search-memory` | `search`, `remember`/`recall` (FTS5 ranking) | 3 |
| 5 | `phase-5-formula-subset` | subset parser/validator, `doctor --formula`, cook, fixture corpus | 3 |
| 6 | `phase-6-gc-compat-ci` | corpus-vs-gc-compiler CI gate, vocabulary cross-check | 5 |
| 7 | `phase-7-campd-skeleton` | socket, poke protocol, cursor catch-up, auto-start, `stop`/`top` | 1, 3 |
| 8 | `phase-8-dispatch-workers` | pack agent resolution, headless spawn, registry-at-birth, SIGCHLD, worktrees, `sling`, fake agent | 2, 5, 7 |
| 9 | `phase-9-graph-execution` | check loops, retry classification, `on_complete` fan-out, `run.finalized` | 8 |
| 10 | `phase-10-orders` | cron min-heap, event orders, catch-up windows, config watch | 7 |
| 11 | `phase-11-patrol-adoption` | stall timers, nudge/restart ladder, `adopt`, worktree sweep | 8 |
| 12 | `phase-12-plugin-packs` | camp plugin (commands, hooks, worker skill, statusline), starter pack | 8, 11 |
| 13 | `phase-13-perf-volume` | 1M-event fixture, §14 assertions, idle harness, `backup`, `make perf` | 4, 9 |
| 14 | `phase-14-export-bridge` | `camp export --city` (bd JSONL, formulas, pack wrapper, order translation) | 5, 10 |
| 15 | `phase-15-e2e` | opt-in real-`claude` e2e, §14 latency + idle assertions, `make e2e` | 12 |

## Target Repository Layout (end of v1)

```
Cargo.toml  rust-toolchain.toml  AGENTS.md  CLAUDE.md  Makefile  .github/workflows/ci.yml
ci/gc-compat/{GASCITY_REF, camp_corpus_validate.go, check_vocab.sh, selftest-invalid.toml}
crates/camp-core/src/{lib,error,event,vocab,clock,config}.rs
crates/camp-core/src/ledger/{mod,schema,fold,refold}.rs
crates/camp-core/src/{readiness,id}.rs
crates/camp-core/src/formula/{mod,ast,parse,validate,cook}.rs
crates/camp-core/src/orders/{mod,parse,cron}.rs
crates/camp-core/src/patrol/{mod,timers}.rs
crates/camp-core/tests/fixtures/{gc-vocab.json, formulas/{valid,invalid}/*.toml}
crates/camp/src/{main,campdir}.rs
crates/camp/src/cmd/{init,doctor,events,rig,claim,close,event_emit,ls,show,search,remember,recall,sling,order,top,adopt,stop,backup,export}.rs
crates/camp/src/daemon/{mod,socket,event_loop,cursor,dispatch,spawn,patrol}.rs
crates/camp/tests/{cli_*.rs, daemon_*.rs, fake-agent.sh, perf/*, e2e/*}
plugin/{commands,hooks,skills,statusline}/…
packs/starter/{agents,formulas,orders.toml}
docs/design/…  docs/superpowers/plans/…
```

---

# Phase 0 — Repo Bootstrap (`phase-0-bootstrap`)

**Goal:** A green, gated skeleton every future session inherits: Cargo workspace, pinned toolchain, fmt/clippy warnings-as-errors, CI on both platforms, AGENTS.md carrying the spec's invariants, and a `camp` binary that answers `--version` (TDD'd).

### Task 0.1: Branch, workspace, toolchain, lint gates

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `crates/camp-core/Cargo.toml`, `crates/camp-core/src/lib.rs`, `crates/camp/Cargo.toml`, `crates/camp/src/main.rs`

**Interfaces:**
- Produces: workspace layout and lint policy every later task builds inside.

- [ ] **Step 1: Create the branch**

```bash
git checkout -b phase-0-bootstrap
```

- [ ] **Step 2: Write workspace files**

`Cargo.toml`:
```toml
[workspace]
resolver = "3"
members = ["crates/camp-core", "crates/camp"]

[workspace.package]
version = "0.1.0"
edition = "2024"
repository = "https://github.com/richardkiene/gascamp"

[workspace.dependencies]
camp-core = { path = "crates/camp-core" }

[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
```

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

`.gitignore`:
```
/target
.camp/
```

`crates/camp-core/Cargo.toml`:
```toml
[package]
name = "camp-core"
version.workspace = true
edition.workspace = true

[lints]
workspace = true
```

`crates/camp-core/src/lib.rs`:
```rust
#![forbid(unsafe_code)]
//! camp-core: the Gas Camp ledger and pure logic. No process spawning here.
```

`crates/camp/Cargo.toml`:
```toml
[package]
name = "camp"
version.workspace = true
edition.workspace = true

[lints]
workspace = true

[dependencies]
camp-core = { workspace = true }
```

`crates/camp/src/main.rs`:
```rust
#![forbid(unsafe_code)]

fn main() {}
```

- [ ] **Step 3: Verify the gates run clean**

Run: `cargo build --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all succeed (zero tests is fine at this step).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore crates/
git commit -m "chore: bootstrap cargo workspace with camp and camp-core crates"
```

### Task 0.2: `camp --version` (first TDD cycle)

**Files:**
- Create: `crates/camp/tests/cli_version.rs`
- Modify: `crates/camp/src/main.rs`, `crates/camp/Cargo.toml`

**Interfaces:**
- Produces: clap-based CLI entry (`Cli` struct in `main.rs`) that every later verb hangs off.

- [ ] **Step 1: Add test dependencies and write the failing test**

```bash
cargo add --package camp --dev assert_cmd predicates
```

`crates/camp/tests/cli_version.rs`:
```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

#[test]
fn version_prints_name_and_semver() {
    Command::cargo_bin("camp")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::is_match(r"^camp \d+\.\d+\.\d+\n$").unwrap());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --package camp --test cli_version`
Expected: FAIL — the binary ignores `--version` and prints nothing.

- [ ] **Step 3: Implement with clap**

```bash
cargo add --package camp clap --features derive
```

`crates/camp/src/main.rs`:
```rust
#![forbid(unsafe_code)]

use clap::Parser;

#[derive(Parser)]
#[command(name = "camp", version, about = "Gas Camp: durable agent work, one SQLite ledger, zero idle cost")]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --package camp --test cli_version`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp
git commit -m "feat: camp CLI skeleton with --version"
```

### Task 0.3: AGENTS.md, CLAUDE.md pointer, README status, plan document

**Files:**
- Create: `AGENTS.md`, `CLAUDE.md`, `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md` (this file)
- Modify: `README.md` (status line only)

- [ ] **Step 1: Write AGENTS.md**

`AGENTS.md` (complete content):
```markdown
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
```

- [ ] **Step 2: Write CLAUDE.md**

`CLAUDE.md` (complete content):
```markdown
Read AGENTS.md — it contains all repository instructions.
```

- [ ] **Step 3: Update README status line**

In `README.md`, replace the line:
```
**Status: design phase.** Nothing here runs yet.
```
with:
```
**Status: under construction.** Implementation plan:
[`docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md`](docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md).
```

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md CLAUDE.md README.md docs/superpowers/plans/
git commit -m "docs: add agent instructions and v1 implementation plan"
```

### Task 0.4: CI workflow, push, PR

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the workflow**

`.github/workflows/ci.yml`:
```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  fmt:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt
      - run: cargo fmt --all --check

  clippy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace --all-targets --all-features -- -D warnings

  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test --workspace --all-features
```

- [ ] **Step 2: Commit, push, open PR**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: fmt, clippy -D warnings, tests on ubuntu and macos"
git push -u origin phase-0-bootstrap
gh pr create --title "Phase 0: repo bootstrap" --body "Cargo workspace, pinned toolchain, fmt/clippy warnings-as-errors, CI matrix, AGENTS.md invariants, camp --version (TDD). Plan: docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md"
```

- [ ] **Step 3: Verify CI green**

Run: `gh pr checks --watch`
Expected: fmt, clippy, test (ubuntu), test (macos) all pass. Phase 0 is complete only when green.

---

# Phase 1 — Ledger Core (`phase-1-ledger-core`)

**Goal:** Spec §7 made real: `camp.db` schema v1, the canonical event JSON form, the single-transaction write path (`append`/`append_batch`), the fold, the refold property with drift detection and repair, and the first three CLI verbs (`init`, `doctor --refold`, `events --json`). Everything else in v1 depends on this PR.

**Files (whole phase):**
- Create: `crates/camp-core/src/{error,clock,event,vocab}.rs`, `crates/camp-core/src/ledger/{mod,schema,fold,refold}.rs`, `crates/camp-core/tests/fixtures/gc-vocab.json`, `crates/camp-core/tests/{refold_prop.rs,vocab_pin.rs}`, `crates/camp/src/campdir.rs`, `crates/camp/src/cmd/{mod,init,doctor,events}.rs`, `crates/camp/tests/{cli_init.rs,cli_doctor.rs,cli_events.rs}`
- Modify: `crates/camp/src/main.rs`, both `Cargo.toml`s

**Interfaces (what later phases rely on — exact):**
```rust
// camp-core
pub type Seq = i64;

pub trait Clock: Send {
    fn now_utc(&self) -> String; // RFC3339 UTC, e.g. "2026-07-05T21:14:03Z"
}
pub struct SystemClock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    BeadCreated, BeadClaimed, BeadUpdated, BeadClosed,
    SessionWoke, SessionStopped, SessionCrashed,
    CampdStarted, CampdStopped,
    // later phases add variants; vocab tests enforce the naming law
}
impl EventType {
    pub fn as_str(self) -> &'static str;               // "bead.created" …
    pub fn parse(s: &str) -> Result<Self, CoreError>;  // unknown name = error
}

pub struct EventInput {
    pub kind: EventType,
    pub rig: Option<String>,
    pub actor: String,
    pub bead: Option<String>,
    pub data: serde_json::Value,
}
#[derive(Serialize, Deserialize)]
pub struct Event { pub seq: Seq, pub ts: String, /* "type" */ pub kind: EventType,
                   pub rig: Option<String>, pub actor: String,
                   pub bead: Option<String>, pub data: serde_json::Value }

pub struct Ledger { /* one rusqlite::Connection + Box<dyn Clock> */ }
impl Ledger {
    pub fn open(db_path: &Path) -> Result<Ledger, CoreError>;
    pub fn open_with_clock(db_path: &Path, clock: Box<dyn Clock>) -> Result<Ledger, CoreError>;
    pub fn append(&mut self, input: EventInput) -> Result<Seq, CoreError>;
    pub fn append_batch(&mut self, inputs: Vec<EventInput>) -> Result<Vec<Seq>, CoreError>; // ONE txn
    pub fn events_range(&self, from: Seq, to: Option<Seq>) -> Result<Vec<Event>, CoreError>;
    pub fn refold_check(&mut self) -> Result<RefoldReport, CoreError>;
    pub fn refold_repair(&mut self) -> Result<RefoldReport, CoreError>;
}
pub struct RefoldReport { pub events_replayed: u64, pub drift: Vec<DriftEntry> }
pub struct DriftEntry { pub table: String, pub detail: String }

// camp (bin)
pub struct CampDir { pub root: PathBuf }  // resolution: --camp flag > $CAMP_DIR > walk-up for .camp/
impl CampDir { pub fn db_path(&self) -> PathBuf; pub fn config_path(&self) -> PathBuf; }
```

**Schema v1 (complete DDL, `crates/camp-core/src/ledger/schema.rs`):**
```sql
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
-- meta rows: ('schema_version','1')

CREATE TABLE events (
  seq   INTEGER PRIMARY KEY AUTOINCREMENT,
  ts    TEXT NOT NULL,
  type  TEXT NOT NULL,
  rig   TEXT,
  actor TEXT NOT NULL,
  bead  TEXT,
  data  TEXT NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX events_bead ON events(bead) WHERE bead IS NOT NULL;
CREATE INDEX events_type ON events(type);

CREATE TABLE beads (
  id           TEXT PRIMARY KEY,
  rig          TEXT NOT NULL,
  type         TEXT NOT NULL DEFAULT 'task',
  title        TEXT NOT NULL,
  description  TEXT NOT NULL DEFAULT '',
  status       TEXT NOT NULL CHECK (status IN ('open','in_progress','closed')),
  assignee     TEXT,
  claimed_by   TEXT,
  outcome      TEXT CHECK (outcome IN ('pass','fail')),
  close_reason TEXT,
  labels       TEXT NOT NULL DEFAULT '[]',
  run_id       TEXT,
  step_id      TEXT,
  created_ts   TEXT NOT NULL,
  updated_ts   TEXT NOT NULL,
  closed_ts    TEXT
) STRICT;
CREATE INDEX beads_status_rig ON beads(status, rig);

CREATE TABLE deps (
  bead_id  TEXT NOT NULL REFERENCES beads(id),
  needs_id TEXT NOT NULL,           -- no FK: forward references are legal in graphs
  PRIMARY KEY (bead_id, needs_id)
) STRICT;
CREATE INDEX deps_needs ON deps(needs_id);

CREATE TABLE sessions (
  name              TEXT PRIMARY KEY,   -- <camp>/<agent>/<n>
  agent             TEXT NOT NULL,
  rig               TEXT,
  claude_session_id TEXT,
  transcript_path   TEXT,
  pid               INTEGER,
  status            TEXT NOT NULL CHECK (status IN ('live','stopped','crashed')),
  bead              TEXT,
  spawned_ts        TEXT NOT NULL,
  ended_ts          TEXT
) STRICT;

CREATE TABLE cursors (
  name TEXT PRIMARY KEY,
  seq  INTEGER NOT NULL
) STRICT;

CREATE VIRTUAL TABLE search USING fts5(
  bead_id UNINDEXED, kind UNINDEXED, content
);
-- kind: 'body' (title+description, one row per bead) | 'close' (close reason)
```
Pragmas applied on every open: `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`. Opening a db whose `schema_version` differs from what the build supports is a hard error (`CoreError::UnsupportedSchema`) — fail fast, no auto-upgrade in v1.

**Fold semantics (complete, `fold.rs` — `fn apply(conn: &Connection, event: &Event) -> Result<(), CoreError>`):**
Payload structs all use `#[serde(deny_unknown_fields)]`; malformed data = `InvalidEventData`, which aborts the transaction (the event row never commits).

| Event | Required | Data payload | State effect |
|---|---|---|---|
| `bead.created` | `bead`, `rig` | `{title, type?='task', description?='', needs?=[], labels?=[], assignee?}` | insert `beads` row (status `open`), `deps` rows, `search` body row. Duplicate id = error. |
| `bead.claimed` | `bead` | `{session}` | `open → in_progress`, set `claimed_by`; any other prior status = `InvalidTransition` |
| `bead.updated` | `bead` | `{title?, description?}` (≥1 required) | patch fields + `updated_ts`, rewrite `search` body row |
| `bead.closed` | `bead` | `{outcome: "pass"\|"fail", reason?}` | `→ closed`, set `outcome`, `close_reason`, `closed_ts`; insert `search` close row if reason non-empty; closing a closed bead = error |
| `session.woke` | — | `{name, agent, rig?, claude_session_id?, transcript_path?, pid?, bead?}` | insert `sessions` row, status `live` |
| `session.stopped` | — | `{name}` | status `stopped`, `ended_ts` |
| `session.crashed` | — | `{name}` | status `crashed`, `ended_ts`; any bead `claimed_by` this session and `in_progress` returns to `open`, `claimed_by=NULL` |
| `campd.started` / `campd.stopped` | — | `{}` | none (log-only) |

**Vocabulary pin (`crates/camp-core/src/vocab.rs` + `tests/fixtures/gc-vocab.json`):**
```rust
/// Names camp shares with Gas City — spelling matches gc verbatim (spec §15.2).
pub const GC_MIRRORED_EVENTS: &[&str] = &[
    "bead.created", "bead.updated", "bead.closed",
    "session.woke", "session.stopped", "session.crashed",
];
/// Camp-specific names — additive; must NOT exist in gc's registry.
pub const CAMP_SPECIFIC_EVENTS: &[&str] = &["bead.claimed", "campd.started", "campd.stopped"];
```
`gc-vocab.json` pins (provenance recorded in the file): gc's full event list from `internal/events/events.go`, `outcome ∈ {pass,fail,skipped,missing_root}`, `final_disposition ∈ {pass,hard_fail,soft_fail,controller_error,orphaned_workflow,control_quarantined}`, `on_exhausted ∈ {hard_fail,soft_fail}`, `gascity_ref = "12410301884b51131a35e101a335dbaae16cdcb0"`. Tests assert: every `EventType` name appears in exactly one of the two camp consts; every mirrored name ∈ gc list; every camp-specific name ∉ gc list; camp outcome/disposition values ⊆ gc's.

### Task 1.1: `Ledger::open` + schema migration

- [ ] **Step 1:** `cargo add --package camp-core rusqlite --features bundled && cargo add --package camp-core thiserror serde serde_json jiff && cargo add --package camp-core --dev tempfile proptest`
- [ ] **Step 2: Failing test** (`ledger/mod.rs` `#[cfg(test)]`): open a temp-file ledger; assert `journal_mode` is `wal`, `synchronous` is `1` (NORMAL), `foreign_keys` on; all six tables exist; `schema_version` is `1`; FTS5 works (insert + `MATCH` round-trip on `search`); re-open succeeds idempotently; a db with `schema_version=999` errors `UnsupportedSchema`. Run; fails (module absent).
- [ ] **Step 3:** Implement `error.rs` (`CoreError`: `Sqlite`, `Json`, `UnsupportedSchema{found,supported}`, `InvalidEventData{event_type,reason}`, `InvalidTransition{bead,reason}`, `UnknownBead(String)`), `schema.rs` (DDL above + `fn init(conn)` + version check), `Ledger::open/open_with_clock`. If `bundled` rusqlite lacks FTS5 the test fails loudly — enable the crate's FTS5 feature then; do not proceed without a passing MATCH round-trip.
- [ ] **Step 4:** Test passes. **Step 5:** Commit `feat: camp.db schema v1 with WAL, STRICT tables, and FTS5`.

### Task 1.2: Event model + canonical JSON

- [ ] **Step 1: Failing tests** (`event.rs`): golden — serializing the spec §7.2 example event produces **exactly** `{"seq":412,"ts":"2026-07-05T21:14:03Z","type":"bead.closed","rig":"gascity","actor":"session:8f3c2e01","bead":"gc-142","data":{"outcome":"pass"}}`; `rig`/`bead` omitted when `None`; JSON round-trip; `EventType::parse("bogus.event")` errors.
- [ ] **Step 2:** Run; fails. **Step 3:** Implement `EventType` (variants + `as_str`/`parse` + serde via string form), `Event` (serde field order `seq,ts,type,rig,actor,bead,data`; `skip_serializing_if` on options), `EventInput`, `clock.rs` (`Clock`, `SystemClock` via jiff, and a test `FixedClock`).
- [ ] **Step 4:** Pass. **Step 5:** Commit `feat: canonical event model matching spec §7.2 JSON form`.

### Task 1.3: `append` — the single-transaction write path

- [ ] **Step 1: Failing tests**: `append(bead.created)` returns seq 1, then 2, 3…; beads/deps/search rows exist with exact field values; the event row round-trips via `events_range`; **atomicity**: appending `bead.created` with a duplicate id errors AND the events table still holds exactly one row (fold failure rolls back the event insert); `bead.claimed` on a missing bead errors and appends nothing; `append_batch` of 3 inputs where the 3rd is invalid leaves the ledger untouched.
- [ ] **Step 2:** Run; fails. **Step 3:** Implement `append`/`append_batch` (`BEGIN IMMEDIATE` via `transaction_with_behavior`; insert event row; construct `Event`; `fold::apply`; commit) and `fold.rs` for `bead.created` only. `events_range` reads back with `parse` on type.
- [ ] **Step 4:** Pass. **Step 5:** Commit `feat: single-transaction write path (event row + state effect)`.

### Task 1.4: Fold coverage for the Phase-1 event set

- [ ] **Step 1: Failing tests** covering the fold table above: claim/close/update transitions incl. every `InvalidTransition` row; close with `outcome:"skipped"` rejected (camp vocabulary is pass|fail); session woke/stopped/crashed incl. crashed-releases-claimed-bead; campd.* are log-only; malformed payloads (unknown field, missing required) error and append nothing.
- [ ] **Step 2:** Run; fails. **Step 3:** Implement remaining fold arms with `deny_unknown_fields` payload structs. **Step 4:** Pass. **Step 5:** Commit `feat: fold state effects for bead and session events`.

### Task 1.5: Refold — check and repair

- [ ] **Step 1: Failing tests**: after a representative event sequence, `refold_check` reports `events_replayed == N`, empty drift; tamper a `beads` row (direct SQL `UPDATE`) → drift names table `beads` and the bead id; tampered `search`/`deps`/`sessions` rows are also caught; `refold_repair` rebuilds and a subsequent check is clean; empty log refolds clean.
- [ ] **Step 2:** Run; fails. **Step 3:** Implement `refold.rs`: open a shadow connection on a temp file, create state tables only, replay `events_range(1, None)` through the same `fold::apply`, `ATTACH` the shadow read-only, diff every state table both directions with `EXCEPT` (drift entries carry the row's primary key), and for repair: replace state-table contents from the shadow inside one `BEGIN IMMEDIATE` transaction (ATTACH before the transaction — SQLite forbids ATTACH inside one).
- [ ] **Step 4:** Pass. **Step 5:** Commit `feat: refold drift check and repair (state ≡ fold of event log)`.

### Task 1.6: Property test — refold equivalence

- [ ] **Step 1:** `crates/camp-core/tests/refold_prop.rs`: proptest strategy generating 0–200 candidate ops (`Create{i}`, `Claim{i}`, `Close{i,pass}`, `Update{i}`, `Woke{s}`, `Crashed{s}`) over small id pools; the harness appends each op only when currently valid (validity is not what's under test); property: `refold_check` drift is empty, and a second ledger fed the identical accepted sequence produces an identical full state dump. Run (passes immediately if 1.5 is correct — the value is the input space; if it finds a counterexample, fix the fold before proceeding).
- [ ] **Step 2:** Commit `test: property-based refold equivalence over generated event sequences`.

### Task 1.7: Vocabulary pin

- [ ] **Step 1:** Write `tests/fixtures/gc-vocab.json` (content pinned in the header above) and the failing test `tests/vocab_pin.rs` asserting the four rules from the header. **Step 2:** Run; fails (no `vocab.rs`). **Step 3:** Implement `vocab.rs` consts + an `EventType::ALL` iterator used by the partition check. **Step 4:** Pass. **Step 5:** Commit `test: pin gc event and outcome vocabulary (spec §15.2 mirror)`.

### Task 1.8: CLI — `camp init`, `camp doctor --refold`, `camp events --json`

- [ ] **Step 1: Failing CLI tests** (`assert_cmd` + `tempfile`, one file per verb):
  - `init`: `camp init` in an empty dir creates `.camp/camp.toml` + `.camp/camp.db` and prints the path; `camp init --camp <dir>` targets explicitly; re-init errors (exit 1, message names the existing dir). `camp.toml` skeleton content: `[camp]\nname = "<dirname>"\n`.
  - resolution: verbs find the camp via `--camp` > `$CAMP_DIR` > walk-up; no camp found = exit 1 with `no camp found; run camp init`.
  - `events --json`: empty log prints nothing, exit 0; after seeding events through `camp-core` (dev-dependency in the test), output is exactly one canonical JSON line per event; `--from`/`--to` bound the range.
  - `doctor --refold`: clean db exits 0 printing `refold: replayed N events; 0 drift rows`; a tampered db exits 1 listing drift; `--repair` fixes it and a rerun exits 0.
- [ ] **Step 2:** Run; fails. **Step 3:** Implement `campdir.rs` (resolution order, fail fast), `cmd/{init,doctor,events}.rs`, wire clap subcommands; `main` maps `Ok`→0, domain errors→1 (clap usage errors are 2); `cargo add --package camp anyhow` and `camp-core` as dev-dependency for seeding.
- [ ] **Step 4:** Pass. **Step 5:** Commit `feat: camp init, doctor --refold, events --json`.

### Task 1.9: Phase gate

- [ ] Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
- [ ] Push, open PR `Phase 1: ledger core`, `gh pr checks --watch` green. Complete only when green.

---

# Phases 2–15 — Contract Level

Each phase below opens by expanding into its own execution-ready plan (decision 10). The content here is binding: files, interfaces, semantics, test obligations, exit criteria.

## Phase 2 — Verify §17 Assumptions A1–A4 (`phase-2-assumptions`)

**Goal:** Verify the four design-insulated assumptions against current Claude Code behavior and docs; record findings in-repo; if any resolve differently than assumed, update the spec in the same PR.

**Method per assumption** (evidence = docs citations + reproducible experiments; record `claude --version` in the findings):
- **A1 (teammate interaction):** research via the claude-code-guide agent (Agent tool/teammate mechanics, mid-run conversation); hands-on TUI check requires the operator — a ~10-minute pairing step, flagged in the PR.
- **A2 (teammate cwd across repos):** experiment — spawn a teammate/subagent targeting a second scratch repo; observe effective cwd and file-access behavior.
- **A3 (no harness team persistence):** verify Claude Code team/task state does not survive restart (docs + experiment); camp must not depend on it either way.
- **A4 (headless mid-run conversation):** spawn `claude -p` headless; verify transcript tailability during the run and `claude --resume <session-id>` conversation after; check whether any input-streaming path into a live headless session exists. Also pin the **dispatch mechanics Phase 8 needs**: how a session id is assigned/captured for `claude -p`, the transcript file path scheme, and exit-code behavior — these become fixture facts in the findings doc.

**Files:** `docs/design/2026-07-06-assumption-findings.md` — per assumption: Assumed / Observed / Evidence / Verdict (holds | weaker | stronger) / Spec impact. Spec edits in the same PR if any verdict diverges.

**Exit criteria:** findings doc merged; spec §17 updated or confirmed; Phase 8's spawn/registry design inputs are all pinned facts.

## Phase 3 — Beads, Rigs, Readiness, Queries (`phase-3-beads-readiness`)

**Goal:** Bead lifecycle as CLI plumbing plus readiness computed on write (spec §7.3, §12): rigs with per-rig ID prefixes, claim/close verbs for the worker contract, `ls`/`show`.

**Files:** `camp-core/src/{config,id,readiness}.rs`; `camp/src/cmd/{rig,claim,close,ls,show}.rs`; extend `fold.rs`; tests alongside.

**Interfaces:**
```rust
// config.rs — camp.toml (serde, deny_unknown_fields)
pub struct CampConfig { pub camp: CampSection, pub rigs: Vec<RigConfig> /* grows later */ }
pub struct RigConfig { pub name: String, pub path: PathBuf, pub prefix: String }

// id.rs — allocation is part of state (folded from bead.created), so refold stays exact
pub fn next_bead_id(conn: &Connection, prefix: &str) -> Result<String, CoreError>; // "gc-143"

// readiness.rs
pub fn is_ready(conn: &Connection, bead: &str) -> Result<bool, CoreError>;
pub fn ready_beads(conn: &Connection, rig: Option<&str>) -> Result<Vec<BeadRow>, CoreError>;
/// dependents of `closed_bead` made ready by its close — campd's subgraph recompute (spec §7.3)
pub fn newly_ready(conn: &Connection, closed_bead: &str) -> Result<Vec<String>, CoreError>;
```
- New events: `rig.added` (camp-specific; `camp rig add` writes `camp.toml` AND appends the event — config changes are events, spec §13.4). Counter state for `next_bead_id` lives in a `counters` state table folded from `bead.created`.
- Readiness rule: decision 6 (needs target must exist, be closed, and have passed).
- Verbs: `camp rig add <path> [--prefix p] [--name n]`, `camp rig ls`, `camp claim <bead> --session <name>`, `camp close <bead> --outcome pass|fail [--reason]`, `camp ls [--ready|--mine <session>|--rig <r>] [--json]`, `camp show <bead>` (current state + full event history from the log — the one sanctioned history read, §7.4).

**Tests:** readiness truth table (open/no deps; unmet dep; dep closed-fail stays blocked; dep missing stays blocked; diamond graphs); `newly_ready` returns exactly the affected subgraph's newly-ready set; id allocation survives refold (property test extension); CLI round-trips incl. `--json` golden output; `rig add` writes both TOML and event.

**Exit criteria:** a bead can live its whole Tier-0 ledger life via CLI; `doctor --refold` stays clean throughout; CI green.

## Phase 4 — Search and Memory (`phase-4-search-memory`)

**Goal:** Spec §7.4's query surface: ranked FTS over everything all-time, and bd-style persistent memory.

**Files:** `camp-core/src/search.rs`; `camp/src/cmd/{search,remember,recall}.rs`.

**Interfaces:**
```rust
pub struct SearchHit { pub bead_id: String, pub kind: String, pub snippet: String, pub rank: f64 }
pub fn search(conn: &Connection, query: &str, type_filter: Option<&str>, limit: usize)
    -> Result<Vec<SearchHit>, CoreError>; // ORDER BY bm25(search)
```
- `camp remember "<fact>" [--rig r]` = `bead.created` with `type='memory'` (title = fact; memory is beads, not a new table); `camp recall <query>` = `search` filtered to memory beads; `camp search <query>` unfiltered.
- Escape/validate FTS query syntax errors into a clean domain error (exit 1), never a panic.

**Tests:** remember→recall round-trip; ranking sanity (exact-phrase beats scattered terms); close-note content is searchable; rig scoping; malformed FTS query → clean error.

**Exit criteria:** worker skill's `recall before / remember after` contract has its verbs; CI green.

## Phase 5 — Formula Subset Compiler + Cook (`phase-5-formula-subset`)

**Goal:** Spec §8.2: parse and validate exactly the camp subset with gc's syntax and semantics, reject everything city-only with a pointer to the city, and cook runs into the ledger.

**Files:** `camp-core/src/formula/{mod,ast,parse,validate,cook}.rs`; `camp/src/cmd/doctor.rs` (add `--formula <path>`); fixture corpus `camp-core/tests/fixtures/formulas/{valid,invalid}/*.toml`.

**Interfaces:**
```rust
pub struct Formula { pub name: String, pub description: Option<String>,
                     pub requires: Option<Requires>, pub steps: Vec<Step> }
pub struct Step { pub id: String, pub title: String, pub description: Option<String>,
                  pub needs: Vec<String>, pub assignee: Option<String>,
                  pub timeout: Option<Duration>, pub check: Option<Check>,
                  pub retry: Option<Retry>, pub on_complete: Option<OnComplete> }
pub struct Check { pub max_attempts: u32, pub mode: CheckMode /* Exec only */,
                   pub path: PathBuf, pub timeout: Option<Duration> }
pub struct Retry { pub max_attempts: u32, pub on_exhausted: Disposition /* HardFail|SoftFail */ }
pub struct OnComplete { pub for_each: String /* must start "output." */, pub bond: String,
                        pub vars: BTreeMap<String,String>, pub parallel: bool }

pub fn parse_and_validate(path: &Path) -> Result<Formula, FormulaError>; // FormulaError lists ALL violations
pub fn cook(ledger: &mut Ledger, formula: &Formula, run_dir: &Path, rig: &str,
            actor: &str) -> Result<CookedRun, CoreError>; // one append_batch txn
pub struct CookedRun { pub run_id: String, pub root_bead: String, pub step_beads: BTreeMap<String,String> }
```
- **Acceptance table** (gc semantics verbatim): header keys `formula`(= file stem, enforced)/`description`/`[requires].formula_compiler`(semver comparator); steps `id`(unique)/`title`(required)/`description`/`needs`(known ids, acyclic)/`assignee`/`timeout`; `[steps.check]` `max_attempts≥1` + `[steps.check.check]` `mode="exec"`/`path`(non-empty)/`timeout`; `[steps.retry]` `max_attempts≥1`/`on_exhausted∈{hard_fail,soft_fail}`(default hard_fail); `[steps.on_complete]` `for_each`+`bond` together, `for_each` starts `output.`, `parallel`/`sequential` mutually exclusive.
- **Combination rules mirrored from gc:** `check` ∦ {`retry`,`assignee`}; `retry` ∦ {`check`,`on_complete`}. **Explicit-declaration rule mirrored:** any of check/retry/on_complete without `[requires] formula_compiler` = error using gc's concept ("graph-only constructs must declare…").
- **Rejection table** (each with an error naming the construct + "Gas City-only; see spec §8.2"): `drain`, `gate`, `loop`, `expand`/`expand_vars`, `children`, `waits_for`, `condition`, `pour`, `phase`, `tally`, `extends`, `vars` tables, `metadata` (any authored — includes all `gc.*`), `depends_on` (camp accepts `needs` only), `type`, `priority`, `tags`, `description_file`, `notes` — plus every unknown key (`deny_unknown_fields`), unlike gc's silent ignore.
- **Cook (spec §8.2):** create `runs/<run-id>/` (`run_id` = `<utc-compact>-<6-hex>`), pin the formula file copy + `manifest.json` (formula name, rig, actor, cooked ts, step→bead map); materialize root bead + one bead per step (`run_id`/`step_id` set, `needs` edges rig-prefixed) + camp-specific `run.cooked` event — all in ONE `append_batch` transaction. From then on the run is file-independent.
- Corpus: `valid/` includes the spec §8.2 `guarded-change` verbatim, a minimal single-step, a retry example, a diamond `needs` graph with `assignee`s, and the first `on_complete` fixture anywhere (gc ships none — validated for real in Phase 6); `invalid/` has one file per rejection-table row plus dup-step-id, unknown-step-id-in-needs, dependency cycle, bad semver, check-without-requires.

**Tests:** table-driven acceptance/rejection (one case per row, asserting the error names the construct); cook is atomic (fault injection mid-batch leaves nothing); cooked runs satisfy Phase 3 readiness (roots of the dag ready, dependents not); `doctor --formula` exit codes 0/1.

**Exit criteria:** corpus green under camp's validator; cook produces dispatchable graphs; CI green.

## Phase 6 — gc Compatibility Gates in CI (`phase-6-gc-compat-ci`)

**Goal:** Spec §15.2 contracts 1 and 2 become CI checks (mechanism from decision 3/4).

**Files:** `ci/gc-compat/{GASCITY_REF, camp_corpus_validate.go, check_vocab.sh, selftest-invalid.toml}`; new `gc-compat` job in `.github/workflows/ci.yml`.

`ci/gc-compat/camp_corpus_validate.go` (complete — copied by CI into the gascity checkout at `cmd/camp-corpus-validate/main.go` so it may import the internal compiler):
```go
// Validates that every formula in a directory compiles under the real Gas
// City formula-v2 compiler. Lives in gascamp; runs inside a gascity checkout.
package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/gastownhall/gascity/internal/formula"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: camp-corpus-validate <formula-dir>")
		os.Exit(2)
	}
	files, err := filepath.Glob(filepath.Join(os.Args[1], "*.toml"))
	if err != nil || len(files) == 0 {
		fmt.Fprintf(os.Stderr, "no formulas found in %s (err=%v)\n", os.Args[1], err)
		os.Exit(2)
	}
	failed := 0
	for _, path := range files {
		name := strings.TrimSuffix(filepath.Base(path), ".toml")
		if _, err := formula.CompileWithoutRuntimeVarValidation(
			context.Background(), name, []string{os.Args[1]}, nil); err != nil {
			fmt.Fprintf(os.Stderr, "FAIL %s: %v\n", name, err)
			failed++
			continue
		}
		fmt.Printf("OK   %s\n", name)
	}
	if failed > 0 {
		os.Exit(1)
	}
}
```
CI job outline: checkout gascamp; checkout `gastownhall/gascity` at `$(cat ci/gc-compat/GASCITY_REF)` into `gascity-src/`; `setup-go` with `go-version-file: gascity-src/go.mod`; copy the shim in; run it against `crates/camp-core/tests/fixtures/formulas/valid`; **shim self-test**: run it against a dir containing only `selftest-invalid.toml` (a deliberately broken formula) and require exit 1 — a shim that always passes cannot go unnoticed; then `check_vocab.sh`: extract the quoted string constants from `internal/events/events.go` and `internal/beadmeta/values.go` in the checkout and assert (a) every name in `gc-vocab.json`'s gc lists exists in the source, (b) no `CAMP_SPECIFIC_EVENTS` name does. Bumping `GASCITY_REF` is a deliberate PR; drift fails loudly.

**Exit criteria:** gc-compat job green on the Phase-5 corpus and required for merge thereafter; a deliberately-bad corpus file demonstrated to fail in CI during PR review (then removed).

## Phase 7 — campd Skeleton (`phase-7-campd-skeleton`)

**Goal:** The only standing process, crash-only and event-driven (spec §5): socket liveness, poke protocol, cursor catch-up, auto-start, `camp stop`, `camp top`.

**Files:** `camp/src/daemon/{mod,socket,event_loop,cursor}.rs`; `camp/src/cmd/{stop,top}.rs`; integration tests `camp/tests/daemon_lifecycle.rs`.

**Interfaces (pinned protocol — newline-delimited JSON over `<camp>/campd.sock`):**
```
{"op":"poke","seq":N}   → {"ok":true}
{"op":"status"}         → {"ok":true,"live_sessions":[…],"ready":N,"open":N,"campd_pid":N}
{"op":"stop"}           → {"ok":true}   then graceful exit (campd.stopped event)
```
- Liveness = socket accepts (spec §5): stale socket file that refuses connections is unlinked and rebound; bind conflict on a live socket = second daemon refuses to start (fail fast).
- Event loop: `polling` crate — socket accept + per-connection reads + timer-heap deadline as poll timeout (no timer armed = infinite wait; **zero wakeups when idle**).
- Cursor: `cursors` row `campd`; on start emit `campd.started`, run catch-up (process events past cursor through an `EventProcessor` trait — Phase 7's processor updates readiness bookkeeping only; dispatch plugs in at Phase 8), then sleep. Post-commit pokes from CLI writers (`Ledger` callers in the `camp` bin poke after append; write succeeds even if campd is down — catch-up covers it).
- Auto-start: any CLI verb needing the daemon connects; on connect failure spawns `camp daemon` detached, appends camp-specific `campd.autostarted` event, retries once, then errors (fail fast, no loop). *Amended 2026-07-06 (PR #8 review finding 5):* detachment is safe `std::process` + `process_group(0)` with init reparenting — not double-fork/setsid, which needs unsafe libc calls (forbidden by the no-unsafe invariant); the daemon holds no terminal fds (stdin null, stdout used once for the readiness line, stderr → `<camp>/campd.log`), so process-group isolation gives the needed detachment.
- `camp top`: one status query rendered as plain text (query, not loop — refresh is a keypress, v1).

**Tests:** start → socket accepts → `status` sane; `kill -9` → stale socket detected → restart → cursor caught up (events appended while dead are processed exactly once — cursor advances transactionally with processing effects); `stop` graceful; second-daemon bind refusal; auto-start path (verb with daemon down brings it up and the event trail shows cause, spec §13.3).

**Exit criteria:** kill -9 is a supported shutdown method, demonstrably; idle daemon blocks in `poll` with no timers (verified by absence of any timeout-driven code path — the §14 0.0% CPU number is asserted in Phase 13's local harness); CI green.

## Phase 8 — Dispatch and Workers (`phase-8-dispatch-workers`)

**Goal:** Spec §8.1/§8.4/§12: pack agent resolution, headless-but-present spawning with registry-at-birth, SIGCHLD reaping, worktree isolation, `camp sling` end to end, and the fake agent that makes all of it CI-testable.

**Files:** `camp-core/src/pack.rs` (agent definition frontmatter parse, last-wins layering); `camp/src/daemon/{dispatch,spawn}.rs`; `camp/src/cmd/{sling,event_emit}.rs`; `camp/tests/{fake-agent.sh,daemon_dispatch.rs}`.

**Interfaces:**
```rust
pub struct AgentDef { pub name: String, pub model: Option<String>, pub tools: Option<Vec<String>>,
                      pub permission_mode: Option<String>, pub isolation: Isolation, pub prompt: String }
pub enum Isolation { None, Worktree }
pub fn resolve_agent(cfg: &CampConfig, name: &str) -> Result<AgentDef, CoreError>; // packs, last-wins

// camp.toml additions
[dispatch]
max_workers = 10          # concurrency cap (spec §8.3)
command = "claude"        # worker executable; tests point this at fake-agent.sh — visible config, not a fallback
default_agent = "dev"     # §8.1 routing: sling with no --agent uses the rig's default_agent, falling back to this camp-wide key; neither set = hard error naming the fix
```
- Rig-level override: `[[rigs]] default_agent = "…"` routes `/sling` per rig (spec §8.1 "the pack's default worker for the current rig").
- Spawn (mechanics per Phase 2 findings): allocate session name `<camp>/<agent>/<n>`; append `session.woke` (registry row at birth — BEFORE exec) with claude session id + transcript path; exec `claude -p` with the agent's prompt + worker-contract instructions + bead id, cwd = rig path or fresh worktree under `<camp>/worktrees/` when `isolation = "worktree"`; non-interactive permissions per agent def (unallowed actions fail fast into the ledger, spec §8.4).
- SIGCHLD (signal-hook → self-pipe into the poll loop): reap; exit 0 → `session.stopped`; nonzero/signal → `session.crashed` (fold releases the claimed bead; spec §10.1).
- Worktrees: removed on clean close; kept with an event on failure (forensics, spec §12).
- `camp sling "<title>" [--agent a] [--rig r]`: one `bead.created`; campd dispatches (Tier 0 = one spawn, ~3 writes total). Attended-teammate surface is Phase 12 (plugin), wired per A1 findings.
- New events: `worker.milestone` (via `camp event emit <text> [--bead]`), `campd.autostarted`, `worktree.kept` (camp-specific), `bead.worktree.reaped` (gc-mirrored name).
- `fake-agent.sh`: speaks the whole worker contract via the `camp` CLI (claim → milestones → close) with env-controlled outcome/timing/crash — the §16 integration workhorse.

**Tests (fake agent, no Claude):** sling → dispatch → claim → milestone → close pass, full event-with-cause trail (spec §13.3 asserted literally); crash mid-work → SIGCHLD → `session.crashed` → bead back to open; concurrency cap honored under a burst of ready beads (11 ready, 10 spawned, 11th dispatched on first close); worktree created/removed on pass, kept on fail; registry row precedes process start.

**Exit criteria:** Tier-0 path complete and evented end to end with the fake agent; real-`claude` spawn arguments match Phase 2's pinned facts; CI green.

## Phase 9 — Graph Execution (`phase-9-graph-execution`)

**Goal:** Spec §8.3: campd as purely mechanical control dispatcher — check loops, retry classification, `on_complete` fan-out, run finalization.

**Files:** `camp/src/daemon/dispatch.rs` (extend); `camp-core/src/formula/runtime.rs` (attempt/iteration bookkeeping as pure functions over ledger state); tests `camp/tests/daemon_graph.rs`.

**Semantics (gc's, verbatim where the concept exists):**
- Close of a step → `newly_ready` subgraph → immediate dispatch up to cap (≤1 s target, asserted locally in Phase 13).
- `check` steps: campd runs `check.path` (cwd = rig, `check.timeout` enforced, step `timeout` as general bound); exit 0 → close pass; nonzero with budget left → next iteration bead + spawn; budget exhausted → step closes fail. Events: camp-specific `check.passed`/`check.failed` with attempt numbers. The checker is a script; campd never judges (spec §8.3).
- `retry` steps: worker close carries the classification — pass; hard fail; or transient (`camp close --outcome fail --transient` → data `failure_class:"transient"`, gc's key vocabulary). Transient + budget → respawn attempt; exhausted → `on_exhausted` disposition (`hard_fail` fails the run; `soft_fail` closes the step failed-soft and dependents' readiness rule treats it per decision 6 — soft-fail still does NOT satisfy `needs`; it exists for runs whose remaining steps don't depend on the soft-failed step).
- `on_complete`: on step close-pass, read the step's recorded structured output (`camp close --output-json <file|->` stores it in the close event's `data.output`), resolve `for_each` path, cook `bond` per item with `{item}`/`{item.field}`/`{index}` substitution into vars, `parallel` or `sequential` (sequential = each child's root needs the previous).
- Root finalization: last step close → root closes with aggregated outcome → `run.finalized` (camp-specific) with cause chain.

**Tests (fake agent):** diamond fan-out runs to completion; check loop passes on 2nd iteration; check budget exhaustion fails the run; transient retry exhaustion → hard vs soft table; `on_complete` over a 3-item output fans out 3 bonds (parallel and sequential variants); dispatch-latency functional assertion (close → dependent dispatch observed; the ≤1 s wall-clock number is Phase 13/15).

**Exit criteria:** every §8.2 construct executes with gc semantics; `doctor --refold` clean after every integration run (asserted in the tests); CI green.

## Phase 10 — Orders (`phase-10-orders`)

**Goal:** Spec §9: cron- and event-triggered formulas with a timer heap, not a tick.

**Files:** `camp-core/src/orders/{mod,parse,cron}.rs`; `camp/src/daemon/event_loop.rs` (heap deadline integration); `camp/src/cmd/order.rs`; tests.

**Interfaces:**
```rust
pub enum Trigger { Cron { expr: CronExpr }, Event { event_type: String, label: Option<String> } }
pub struct Order { pub name: String, pub trigger: Trigger, pub formula: String,
                   pub rig: Option<String>, pub catch_up_window: Duration /* default 2h; 0 disables */ }
pub struct CronHeap { /* min-heap of (next_fire, order) */ }
impl CronHeap {
    pub fn next_deadline(&self) -> Option<Timestamp>;           // becomes the poll timeout
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<&Order>;  // pops due, reschedules
    pub fn recompute(&mut self, now: Timestamp, last_seen: Timestamp) -> Vec<CatchUp>; // wall-clock jumps
}
```
- `camp.toml` `[[order]]` exactly as spec §9 (`on = "cron:…"` / `"event:type[label=x]"`); parse errors name the order and field.
- Event orders evaluate on the same post-commit processing path as readiness (zero standing cost); label filter matches `bead.*` events whose bead carries the label.
- Wall-clock jump handling: each loop wake compares expected vs actual wall time; jumps recompute deadlines; missed fires within `catch_up_window` fire once on wake.
- `camp order ls` / `camp order run <name>` (manual fire). camp.toml watched via `notify`; reload emits camp-specific `config.changed` event (spec §13.4).
- Ship `contrib/launchd/com.gascamp.campd.plist.example` — the optional fire-at-login launchd agent (spec §5/§9: example only, never auto-installed), with its install one-liner documented in the order docs alongside the honest away-mode limits.
- Vocab: `order.fired`/`order.completed`/`order.failed` move into `GC_MIRRORED_EVENTS` (names verified against the pin).

**Tests:** cron parse/next-fire table (5-field, DST boundaries, month ends) with `FixedClock`; heap ordering under interleaved schedules; sleep/wake catch-up inside and outside the window; `"0"` disables; event order fires on matching close and not otherwise; integration: cron order cooks and completes a formula via fake agent; config edit hot-reloads with event.

**Exit criteria:** away-mode is the same code path demonstrably (order fires with no user session; ledger tells the story); no polling introduced (heap deadline = poll timeout, idle heap = infinite wait); CI green.

## Phase 11 — Health Patrol and Adoption (`phase-11-patrol-adoption`)

**Goal:** Spec §10 and §8.5: death (done in Phase 8), stall, escalation-as-content, and registry↔reality reconciliation.

**Files:** `camp-core/src/patrol/{mod,timers}.rs` (pure timer/ladder state machines); `camp/src/daemon/patrol.rs` (notify watches + nudge/restart actions); `camp/src/cmd/adopt.rs`; tests.

**Semantics:**
- One armed timer per active campd-spawned worker (heap-integrated, same poll-timeout mechanism as orders); reset by transcript-file activity (`notify` watch on the registry's transcript path) and by any ledger event from that session. Threshold: `[patrol] stall_after = "10m"` in camp.toml, agent-frontmatter override.
- Fire → `agent.stalled` event (camp-specific) → mechanical ladder from the agent definition: `nudge` (resume the session with a status-request turn — mechanics per A4 findings) then `restart` (kill, respawn, re-hook the bead) with exponential backoff and a bounded budget; ladder exhaustion emits and stops — escalation to judgment is an order matching `event:agent.stalled` (pack content, not Rust).
- Attended teammates: annotate only (`agent.stalled` + statusline badge), never kill (spec §10).
- `camp adopt` (auto at campd start, manual verb): for each registered live session probe process (pid via safe `kill(pid, 0)` wrapper or `/proc`/`ps`) and transcript mtime; dead → `session.crashed` (fold releases beads, budgets intact); living → re-arm; sweep `worktrees/` against the registry — orphans removed with `bead.worktree.reaped` events. Ground truth is observation, never state (spec §8.5).

**Tests:** timer arm/reset/fire state machine with `FixedClock` (transcript touch resets; ledger event resets; threshold fires); ladder table (nudge → restart → budget exhausted) incl. backoff series; integration: fake agent goes silent → stall → nudge revives it; nudge fails → restart re-hooks the bead; `kill -9` campd mid-run → restart → adopt reconciles exactly (crashed marked, live re-armed, orphan worktree swept).

**Exit criteria:** every patrol action is an event with a cause; zero patrol code paths poll (watches + timers only); CI green.

## Phase 12 — Plugin and Packs (`phase-12-plugin-packs`)

**Goal:** Spec §11: the Claude Code session becomes the control plane; machinery only, zero shipped roles.

**Files:** `plugin/` (plugin manifest, `commands/{sling,status,adopt,events}.md`, `hooks/` (SessionStart, SessionEnd, optional PostToolUse breadcrumb — off by default), `skills/worker/SKILL.md`, `statusline/`); `packs/starter/` (example `agents/dev.md`, `agents/reviewer.md`, `formulas/guarded-change.toml` = the corpus file, `orders.toml` example); hook tests under `plugin/tests/` driven by fixture stdin payloads.

**Content contracts:**
- Slash commands are thin wrappers over the `camp` CLI (identical scripting surface, spec §13.6).
- Worker skill text = the lifecycle contract: `recall` → `claim` → work → `event emit` milestones → `remember` non-obvious findings → `close` with outcome → exit.
- Hooks: SessionStart registers/adopts; SessionEnd appends the session-end event (NOT Stop, which fires per turn, and NOT SubagentStop, whose session_id is the parent — see the Phase 12 plan's D5); all hooks are fire-and-forget appends with throttling (spec §16 requires this verified).
- Statusline snippet: one `status` socket query rendering `▲live ●ready ✖red`; degrades to empty output (with stderr note) when campd is down — visible degradation, not silence.
- Attended Tier-0 sling spawns a teammate per A1 findings; if A1 resolved weaker, `/sling` prints the instant-attach line instead (the decided fallback).

**Tests:** each hook exercised against recorded fixture stdin JSON (exit codes, appended events, throttle behavior); command markdown ↔ CLI flag parity check (script); starter pack passes `camp doctor --formula` and the Phase 6 gc gate (corpus symlink).

**Exit criteria:** driving a camp from inside a Claude Code session works end to end; plugin ships zero agent definitions (checked by a repo-policy test); CI green.

## Phase 13 — Perf and Volume Suite, local-only (`phase-13-perf-volume`)

**Goal:** The §14 cost budget as executable assertions where §16 places them — run via `make perf` (local-only per the 2026-07-05 decision), exact targets, never skipped-silently (the suite either runs and asserts or is not invoked).

**Files:** `crates/camp-core/tests/perf_volume.rs`, `crates/camp/tests/perf_daemon.rs` (all `#[ignore]`, run by `make perf`); `camp/src/cmd/backup.rs`; `Makefile`.

**Assertions (exact §14 numbers):**
| Assertion | Where |
|---|---|
| Volume fixture: 30 heavy days, ≥1M events, ~100k beads (seeded RNG, generated through the real `append` path) builds without error; `doctor --refold` clean at volume | `perf_volume.rs` |
| Ledger write (event + state effect, one WAL txn): p50 and p99 < 1 ms over 10k appends into the 1M-event db | `perf_volume.rs` |
| Ranked FTS over the year-scale corpus: a 10-query set, each < 50 ms | `perf_volume.rs` |
| `ls --ready` indexed read at volume: < 10 ms (spec "low-millisecond") | `perf_volume.rs` |
| Idle campd: CPU time delta == 0 (±10 ms) over a 30 s idle window; RSS < 20 MB (via `ps -o cputime=,rss=`) | `perf_daemon.rs` |
| Sling → worker spawn ≤ 2 s and step close → dependent dispatched ≤ 1 s, fake-agent timing | `perf_daemon.rs` |
| `camp backup` (`VACUUM INTO`) of the 1M-event db: completes, integrity_check ok | `perf_volume.rs` |

`Makefile`: `perf:` runs the ignored tests `--release`; `e2e:` (Phase 15). AGENTS.md already tells every session when to run them.

**Exit criteria:** `make perf` green on the dev machine with numbers recorded in the PR description; CI untouched.

## Phase 14 — Export Bridge (`phase-14-export-bridge`)

**Goal:** Spec §15.3: `camp export --city <dir>` — graduation is an export, not a backend.

**Files:** `camp-core/src/export.rs`; `camp/src/cmd/export.rs`; golden-output tests.

**Contracts:**
- `beads.jsonl`: bd wire format (per gascity `docs/reference/exec-beads-provider.md` and `internal/beads/beads.go` JSON tags): `id`, `title`, `status` (camp's open/in_progress/closed map 1:1), `issue_type` (`task`→`task`; camp `memory` beads → `issue_type:"task"` + label `camp-memory` unless the phase-plan research against bd import finds a native memory type), `created_at`/`updated_at`, `assignee`, `description`, `labels`, `needs`, `metadata` carrying `gc.outcome`/`gc.final_disposition` per the vocabulary mirror. The field-level mapping table is written into the phase plan and into `docs/reference/export.md`.
- `formulas/`: the pinned copies from `runs/` (already valid v2 subset files — Phase 6 proved it).
- `pack/`: agent definitions verbatim + generated `pack.toml` wrapper + camp orders translated to gc order TOML (`on="cron:X"` → `trigger="cron", schedule="X"`; `on="event:T"` → `trigger="event", on="T"`); untranslatable orders (e.g. `[label=…]` filters) **fail the export** listing them, with `--skip-untranslatable` as the explicit opt-out.

**Tests:** golden export of a fixture camp (beads incl. closed-with-outcome history, one cooked run, orders both kinds); JSONL parses line-by-line and field-maps exactly; untranslatable-order failure and explicit skip; optional local check: if a `bd` binary is present, `bd import` the JSONL (not in CI).

**Exit criteria:** a Gas City operator could import the output directory with standard tooling; CI green.

## Phase 15 — Opt-in E2E with real Claude (`phase-15-e2e`)

**Goal:** Spec §16's e2e bullet: prove the whole thesis on the real runtime, locally.

**Files:** `crates/camp/tests/e2e.rs` (`#[ignore]`, requires `CAMP_E2E=1` + authenticated `claude` CLI); fixture mini-repo under `crates/camp/tests/fixtures/toy-project/` (a tiny CLI with a real test suite that a worker can extend).

**Scenarios:** (1) Tier-0: `camp sling "add a --json flag to toy ls, TDD it"` against the toy rig → worker claims, works, closes pass; assert sling→first transcript token ≤ 2 s, total ledger writes for the Tier-0 envelope ≈ 3 (created/claimed/closed + milestones), `camp show` tells the whole story. (2) One `guarded-change` formula run with a real verification script; assert step-close→dependent-dispatch ≤ 1 s. (3) Post-run: idle campd 0.0% CPU window re-asserted with real transcripts on disk.

**Exit criteria:** `make e2e` green locally with numbers in the PR description; the "hours for a flag" problem is measurably dead.

---

# Spec Coverage Matrix

| Spec section | Where |
|---|---|
| §5 verbs: init/doctor/events | P1 · sling/event emit P8 · ls/show/claim/close/rig P3 · search/remember/recall P4 · stop/top P7 · order P10 · adopt P11 · backup P13 · export P14 |
| §7.1 layout, §7.2 event log + txn write, §7.4 registry/queries | P1 (schema, append, canonical JSON), P3 (queries), P4 (search/memory) |
| §7.3 readiness on write, no query loops | P3 (subgraph fn), P7 (poke/cursor), P8/P9 (dispatch) |
| §7.6 scale envelope | P13 volume fixture |
| §8.1 Tier-0 | P8 + P15 |
| §8.2 formula subset | P5 + P6 (CI gate) |
| §8.3 mechanical dispatcher / §8.4 visibility invariant | P9 / P8 (registry-at-birth tests) |
| §8.5 adoption | P11 |
| §9 orders | P10 |
| §10 patrol | P8 (death), P11 (stall/ladder) |
| §11 packs/plugin | P12 |
| §12 multi-rig/worktrees | P3 (rigs), P8 (cwd/worktrees), P11 (sweep) |
| §13 guarantees 1–6 | tests named in P1 (1,5), P8 (2,3), P10 (4), P12 (6) |
| §14 cost budget | P13 (`make perf`), P15 (`make e2e`) — local-only by decision |
| §15.2 compatibility contracts | P6 (1,2), P12+P14 (3) |
| §15.3 migration | P14 |
| §16 testing strategy | unit throughout; fake agent P8+; compatibility P6; e2e P15; hooks P12 |
| §17 A1–A4 | P2 |

# Risks and Watch Items

1. **A1/A4 mechanics** may differ from assumption — Phase 2 exists to catch this before Phase 8 consumes it; fallbacks are pre-decided in the spec (§17).
2. **rusqlite FTS5 feature flag** — Task 1.1's MATCH round-trip test surfaces it immediately.
3. **gc internal API drift** — the compat shim compiles against a pinned ref, so gascity changes can't break camp CI silently; bumping the pin is a deliberate PR with the vocab cross-check re-verifying.
4. **`claude -p` flag surface changes** — pinned as facts in the Phase 2 findings doc; e2e (P15) is the canary.
5. **Timing assertions** are all local (`make perf`/`make e2e`) by decision; CI carries only deterministic tests.

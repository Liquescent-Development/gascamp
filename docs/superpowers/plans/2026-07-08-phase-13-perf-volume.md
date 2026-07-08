# Phase 13 — Perf and Volume Suite Implementation Plan

> **APPROVED (2026-07-08)** by the automated Opus plan review, relayed through the team lead: execution-ready, all interface claims verified against origin/main, the ps-cputime ±10 ms tolerance confirmed well-calibrated for macOS centisecond resolution (detects both busy-loop and tick-storm regressions), and the fixture faithfully implements the binding master-plan 1M-event / 100k-bead target with no §14 number weakened. Both flagged items ruled sound (FTS latency depends on corpus SIZE not calendar span; ps-cputime methodology sound on macOS). Three non-blocking corrections folded into this doc: (1) reconciliation prose corrected to the 30-heavy-day / 1M-event scale, not "year-scale volume"; (2) idle-CPU harness carries a macOS-resolution caveat; (3) the harness copy range is `daemon_dispatch.rs:10-149` (the `use` block starts at line 10).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the spec §14 cost budget into executable, exact assertions run locally via `make perf` — a seeded 1M-event / 100k-bead volume fixture built through the real append path, ledger-write / FTS / readiness latency benchmarks, an idle-daemon CPU+RSS harness, dispatch-latency timing with the fake agent, and a `camp backup` verb (SQLite `VACUUM INTO`) that produces an integrity-checked copy.

**Architecture:** The measured assertions live in two `#[ignore]`d integration-test files (`crates/camp-core/tests/perf_volume.rs`, `crates/camp/tests/perf_daemon.rs`) invoked only by `make perf` in `--release`. They are LOCAL-ONLY by the 2026-07-05 perf decision: CI never runs them. CI green proves only that the suite **compiles** and that the **non-ignored** helper/unit tests (percentile, generator determinism, cputime parsing, the `camp backup` CLI verb) stay green. The `camp backup` verb is real product code: a `Ledger::backup_into` method in camp-core (`VACUUM INTO` + `PRAGMA integrity_check`) with a thin CLI wrapper.

**Tech Stack:** Rust (edition 2024), rusqlite (bundled SQLite, WAL+FTS5), `fastrand` (already a camp-core dependency — the seeded RNG needs no new crate), `jiff` (timestamps), `tempfile` (dev-dep), `assert_cmd`/`predicates` (dev-deps), GNU make.

## Global Constraints

- Branch: `phase-13-perf-volume`. Never commit to `main`; one reviewable PR. No co-author lines; never mention the assistant in commits.
- Gates green before push: `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`.
- Library code (camp-core) never panics: workspace clippy denies `unwrap_used`/`expect_used`/`panic`. Test files opt out with a file-level `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (the established pattern — see `daemon_dispatch.rs:1`).
- `#![forbid(unsafe_code)]` in camp-core; no `unsafe` anywhere.
- Fail fast, no fallbacks, no silenced errors. Perf targets are the spec's — never weaken an assertion to make it pass; a miss is an escalation to the lead.
- The perf suite either runs and asserts or is not invoked — **no silent skips**. `#[ignore]` (run explicitly by `make perf`) satisfies this; a runtime `return`-early skip does not.
- Exact spec §14 numbers (do not round or relax): ledger write p50 AND p99 < 1 ms; ranked FTS < 50 ms per query; `ls --ready` < 10 ms; idle CPU delta == 0 (±10 ms) over 30 s; idle RSS < 20 MB; sling → worker spawn ≤ 2 s; step close → dependent dispatched ≤ 1 s; volume fixture ≥ 1M events, ~100k beads, `doctor --refold` clean.
- Ownership: this phase owns `crates/camp-core/tests/perf_volume.rs`, `crates/camp/tests/perf_daemon.rs`, the `Makefile`, the `camp backup` verb (`cmd/backup.rs` + `Ledger::backup_into` + `CoreError::Backup` + main.rs wiring), and a new `crates/camp/tests/cli_backup.rs`. Keep `main.rs` edits strictly additive (sibling phase-11 may also touch it). Do not touch `patrol/**`, `daemon/patrol.rs`, `cmd/adopt.rs`, or `daemon/event_loop.rs` (phase-11's surface).

### Spec reconciliation (documented, not an ambiguity to escalate)

The master plan calls the fixture "30 heavy days, ≥1M events, ~100k beads"; spec §14 says FTS "over a year of history" and the Phase 13 table says "year-scale corpus." The operative point on which these agree: **the FTS latency target is a function of corpus SIZE, not calendar span** — a ranked bm25 query over 1M FTS rows is the workload being bounded, whichever wall-clock window produced those rows.

Be precise about the scale, though: this fixture is the **master-plan-mandated 30-heavy-day / ≥1M-event / ~100k-bead scale**, which is roughly **1/10** of spec §7.6's literal *year* of heavy use (~10–15M events / ~1M beads). It is deliberately NOT "a year's worth of volume." The `AdvancingClock` spreads timestamps across ~34 days (≈1 month, 3 s/event × ~1.0M events) so `created_ts` is realistic and monotonic; no assertion depends on the exact span.

The fixture scale is a single pair of constants in one call — `build_fixture(&db, 100_000, 1_000_000)` in `volume_suite`. A future lead could raise it toward literal §7.6 year-scale (~1M beads / ~10–15M events) if desired; that would **exceed** the master plan's binding ≥1M/~100k target, so it is an enhancement decision, not a contract requirement. **Do not do it in this phase** — meet the mandated target exactly.

### Consumed interfaces (from merged phases — verified, exact)

- **Ledger (Phase 1)** `camp_core::ledger::Ledger` (`crates/camp-core/src/ledger/mod.rs`):
  - `Ledger::open(db_path: &Path) -> Result<Self, CoreError>` (mod.rs:39) — WAL, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`, schema v1 auto-created on first open.
  - `Ledger::open_with_clock(db_path: &Path, clock: Box<dyn Clock>) -> Result<Self, CoreError>` (mod.rs:43).
  - `Ledger::open_read_only(db_path: &Path) -> Result<Self, CoreError>` (mod.rs:52) — errors on a missing/schema-less db; **never creates**.
  - `Ledger::append(&mut self, input: EventInput) -> Result<Seq, CoreError>` (mod.rs:69) — one `BEGIN IMMEDIATE` WAL txn: event insert + `fold::apply`.
  - `Ledger::append_batch(&mut self, inputs: Vec<EventInput>) -> Result<Vec<Seq>, CoreError>` (mod.rs:81) — all-or-nothing in one txn; `append` delegates to it (so batch build IS the real append path).
  - `Ledger::refold_check(&mut self) -> Result<RefoldReport, CoreError>` (refold.rs:63; re-exported at mod.rs:8). `RefoldReport { events_replayed: u64, drift: Vec<DriftEntry> }`.
  - `Ledger::search(&self, query: &str, type_filter: Option<&str>, limit: usize) -> Result<Vec<SearchHit>, CoreError>` (mod.rs:479).
  - `Ledger::ready_beads(&self, rig: Option<&str>) -> Result<Vec<BeadRow>, CoreError>` (mod.rs:115) — the `ls --ready` indexed read (`beads_status_rig` + `deps_needs` indexes, `schema.rs:36,43`).
  - Private `conn: Connection` field — accessible only inside `mod.rs` (where `backup_into` is added).
- **Event model (Phase 1)** `camp_core::event` (`event.rs`): `EventInput { kind: EventType, rig: Option<String>, actor: String, bead: Option<String>, data: serde_json::Value }`. Payload shapes the fold requires (verified in `fold.rs`):
  - `bead.created`: `{ "title": <non-empty str>, "type"?: one of ["task","mail","memory"] (default "task"), "description"?: str, "needs"?: [bead-id str], "labels"?: [str], "assignee"?: str }`; event **must** set `rig`. Inserts a `beads` row + a `search` `'body'` row (`"{title}\n{description}"`) + `deps` rows for `needs` + bumps the id counter.
  - `bead.claimed`: `{ "session": str }`; requires the bead status `open`.
  - `bead.updated`: `{ "title"?: str, "description"?: str }` (at least one; title non-empty if set); rewrites the `search` `'body'` row.
  - `bead.closed`: `{ "outcome": one of ["pass","fail","skipped"], "reason"?: str, "failure_class"?: "transient" (fail only), "final_disposition"?: ["hard_fail","soft_fail"] (fail only) }`; requires bead not already closed; a non-empty `reason` inserts a `search` `'close'` row.
  - `worker.milestone`: `{ "text": <non-empty str> }`; bead optional but if set must exist.
- **Clock (Phase 1)** `camp_core::clock::Clock { fn now_utc(&self) -> String }` (`clock.rs:4`), `SystemClock`, `FixedClock`.
- **Daemon harness (Phases 7/8/9)**: real child `campd`, `[dispatch].command` points at `crates/camp/tests/fake-agent.sh`, readiness line `"campd listening on "`. Full template: `crates/camp/tests/daemon_dispatch.rs:10-149` (the `use` block starts at line 10 — `use std::io::{BufRead, BufReader};` / `use std::path::{Path, PathBuf};` — and `BufRead`/`BufReader` are required by `Daemon::spawn`'s readiness read). `session.woke` carries `e["data"]["bead"]`. `sling`/`create` print the new bead id to stdout. Close→dependent pattern: `daemon_dispatch.rs:239-274`.
- **CampDir (Phase 2)** `crate::campdir::CampDir`: `db_path() -> PathBuf` = `<root>/camp.db`; `CampDir::resolve(flag: Option<&Path>) -> anyhow::Result<CampDir>`.

---

## Task 1: `Ledger::backup_into` + `CoreError::Backup` (camp-core)

**Files:**
- Modify: `crates/camp-core/src/error.rs` (add one enum variant)
- Modify: `crates/camp-core/src/ledger/mod.rs` (add `backup_into` method + a unit test)

**Interfaces:**
- Consumes: `Ledger`'s private `conn`, `open`/`open_read_only`, `CoreError`.
- Produces:
  - `camp_core::error::CoreError::Backup(String)` — `#[error("backup: {0}")]`.
  - `Ledger::backup_into(&self, dest: &std::path::Path) -> Result<(), CoreError>` — writes a consistent, defragmented copy via `VACUUM INTO`, then verifies it with `PRAGMA integrity_check`. Works on a read-only or read-write `Ledger` (SQLite `VACUUM INTO` supports read-only source databases and never modifies the source). Fails fast if `dest` already exists.

- [ ] **Step 1: Add the `Backup` error variant**

In `crates/camp-core/src/error.rs`, add this variant to `enum CoreError` (place it after the `Export` variant at line 50, before `UntranslatableOrders`):

```rust
    /// A `camp backup` failure: the destination already exists, the VACUUM
    /// INTO copy failed, or the copy did not pass `PRAGMA integrity_check`.
    #[error("backup: {0}")]
    Backup(String),
```

- [ ] **Step 2: Write the failing unit test**

In `crates/camp-core/src/ledger/mod.rs`, inside the existing `#[cfg(test)] mod tests { ... }` block (it already has `use super::*;` and the `temp_ledger()` helper at mod.rs:547), add:

```rust
    #[test]
    fn backup_into_copies_and_passes_integrity_check() {
        let (dir, mut l) = temp_ledger();
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({ "title": "backup me" }),
        })
        .unwrap();

        let dest = dir.path().join("backup.db");
        l.backup_into(&dest).unwrap();
        assert!(dest.exists());

        // The copy is a standalone, valid ledger carrying the same event.
        let copy = rusqlite::Connection::open(&dest).unwrap();
        let ok: String = copy
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        let n: i64 = copy
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        // Fail fast: refuse to overwrite an existing destination.
        let err = l.backup_into(&dest).unwrap_err();
        assert!(
            matches!(err, CoreError::Backup(msg) if msg.contains("already exists")),
            "expected an already-exists Backup error"
        );
    }
```

- [ ] **Step 3: Run it to watch it fail**

Run: `cargo test -p camp-core --lib backup_into_copies_and_passes_integrity_check`
Expected: FAIL — `no method named backup_into found for struct Ledger`.

- [ ] **Step 4: Implement `backup_into`**

In `crates/camp-core/src/ledger/mod.rs`, add this method to `impl Ledger` (place it near the other read/utility methods, e.g. just after the `search` method around mod.rs:486). Confirm `use std::path::Path;` is in scope at the top of the file (the module already opens paths — it is).

```rust
    /// Write a consistent, defragmented copy of the ledger to `dest` via
    /// SQLite `VACUUM INTO`, then verify the copy with `PRAGMA
    /// integrity_check`. The copy is a single standalone db file with no
    /// WAL sidecar — safe to archive or move. `dest` must not already
    /// exist. Never modifies the source; safe on a read-only `Ledger`.
    pub fn backup_into(&self, dest: &Path) -> Result<(), CoreError> {
        if dest.exists() {
            return Err(CoreError::Backup(format!(
                "destination {} already exists",
                dest.display()
            )));
        }
        let dest_str = dest.to_str().ok_or_else(|| {
            CoreError::Backup(format!("destination {} is not valid UTF-8", dest.display()))
        })?;
        // VACUUM INTO does not accept a bound parameter for the filename;
        // inline it with single-quotes doubled to escape.
        let escaped = dest_str.replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{escaped}'"))?;

        let verify = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let report: String =
            verify.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if report != "ok" {
            return Err(CoreError::Backup(format!(
                "integrity_check on backup {} reported: {report}",
                dest.display()
            )));
        }
        Ok(())
    }
```

Ensure `Connection` and `OpenFlags` are imported at the top of `mod.rs`. `Connection` is already imported (the module uses it throughout). Add `OpenFlags` to the rusqlite import — find the existing `use rusqlite::{...};` line and add `OpenFlags`, e.g. `use rusqlite::{Connection, OpenFlags, TransactionBehavior};` (keep whatever else is already listed).

- [ ] **Step 5: Run it to watch it pass**

Run: `cargo test -p camp-core --lib backup_into_copies_and_passes_integrity_check`
Expected: PASS.

- [ ] **Step 6: Full camp-core gates**

Run: `cargo fmt -p camp-core && cargo clippy -p camp-core --all-targets --all-features -- -D warnings && cargo test -p camp-core`
Expected: clean; all camp-core tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/camp-core/src/error.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat(core): Ledger::backup_into (VACUUM INTO + integrity_check)"
```

---

## Task 2: `camp backup` CLI verb

**Files:**
- Create: `crates/camp/src/cmd/backup.rs`
- Modify: `crates/camp/src/main.rs` (declare the module, add the `Backup` subcommand variant, add the dispatch arm — all additive)
- Create (test): `crates/camp/tests/cli_backup.rs` (non-ignored — CI exercises the verb)

**Interfaces:**
- Consumes: `Ledger::open_read_only`, `Ledger::backup_into` (Task 1), `CampDir::resolve`, `CampDir::db_path`.
- Produces: `camp backup <DEST>` CLI subcommand; `cmd::backup::run(camp: &CampDir, dest: std::path::PathBuf) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing CLI test**

Create `crates/camp/tests/cli_backup.rs`:

```rust
//! Phase 13: the `camp backup` verb (VACUUM INTO + integrity_check). This is
//! the CI-safe coverage of the verb; the 1M-event volume backup lives in the
//! #[ignore]d perf suite (`make perf`).
use assert_cmd::Command;
use predicates::prelude::*;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

#[test]
fn backup_writes_an_integrity_checked_copy_and_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    camp().arg("--camp").arg(root).arg("init").assert().success();

    let dest = root.join("snapshot.db");
    camp()
        .arg("--camp")
        .arg(root)
        .arg("backup")
        .arg(&dest)
        .assert()
        .success()
        .stdout(predicate::str::contains("integrity_check ok"));
    assert!(dest.exists());

    // Fail fast on an existing destination.
    camp()
        .arg("--camp")
        .arg(root)
        .arg("backup")
        .arg(&dest)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn backup_without_a_camp_fails() {
    let dir = tempfile::tempdir().unwrap();
    // no `camp init` here: no camp.toml -> resolve fails
    camp()
        .arg("--camp")
        .arg(dir.path())
        .arg("backup")
        .arg(dir.path().join("x.db"))
        .assert()
        .failure();
}
```

- [ ] **Step 2: Run it to watch it fail**

Run: `cargo test -p camp --test cli_backup`
Expected: FAIL — clap rejects the unknown `backup` subcommand (compile succeeds; assertions fail because the command errors with an unrecognized-subcommand message, not the expected stdout).

- [ ] **Step 3: Create the command module**

Create `crates/camp/src/cmd/backup.rs`:

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp backup <DEST>`: write a consistent, integrity-checked copy of the
/// camp ledger to DEST via SQLite `VACUUM INTO`. DEST must not already
/// exist. Read-only on the source, so it is safe to run against a live camp.
pub fn run(camp: &CampDir, dest: PathBuf) -> Result<()> {
    let ledger = Ledger::open_read_only(&camp.db_path())?;
    ledger.backup_into(&dest).with_context(|| {
        format!(
            "backing up {} to {}",
            camp.db_path().display(),
            dest.display()
        )
    })?;
    println!("backup written to {} (integrity_check ok)", dest.display());
    Ok(())
}
```

- [ ] **Step 4: Wire it into `main.rs` (additive only)**

In `crates/camp/src/main.rs`:

(a) In the `mod cmd { ... }` block (main.rs:5-24), add — keep the list grouped as the file does:

```rust
    pub mod backup;
```

(b) In `enum Command` (main.rs:50-221), add a new variant (place it next to other file-producing verbs; `PathBuf` is already in scope because `Doctor` uses it):

```rust
    /// Write a consistent, integrity-checked copy of the ledger (VACUUM
    /// INTO). DEST must not already exist.
    Backup {
        /// Destination file for the backup copy.
        dest: std::path::PathBuf,
    },
```

(c) In the `match cli.command { ... }` dispatch in `fn run` (main.rs:318-447), add an arm (mirror the `Top` arm's camp-resolving shape at main.rs:442-445):

```rust
        Command::Backup { dest } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::backup::run(&camp, dest)
        }
```

- [ ] **Step 5: Run the CLI test to watch it pass**

Run: `cargo test -p camp --test cli_backup`
Expected: PASS (both tests).

- [ ] **Step 6: camp-crate gates**

Run: `cargo fmt -p camp && cargo clippy -p camp --all-targets --all-features -- -D warnings && cargo test -p camp --test cli_backup`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/cmd/backup.rs crates/camp/src/main.rs crates/camp/tests/cli_backup.rs
git commit -m "feat(camp): camp backup verb (VACUUM INTO)"
```

---

## Task 3: Volume + throughput suite (`perf_volume.rs`)

**Files:**
- Create: `crates/camp-core/tests/perf_volume.rs`

**Interfaces:**
- Consumes: `Ledger::{open, open_with_clock, append, append_batch, refold_check, search, ready_beads, backup_into}`, `EventInput`/`EventType`, `Clock`, `fastrand`, `jiff`, `rusqlite`, `tempfile`.
- Produces (within this file): `fn build_fixture(db_path: &Path, bead_target: usize, event_floor: usize) -> (u64, u64)`; `fn percentile(sorted: &[u128], p: f64) -> u128`; the `#[ignore]` `volume_suite` test; the non-ignored `percentile_is_nearest_rank` and `fixture_generation_is_deterministic` tests.

- [ ] **Step 1: Write the file's non-ignored helper tests first (percentile + determinism), plus the generator they need**

Create `crates/camp-core/tests/perf_volume.rs` with the full contents below. The two non-ignored tests (`percentile_is_nearest_rank`, `fixture_generation_is_deterministic`) are the failing tests for this step; the `#[ignore]` `volume_suite` is added in the same file but not run until `make perf`.

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 13 volume + throughput suite (spec §14 / §16). LOCAL-ONLY: the
//! measured assertions in `volume_suite` are #[ignore]d and run only by
//! `make perf` in --release. The non-ignored tests exercise the fixture
//! generator and the pure helpers so CI keeps them correct and compiling.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use camp_core::clock::Clock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

/// Fixed seed: the whole corpus is a deterministic function of this.
const SEED: u64 = 0x00C0_FFEE_CA11_0013;

/// Rigs the corpus spreads across (name == prefix). Multiple rigs give the
/// `beads_status_rig` index real selectivity and let `ls --ready` filter.
const RIGS: &[&str] = &["gc", "app", "core", "web", "data"];

/// Vocabulary the corpus draws titles/descriptions/close-reasons from, so
/// FTS queries built from these words return real ranked hits.
const WORDS: &[&str] = &[
    "ledger", "worker", "spawn", "dispatch", "formula", "bead", "rig", "pack",
    "cron", "order", "search", "memory", "refold", "vacuum", "socket", "timer",
    "graph", "retry", "check", "close", "claim", "milestone", "session", "event",
    "flag", "parser", "index", "cursor", "throttle", "watch", "signal", "queue",
    "backup", "corpus", "latency", "volume", "seed", "fixture", "assert", "budget",
];

const LABELS: &[&str] = &["perf", "infra", "bug", "chore", "docs", "spike"];

/// A clock whose timestamps advance a fixed step per event, so the fixture
/// spans ~30 heavy days of history (spec §16) instead of collapsing to one
/// instant. `Clock: Send`; the build is single-threaded but the counter is
/// atomic so the boxed clock stays `Send`.
struct AdvancingClock {
    start_secs: i64,
    step_secs: i64,
    n: AtomicI64,
}

impl AdvancingClock {
    fn new() -> Self {
        // 2026-01-01T00:00:00Z == unix 1_767_225_600. 3 s/event over ~1.0M
        // events ≈ 34 days ≥ 30 heavy days.
        Self {
            start_secs: 1_767_225_600,
            step_secs: 3,
            n: AtomicI64::new(0),
        }
    }
}

impl Clock for AdvancingClock {
    fn now_utc(&self) -> String {
        let i = self.n.fetch_add(1, Ordering::Relaxed);
        let secs = self.start_secs + i * self.step_secs;
        jiff::Timestamp::from_second(secs)
            .unwrap()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
}

fn pick<'a>(rng: &fastrand::Rng, xs: &[&'a str]) -> &'a str {
    xs[rng.usize(..xs.len())]
}

fn words(rng: &fastrand::Rng, min: usize, max: usize) -> String {
    let n = rng.usize(min..=max);
    (0..n)
        .map(|_| pick(rng, WORDS))
        .collect::<Vec<_>>()
        .join(" ")
}

fn flush(ledger: &mut Ledger, batch: &mut Vec<EventInput>) {
    if !batch.is_empty() {
        ledger.append_batch(std::mem::take(batch)).unwrap();
    }
}

/// Build a deterministic corpus at `db_path`: exactly `bead_target` beads
/// across `RIGS`, each with a seeded lifecycle (created → maybe claimed →
/// milestones → updates → maybe closed), then topped up with milestone
/// breadcrumbs until at least `event_floor` events exist. Events are written
/// through `append_batch` (the real append path) in 5000-event WAL txns.
/// Returns (events_appended, beads_created).
fn build_fixture(db_path: &Path, bead_target: usize, event_floor: usize) -> (u64, u64) {
    let clock: Box<dyn Clock> = Box::new(AdvancingClock::new());
    let mut ledger = Ledger::open_with_clock(db_path, clock).unwrap();
    let rng = fastrand::Rng::with_seed(SEED);

    let mut counters: HashMap<&str, i64> = HashMap::new();
    let mut per_rig_ids: HashMap<&str, Vec<String>> = HashMap::new();
    let mut all_ids: Vec<String> = Vec::new();
    let mut batch: Vec<EventInput> = Vec::new();
    let mut events: u64 = 0;

    for _ in 0..bead_target {
        let rig = pick(&rng, RIGS);
        let n = {
            let c = counters.entry(rig).or_insert(0);
            *c += 1;
            *c
        };
        let id = format!("{rig}-{n}");

        // backward deps within the same rig (exercises readiness NOT EXISTS)
        let mut needs: Vec<String> = Vec::new();
        if let Some(prev) = per_rig_ids.get(rig) {
            if !prev.is_empty() && rng.f32() < 0.35 {
                let k = rng.usize(1..=2usize.min(prev.len()));
                for _ in 0..k {
                    let dep = prev[rng.usize(..prev.len())].clone();
                    if !needs.contains(&dep) {
                        needs.push(dep);
                    }
                }
            }
        }

        let mut data = serde_json::json!({
            "title": words(&rng, 3, 6),
            "description": words(&rng, 6, 12),
        });
        if !needs.is_empty() {
            data["needs"] = serde_json::json!(needs);
        }
        let nlabels = rng.usize(0..=2);
        if nlabels > 0 {
            let labels: Vec<&str> = (0..nlabels).map(|_| pick(&rng, LABELS)).collect();
            data["labels"] = serde_json::json!(labels);
        }
        batch.push(EventInput {
            kind: EventType::BeadCreated,
            rig: Some(rig.to_owned()),
            actor: "seed".into(),
            bead: Some(id.clone()),
            data,
        });
        events += 1;

        let claimed = rng.f32() < 0.9;
        if claimed {
            batch.push(EventInput {
                kind: EventType::BeadClaimed,
                rig: Some(rig.to_owned()),
                actor: "seed".into(),
                bead: Some(id.clone()),
                data: serde_json::json!({ "session": format!("s-{id}") }),
            });
            events += 1;
        }

        for _ in 0..rng.usize(4..=8) {
            batch.push(EventInput {
                kind: EventType::WorkerMilestone,
                rig: Some(rig.to_owned()),
                actor: format!("s-{id}"),
                bead: Some(id.clone()),
                data: serde_json::json!({ "text": words(&rng, 3, 8) }),
            });
            events += 1;
        }

        for _ in 0..rng.usize(0..=2) {
            batch.push(EventInput {
                kind: EventType::BeadUpdated,
                rig: Some(rig.to_owned()),
                actor: "seed".into(),
                bead: Some(id.clone()),
                data: serde_json::json!({ "description": words(&rng, 6, 12) }),
            });
            events += 1;
        }

        if claimed && rng.f32() < 0.78 {
            let roll = rng.f32();
            let outcome = if roll < 0.8 {
                "pass"
            } else if roll < 0.95 {
                "fail"
            } else {
                "skipped"
            };
            batch.push(EventInput {
                kind: EventType::BeadClosed,
                rig: Some(rig.to_owned()),
                actor: format!("s-{id}"),
                bead: Some(id.clone()),
                data: serde_json::json!({ "outcome": outcome, "reason": words(&rng, 4, 10) }),
            });
            events += 1;
        }

        per_rig_ids.entry(rig).or_default().push(id.clone());
        all_ids.push(id);

        if batch.len() >= 5000 {
            flush(&mut ledger, &mut batch);
        }
    }
    flush(&mut ledger, &mut batch);

    // Top up to the event floor with milestone breadcrumbs on existing beads.
    let mut i = 0usize;
    while events < event_floor as u64 {
        let id = all_ids[i % all_ids.len()].clone();
        let rig = id.split_once('-').unwrap().0.to_owned();
        batch.push(EventInput {
            kind: EventType::WorkerMilestone,
            rig: Some(rig),
            actor: "seed".into(),
            bead: Some(id),
            data: serde_json::json!({ "text": words(&rng, 3, 8) }),
        });
        events += 1;
        i += 1;
        if batch.len() >= 5000 {
            flush(&mut ledger, &mut batch);
        }
    }
    flush(&mut ledger, &mut batch);

    (events, bead_target as u64)
}

/// Nearest-rank percentile of an ascending-sorted slice. `p` in (0, 100].
fn percentile(sorted: &[u128], p: f64) -> u128 {
    assert!(!sorted.is_empty());
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

#[test]
fn percentile_is_nearest_rank() {
    let xs: Vec<u128> = (1..=100).collect();
    assert_eq!(percentile(&xs, 50.0), 50);
    assert_eq!(percentile(&xs, 99.0), 99);
    assert_eq!(percentile(&xs, 100.0), 100);
    assert_eq!(percentile(&[42], 50.0), 42);
}

#[test]
fn fixture_generation_is_deterministic() {
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let db1 = d1.path().join("camp.db");
    let db2 = d2.path().join("camp.db");
    let a = build_fixture(&db1, 50, 0);
    let b = build_fixture(&db2, 50, 0);
    assert_eq!(a, b);

    let dump = |db: &Path| -> Vec<(String, Option<String>, String, String)> {
        let conn = rusqlite::Connection::open(db).unwrap();
        let mut stmt = conn
            .prepare("SELECT type, bead, ts, data FROM events ORDER BY seq")
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert_eq!(dump(&db1), dump(&db2), "the seeded corpus must be identical");
}
```

- [ ] **Step 2: Run the non-ignored tests to watch them pass**

Run: `cargo test -p camp-core --test perf_volume`
Expected: `percentile_is_nearest_rank` and `fixture_generation_is_deterministic` PASS; `volume_suite` reported as `ignored` (added next step). If `jiff::Timestamp::from_second` is named differently in the pinned jiff 0.2, this step surfaces it as a compile error — fix by consulting `cargo doc -p jiff` for the seconds→Timestamp constructor (do not add a new dependency).

- [ ] **Step 3: Add the `#[ignore]` `volume_suite` measured test**

Append to `crates/camp-core/tests/perf_volume.rs`:

```rust
/// The spec §14 volume + throughput budget as one orchestrated assertion.
/// Builds the 1M-event / 100k-bead fixture ONCE (an expensive multi-minute
/// build in --release) and runs every volume assertion against it in order,
/// printing each measured number for the PR record. LOCAL-ONLY: run via
/// `make perf`.
#[test]
#[ignore = "volume suite: run via `make perf` (release, local-only)"]
fn volume_suite() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("camp.db");

    // 1) Build the fixture through the real append path.
    let t = Instant::now();
    let (events, beads) = build_fixture(&db, 100_000, 1_000_000);
    eprintln!(
        "[volume] built {events} events / {beads} beads in {:?}",
        t.elapsed()
    );
    assert!(events >= 1_000_000, "fixture must have >=1M events, got {events}");
    assert_eq!(beads, 100_000, "fixture must have ~100k beads, got {beads}");

    let mut ledger = Ledger::open(&db).unwrap();

    // 2) doctor --refold clean at volume.
    let t = Instant::now();
    let report = ledger.refold_check().unwrap();
    eprintln!(
        "[volume] refold replayed {} events, {} drift rows in {:?}",
        report.events_replayed,
        report.drift.len(),
        t.elapsed()
    );
    assert!(report.drift.is_empty(), "refold drift at volume: {:?}", report.drift);
    assert!(report.events_replayed >= 1_000_000);

    // 3) Ranked FTS: a 10-query set, each < 50 ms.
    let queries = [
        "ledger", "worker spawn", "dispatch formula", "bead rig",
        "search memory", "refold", "cron order", "retry check",
        "backup corpus", "latency budget",
    ];
    for q in queries {
        let t = Instant::now();
        let hits = ledger.search(q, None, 20).unwrap();
        let dt = t.elapsed();
        eprintln!("[volume] FTS {q:?}: {} hits in {dt:?}", hits.len());
        assert!(!hits.is_empty(), "FTS {q:?} returned no hits — query/corpus mismatch");
        assert!(dt < Duration::from_millis(50), "FTS {q:?} took {dt:?} (>50ms)");
    }

    // 4) ls --ready indexed read < 10 ms.
    let t = Instant::now();
    let ready = ledger.ready_beads(None).unwrap();
    let dt = t.elapsed();
    eprintln!("[volume] ls --ready: {} rows in {dt:?}", ready.len());
    assert!(!ready.is_empty(), "the corpus must have ready beads");
    assert!(dt < Duration::from_millis(10), "ls --ready took {dt:?} (>10ms)");

    // 5) camp backup (VACUUM INTO) of the 1M-event db, integrity_check ok.
    let backup = dir.path().join("backup.db");
    let t = Instant::now();
    ledger.backup_into(&backup).unwrap();
    eprintln!("[volume] backup VACUUM INTO + integrity_check in {:?}", t.elapsed());
    assert!(backup.exists());
    {
        let conn = rusqlite::Connection::open(&backup).unwrap();
        let ok: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok", "backup failed integrity_check");
        let n: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 1_000_000, "backup must carry the whole ledger, got {n}");
    }

    // 6) Ledger write p50 AND p99 < 1 ms over 10k appends into the 1M db.
    let mut samples: Vec<u128> = Vec::with_capacity(10_000);
    for i in 1..=10_000i64 {
        let input = EventInput {
            kind: EventType::BeadCreated,
            rig: Some("perf".into()),
            actor: "bench".into(),
            bead: Some(format!("perf-{i}")),
            data: serde_json::json!({ "title": "perf write path sample" }),
        };
        let t = Instant::now();
        ledger.append(input).unwrap();
        samples.push(t.elapsed().as_nanos());
    }
    samples.sort_unstable();
    let p50 = Duration::from_nanos(percentile(&samples, 50.0) as u64);
    let p99 = Duration::from_nanos(percentile(&samples, 99.0) as u64);
    eprintln!("[volume] ledger write over 10k: p50={p50:?} p99={p99:?}");
    assert!(p50 < Duration::from_millis(1), "write p50 {p50:?} (>1ms)");
    assert!(p99 < Duration::from_millis(1), "write p99 {p99:?} (>1ms)");
}
```

> **SUPERSEDED (operator ruling 2026-07-08).** Block 6 above was the pre-ruling single-pass write bench. The first 1M run showed the raw `append()` p99 (~1.1 ms) is dominated by periodic WAL autocheckpoint (~every 66 appends), which is deferred maintenance, not "one WAL transaction" cost (§14's metric; `synchronous=NORMAL` commits do not fsync). The **shipped** block 6 (`crates/camp-core/tests/perf_volume.rs`, authoritative) runs two passes — a RAW pass (autocheckpoint on, reported) and a TRANSACTION pass (a background checkpointer drains the WAL out-of-band, asserted p50 AND p99 < 1 ms) — and prints both p99s. The `< 1 ms` target is unchanged.

- [ ] **Step 4: Verify the ignored test compiles and is listed as ignored**

Run: `cargo test -p camp-core --test perf_volume -- --list`
Expected: `volume_suite` appears with `(ignored)`; `percentile_is_nearest_rank` and `fixture_generation_is_deterministic` appear as runnable.

- [ ] **Step 5: Smoke-run the volume suite once at reduced scale (dev-only, not committed)**

To de-risk before the full multi-minute run, temporarily lower the scale locally: change `build_fixture(&db, 100_000, 1_000_000)` to `build_fixture(&db, 2_000, 20_000)`, run `cargo test --release -p camp-core --test perf_volume -- --ignored --nocapture volume_suite`, confirm every block prints and passes, then **revert the numbers to `100_000, 1_000_000`**. (This is a manual sanity check; do not commit the reduced scale.)

- [ ] **Step 6: camp-core gates**

Run: `cargo fmt -p camp-core && cargo clippy -p camp-core --all-targets --all-features -- -D warnings && cargo test -p camp-core --test perf_volume`
Expected: clean; the two non-ignored tests pass; `volume_suite` ignored.

- [ ] **Step 7: Commit**

```bash
git add crates/camp-core/tests/perf_volume.rs
git commit -m "test(core): volume + throughput perf suite (spec §14, make perf)"
```

---

## Task 4: Idle + dispatch-latency suite (`perf_daemon.rs`)

**Files:**
- Create: `crates/camp/tests/perf_daemon.rs`

**Interfaces:**
- Consumes: the daemon child-process harness (copy verbatim from `daemon_dispatch.rs:10-149`), `crates/camp/tests/fake-agent.sh`, `ps`.
- Produces (within this file): `fn parse_cputime(s: &str) -> Duration`; `fn parse_rss_kb(s: &str) -> u64`; `fn ps_cputime_rss(pid: u32) -> (Duration, u64)`; `fn wait_for_instant(...) -> Instant`; `Daemon::pid`; the non-ignored parser unit tests; the three `#[ignore]` tests `idle_campd_cpu_delta_zero_and_rss_under_20mb`, `sling_to_worker_spawn_under_2s`, `close_to_dependent_dispatch_under_1s`.

- [ ] **Step 1: Create the file with the copied harness + parser helpers + non-ignored parser tests**

Create `crates/camp/tests/perf_daemon.rs`. Start with the exact harness block copied from `daemon_dispatch.rs:10-149` (the `use` lines through the end of `impl Drop for Daemon`), then add the perf-specific helpers and parser unit tests shown below. Copy verbatim so the two files stay reviewably identical; do not paraphrase the harness.

Copy these items unchanged from `daemon_dispatch.rs` (lines 10-149): the **full** `use` block — it starts at line 10 with `use std::io::{BufRead, BufReader};` and `use std::path::{Path, PathBuf};` (both `BufRead` and `BufReader` are needed by `Daemon::spawn`'s readiness read, so do not drop them) through `use std::time::{Duration, Instant};` — then `const BIN`, `const READY_PREFIX`, `fn fake_agent`, `fn camp`, `fn camp_ok`, `fn scaffold`, `fn write_agent`, `fn events_json`, `fn wait_until`, `fn count`, `fn seq_of`, `struct Daemon`, `impl Daemon` (`spawn`), `impl Drop for Daemon`. Keep the file-level `#![allow(...)]` header line too. (`seq_of` may be unused here — if clippy flags it, delete it; it is not referenced by the tests below.)

Then add, after the harness:

```rust
impl Daemon {
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

/// Tight-poll the ledger and return the `Instant` at which `pred` first holds
/// for some event. Poll granularity (5 ms) bounds the measurement error.
fn wait_for_instant(
    root: &Path,
    what: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if events_json(root).iter().any(&pred) {
            return Instant::now();
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Parse a `ps -o cputime` field into a Duration. Formats:
///  - macOS: `MM:SS.ss` or `HH:MM:SS`
///  - Linux: `[[DD-]HH:]MM:SS`
fn parse_cputime(s: &str) -> Duration {
    let s = s.trim();
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap(), r),
        None => (0, s),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, mins, secs): (u64, u64, f64) = match parts.as_slice() {
        [s] => (0, 0, s.parse().unwrap()),
        [m, s] => (0, m.parse().unwrap(), s.parse().unwrap()),
        [h, m, s] => (h.parse().unwrap(), m.parse().unwrap(), s.parse().unwrap()),
        _ => panic!("unrecognized cputime {s:?}"),
    };
    let total = days as f64 * 86400.0 + hours as f64 * 3600.0 + mins as f64 * 60.0 + secs;
    Duration::from_secs_f64(total)
}

fn parse_rss_kb(s: &str) -> u64 {
    s.trim().parse().unwrap()
}

/// Sample the process's accumulated CPU time and resident set size via `ps`.
fn ps_cputime_rss(pid: u32) -> (Duration, u64) {
    let out = std::process::Command::new("ps")
        .args(["-o", "cputime=,rss=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success(), "ps failed for pid {pid}");
    let line = String::from_utf8(out.stdout).unwrap();
    let mut it = line.split_whitespace();
    let cpu = it.next().unwrap_or_else(|| panic!("no cputime in ps output {line:?}"));
    let rss = it.next().unwrap_or_else(|| panic!("no rss in ps output {line:?}"));
    (parse_cputime(cpu), parse_rss_kb(rss))
}

#[test]
fn parse_cputime_formats() {
    assert_eq!(parse_cputime("00:00.00").as_millis(), 0);
    assert!((parse_cputime("00:00.03").as_millis() as i64 - 30).abs() <= 1);
    assert_eq!(parse_cputime("0:02.50").as_millis(), 2500);
    assert_eq!(parse_cputime("01:02:03").as_secs(), 3723);
    assert_eq!(
        parse_cputime("1-02:03:04").as_secs(),
        86400 + 2 * 3600 + 3 * 60 + 4
    );
}

#[test]
fn parse_rss_kb_parses() {
    assert_eq!(parse_rss_kb("  12345 "), 12345);
}
```

- [ ] **Step 2: Run the non-ignored parser tests to watch them pass**

Run: `cargo test -p camp --test perf_daemon`
Expected: `parse_cputime_formats` and `parse_rss_kb_parses` PASS; the three measured tests reported `ignored`.

- [ ] **Step 3: Add the three `#[ignore]` measured tests**

Append to `crates/camp/tests/perf_daemon.rs`:

```rust
/// Invariant 1 (idle is free): a campd with no work and no orders blocks in
/// `poll` — over a 30 s idle window its accumulated CPU time does not move
/// (±10 ms) and its RSS stays under 20 MB.
///
/// The ±10 ms tolerance assumes macOS `ps` cputime CENTISECOND resolution
/// (`MM:SS.ss`): 10 ms is one tick, so this detects both a busy-loop and a
/// tick-storm regression. NOTE: on Linux `ps -o cputime` has 1-SECOND
/// resolution, which would make this a coarse busy-loop-only check — a
/// future Linux runner must not read a 1 s-granularity false-green as a
/// pass; tighten the sampling (e.g. `/proc/<pid>/stat` jiffies) there.
#[test]
#[ignore = "idle harness: run via `make perf` (release, local-only)"]
fn idle_campd_cpu_delta_zero_and_rss_under_20mb() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let campd = Daemon::spawn(&root, &[]);
    let pid = campd.pid();

    let (cpu0, _rss0) = ps_cputime_rss(pid);
    std::thread::sleep(Duration::from_secs(30));
    let (cpu1, rss1) = ps_cputime_rss(pid);

    let delta = cpu1.saturating_sub(cpu0);
    eprintln!("[daemon] idle 30s: cpu delta {delta:?}, rss {rss1} KB");
    assert!(
        delta <= Duration::from_millis(10),
        "idle CPU delta {delta:?} exceeds 10 ms (invariant 1: idle is free)"
    );
    assert!(rss1 < 20 * 1024, "idle RSS {rss1} KB exceeds 20 MB");
}

/// Spec §14: sling → worker spawn ≤ 2 s. Measured wall-clock from issuing the
/// sling to observing the worker's dispatch (session.woke for the bead).
#[test]
#[ignore = "dispatch latency: run via `make perf` (release, local-only)"]
fn sling_to_worker_spawn_under_2s() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let _campd = Daemon::spawn(&root, &[]);

    let t0 = Instant::now();
    let bead = camp_ok(&root, &["sling", "add a --json flag"]).trim().to_owned();
    let woke = wait_for_instant(&root, "worker spawn", |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str()
    });
    let elapsed = woke.duration_since(t0);
    eprintln!("[daemon] sling -> worker spawn: {elapsed:?}");
    assert!(elapsed <= Duration::from_secs(2), "sling->spawn {elapsed:?} (>2s)");
}

/// Spec §14: step close → dependent dispatched ≤ 1 s. A is held mid-work; B
/// needs A. Releasing A's gate closes A (pass), which unblocks and dispatches
/// B. Measured wall-clock from observing A's close to observing B's dispatch.
#[test]
#[ignore = "dispatch latency: run via `make perf` (release, local-only)"]
fn close_to_dependent_dispatch_under_1s() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);

    let a = camp_ok(&root, &["sling", "A"]).trim().to_owned();
    wait_until(&root, "A claimed", |e| count(e, "bead.claimed") == 1);
    let b = camp_ok(&root, &["create", "B", "--needs", &a]).trim().to_owned();

    // Release A: it closes pass, which unblocks and dispatches B.
    std::fs::write(hold.join(&a), "go").unwrap();
    let t_close = wait_for_instant(&root, "A closed", |e| {
        e["type"] == "bead.closed" && e["bead"] == a.as_str()
    });
    let t_woke = wait_for_instant(&root, "B dispatched", |e| {
        e["type"] == "session.woke" && e["data"]["bead"] == b.as_str()
    });
    let elapsed = t_woke.duration_since(t_close);
    eprintln!("[daemon] close {a} -> dispatch {b}: {elapsed:?}");
    assert!(elapsed <= Duration::from_secs(1), "close->dispatch {elapsed:?} (>1s)");

    // Let B finish so Drop is clean.
    std::fs::write(hold.join(&b), "go").unwrap();
}
```

- [ ] **Step 4: Verify listing + smoke-run the fast dispatch tests once (dev-only)**

Run: `cargo test -p camp --test perf_daemon -- --list`
Expected: the three tests marked `(ignored)`; two parser tests runnable.

Then smoke-run the two fast dispatch tests once to confirm they pass end-to-end (skip the 30 s idle one for speed):

Run: `cargo test --release -p camp --test perf_daemon -- --ignored --nocapture --test-threads=1 sling_to_worker_spawn_under_2s close_to_dependent_dispatch_under_1s`
Expected: both PASS; the measured latencies print. If `session.woke`'s bead field differs from `e["data"]["bead"]`, cross-check the exact predicate in `daemon_dispatch.rs:175-177,260,268` and mirror it (do not invent a new field).

- [ ] **Step 5: camp gates**

Run: `cargo fmt -p camp && cargo clippy -p camp --all-targets --all-features -- -D warnings && cargo test -p camp --test perf_daemon`
Expected: clean; two parser tests pass; three ignored.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/tests/perf_daemon.rs
git commit -m "test(camp): idle + dispatch-latency perf suite (spec §14, make perf)"
```

---

## Task 5: `Makefile` (`perf` + `e2e` targets)

**Files:**
- Create: `Makefile` (repo root)

**Interfaces:**
- Consumes: the `#[ignore]` tests in `perf_volume.rs` and `perf_daemon.rs`.
- Produces: `make perf` (runs both suites in --release, ignored, single-threaded, with output); `make e2e` (loud placeholder for Phase 15).

- [ ] **Step 1: Write the Makefile**

Create `Makefile` at the repo root. **Recipe lines must begin with a literal TAB, not spaces.**

```makefile
# Gas Camp — developer targets.
#
# The perf/volume suite is LOCAL-ONLY by decision (2026-07-05): it is never
# run in CI. It asserts the spec §14 cost-budget numbers exactly, in
# --release, single-threaded (so timing/CPU measurements are isolated), and
# prints each measured value via --nocapture for the PR record.
.PHONY: perf e2e

perf:
	cargo test --release -p camp-core --test perf_volume -- --ignored --nocapture --test-threads=1
	cargo test --release -p camp --test perf_daemon -- --ignored --nocapture --test-threads=1

# Opt-in real-`claude` end-to-end suite (spec §16). Delivered by Phase 15
# (phase-15-e2e); this placeholder fails loudly until then rather than
# silently passing.
e2e:
	@echo "make e2e: the real-claude e2e suite is delivered by Phase 15 (not yet on this branch)" >&2
	@exit 1
```

- [ ] **Step 2: Verify the make targets parse and `e2e` fails loudly**

Run: `make -n perf` (dry run — prints the two cargo commands, does not execute)
Expected: prints the two `cargo test --release ...` lines.

Run: `make e2e; echo "exit=$?"`
Expected: the stderr message and `exit=1`.

- [ ] **Step 3: Commit**

```bash
git add Makefile
git commit -m "build: make perf (local §14 suite) and make e2e placeholder"
```

---

## Task 6: Full gates, `make perf` measurement, PR

**Files:** none (verification + PR).

- [ ] **Step 1: Run the full CI-equivalent gates**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: all clean. `cargo test --workspace` runs every non-ignored test (including the new `percentile_is_nearest_rank`, `fixture_generation_is_deterministic`, `parse_cputime_formats`, `parse_rss_kb_parses`, `backup_into_copies_and_passes_integrity_check`, and `cli_backup`) and **compiles** (does not run) the `#[ignore]`d perf tests. This is exactly what CI verifies.

- [ ] **Step 2: Run `make perf` in full and capture the numbers + machine spec**

Run: `make perf 2>&1 | tee /tmp/perf-run.txt` (allow several minutes: the 1M build, the refold replay, the VACUUM INTO, and a 30 s idle window each take real time in --release).

Also capture the machine spec:
```bash
uname -a
sysctl -n machdep.cpu.brand_string hw.memsize hw.ncpu   # macOS
```
Expected: every assertion passes; the `[volume]` and `[daemon]` lines print the measured p50/p99, FTS times, ls --ready time, backup time, idle CPU delta + RSS, and the two dispatch latencies. **If any measured number misses its spec §14 target, STOP and escalate to the lead — do NOT weaken the assertion.**

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin phase-13-perf-volume
```
Open a PR to `main` whose description records, verbatim from `/tmp/perf-run.txt`: the machine spec, and each measured number against its target (write p50/p99, 10 FTS query times, ls --ready, idle CPU delta + RSS, sling→spawn, close→dispatch, backup time + integrity ok, fixture events/beads, refold drift = 0). State plainly that the perf suite is LOCAL-ONLY and CI does not run it.

- [ ] **Step 4: Watch CI to a terminal result (foreground)**

Run: `gh pr checks <PR> --watch`
Expected: all five checks (fmt, clippy, test ×2, gc-compat) green. Do not end the turn while the watch is backgrounded.

- [ ] **Step 5: Report to the lead**

Report: PR number, CI status, the measured `make perf` numbers with the machine spec, and the master-plan exit criteria quoted line-by-line with the evidence for each.

---

## Self-Review

**Spec coverage** (Phase 13 assertion table, master plan lines 925-931):
- Volume fixture 30 heavy days / ≥1M events / ~100k beads / seeded RNG / real append path / refold clean → Task 3 `build_fixture` + `volume_suite` blocks 1-2. ✓
- Ledger write p50 AND p99 < 1 ms over 10k appends into the 1M db → Task 3 `volume_suite` block 6. ✓
- Ranked FTS 10-query set, each < 50 ms → Task 3 `volume_suite` block 3. ✓
- `ls --ready` indexed read < 10 ms → Task 3 `volume_suite` block 4. ✓
- Idle campd CPU delta == 0 (±10 ms) over 30 s; RSS < 20 MB via `ps -o cputime=,rss=` → Task 4 `idle_campd_cpu_delta_zero_and_rss_under_20mb`. ✓
- Sling → worker spawn ≤ 2 s and step close → dependent dispatched ≤ 1 s, fake-agent timing → Task 4 `sling_to_worker_spawn_under_2s` + `close_to_dependent_dispatch_under_1s`. ✓
- `camp backup` (VACUUM INTO) of the 1M-event db completes, integrity_check ok → Task 1 (`backup_into`) + Task 2 (CLI verb) + Task 3 `volume_suite` block 5. ✓
- Makefile `perf:` (--release, ignored) + `e2e:` stub → Task 5. ✓
- Exit criterion `make perf` green on the dev machine with numbers in the PR; CI untouched → Task 6. ✓

**Placeholder scan:** No "TBD"/"handle errors"/"similar to Task N". The one deferred item — the exact `jiff` seconds→Timestamp constructor name and the exact `session.woke` bead-field path — are each caught by a concrete step (Task 3 Step 2; Task 4 Step 4) that runs a real test and names the file:line to mirror; neither is a vague instruction.

**Type consistency:** `build_fixture(&Path, usize, usize) -> (u64, u64)` and `percentile(&[u128], f64) -> u128` are used identically in their tests and in `volume_suite`. `backup_into(&self, &Path) -> Result<(), CoreError>` is defined in Task 1 and consumed by Task 2's `cmd::backup::run` and Task 3's `volume_suite` with matching signatures. `CoreError::Backup(String)` matched in Task 1's test and produced by `backup_into`. `Daemon::pid(&self) -> u32` defined and used in Task 4. `wait_for_instant`/`ps_cputime_rss`/`parse_cputime`/`parse_rss_kb` signatures match their call sites.

## CI vs operator verification (flag to the lead)

Because both suites are `#[ignore]`d and LOCAL-ONLY, the five CI checks will **not** exercise the perf measurements. CI green proves: the perf files compile; the non-ignored helper/unit tests pass (percentile, generator determinism, cputime/rss parsing); and the `camp backup` verb works (`cli_backup.rs` + the core unit test). The measured §14 numbers are the **one** operator-verified item in the whole project — produced by running `make perf` on the dev machine and recorded in the PR description (Task 6). A measured miss is an escalation, never a reason to relax a spec target.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-08-phase-13-perf-volume.md`. Per the kickoff contract, execution is gated on lead approval (automated Opus plan review relayed through the lead). On approval, recommended execution is subagent-driven-development (fresh subagent per task, two-stage review between tasks); inline execution via executing-plans is the alternative.

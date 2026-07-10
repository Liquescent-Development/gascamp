# Operator Skill + Quiet, Awaitable Read Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the operator's own Claude Code session a contract for driving a camp, and make `camp show` a quiet, awaitable read surface so there is no raw-output thrash.

**Architecture:** Two independent deliverables. (1) A new `plugin/skills/operator/SKILL.md` — the mirror of the worker skill — plus a contract test. (2) `camp show` gains `--json`, promoted deliverable coordinates (branch/commit + a "see the diff" pointer read from the `bead.closed` event), and `--wait` (block on a `notify` file-watch of the ledger until the bead is closed, never polling). A single `BeadView` value feeds both the human and JSON renderings (DRY). The authoritative spec is amended in the same PR.

**Tech Stack:** Rust (workspace crate `camp`), clap 4 derive, `notify` 8.2 (already a dependency), `serde_json` 1.0 (already a dependency), `camp_core` (ledger/config/event). Integration tests use `assert_cmd` + `predicates` + `tempfile` (the existing `crates/camp/tests/cli_show.rs` harness).

## Global Constraints

Copied verbatim from the design spec (`docs/superpowers/specs/2026-07-10-operator-skill-quiet-read-surface-design.md`) and `AGENTS.md` — every task's requirements implicitly include these:

- **Invariant #1 — idle is free, no polling anywhere.** `--wait` sleeps on a `notify` OS file-watch; it must contain no tick, no `sleep`-and-recheck poll loop.
- **Invariant #3 — nothing hidden.** All additions are additive reads over the one ledger.
- **Invariant #5 — fail fast.** No fallbacks, no silenced errors, no placeholders. No `unwrap`/`expect`/`panic` in non-test code (clippy `unwrap_used`/`expect_used`/`panic` are denied; `unsafe_code` forbidden). Test files opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (existing pattern).
- **Invariant #7 — vocabulary mirror.** No new event types; `--wait` emits nothing.
- **`--wait` is a pure observer:** writes no ledger events, does not autostart campd. Works whether campd is up or down.
- **Skill named `operator`** (not "overseer" — that is a pack role in spec §8.4).
- **TDD, strict:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new/changed test.
- **Never commit to main.** This work is on branch `operator-skill-quiet-read-surface` (already created; the design commit is its first commit). No co-author lines.
- **Gates green before push:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`.
- **Spec and code never silently diverge** — the authoritative spec amendment (Task 5) ships in this same PR.

## File Structure

- `crates/camp/src/cmd/show.rs` — **rewritten** (Tasks 1–3): `BeadView` + `load_view` (read-only) + `render_human` + `render_json` + `wait_for_close`, orchestrated by `run`. One responsibility: read one bead and render it (optionally after awaiting close).
- `crates/camp/src/main.rs` — **modified** (Tasks 1, 3): `Command::Show` gains `json` / `wait` / `timeout` args; the dispatch arm passes them.
- `crates/camp/tests/cli_show.rs` — **modified** (Tasks 1–3): new tests alongside the existing four (which must keep passing).
- `plugin/skills/operator/SKILL.md` — **created** (Task 4): the operator contract.
- `crates/camp/tests/plugin_operator_skill.rs` — **created** (Task 4): pins the contract text (mirror of `plugin_worker_skill.rs`).
- `plugin/commands/sling.md`, `plugin/README.md` — **modified** (Task 4): point at the operator skill.
- `docs/design/2026-07-05-gas-camp-design.md` — **modified** (Task 5): §13 and §8.4 amendments.

---

## Task 1: `camp show --json` + the `BeadView` refactor

Refactor `show` so a single `BeadView` value feeds two renderings, and add `--json`. No deliverable-coordinate promotion yet (Task 2); no `--wait` yet (Task 3).

**Files:**
- Modify: `crates/camp/src/cmd/show.rs` (full rewrite of the file)
- Modify: `crates/camp/src/main.rs:194-197` (the `Show` variant) and `crates/camp/src/main.rs:534-537` (the dispatch arm)
- Test: `crates/camp/tests/cli_show.rs`

**Interfaces:**
- Consumes: `camp_core::ledger::Ledger::{open_read_only, get_bead, is_ready, events_for_bead}`; `camp_core::readiness::BeadRow`; `camp_core::event::Event` (derives `Serialize`, fields `seq/ts/kind/rig/actor/bead/data`); `crate::campdir::CampDir::{db_path, root}`.
- Produces (used by Tasks 2–3):
  - `struct BeadView { row: BeadRow, ready: bool, history: Vec<Event>, deliverable: Option<Deliverable> }`
  - `struct Deliverable { branch: String, commit: String, rig_path: String }`
  - `fn load_view(camp: &CampDir, bead: &str) -> anyhow::Result<BeadView>`
  - `fn render_human(view: &BeadView)`
  - `fn render_json(view: &BeadView) -> anyhow::Result<()>`
  - `pub fn run(camp: &CampDir, bead: String, json: bool, wait: bool, timeout: Option<u64>) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing test**

Add to `crates/camp/tests/cli_show.rs`:

```rust
/// `--json` emits ONE object: the bead's state fields plus a `history`
/// array — the operator's machine read (design §5).
#[test]
fn show_json_emits_state_and_history() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    let out = camp()
        .current_dir(dir.path())
        .args(["show", "gc-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["bead"], "gc-1");
    assert_eq!(v["title"], "do the thing");
    assert_eq!(v["status"], "in_progress");
    assert_eq!(v["ready"], false);
    let kinds: Vec<&str> = v["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"bead.created"), "history kinds: {kinds:?}");
    assert!(kinds.contains(&"bead.claimed"), "history kinds: {kinds:?}");
    // Not shipped → no deliverable coordinates yet.
    assert!(v["branch"].is_null());
    assert!(v["commit"].is_null());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p camp --test cli_show show_json_emits_state_and_history`
Expected: FAIL — `camp show` rejects the unknown `--json` flag (clap error), so the command does not succeed.

- [ ] **Step 3: Write minimal implementation**

Replace the entire contents of `crates/camp/src/cmd/show.rs` with:

```rust
use anyhow::{Result, anyhow};
use camp_core::event::Event;
use camp_core::ledger::Ledger;
use camp_core::readiness::BeadRow;

use crate::campdir::CampDir;

/// A bead's current state plus its full history — the single value both
/// renderings consume (DRY). `deliverable` is populated only for a shipped
/// bead (Task 2); it stays `None` otherwise.
pub(crate) struct BeadView {
    row: BeadRow,
    ready: bool,
    history: Vec<Event>,
    deliverable: Option<Deliverable>,
}

/// Shipped deliverable coordinates, promoted to first-class fields so no
/// one does git archaeology to find the result (design §6).
pub(crate) struct Deliverable {
    branch: String,
    commit: String,
    rig_path: String,
}

/// `camp show <bead> [--json]`: current state plus full event history — the
/// one sanctioned history read (spec §7.4). Read-only: `show` never writes.
pub fn run(camp: &CampDir, bead: String, json: bool, _wait: bool, _timeout: Option<u64>) -> Result<()> {
    let view = load_view(camp, &bead)?;
    if json {
        render_json(&view)
    } else {
        render_human(&view);
        Ok(())
    }
}

/// Read one bead read-only: row + readiness + history. Errors if unknown.
fn load_view(camp: &CampDir, bead: &str) -> Result<BeadView> {
    let ledger = Ledger::open_read_only(&camp.db_path())?;
    let row = ledger
        .get_bead(bead)?
        .ok_or_else(|| anyhow!("no such bead: {bead}"))?;
    let ready = ledger.is_ready(bead)?;
    let history = ledger.events_for_bead(bead)?;
    // Deliverable promotion is wired in Task 2; until then, always None.
    let deliverable = None;
    Ok(BeadView {
        row,
        ready,
        history,
        deliverable,
    })
}

/// The plain-text rendering — byte-for-byte the historical layout, plus the
/// promoted deliverable lines when present.
fn render_human(view: &BeadView) {
    let row = &view.row;
    println!("bead     {}", row.id);
    println!("rig      {}", row.rig);
    println!("type     {}", row.kind);
    println!("title    {}", row.title);
    println!(
        "status   {}{}",
        row.status,
        if view.ready { "  (ready)" } else { "" }
    );
    if let Some(a) = &row.assignee {
        println!("assignee {a}");
    }
    if let Some(c) = &row.claimed_by {
        println!("claimed  {c}");
    }
    if let Some(o) = &row.outcome {
        println!("outcome  {o}");
    }
    if let Some(wo) = &row.work_outcome {
        println!("work     {wo}");
    }
    if let Some(d) = &view.deliverable {
        println!("branch   {}", d.branch);
        println!(
            "commit   {}   (see: git -C {} show {})",
            d.commit, d.rig_path, d.commit
        );
    }
    if let Some(df) = &row.dispatch_failure {
        // Assessment finding A (PR #54): the marker alone hides the retry
        // semantics — campd's in-memory failed set suppresses re-dispatch
        // for its lifetime (fail-fast by design), so fixing the cause is
        // not enough; say so where the reason is read.
        println!("dispatch-failed  {df}");
        println!(
            "                 (campd retries once per restart — after fixing the cause, restart campd)"
        );
    }
    if !row.labels.is_empty() {
        println!("labels   {}", row.labels.join(", "));
    }
    println!("created  {}", row.created_ts);
    println!("updated  {}", row.updated_ts);
    println!();
    println!("history:");
    for e in &view.history {
        println!("  {:>4}  {}  {:<14}  {}", e.seq, e.ts, e.kind.as_str(), e.data);
    }
}

/// The machine rendering — one JSON object: state fields + `history` array.
fn render_json(view: &BeadView) -> Result<()> {
    let row = &view.row;
    let mut obj = serde_json::json!({
        "bead": row.id,
        "rig": row.rig,
        "type": row.kind,
        "title": row.title,
        "status": row.status,
        "ready": view.ready,
        "assignee": row.assignee,
        "claimed_by": row.claimed_by,
        "outcome": row.outcome,
        "work_outcome": row.work_outcome,
        "dispatch_failure": row.dispatch_failure,
        "labels": row.labels,
        "created": row.created_ts,
        "updated": row.updated_ts,
        "history": view.history,
    });
    if let Some(d) = &view.deliverable {
        obj["branch"] = serde_json::json!(d.branch);
        obj["commit"] = serde_json::json!(d.commit);
    }
    println!("{}", serde_json::to_string_pretty(&obj)?);
    Ok(())
}
```

Then wire the flags in `crates/camp/src/main.rs`. Replace the `Show` variant (currently `crates/camp/src/main.rs:194-197`):

```rust
    /// Show a bead's current state and full event history
    Show {
        /// Bead id
        bead: String,
        /// Emit the bead's state and history as one JSON object
        #[arg(long)]
        json: bool,
        /// Block until the bead reaches a closed status, then render
        #[arg(long)]
        wait: bool,
        /// With --wait, bound the wait to N seconds (default: unbounded)
        #[arg(long, value_name = "SECONDS", requires = "wait")]
        timeout: Option<u64>,
    },
```

And replace the dispatch arm (currently `crates/camp/src/main.rs:534-537`):

```rust
        Command::Show {
            bead,
            json,
            wait,
            timeout,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::show::run(&camp, bead, json, wait, timeout)
        }
```

*(Note on `_wait`/`_timeout`: `run` accepts them now so Task 3 only fills the body — no signature churn across tasks. The leading underscores silence unused-arg lints until Task 3.)*

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p camp --test cli_show`
Expected: PASS — the new `show_json_emits_state_and_history` plus the four pre-existing tests (`show_reports_state_and_history`, `show_prints_the_work_outcome`, `show_of_unknown_bead_errors`, `show_prints_the_dispatch_failure_with_the_retry_hint`), since `render_human` reproduces the historical layout exactly.

- [ ] **Step 5: Gates**

Run: `cargo fmt --all` then `cargo clippy -p camp --all-targets --all-features -- -D warnings`
Expected: clean (no `unwrap`/`expect`/`panic` in `show.rs`; `_wait`/`_timeout` underscored).

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/show.rs crates/camp/src/main.rs crates/camp/tests/cli_show.rs
git commit -m "feat(show): --json + BeadView refactor (one value, two renderings)"
```

---

## Task 2: Promote deliverable coordinates

When a bead is closed `shipped`, surface `branch` / `commit` as first-class fields plus a `git -C <rig-path> show <commit>` pointer — in both the human and JSON renderings. Reads the coordinates from the last `bead.closed` event (no schema change) and resolves the rig path from config, exactly as `close.rs` does.

**Files:**
- Modify: `crates/camp/src/cmd/show.rs` (fill in `load_view`'s deliverable + add `build_deliverable`)
- Test: `crates/camp/tests/cli_show.rs`

**Interfaces:**
- Consumes: `camp_core::config::CampConfig::{load, rig}` (returns `&RigConfig` with `path: PathBuf`); `camp_core::event::EventType::BeadClosed`; `crate::campdir::CampDir::config_path`. The `bead.closed` event's `data` carries string keys `work_branch` and `work_commit` (written by `cmd/close.rs`).
- Produces: `fn build_deliverable(camp: &CampDir, row: &BeadRow, history: &[Event]) -> anyhow::Result<Deliverable>`; `load_view` now returns `Some(Deliverable)` iff `row.work_outcome == Some("shipped")`.

- [ ] **Step 1: Write the failing test**

The shipped git-gate is enforced only by `camp close`; the fold records `work_outcome`/`work_branch`/`work_commit` from the event data. So the test appends a `bead.closed` event directly (the pattern `show_prints_the_dispatch_failure_with_the_retry_hint` already uses), which folds `work_outcome = shipped` onto the row and gives `show` the coordinates to promote. Add to `crates/camp/tests/cli_show.rs`:

```rust
/// A shipped bead promotes branch/commit to first-class fields plus a
/// copy-paste pointer, in BOTH renderings — no git archaeology (design §6).
#[test]
fn show_promotes_shipped_deliverable_coordinates() {
    let dir = camp_with_bead();
    // Append a shipped close directly — the fold records the coordinates;
    // the git gate lives in `camp close`, not the fold.
    {
        let mut ledger =
            camp_core::ledger::Ledger::open(&dir.path().join(".camp/camp.db")).unwrap();
        ledger
            .append(camp_core::event::EventInput {
                kind: camp_core::event::EventType::BeadClosed,
                rig: Some("gascity".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({
                    "outcome": "pass",
                    "work_outcome": "shipped",
                    "work_branch": "camp/gc-1",
                    "work_commit": "b1d59a2df83a060382ee78b5546cd2f858e3702f",
                }),
            })
            .unwrap();
    }
    // Human rendering: branch + commit + the "see:" pointer to the rig.
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("branch   camp/gc-1"))
        .stdout(predicates::str::contains(
            "commit   b1d59a2df83a060382ee78b5546cd2f858e3702f",
        ))
        .stdout(predicates::str::contains("see: git -C "))
        .stdout(predicates::str::contains(
            "git show b1d59a2df83a060382ee78b5546cd2f858e3702f",
        ));
    // JSON rendering: branch + commit are first-class.
    let out = camp()
        .current_dir(dir.path())
        .args(["show", "gc-1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["branch"], "camp/gc-1");
    assert_eq!(v["commit"], "b1d59a2df83a060382ee78b5546cd2f858e3702f");
    assert_eq!(v["work_outcome"], "shipped");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p camp --test cli_show show_promotes_shipped_deliverable_coordinates`
Expected: FAIL — `load_view` sets `deliverable = None`, so neither the `branch` line nor the JSON `branch` field appears.

- [ ] **Step 3: Write minimal implementation**

In `crates/camp/src/cmd/show.rs`, extend the imports:

```rust
use camp_core::config::CampConfig;
use camp_core::event::{Event, EventType};
```

Replace the deliverable line in `load_view` (`let deliverable = None;`) with:

```rust
    let deliverable = if row.work_outcome.as_deref() == Some("shipped") {
        Some(build_deliverable(camp, &row, &history)?)
    } else {
        None
    };
```

Add this function to the file:

```rust
/// Resolve a shipped bead's deliverable coordinates: branch + commit from
/// the last `bead.closed` event's data, and the rig path from config (the
/// same resolution `cmd/close.rs` uses). The commit lives on `camp/<bead>`
/// in the RIG repo — campd reaps the worktree on close (spec §12), so the
/// rig repo is the durable location the pointer names.
fn build_deliverable(camp: &CampDir, row: &BeadRow, history: &[Event]) -> Result<Deliverable> {
    let closed = history
        .iter()
        .rev()
        .find(|e| e.kind == EventType::BeadClosed)
        .ok_or_else(|| anyhow!("bead {} is shipped but has no bead.closed event", row.id))?;
    let field = |key: &str| -> Result<String> {
        closed
            .data
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("shipped close for {} records no {key}", row.id))
    };
    let branch = field("work_branch")?;
    let commit = field("work_commit")?;
    let config = CampConfig::load(&camp.config_path())?;
    let rig_path = config.rig(&row.rig)?.path.display().to_string();
    Ok(Deliverable {
        branch,
        commit,
        rig_path,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p camp --test cli_show`
Expected: PASS — the new promotion test plus all earlier tests (the non-shipped `show_json_emits_state_and_history` still sees `branch`/`commit` null because `deliverable` is `None` for a non-shipped bead).

- [ ] **Step 5: Gates**

Run: `cargo fmt --all` then `cargo clippy -p camp --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/show.rs crates/camp/tests/cli_show.rs
git commit -m "feat(show): promote shipped deliverable coordinates (branch/commit + see-diff pointer)"
```

---

## Task 3: `camp show --wait [--timeout SECONDS]`

Block until the bead reaches a `closed` status, then render (§6 output). The wait sleeps on a `notify` file-watch of the camp directory (WAL writes land there) — never a poll. Pure observer: no ledger writes, no campd autostart.

**Files:**
- Modify: `crates/camp/src/cmd/show.rs` (add `wait_for_close`; fill in `run`'s wait branch)
- Test: `crates/camp/tests/cli_show.rs`

**Interfaces:**
- Consumes: `notify::{recommended_watcher, Watcher, RecursiveMode, Event as NotifyEvent, Result as NotifyResult}`; `std::sync::mpsc`; `std::time::{Duration, Instant}`; `crate::campdir::CampDir::root`. Reuses `load_view` (Task 1) — which is already read-only via `Ledger::open_read_only`.
- Produces: `fn wait_for_close(camp: &CampDir, bead: &str, timeout: Option<Duration>) -> anyhow::Result<BeadView>`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/camp/tests/cli_show.rs` (add `use std::time::Instant;` at the top of the file if not present):

```rust
/// An already-closed bead returns immediately (no watch armed).
#[test]
fn show_wait_returns_immediately_when_already_closed() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args([
            "close", "gc-1", "--outcome", "fail", "--work-outcome", "blocked",
            "--reason", "cannot land",
        ])
        .assert()
        .success();
    let start = Instant::now();
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1", "--wait"])
        .assert()
        .success()
        .stdout(predicates::str::contains("status   closed"));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(3),
        "an already-closed bead must not block"
    );
}

/// `--wait` blocks on the file-watch and wakes when the bead closes from
/// another process — event-driven, not returned-early, not a fixed poll.
#[test]
fn show_wait_wakes_on_an_external_close() {
    let dir = camp_with_bead();
    let path = dir.path().to_path_buf();
    // Close gc-1 after ~600ms, from a separate process, while --wait blocks.
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        Command::cargo_bin("camp")
            .unwrap()
            .env_remove("CAMP_DIR")
            .current_dir(&path)
            .args([
                "close", "gc-1", "--outcome", "fail", "--work-outcome", "blocked",
                "--reason", "done",
            ])
            .assert()
            .success();
    });
    let start = Instant::now();
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1", "--wait"])
        .assert()
        .success()
        .stdout(predicates::str::contains("status   closed"));
    let elapsed = start.elapsed();
    closer.join().unwrap();
    // Waited for the close (did not return early)…
    assert!(
        elapsed >= std::time::Duration::from_millis(400),
        "must actually wait for the close, elapsed {elapsed:?}"
    );
    // …and woke on the event rather than a coarse poll interval.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "must wake promptly on the watch event, elapsed {elapsed:?}"
    );
}

/// `--timeout` bounds the wait and fails fast (never a silent hang).
#[test]
fn show_wait_times_out_nonzero() {
    let dir = camp_with_bead(); // gc-1 stays open
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1", "--wait", "--timeout", "1"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("timed out"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p camp --test cli_show show_wait_`
Expected: FAIL — `run` ignores `_wait`, so `show --wait` on an open bead renders the open state and returns (the already-closed test may pass by accident, but the wake and timeout tests fail: no blocking, and `--timeout` on an open bead currently succeeds instead of failing).

- [ ] **Step 3: Write minimal implementation**

In `crates/camp/src/cmd/show.rs`, extend imports:

```rust
use std::sync::mpsc;
use std::time::{Duration, Instant};
```

Replace `run` with the wait-aware version:

```rust
/// `camp show <bead> [--json] [--wait [--timeout SECONDS]]`: current state
/// plus full event history (spec §7.4). Read-only: `show` never writes and
/// never autostarts campd — `--wait` is a pure observer (design §7).
pub fn run(camp: &CampDir, bead: String, json: bool, wait: bool, timeout: Option<u64>) -> Result<()> {
    let view = if wait {
        wait_for_close(camp, &bead, timeout.map(Duration::from_secs))?
    } else {
        load_view(camp, &bead)?
    };
    if json {
        render_json(&view)
    } else {
        render_human(&view);
        Ok(())
    }
}
```

Add `wait_for_close`:

```rust
/// Block until `bead` reaches a `closed` status, then return its view.
///
/// Sleeps on a `notify` file-watch of the camp directory — WAL commits land
/// there, so every close wakes us; there is NO poll loop (invariant #1). The
/// watch is armed BEFORE the re-check, so a close landing between the first
/// read and arming cannot be missed (arm-before-check). Pure observer: no
/// ledger writes, no campd dependency — a worker's `camp close` writes the
/// terminal event to the ledger directly, and we observe that ground truth.
fn wait_for_close(camp: &CampDir, bead: &str, timeout: Option<Duration>) -> Result<BeadView> {
    let view = load_view(camp, bead)?; // also validates the bead exists
    if view.row.status == "closed" {
        return Ok(view);
    }
    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
        // Any change under the camp dir is a wake; the reload reads ground
        // truth. One pending wake is enough — a failed send just means a
        // wake is already queued.
        let _ = tx.send(());
    })
    .map_err(|e| anyhow!("creating the ledger watcher: {e}"))?;
    notify::Watcher::watch(&mut watcher, &camp.root, notify::RecursiveMode::NonRecursive)
        .map_err(|e| anyhow!("watching the camp directory {}: {e}", camp.root.display()))?;
    // Re-check AFTER arming (closes the arm-before-check race).
    let view = load_view(camp, bead)?;
    if view.row.status == "closed" {
        return Ok(view);
    }
    eprintln!("waiting for {bead} to close (Ctrl-C to stop)…");
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        match deadline {
            None => match rx.recv() {
                // Unbounded: a pure blocking wait on the OS watch — no tick.
                Ok(()) => {}
                Err(_) => anyhow::bail!("ledger watcher disconnected while waiting for {bead}"),
            },
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    let view = load_view(camp, bead)?;
                    anyhow::bail!(
                        "timed out waiting for {bead} to close — still {} after the timeout",
                        view.row.status
                    );
                }
                match rx.recv_timeout(d - now) {
                    Ok(()) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => continue, // re-eval deadline → bail
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        anyhow::bail!("ledger watcher disconnected while waiting for {bead}")
                    }
                }
            }
        }
        let view = load_view(camp, bead)?;
        if view.row.status == "closed" {
            return Ok(view);
        }
        // Spurious/unrelated fs event (e.g. a -shm touch): keep waiting.
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p camp --test cli_show`
Expected: PASS — all wait tests plus every earlier test. (The wake test typically returns ~0.6 s after start.)

- [ ] **Step 5: Gates**

Run: `cargo fmt --all` then `cargo clippy -p camp --all-targets --all-features -- -D warnings`
Expected: clean. Confirm by eye that `wait_for_close` contains no `std::thread::sleep` / interval — only `recv` / `recv_timeout` (the timeout bound, not a poll).

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/show.rs crates/camp/tests/cli_show.rs
git commit -m "feat(show): --wait blocks on a ledger file-watch until close (no polling, pure observer)"
```

---

## Task 4: The operator skill + plugin doc wiring

Ship `plugin/skills/operator/SKILL.md` — the mirror of the worker skill — a contract test that pins its key lines, and pointers to it from `sling.md` and the plugin README.

**Files:**
- Create: `plugin/skills/operator/SKILL.md`
- Create: `crates/camp/tests/plugin_operator_skill.rs`
- Modify: `plugin/commands/sling.md`
- Modify: `plugin/README.md`

**Interfaces:**
- Consumes: nothing (documentation + a text-assertion test). The test mirrors `crates/camp/tests/plugin_worker_skill.rs`.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Write the failing test**

Create `crates/camp/tests/plugin_operator_skill.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The operator skill IS the control-plane contract (mirror of the worker
//! skill test). This pins that the shipped SKILL.md keeps the mental model,
//! the delivery model, the output discipline, and the don't-poll rule — so
//! the contract can never silently lose a load-bearing line.

use std::path::PathBuf;

fn operator_skill() -> String {
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin/skills/operator/SKILL.md");
    std::fs::read_to_string(&p).expect("plugin/skills/operator/SKILL.md must exist")
}

#[test]
fn operator_skill_has_skill_frontmatter() {
    let s = operator_skill();
    assert!(s.starts_with("---"), "must open with YAML frontmatter");
    assert!(s.contains("name: operator"), "frontmatter must set name: operator");
    assert!(s.contains("description:"), "frontmatter must have a description");
}

#[test]
fn operator_skill_states_the_mental_model() {
    let s = operator_skill();
    for needle in [
        "campd",         // the sole dispatcher
        "enqueue",       // sling only enqueues
        "camp/<bead>",   // the branch is the deliverable
        "no remote",     // v1 has no remote/PR/merge
        "shipped",       // shipped is mechanically verified already
    ] {
        assert!(s.contains(needle), "operator skill must state `{needle}`");
    }
}

#[test]
fn operator_skill_carries_the_output_and_polling_discipline() {
    let s = operator_skill();
    for needle in [
        "never paste", // read-and-summarize, don't dump raw output
        "--json",      // machine read
        "poll",        // don't poll
        "--wait",      // the awaitable read
    ] {
        assert!(s.contains(needle), "operator skill must state `{needle}`");
    }
}

#[test]
fn operator_skill_lists_the_operator_verbs() {
    let s = operator_skill();
    for needle in ["camp sling", "camp show", "camp nudge", "camp top"] {
        assert!(s.contains(needle), "operator skill must reference `{needle}`");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p camp --test plugin_operator_skill`
Expected: FAIL — `plugin/skills/operator/SKILL.md` does not exist (`.expect(...)` message).

- [ ] **Step 3: Write the skill**

Create `plugin/skills/operator/SKILL.md`:

```markdown
---
name: operator
description: Use when you are driving a camp from your own Claude Code session — slinging work, watching the fleet, conversing with workers, and checking results. The control-plane contract: campd is the sole dispatcher, the local bead branch is the deliverable, and you read camp output yourself and report a tight summary rather than pasting it.
---

# Camp operator contract

You are the **operator** — the human's own session driving a camp. (A
campd-spawned worker follows the `worker` skill instead; this is its mirror
for the control plane.) Everything here is a `camp` CLI call, identical to
what the human would type.

## 1. Mental model — get this right and you stop thrashing

- **campd is the sole dispatcher.** `camp sling "<title>"` only **enqueue**s
  one bead; campd immediately spawns a headless-but-present worker (spec
  §8.4). You spawn nothing, and you do not reconstruct what campd is doing
  from `campd.log`, the `sessions/` dir, or the process table — the ledger is
  the story.
- **The local `camp/<bead>` branch IS the deliverable.** Camp v1 has **no
  remote**, no PR, and no merge step (spec §8.4, §12). Do not apply a global
  "code reaches main only via a PR" rule to a camp bead — there is nowhere to
  push and nothing to merge.
- **`shipped` is already verified.** When a worker closes a bead `shipped`,
  camp has already checked mechanically that the branch is real, the commit
  is reachable on it, descends from the dispatch base, and is new work. You
  never re-verify *integration* by hand.

## 2. The loop

sling → (optionally `camp show <bead> --wait`) → read the result → report it
concisely → `camp nudge` to converse if needed.

## 3. Output discipline — read it, don't paste it

Run camp, **read the output yourself, and report a tight summary in prose.**
**Never paste** raw `camp events` tables, full `camp show` history,
`campd.log`, the `sessions/` dir, or `git ls-tree` / `git show` walls into the
conversation. When you need to parse a result rather than eyeball it, use
`camp show <bead> --json` and summarize the fields that matter.

## 4. Verifying a deliverable

Integration is already guaranteed for `shipped` (§1). `camp show <bead>`
promotes the deliverable's `branch` and `commit` and prints a
`git -C <rig> show <commit>` pointer — use it. Only if the human asks for
*functional* verification (does it build, do the tests pass) do you run the
build/tests — **once, quietly** — and report pass/fail. Do not paste the
build log, and do not hand-build throwaway worktrees unless functional
verification was actually requested.

## 5. Don't poll

Camp is event-driven and idle is free. To wait for a bead to finish, use
`camp show <bead> --wait` — it sleeps on a ledger watch and returns the
moment the bead closes. **Never** write a bash `poll` loop or a
`sleep`-and-recheck. (See the `subagent-hygiene` skill for waiting on async
results without polling.)

## 6. Verbs

- `camp sling "<title>" [--agent A] [--rig R]` — enqueue one bead (`/sling`).
- `camp show <bead> [--wait] [--json]` — one bead's state; `--wait` blocks
  until it closes, `--json` for machine reads.
- `camp top` — fleet snapshot: live sessions, ready/open beads (`/status`).
- `camp nudge <session> "<message>"` — converse with any session (`/nudge`).
- `camp events` — the whole event log (`/events`) — read it, don't paste it.
- `camp adopt` — reconcile the session registry against reality (`/adopt`).
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p camp --test plugin_operator_skill`
Expected: PASS — all four tests.

- [ ] **Step 5: Wire the doc pointers**

In `plugin/commands/sling.md`, after the final paragraph (the line ending
"Report the created bead id (or run id) to the user."), add:

```markdown

Driving the camp from this session? The `operator` skill is your contract:
campd is the sole dispatcher, the `camp/<bead>` branch is the deliverable,
and you read camp output and summarize it — never paste raw walls.
```

In `plugin/README.md`, replace the closing "## Worker skill" section with
both skills:

```markdown
## Skills

`skills/worker/SKILL.md` is the lifecycle contract a campd-spawned pack
worker follows: recall → claim → work → emit milestones → remember → close →
exit.

`skills/operator/SKILL.md` is its mirror for the human's own control-plane
session: campd is the sole dispatcher, the `camp/<bead>` branch is the
deliverable, and the operator reads camp output and reports a concise summary
(and awaits with `camp show --wait`) rather than thrashing raw output.
```

- [ ] **Step 6: Verify docs and re-run the test**

Run: `cargo test -p camp --test plugin_operator_skill`
Expected: PASS (unchanged — the doc edits do not affect these assertions, but confirm nothing regressed).
Visually confirm `plugin/commands/sling.md` and `plugin/README.md` read correctly.

- [ ] **Step 7: Commit**

```bash
git add plugin/skills/operator/SKILL.md crates/camp/tests/plugin_operator_skill.rs plugin/commands/sling.md plugin/README.md
git commit -m "feat(plugin): operator skill — the control-plane contract, mirror of the worker skill"
```

---

## Task 5: Amend the authoritative spec (§13, §8.4)

`AGENTS.md` requires the authoritative spec and the code to never silently
diverge, in the same PR. Add the quiet read surface to §13 and note the
promoted/awaitable deliverable in §8.4. Documentation only — reviewer-gated,
no automated test.

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md`

- [ ] **Step 1: Read the two sections**

Read `docs/design/2026-07-05-gas-camp-design.md` §8.4 (the delivery
paragraph around "shipped is mechanically gated") and §13 (nothing-hidden
guarantees). Confirm the wording below does not contradict what is there; if
it does, adjust the wording to match reality and note it in the commit body
(spec and code must agree).

- [ ] **Step 2: Add the §13 paragraph**

At the end of §13 (the nothing-hidden guarantees section), add:

```markdown
**The read surface is quiet and awaitable.** `camp show <bead>` renders one
bead's state and history; `--json` emits the same as one machine-readable
object (parity with `events`/`ls`), and a `shipped` bead promotes its
deliverable coordinates — `branch` and `commit`, plus a `git -C <rig> show
<commit>` pointer — to first-class fields, so the result needs no git
archaeology. `camp show <bead> --wait` blocks until the bead reaches a closed
status and then renders it; it sleeps on a `notify` file-watch of the ledger
(no polling, invariant 1), emits no events, and does not autostart campd — a
pure observer of ground truth that works whether campd is up or down.
```

- [ ] **Step 3: Add the §8.4 sentence**

In §8.4, at the end of the "Worker lifecycle contract" paragraph (after the
delivery/`shipped`-gate description), add:

```markdown
The deliverable coordinates a worker records at a `shipped` close are surfaced
first-class by `camp show` (with a `git -C <rig> show <commit>` pointer) and
are awaitable via `camp show --wait` — the operator reads the outcome without
reconstructing it. The operator's own contract for driving all of this is the
plugin's `operator` skill, the mirror of the worker skill.
```

- [ ] **Step 4: Verify the spec reads coherently**

Re-read §8.4 and §13 end-to-end. Confirm no contradiction with the existing
decision record (§4) or the dispatch model, and that terminology matches
(`camp/<bead>`, "shipped", "dispatch-time base").

- [ ] **Step 5: Commit**

```bash
git add docs/design/2026-07-05-gas-camp-design.md
git commit -m "docs(spec): §13/§8.4 — the quiet, awaitable read surface + operator skill"
```

---

## Final verification (before opening the PR)

- [ ] Run the full gates: `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`. All green.
- [ ] Manually drive the loop end-to-end in a scratch camp: `camp init`; add a rig with a base commit; `camp sling "<title>"`; `camp show <bead> --wait`; confirm it blocks then renders `status closed` with promoted `branch`/`commit` and the `see:` pointer; `camp show <bead> --json | jq .` shows the object.
- [ ] Open the PR against `main` from `operator-skill-quiet-read-surface`; PR body states each verified claim.

## Self-Review

**1. Spec coverage** (against `docs/superpowers/specs/2026-07-10-operator-skill-quiet-read-surface-design.md`):
- §4 operator skill (6 sections) → Task 4 (skill file) + its contract test.
- §5 `show --json` → Task 1.
- §6 promoted deliverable coordinates → Task 2.
- §7 `show --wait` (ledger watch, pure observer, `--timeout` fail-fast) → Task 3.
- §8 decisions (`--wait` modifier; `operator` name; ledger-watch; no writes/no autostart; no new events) → Tasks 1–4 constraints + Global Constraints.
- §9 spec amendments (§13, §8.4) → Task 5.
- §10 tests (json shape; promotion; event-driven wait; wait-needs-no-campd; timeout) → Tasks 1–3 tests. *(The external-close wake test runs the close from a separate `camp` process with no daemon running, which also exercises "`--wait` needs no campd" — the close writes to the ledger directly.)*
- §11 invariants → Global Constraints + Task 3 clippy/by-eye check.

**2. Placeholder scan:** No "TBD"/"handle edge cases"/"similar to Task N" — every code step shows full code; `<bead>`/`<rig>`/`SECONDS` are command-syntax placeholders, not plan gaps.

**3. Type consistency:** `BeadView`/`Deliverable`/`load_view`/`render_human`/`render_json`/`build_deliverable`/`wait_for_close` and `run(camp, bead, json, wait, timeout)` are used identically across Tasks 1–3; the `run` signature is fixed in Task 1 so Tasks 2–3 add bodies only. Test helpers `camp()` / `camp_with_bead()` and bead id `gc-1` / rig `gascity` match the existing `cli_show.rs` harness.

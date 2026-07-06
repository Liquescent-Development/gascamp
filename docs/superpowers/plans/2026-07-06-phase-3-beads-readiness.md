# Gas Camp Phase 3 — Beads, Rigs, Readiness, Queries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a bead its full Tier-0 ledger life through the `camp` CLI — create → claim → close — with per-rig prefixed IDs whose allocation is folded state, readiness computed on write (spec §7.3, decision 6), and the `ls`/`show` query surface, all while `doctor --refold` stays exact.

**Architecture:** Extend the merged Phase-1 ledger core without reworking it. New pure camp-core modules (`config`, `id`, `readiness`) fold and query the existing state tables; a new `counters` state table makes ID allocation a fold of `bead.created` so refold reconstructs it exactly; a new camp-specific `rig.added` event records config changes (spec §13.4) while `camp.toml` stays the human-readable rig source of truth. New CLI verbs (`create`, `rig add`/`rig ls`, `claim`, `close`, `ls`, `show`) are thin wrappers over typed `Ledger` methods — no raw connection leaks, one write path.

**Tech Stack:** Rust (edition 2024), rusqlite (bundled SQLite, WAL + FTS5), serde/serde_json, `toml` (new — camp.toml parse + rig serialization), clap, jiff; proptest + assert_cmd + predicates + tempfile (tests). No async runtime (spec §15.1).

## Global Constraints

Copied from AGENTS.md, the master plan, and the operator's standing rules. Every task's requirements implicitly include this section.

- **Spec is authoritative:** `docs/design/2026-07-05-gas-camp-design.md`; its §4 decision record is settled. If implementation reality contradicts the spec, stop and update the spec via PR in the same change — spec and code never silently diverge.
- **Master plan contract:** `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md`, "Phase 3 — Beads, Rigs, Readiness, Queries" section. Its files, interfaces, semantics, tests, and exit criteria are binding.
- **Readiness rule (decision 6, binding):** ready = `status='open'` ∧ every `needs` target exists, is `closed`, **and** has `outcome='pass'`. A failed or missing dependency never unblocks its dependents.
- **A write is one transaction:** event row + state effect commit together (spec §7.2). State tables ≡ fold of the event log, verified by `doctor --refold`. New folded state (the `counters` table) must be included in refold's diff.
- **Respect merged interfaces:** extend, don't rework. New event payloads use `#[serde(deny_unknown_fields)]` structs; keep the one-transaction event+state property; keep the vocab-pin partition test and the refold property test green.
- **Zero role names in machinery** (spec §2.4). Nothing in camp-core or the CLI reasons about *what* work is — only structure (ids, edges, status, outcome).
- **Fail fast:** no silent fallbacks, no silenced errors, no placeholders. No panics in library code (`clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic` denied; `#![forbid(unsafe_code)]`). Every error path surfaces to the caller.
- **Vocabulary mirror** (spec §15.2): `rig.added` is camp-specific and additive — it must be declared in `CAMP_SPECIFIC_EVENTS` and must NOT appear in gc's pinned registry (verified: `rig.added` is absent from `gc-vocab.json`'s 71 events).
- **TDD, strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything. When a step modifies an existing test, run that test too.
- **Git:** never commit to main; branch `phase-3-beads-readiness`; no co-author lines, no self-mention in commits. Conventional-commit style (`feat:`, `test:`, `docs:`, `chore:`).
- **Gates before push:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- **SQLite pragmas (Phase 1, unchanged):** `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`; writes use `BEGIN IMMEDIATE`; all tables `STRICT`.
- **Nothing is complete until pushed, CI green (`gh pr checks --watch`), and every claim in the PR description verified.**

## Key Paths and Conventions

- `GASCAMP=/Users/kiener/code/gascamp` — the repo; work in the main checkout (Phase 3 is the only phase in flight).
- Branch: `phase-3-beads-readiness`, off `main`.
- camp-core new files: `crates/camp-core/src/{config,id,readiness}.rs`; modified: `crates/camp-core/src/lib.rs`, `error.rs`, `event.rs`, `vocab.rs`, `ledger/{schema,fold,refold,mod}.rs`, `tests/refold_prop.rs`.
- camp (bin) new files: `crates/camp/src/cmd/{create,rig,claim,close,ls,show}.rs`; modified: `crates/camp/src/main.rs`, `campdir.rs`; new tests: `crates/camp/tests/{cli_rig,cli_create,cli_claim_close,cli_ls,cli_show,cli_lifecycle}.rs`.
- CLI-appended events use `actor = "cli"`.
- Test determinism: seed writes through camp-core with `FixedClock::new("2026-07-05T21:14:03Z")` (the binary uses `SystemClock`), then run the binary for reads — exactly the pattern in the existing `crates/camp/tests/cli_events.rs`.

## Plan-Time Decision Log

Decisions made while writing this plan, so execution sessions do not re-derive them. **Decision A needs operator sign-off at plan approval** (flagged in the handoff).

- **A. `camp create` bead-create verb (operator-approved 2026-07-06).** The master-plan Phase 3 verb list does not name a bead-create verb, but the exit criterion requires a bead to "live its whole Tier-0 ledger life via the CLI (create → claim → close)". `sling` (create + spawn worker) is Phase 8 and must not be pulled forward. So Phase 3 adds `camp create <title>` — pure ledger create, the plumbing `sling` will later wrap. This is the same "additive CLI surface implied by the exit criteria" justification as master-plan decision 7 (claim/close/event-emit). The verb is named `create` (not `new`) for parity with bd's `bd create` — camp mirrors gc/bd vocabulary wherever a concept exists (spec §15.2).
- **B. ID allocation is a folded `counters` table, no schema-version bump.** `next_bead_id` reads `counters(prefix, high)`; the fold of `bead.created` raises `high` to the created id's number (`ON CONFLICT … DO UPDATE SET high = max(high, excluded.high)`). Refold reconstructs `counters` exactly from history, so `doctor --refold` stays exact. The table is added to the v1 DDL (not a new schema version): camp is pre-release, no live `camp.db` exists outside ephemeral tests, and the operator's standing rule is "no backwards-compatibility assumption." `SCHEMA_VERSION` stays `1`.
- **C. Prefixes have no hyphen.** A bead id is `{prefix}-{n}`; it is split on its **first** `-`. Prefix rule: `^[a-z][a-z0-9]*$` (validated in `id::validate_prefix`). This makes id parsing unambiguous. Default prefix = the rig name lowercased with non-`[a-z0-9]` removed (e.g. `gascity` → `gascity`); the user passes `--prefix gc` for brevity. A slug that is empty or starts with a digit is a hard error asking for `--prefix`.
- **D. `rig.added` is log-only in the fold; `camp.toml` is the rig source of truth.** Rigs are user-editable config (spec §7.1, §12); a rigs *state table* would create a second source of truth that could drift from hand-edited TOML. So the fold validates the `rig.added` payload shape (fail-fast) but writes no state; `rig ls` reads `camp.toml`; `ls --rig`/`ls --mine` filter the `beads` table directly (beads carry their rig). `rig add` records the event first (the ledger is durable truth, spec §13.4), then appends a `[[rigs]]` block to `camp.toml` (textual append preserves comments and prior rigs). A filesystem failure after the event commits surfaces as a hard error; the two-store update is not perfectly atomic, which the spec's file-config + config-as-events model accepts. Concurrent `rig add` invocations are serialized by decision H so they cannot pass the duplicate check simultaneously and clobber the file.
- **E. Create is read-counter-then-append (not allocate-in-txn).** `camp create` calls `ledger.next_bead_id(prefix)` then `ledger.append(bead.created{id})`; the fold bumps the counter. Two concurrent creators can read the same counter and both emit the same id — the loser's `bead.created` fails on the `beads` primary key with a clean "bead already exists" error (no corruption, no silent retry). campd (single-threaded, Phase 7) never races itself; CLI use is user-paced. The `next_bead_id(conn: &Connection, …)` free-function signature is preserved so a future phase can allocate inside a write transaction if contention ever matters (YAGNI now).
- **F. Encapsulation:** camp-core `id`/`readiness` free functions take `&Connection` (the contract signatures, and reusable inside a transaction). The CLI never touches a raw connection — it calls thin public `Ledger` wrapper methods that delegate to those free functions with the private `self.conn`. Tests exercise the wrappers (the real shipped path).
- **G. `ls --json` emits explicit `null`s** (no `skip_serializing_if`) so the machine-readable surface is stable for `jq`/golden tests. Ordering: `ORDER BY created_ts, id` (deterministic; id tiebreak).
- **H. `rig add` holds an exclusive advisory lock on `camp.toml` across its whole critical section (added 2026-07-06 from PR #5 review, Finding 2).** `camp rig add` reads config, checks for a duplicate name/prefix, appends `rig.added`, then read-modify-writes `camp.toml`. Two concurrent invocations could both pass the duplicate check, both emit `rig.added`, and clobber each other's write — losing a rig from the source-of-truth file while its event persists. Fix: acquire an exclusive advisory lock (`std::fs::File::lock()`, stable since Rust 1.89) on `camp.toml` before the load and hold it through the write. The lock serializes the section, so the loser re-reads the winner's rig and fails its duplicate check cleanly (fail-fast — a proper "already exists" error, never a silent clobber). Advisory locks release on drop / process exit, so a crash never leaves a stuck lock, preserving crash-only design (spec §5). No new lockfile is introduced (the lock is on `camp.toml` itself), so the no-status-files principle is untouched. Chosen over the weaker "re-read + re-verify before write" mitigation because that leaves a TOCTOU window and does not prevent the duplicate `rig.added` event.

## What later phases rely on (interfaces Phase 3 produces)

- **Phase 4 (search/memory):** `camp remember` reuses the `bead.created` path with `type='memory'` and per-rig id allocation — no new create machinery.
- **Phase 7 (campd):** `readiness::newly_ready(conn, closed_bead)` is the affected-subgraph recompute campd runs on each close (spec §7.3); `readiness::ready_beads` / `is_ready` back its dispatch decisions. These take `&Connection` so campd can call them against its own ledger handle.

## File Structure

| File | Responsibility |
|---|---|
| `crates/camp-core/src/config.rs` (new) | `CampConfig`/`CampSection`/`RigConfig`, `camp.toml` parse with `deny_unknown_fields`, rig lookup |
| `crates/camp-core/src/id.rs` (new) | prefix validation, id parse, `next_bead_id`, `bump_counter` (fold hook) |
| `crates/camp-core/src/readiness.rs` (new) | `BeadRow`, `ListFilter`, `is_ready`, `ready_beads`, `newly_ready`, `list_beads`, `get_bead` |
| `crates/camp-core/src/error.rs` (mod) | add `Config`, `UnknownRig`, `InvalidPrefix` variants |
| `crates/camp-core/src/event.rs` (mod) | add `EventType::RigAdded` (`"rig.added"`) |
| `crates/camp-core/src/vocab.rs` (mod) | add `"rig.added"` to `CAMP_SPECIFIC_EVENTS` |
| `crates/camp-core/src/ledger/schema.rs` (mod) | add `counters` table to `STATE_DDL` |
| `crates/camp-core/src/ledger/fold.rs` (mod) | `bead_created` bumps counter; new `rig_added` arm |
| `crates/camp-core/src/ledger/refold.rs` (mod) | add `counters` to `STATE_TABLES` |
| `crates/camp-core/src/ledger/mod.rs` (mod) | `events_for_bead` + `Ledger` read wrappers; schema test +`counters` |
| `crates/camp-core/src/lib.rs` (mod) | `pub mod config; pub mod id; pub mod readiness;` + re-exports |
| `crates/camp-core/tests/refold_prop.rs` (mod) | `counters` in state dump + id-survives-refold property |
| `crates/camp/src/campdir.rs` (mod) | `config_path()` |
| `crates/camp/src/cmd/{create,rig,claim,close,ls,show}.rs` (new) | the six verbs |
| `crates/camp/src/main.rs` (mod) | wire subcommands |
| `crates/camp/tests/cli_*.rs` (new) | verb round-trips, golden JSON, full lifecycle |

---

## Task 3.1: `config.rs` — camp.toml model

**Files:**
- Create: `crates/camp-core/src/config.rs`
- Modify: `crates/camp-core/src/error.rs`, `crates/camp-core/src/lib.rs`, `crates/camp-core/Cargo.toml`

**Interfaces:**
- Produces: `camp_core::config::{CampConfig, CampSection, RigConfig}`; `CampConfig::load(&Path)`, `CampConfig::parse(&str)`, `CampConfig::rig(&str)`; `CoreError::{Config, UnknownRig}`.

- [ ] **Step 1: Add the `toml` dependency**

```bash
cargo add --package camp-core toml
```

- [ ] **Step 2: Add error variants**

In `crates/camp-core/src/error.rs`, add to the `CoreError` enum (after `UnknownEventType`):

```rust
    #[error("config: {0}")]
    Config(String),
    #[error("unknown rig {0:?}")]
    UnknownRig(String),
    #[error("invalid rig prefix {0:?}: must match ^[a-z][a-z0-9]*$")]
    InvalidPrefix(String),
```

(`InvalidPrefix` is used in Task 3.2; adding it here keeps error.rs edited once.)

- [ ] **Step 3: Write the failing test**

Create `crates/camp-core/src/config.rs`:

```rust
//! camp.toml: the human-readable config that names the camp and its rigs
//! (spec §7.1, §12). Parsing is fail-fast — unknown keys are rejected
//! (`deny_unknown_fields`) so a typo never silently becomes dead config.
//! `camp.toml` is the source of truth for rigs; `rig.added` events are the
//! audit trail (spec §13.4), not a competing store.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampConfig {
    pub camp: CampSection,
    #[serde(default)]
    pub rigs: Vec<RigConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampSection {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigConfig {
    pub name: String,
    pub path: PathBuf,
    pub prefix: String,
}

impl CampConfig {
    /// Parse a camp.toml file. Missing file, bad TOML, and unknown keys are
    /// all hard errors.
    pub fn load(path: &Path) -> Result<CampConfig, CoreError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CoreError::Config(format!("cannot read {}: {e}", path.display())))?;
        CampConfig::parse(&text)
    }

    pub fn parse(text: &str) -> Result<CampConfig, CoreError> {
        toml::from_str(text).map_err(|e| CoreError::Config(e.to_string()))
    }

    /// The rig with this name, or `UnknownRig`.
    pub fn rig(&self, name: &str) -> Result<&RigConfig, CoreError> {
        self.rigs
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| CoreError::UnknownRig(name.to_owned()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_camp_and_rigs() {
        let cfg = CampConfig::parse(
            r#"
# a comment
[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/code/gascity"
prefix = "gc"
"#,
        )
        .unwrap();
        assert_eq!(cfg.camp.name, "dev");
        assert_eq!(cfg.rigs.len(), 1);
        assert_eq!(cfg.rig("gascity").unwrap().prefix, "gc");
    }

    #[test]
    fn rigs_default_to_empty() {
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        assert!(cfg.rigs.is_empty());
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let err = CampConfig::parse("[camp]\nname = \"dev\"\nbogus = 1\n").unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn unknown_rig_key_is_rejected() {
        let err = CampConfig::parse(
            "[camp]\nname=\"d\"\n[[rigs]]\nname=\"r\"\npath=\"/p\"\nprefix=\"r\"\nzzz=1\n",
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn missing_rig_is_unknown_rig() {
        let cfg = CampConfig::parse("[camp]\nname=\"d\"\n").unwrap();
        assert!(matches!(cfg.rig("nope"), Err(CoreError::UnknownRig(n)) if n == "nope"));
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = CampConfig {
            camp: CampSection { name: "dev".into() },
            rigs: vec![RigConfig {
                name: "gascity".into(),
                path: "/code/gascity".into(),
                prefix: "gc".into(),
            }],
        };
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(CampConfig::parse(&text).unwrap(), cfg);
    }
}
```

Add to `crates/camp-core/src/lib.rs` (after `pub mod clock;`):

```rust
pub mod config;
```

- [ ] **Step 4: Run to verify it fails, then compiles+passes**

Run: `cargo test --package camp-core config::`
Expected: first FAIL (module wiring / `toml` not yet added if steps skipped), then after Steps 1–3 all six `config` tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/config.rs crates/camp-core/src/error.rs crates/camp-core/src/lib.rs crates/camp-core/Cargo.toml Cargo.lock
git commit -m "feat: camp.toml config model with deny_unknown_fields"
```

---

## Task 3.2: `counters` table + `id.rs` — allocation as folded state

**Files:**
- Create: `crates/camp-core/src/id.rs`
- Modify: `crates/camp-core/src/ledger/schema.rs`, `crates/camp-core/src/ledger/fold.rs`, `crates/camp-core/src/ledger/refold.rs`, `crates/camp-core/src/ledger/mod.rs`, `crates/camp-core/src/lib.rs`

**Interfaces:**
- Consumes: `Ledger`, `CoreError`, the `beads`/`counters` tables.
- Produces: `camp_core::id::{validate_prefix, parse_bead_id, next_bead_id}`; `id::bump_counter` (`pub(crate)`, fold hook); `Ledger::next_bead_id(&self, prefix)`.

- [ ] **Step 1: Add the `counters` table to `STATE_DDL`**

In `crates/camp-core/src/ledger/schema.rs`, append to the `STATE_DDL` string (after the `search` virtual table, before the closing `"#`):

```sql

CREATE TABLE counters (
  prefix TEXT PRIMARY KEY,
  high   INTEGER NOT NULL
) STRICT;
```

- [ ] **Step 2: Register `counters` in the refold diff**

In `crates/camp-core/src/ledger/refold.rs`, add a fifth entry to `STATE_TABLES` (after the `search` spec):

```rust
    TableSpec {
        name: "counters",
        cols: "prefix, high",
        key: "prefix",
    },
```

- [ ] **Step 3: Update the schema test to expect `counters`**

In `crates/camp-core/src/ledger/mod.rs`, in `open_applies_pragmas_and_creates_schema_v1`, change the table list:

```rust
        for table in [
            "meta", "events", "beads", "deps", "sessions", "cursors", "search", "counters",
        ] {
```

- [ ] **Step 4: Write `id.rs` with its failing unit tests**

Create `crates/camp-core/src/id.rs`:

```rust
//! Per-rig bead id allocation (spec §12). Ids are `{prefix}-{n}` with a
//! monotonic per-prefix counter that is *folded state*: `bead.created` bumps
//! the `counters` table, so a refold reconstructs the exact allocation
//! high-water mark from history and `doctor --refold` stays exact.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::CoreError;

/// A prefix is a lowercase letter followed by lowercase alphanumerics. No
/// hyphens: an id splits on its first '-', so the prefix must not contain one.
pub fn validate_prefix(prefix: &str) -> Result<(), CoreError> {
    let mut chars = prefix.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(CoreError::InvalidPrefix(prefix.to_owned()))
    }
}

/// Split an id into `(prefix, number)`. `None` if it is not a well-formed,
/// canonical camp bead id (no leading zeros, valid prefix, non-negative int).
pub fn parse_bead_id(id: &str) -> Option<(&str, i64)> {
    let (prefix, num) = id.split_once('-')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if num.len() > 1 && num.starts_with('0') {
        return None; // non-canonical
    }
    validate_prefix(prefix).ok()?;
    let n: i64 = num.parse().ok()?;
    Some((prefix, n))
}

/// The next unused id for `prefix`, e.g. `gc-143`. Reads the folded counter;
/// the caller appends `bead.created` with this id and the fold advances the
/// counter to match (decision E).
pub fn next_bead_id(conn: &Connection, prefix: &str) -> Result<String, CoreError> {
    validate_prefix(prefix)?;
    let high: i64 = conn
        .query_row("SELECT high FROM counters WHERE prefix = ?1", [prefix], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(0);
    Ok(format!("{prefix}-{}", high + 1))
}

/// Fold effect of `bead.created`: raise the prefix counter to at least this
/// id's number. Called from `fold::bead_created` inside the write txn.
pub(crate) fn bump_counter(conn: &Connection, id: &str) -> Result<(), CoreError> {
    let (prefix, n) = parse_bead_id(id).ok_or_else(|| CoreError::InvalidEventData {
        event_type: "bead.created".to_owned(),
        reason: format!("bead id {id:?} is not a well-formed {{prefix}}-{{n}} id"),
    })?;
    conn.execute(
        "INSERT INTO counters (prefix, high) VALUES (?1, ?2)
         ON CONFLICT(prefix) DO UPDATE SET high = max(high, excluded.high)",
        params![prefix, n],
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn validate_prefix_accepts_and_rejects() {
        for good in ["gc", "t3", "gascity", "a0"] {
            assert!(validate_prefix(good).is_ok(), "{good} should be valid");
        }
        for bad in ["", "3d", "GC", "g-c", "g_c", "g c"] {
            assert!(validate_prefix(bad).is_err(), "{bad} should be invalid");
        }
    }

    #[test]
    fn parse_bead_id_rules() {
        assert_eq!(parse_bead_id("gc-142"), Some(("gc", 142)));
        assert_eq!(parse_bead_id("gc-0"), Some(("gc", 0)));
        assert_eq!(parse_bead_id("t3-17"), Some(("t3", 17)));
        for bad in ["gc", "gc-", "-1", "gc-x", "gc-01", "GC-1", "3d-1", "gc-1-2"] {
            // "gc-1-2" splits to ("gc","1-2") -> non-digit -> None
            assert_eq!(parse_bead_id(bad), None, "{bad} should not parse");
        }
    }
}
```

Add to `crates/camp-core/src/lib.rs` (after `pub mod event;`):

```rust
pub mod id;
```

- [ ] **Step 5: Wire the fold hook and the `Ledger` wrapper**

In `crates/camp-core/src/ledger/fold.rs`, inside `bead_created`, after the `search` insert and before `Ok(())`:

```rust
    crate::id::bump_counter(conn, id)?;
```

In `crates/camp-core/src/ledger/mod.rs`, add a wrapper method inside `impl Ledger` (after `events_range`):

```rust
    /// The next unused bead id for `prefix` (spec §12). See `camp_core::id`.
    pub fn next_bead_id(&self, prefix: &str) -> Result<String, CoreError> {
        crate::id::next_bead_id(&self.conn, prefix)
    }
```

- [ ] **Step 6: Add ledger-level tests for allocation + refold exactness**

In `crates/camp-core/src/ledger/mod.rs` `mod tests`, add (the helpers `temp_ledger`, `created`, `count` already exist in this module):

```rust
    #[test]
    fn next_bead_id_starts_at_one_and_follows_creates() {
        let (_dir, mut ledger) = temp_ledger();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-1");
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-2");
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-3");
        // per-prefix, independent
        assert_eq!(ledger.next_bead_id("t3").unwrap(), "t3-1");
        // the counter is folded state
        let high: i64 = ledger
            .conn
            .query_row("SELECT high FROM counters WHERE prefix = 'gc'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(high, 2);
    }

    #[test]
    fn rolled_back_create_does_not_bump_the_counter() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        // duplicate id: whole txn rolls back, counter must stay at 1
        assert!(
            ledger
                .append(created("gc-1", serde_json::json!({"title": "dup"})))
                .is_err()
        );
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-2");
    }

    #[test]
    fn counters_are_refold_exact() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        assert!(ledger.refold_check().unwrap().drift.is_empty());
        // tamper the counter, refold must catch it, repair must fix it
        ledger
            .conn
            .execute("UPDATE counters SET high = 99 WHERE prefix = 'gc'", [])
            .unwrap();
        assert!(
            ledger
                .refold_check()
                .unwrap()
                .drift
                .iter()
                .any(|d| d.table == "counters")
        );
        ledger.refold_repair().unwrap();
        assert_eq!(ledger.next_bead_id("gc").unwrap(), "gc-3");
        assert_eq!(count(&ledger, "SELECT count(*) FROM counters"), 1);
    }
```

- [ ] **Step 7: Run all camp-core tests**

Run: `cargo test --package camp-core`
Expected: PASS — new `id` tests, new ledger tests, and the existing refold/fold/schema tests all green (the modified schema test now expects `counters`).

- [ ] **Step 8: Commit**

```bash
git add crates/camp-core/src/id.rs crates/camp-core/src/lib.rs crates/camp-core/src/ledger/
git commit -m "feat: per-rig bead id allocation folded into a counters table"
```

---

## Task 3.3: `rig.added` event + vocabulary

**Files:**
- Modify: `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`

**Interfaces:**
- Produces: `EventType::RigAdded` (`"rig.added"`); fold arm validating the `{path, prefix}` payload (log-only).

- [ ] **Step 1: Write the failing tests**

In `crates/camp-core/src/event.rs` `mod tests`, add:

```rust
    #[test]
    fn rig_added_round_trips_through_its_name() {
        assert_eq!(EventType::parse("rig.added").unwrap(), EventType::RigAdded);
        assert_eq!(EventType::RigAdded.as_str(), "rig.added");
    }
```

In `crates/camp-core/src/ledger/mod.rs` `mod tests`, add:

```rust
    #[test]
    fn rig_added_is_validated_and_log_only() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(EventInput {
                kind: EventType::RigAdded,
                rig: Some("gascity".into()),
                actor: "cli".into(),
                bead: None,
                data: serde_json::json!({"path": "/code/gascity", "prefix": "gc"}),
            })
            .unwrap();
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
        assert_eq!(count(&ledger, "SELECT count(*) FROM beads"), 0);
        // malformed payload fails fast, appends nothing
        assert!(
            ledger
                .append(EventInput {
                    kind: EventType::RigAdded,
                    rig: Some("x".into()),
                    actor: "cli".into(),
                    bead: None,
                    data: serde_json::json!({"path": "/p", "prefix": "x", "extra": 1}),
                })
                .is_err()
        );
        assert_eq!(count(&ledger, "SELECT count(*) FROM events"), 1);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp-core rig_added`
Expected: FAIL — `EventType::RigAdded` does not exist.

- [ ] **Step 3: Add the variant, the name, the vocab entry, and the fold arm**

In `crates/camp-core/src/event.rs`: add `RigAdded,` to the `EventType` enum (after `CampdStopped`), add `EventType::RigAdded,` to `EventType::ALL`, and add the match arms:

```rust
            EventType::RigAdded => "rig.added",
```

(in `as_str`, after the `CampdStopped` arm).

In `crates/camp-core/src/vocab.rs`, extend `CAMP_SPECIFIC_EVENTS`:

```rust
pub const CAMP_SPECIFIC_EVENTS: &[&str] =
    &["bead.claimed", "campd.started", "campd.stopped", "rig.added"];
```

In `crates/camp-core/src/ledger/fold.rs`: add the arm to `apply`:

```rust
        EventType::RigAdded => rig_added(event),
```

and the function (near the other fold fns):

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RigAdded {
    path: String,
    prefix: String,
}

/// `rig.added` is log-only: rigs live in camp.toml (decision D). The fold
/// validates the audit payload shape and the rig name so a malformed config
/// event fails fast.
fn rig_added(event: &Event) -> Result<(), CoreError> {
    if event.rig.is_none() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "missing rig name".to_owned(),
        });
    }
    let _p: RigAdded = payload(event)?;
    Ok(())
}
```

- [ ] **Step 4: Run camp-core tests including the vocab pin**

Run: `cargo test --package camp-core`
Expected: PASS — `rig_added` tests green, and `tests/vocab_pin.rs` still green (the partition test now sees `rig.added` in both `EventType::ALL` and `CAMP_SPECIFIC_EVENTS`; the collision test confirms `rig.added` ∉ gc).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs
git commit -m "feat: rig.added camp-specific event (log-only, config as events)"
```

---

## Task 3.4: `readiness.rs` — is_ready, ready_beads, newly_ready, queries

**Files:**
- Create: `crates/camp-core/src/readiness.rs`
- Modify: `crates/camp-core/src/lib.rs`, `crates/camp-core/src/ledger/mod.rs`

**Interfaces:**
- Consumes: the `beads`/`deps` tables.
- Produces: `camp_core::readiness::{BeadRow, ListFilter, is_ready, ready_beads, newly_ready, list_beads, get_bead}`; `Ledger` wrappers `is_ready`, `ready_beads`, `newly_ready`, `list_beads`, `get_bead`.

- [ ] **Step 1: Write `readiness.rs` with failing unit tests**

Create `crates/camp-core/src/readiness.rs`:

```rust
//! Readiness (spec §7.3, plan decision 6): a bead is ready when it is open
//! and every `needs` target exists, is closed, and passed. A failed or
//! missing dependency never unblocks its dependents. Also the read surface
//! `camp ls` uses. Pure queries over the state tables — no writes.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::error::CoreError;

/// One bead as `camp ls`/`camp show` present it. Optional fields serialize as
/// explicit `null` (stable machine-readable JSON, decision G).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BeadRow {
    pub id: String,
    pub rig: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: String,
    pub assignee: Option<String>,
    pub claimed_by: Option<String>,
    pub outcome: Option<String>,
    pub labels: Vec<String>,
    pub created_ts: String,
    pub updated_ts: String,
}

/// Filter for `list_beads`. `None` fields impose no constraint.
#[derive(Debug, Default)]
pub struct ListFilter<'a> {
    pub rig: Option<&'a str>,
    pub mine: Option<&'a str>,
}

const BEAD_COLS: &str = "id, rig, type, title, status, assignee, claimed_by, outcome, \
                         labels, created_ts, updated_ts";

fn row_to_bead(row: &rusqlite::Row<'_>) -> rusqlite::Result<BeadRow> {
    let labels_json: String = row.get(8)?;
    let labels: Vec<String> = serde_json::from_str(&labels_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(BeadRow {
        id: row.get(0)?,
        rig: row.get(1)?,
        kind: row.get(2)?,
        title: row.get(3)?,
        status: row.get(4)?,
        assignee: row.get(5)?,
        claimed_by: row.get(6)?,
        outcome: row.get(7)?,
        labels,
        created_ts: row.get(9)?,
        updated_ts: row.get(10)?,
    })
}

fn collect(
    rows: impl Iterator<Item = rusqlite::Result<BeadRow>>,
) -> Result<Vec<BeadRow>, CoreError> {
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// A `needs` target counts as unmet unless it exists, is closed, and passed.
const UNMET_DEP: &str = "(t.id IS NULL OR t.status <> 'closed' OR t.outcome IS NOT 'pass')";

pub fn is_ready(conn: &Connection, bead: &str) -> Result<bool, CoreError> {
    let status: Option<String> = conn
        .query_row("SELECT status FROM beads WHERE id = ?1", [bead], |r| r.get(0))
        .optional()?;
    let status = status.ok_or_else(|| CoreError::UnknownBead(bead.to_owned()))?;
    if status != "open" {
        return Ok(false);
    }
    let unmet: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM deps d LEFT JOIN beads t ON t.id = d.needs_id
             WHERE d.bead_id = ?1 AND {UNMET_DEP}"
        ),
        [bead],
        |r| r.get(0),
    )?;
    Ok(unmet == 0)
}

pub fn ready_beads(conn: &Connection, rig: Option<&str>) -> Result<Vec<BeadRow>, CoreError> {
    let sql = format!(
        "SELECT {BEAD_COLS} FROM beads b
         WHERE b.status = 'open' AND (?1 IS NULL OR b.rig = ?1)
           AND NOT EXISTS (
             SELECT 1 FROM deps d LEFT JOIN beads t ON t.id = d.needs_id
             WHERE d.bead_id = b.id AND {UNMET_DEP})
         ORDER BY b.created_ts, b.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![rig], row_to_bead)?;
    collect(rows)
}

/// The dependents of `closed_bead` that its close just made ready — campd's
/// affected-subgraph recompute (spec §7.3). A fail close unblocks nothing.
pub fn newly_ready(conn: &Connection, closed_bead: &str) -> Result<Vec<String>, CoreError> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT bead_id FROM deps WHERE needs_id = ?1 ORDER BY bead_id")?;
    let dependents: Vec<String> = stmt
        .query_map([closed_bead], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut ready = Vec::new();
    for dep in dependents {
        if is_ready(conn, &dep)? {
            ready.push(dep);
        }
    }
    Ok(ready)
}

pub fn list_beads(conn: &Connection, filter: &ListFilter) -> Result<Vec<BeadRow>, CoreError> {
    let sql = format!(
        "SELECT {BEAD_COLS} FROM beads
         WHERE (?1 IS NULL OR rig = ?1) AND (?2 IS NULL OR claimed_by = ?2)
         ORDER BY created_ts, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![filter.rig, filter.mine], row_to_bead)?;
    collect(rows)
}

pub fn get_bead(conn: &Connection, id: &str) -> Result<Option<BeadRow>, CoreError> {
    let row = conn
        .query_row(
            &format!("SELECT {BEAD_COLS} FROM beads WHERE id = ?1"),
            [id],
            row_to_bead,
        )
        .optional()?;
    Ok(row)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        (dir, l)
    }

    fn create(l: &mut Ledger, id: &str, needs: &[&str]) {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"title": id, "needs": needs}),
        })
        .unwrap();
    }

    fn close(l: &mut Ledger, id: &str, outcome: &str) {
        l.append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"outcome": outcome}),
        })
        .unwrap();
    }

    #[test]
    fn open_bead_with_no_deps_is_ready() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        assert!(l.is_ready("gc-1").unwrap());
    }

    #[test]
    fn open_dependency_blocks() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        assert!(!l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn closed_fail_dependency_stays_blocked() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "fail");
        assert!(!l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn missing_dependency_stays_blocked() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-2", &["gc-404"]);
        assert!(!l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn closed_pass_dependency_unblocks() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "pass");
        assert!(l.is_ready("gc-2").unwrap());
    }

    #[test]
    fn claimed_bead_is_not_ready() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        l.append(EventInput {
            kind: EventType::BeadClaimed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"session": "camp/dev/1"}),
        })
        .unwrap();
        assert!(!l.is_ready("gc-1").unwrap());
    }

    #[test]
    fn is_ready_on_unknown_bead_errors() {
        let (_d, l) = ledger();
        assert!(matches!(
            l.is_ready("gc-nope"),
            Err(crate::error::CoreError::UnknownBead(_))
        ));
    }

    #[test]
    fn ready_beads_lists_only_the_unblocked() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]); // ready
        create(&mut l, "gc-2", &["gc-1"]); // blocked
        let ready: Vec<String> = l
            .ready_beads(None)
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ready, vec!["gc-1"]);
    }

    #[test]
    fn diamond_graph_readiness() {
        // A <- B, A <- C, {B,C} <- D
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]); // A
        create(&mut l, "gc-2", &["gc-1"]); // B
        create(&mut l, "gc-3", &["gc-1"]); // C
        create(&mut l, "gc-4", &["gc-2", "gc-3"]); // D

        // close A -> B and C become ready, D still blocked
        close(&mut l, "gc-1", "pass");
        assert_eq!(l.newly_ready("gc-1").unwrap(), vec!["gc-2", "gc-3"]);
        assert!(!l.is_ready("gc-4").unwrap());

        // close B -> D not yet ready (C still open)
        close(&mut l, "gc-2", "pass");
        assert!(l.newly_ready("gc-2").unwrap().is_empty());

        // close C -> D becomes ready
        close(&mut l, "gc-3", "pass");
        assert_eq!(l.newly_ready("gc-3").unwrap(), vec!["gc-4"]);
        assert!(l.is_ready("gc-4").unwrap());
    }

    #[test]
    fn newly_ready_is_empty_for_a_fail_close() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        create(&mut l, "gc-2", &["gc-1"]);
        close(&mut l, "gc-1", "fail");
        assert!(l.newly_ready("gc-1").unwrap().is_empty());
    }

    #[test]
    fn list_and_get_beads() {
        let (_d, mut l) = ledger();
        create(&mut l, "gc-1", &[]);
        assert_eq!(l.list_beads(&Default::default()).unwrap().len(), 1);
        assert_eq!(l.get_bead("gc-1").unwrap().unwrap().status, "open");
        assert!(l.get_bead("gc-404").unwrap().is_none());
    }
}
```

Add to `crates/camp-core/src/lib.rs`:

```rust
pub mod readiness;
```

and a re-export (after the `pub type Seq` line):

```rust
pub use readiness::{BeadRow, ListFilter};
```

- [ ] **Step 2: Add the `Ledger` wrappers**

In `crates/camp-core/src/ledger/mod.rs`, inside `impl Ledger` (after `next_bead_id`):

```rust
    /// True when `bead` is open and every `needs` target passed (decision 6).
    pub fn is_ready(&self, bead: &str) -> Result<bool, CoreError> {
        crate::readiness::is_ready(&self.conn, bead)
    }

    /// Open, unblocked beads, optionally scoped to a rig.
    pub fn ready_beads(
        &self,
        rig: Option<&str>,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::ready_beads(&self.conn, rig)
    }

    /// Dependents of `closed_bead` its close just made ready (spec §7.3).
    pub fn newly_ready(&self, closed_bead: &str) -> Result<Vec<String>, CoreError> {
        crate::readiness::newly_ready(&self.conn, closed_bead)
    }

    /// Beads matching `filter`, in creation order.
    pub fn list_beads(
        &self,
        filter: &crate::readiness::ListFilter,
    ) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::list_beads(&self.conn, filter)
    }

    /// One bead's current state, or `None`.
    pub fn get_bead(&self, id: &str) -> Result<Option<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::get_bead(&self.conn, id)
    }
```

- [ ] **Step 3: Run to verify failure, then pass**

Run: `cargo test --package camp-core readiness`
Expected: FAIL first (module absent), then all `readiness` tests PASS after Steps 1–2.

- [ ] **Step 4: Commit**

```bash
git add crates/camp-core/src/readiness.rs crates/camp-core/src/lib.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat: readiness computation and bead query surface (decision 6)"
```

---

## Task 3.5: `Ledger::events_for_bead` — the sanctioned history read

**Files:**
- Modify: `crates/camp-core/src/ledger/mod.rs`

**Interfaces:**
- Produces: `Ledger::events_for_bead(&self, bead) -> Result<Vec<Event>, CoreError>` (uses the `events_bead` index; spec §7.4 — the one sanctioned history read, consumed by `camp show`).

- [ ] **Step 1: Write the failing test**

In `crates/camp-core/src/ledger/mod.rs` `mod tests`, add:

```rust
    #[test]
    fn events_for_bead_returns_only_that_beads_history_in_order() {
        let (_dir, mut ledger) = temp_ledger();
        ledger
            .append(created("gc-1", serde_json::json!({"title": "one"})))
            .unwrap();
        ledger
            .append(created("gc-2", serde_json::json!({"title": "two"})))
            .unwrap();
        ledger
            .append(input(
                EventType::BeadClosed,
                Some("gc"),
                Some("gc-1"),
                serde_json::json!({"outcome": "pass"}),
            ))
            .unwrap();
        let hist = ledger.events_for_bead("gc-1").unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].kind, EventType::BeadCreated);
        assert_eq!(hist[1].kind, EventType::BeadClosed);
        assert!(hist.iter().all(|e| e.bead.as_deref() == Some("gc-1")));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp-core events_for_bead`
Expected: FAIL — method does not exist.

- [ ] **Step 3: Implement**

In `crates/camp-core/src/ledger/mod.rs`, inside `impl Ledger` (after `events_range`):

```rust
    /// Full event history for one bead, in seq order (spec §7.4 — the one
    /// sanctioned history read, used by `camp show`). Indexed via `events_bead`.
    pub fn events_for_bead(&self, bead: &str) -> Result<Vec<Event>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, type, rig, actor, bead, data FROM events
             WHERE bead = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([bead], row_to_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp-core events_for_bead`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/ledger/mod.rs
git commit -m "feat: Ledger::events_for_bead for camp show history (spec 7.4)"
```

---

## Task 3.6: `camp rig add` / `camp rig ls`

**Files:**
- Create: `crates/camp/src/cmd/rig.rs`
- Modify: `crates/camp/src/main.rs`, `crates/camp/src/campdir.rs`, `crates/camp/Cargo.toml`
- Test: `crates/camp/tests/cli_rig.rs`

**Interfaces:**
- Consumes: `CampConfig`, `RigConfig`, `id::validate_prefix`, `Ledger::append`, `EventType::RigAdded`.
- Produces: `cmd::rig::{add, ls}`; `CampDir::config_path()`.

- [ ] **Step 1: Add the `toml` dep and `config_path`**

```bash
cargo add --package camp toml
```

In `crates/camp/src/campdir.rs`, add inside `impl CampDir` (after `db_path`):

```rust
    pub fn config_path(&self) -> PathBuf {
        self.root.join("camp.toml")
    }
```

- [ ] **Step 2: Write the failing CLI test**

Create `crates/camp/tests/cli_rig.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// A camp plus a throwaway directory to register as a rig.
fn camp_with_rig_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let rig_dir = dir.path().join("myrepo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    (dir, rig_dir)
}

#[test]
fn rig_add_writes_toml_and_appends_event() {
    let (dir, rig_dir) = camp_with_rig_dir();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();

    let toml = std::fs::read_to_string(dir.path().join(".camp/camp.toml")).unwrap();
    assert!(toml.contains("[[rigs]]"), "toml was: {toml}");
    assert!(toml.contains("name = \"gascity\""));
    assert!(toml.contains("prefix = \"gc\""));

    let events = camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events = String::from_utf8(events).unwrap();
    assert!(events.contains(r#""type":"rig.added""#), "events: {events}");

    // rig ls shows it
    camp()
        .current_dir(dir.path())
        .args(["rig", "ls"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gascity"));
}

#[test]
fn duplicate_prefix_is_rejected() {
    let (dir, rig_dir) = camp_with_rig_dir();
    let rig_dir2 = dir.path().join("other");
    std::fs::create_dir_all(&rig_dir2).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "a"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir2)
        .args(["--prefix", "gc", "--name", "b"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("prefix"));
}

#[test]
fn bad_prefix_is_rejected() {
    let (dir, rig_dir) = camp_with_rig_dir();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "Bad-One", "--name", "x"])
        .assert()
        .failure()
        .code(1);
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --package camp --test cli_rig`
Expected: FAIL — no `rig` subcommand.

- [ ] **Step 4: Implement `cmd/rig.rs`**

Create `crates/camp/src/cmd/rig.rs`:

```rust
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use camp_core::config::{CampConfig, RigConfig};
use camp_core::event::{EventInput, EventType};
use camp_core::id::validate_prefix;
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp rig add <path> [--prefix p] [--name n]`: register a repo as a rig.
/// Records `rig.added` (spec §13.4), then appends a `[[rigs]]` block to
/// camp.toml (decision D). camp.toml is the rig source of truth.
pub fn add(camp: &CampDir, path: PathBuf, prefix: Option<String>, name: Option<String>) -> Result<()> {
    let abs = std::fs::canonicalize(&path)
        .with_context(|| format!("rig path {} does not exist", path.display()))?;
    if !abs.is_dir() {
        bail!("rig path {} is not a directory", abs.display());
    }
    let name = name.unwrap_or_else(|| default_name(&abs));
    let prefix = match prefix {
        Some(p) => p,
        None => default_prefix(&name)?,
    };
    validate_prefix(&prefix).map_err(|e| anyhow::anyhow!("{e}"))?;

    let config_path = camp.config_path();
    let config = CampConfig::load(&config_path)?;
    if config.rigs.iter().any(|r| r.name == name) {
        bail!("a rig named {name:?} already exists");
    }
    if config.rigs.iter().any(|r| r.prefix == prefix) {
        bail!("prefix {prefix:?} is already used by another rig");
    }

    let rig = RigConfig {
        name: name.clone(),
        path: abs.clone(),
        prefix: prefix.clone(),
    };

    let mut ledger = Ledger::open(&camp.db_path())?;
    ledger.append(EventInput {
        kind: EventType::RigAdded,
        rig: Some(name.clone()),
        actor: "cli".into(),
        bead: None,
        data: serde_json::json!({ "path": abs, "prefix": prefix }),
    })?;
    append_rig_toml(&config_path, &rig)?;

    println!("added rig {name} ({prefix}) -> {}", abs.display());
    Ok(())
}

/// `camp rig ls [--json]`: list configured rigs (read from camp.toml).
pub fn ls(camp: &CampDir, json: bool) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    if json {
        println!("{}", serde_json::to_string(&config.rigs)?);
    } else {
        for r in &config.rigs {
            println!("{}\t{}\t{}", r.name, r.prefix, r.path.display());
        }
    }
    Ok(())
}

fn default_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("rig")
        .to_owned()
}

fn default_prefix(name: &str) -> Result<String> {
    let slug: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if slug.chars().next().is_none_or(|c| !c.is_ascii_lowercase()) {
        bail!("cannot derive a prefix from rig name {name:?}; pass --prefix");
    }
    Ok(slug)
}

fn append_rig_toml(config_path: &Path, rig: &RigConfig) -> Result<()> {
    let fragment: BTreeMap<&str, Vec<&RigConfig>> = BTreeMap::from([("rigs", vec![rig])]);
    let block = toml::to_string(&fragment).context("cannot serialize rig entry")?;
    let mut existing = std::fs::read_to_string(config_path)
        .with_context(|| format!("cannot read {}", config_path.display()))?;
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push('\n');
    existing.push_str(&block);
    std::fs::write(config_path, existing)
        .with_context(|| format!("cannot write {}", config_path.display()))?;
    Ok(())
}
```

- [ ] **Step 5: Wire the subcommand in `main.rs`**

In `crates/camp/src/main.rs`: add `pub mod rig;` to the `cmd` module block. Add to the `Command` enum:

```rust
    /// Manage rigs (registered repositories)
    Rig {
        #[command(subcommand)]
        command: RigCommand,
    },
```

Add a new enum after `Command`:

```rust
#[derive(Subcommand)]
enum RigCommand {
    /// Register a repository as a rig
    Add {
        /// Path to the repository
        path: PathBuf,
        /// Bead id prefix (default: derived from the name; e.g. --prefix gc)
        #[arg(long)]
        prefix: Option<String>,
        /// Rig name (default: the directory's basename)
        #[arg(long)]
        name: Option<String>,
    },
    /// List configured rigs
    Ls {
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
}
```

Add to the `match cli.command` in `run`:

```rust
        Command::Rig { command } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match command {
                RigCommand::Add {
                    path,
                    prefix,
                    name,
                } => cmd::rig::add(&camp, path, prefix, name),
                RigCommand::Ls { json } => cmd::rig::ls(&camp, json),
            }
        }
```

- [ ] **Step 6: Run to verify pass**

Run: `cargo test --package camp --test cli_rig`
Expected: PASS — all three tests.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/cmd/rig.rs crates/camp/src/main.rs crates/camp/src/campdir.rs crates/camp/Cargo.toml crates/camp/tests/cli_rig.rs Cargo.lock
git commit -m "feat: camp rig add/ls (camp.toml + rig.added event)"
```

---

## Task 3.7: `camp create` — create a bead

**Files:**
- Create: `crates/camp/src/cmd/create.rs`
- Modify: `crates/camp/src/main.rs`
- Test: `crates/camp/tests/cli_create.rs`

**Interfaces:**
- Consumes: `CampConfig::rig`, `Ledger::next_bead_id`, `Ledger::append`, `EventType::BeadCreated`.
- Produces: `cmd::create::run`.

- [ ] **Step 1: Write the failing CLI test**

Create `crates/camp/tests/cli_create.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// Init a camp and register one rig `gascity` (prefix `gc`).
fn camp_with_rig() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    dir
}

#[test]
fn create_allocates_prefixed_ids_and_stays_refold_clean() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "add a --json flag", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args(["create", "second task", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-2\n"));

    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
}

#[test]
fn create_defaults_to_the_only_rig() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "no --rig needed"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
}

#[test]
fn create_with_no_rigs_errors() {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    camp()
        .current_dir(dir.path())
        .args(["create", "orphan"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("no rigs configured"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp --test cli_create`
Expected: FAIL — no `create` subcommand.

- [ ] **Step 3: Implement `cmd/create.rs`**

Create `crates/camp/src/cmd/create.rs`:

```rust
use anyhow::{Result, bail};
use camp_core::config::{CampConfig, RigConfig};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp create <title> [--rig r] [--needs id]… [--label l]… [--type t]
/// [--description d] [--assignee a]`: append `bead.created` with a freshly
/// allocated per-rig id, and print the id. Named for parity with `bd create`;
/// the plumbing `sling` (Phase 8) will wrap it.
#[allow(clippy::too_many_arguments)]
pub fn run(
    camp: &CampDir,
    title: String,
    rig: Option<String>,
    description: Option<String>,
    needs: Vec<String>,
    labels: Vec<String>,
    bead_type: Option<String>,
    assignee: Option<String>,
) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let rig_cfg = resolve_rig(&config, rig.as_deref())?;

    let mut ledger = Ledger::open(&camp.db_path())?;
    let id = ledger.next_bead_id(&rig_cfg.prefix)?;

    let mut data = serde_json::json!({ "title": title });
    if let Some(d) = description {
        data["description"] = serde_json::json!(d);
    }
    if !needs.is_empty() {
        data["needs"] = serde_json::json!(needs);
    }
    if !labels.is_empty() {
        data["labels"] = serde_json::json!(labels);
    }
    if let Some(t) = bead_type {
        data["type"] = serde_json::json!(t);
    }
    if let Some(a) = assignee {
        data["assignee"] = serde_json::json!(a);
    }

    ledger.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig_cfg.name.clone()),
        actor: "cli".into(),
        bead: Some(id.clone()),
        data,
    })?;
    println!("{id}");
    Ok(())
}

fn resolve_rig<'a>(config: &'a CampConfig, rig: Option<&str>) -> Result<&'a RigConfig> {
    match rig {
        Some(name) => Ok(config.rig(name)?),
        None => match config.rigs.as_slice() {
            [only] => Ok(only),
            [] => bail!("no rigs configured; run camp rig add <path> first"),
            _ => bail!("multiple rigs configured; pass --rig <name>"),
        },
    }
}
```

- [ ] **Step 4: Wire the subcommand in `main.rs`**

Add `pub mod create;` to the `cmd` module block. Add to the `Command` enum:

```rust
    /// Create a bead in the ledger
    Create {
        /// Bead title
        title: String,
        /// Rig (default: the only configured rig)
        #[arg(long)]
        rig: Option<String>,
        /// Longer description
        #[arg(long)]
        description: Option<String>,
        /// A bead this one depends on (repeatable)
        #[arg(long = "needs")]
        needs: Vec<String>,
        /// A label (repeatable)
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Bead type (task|mail|memory; default task)
        #[arg(long = "type")]
        bead_type: Option<String>,
        /// Routing hint to a pack agent
        #[arg(long)]
        assignee: Option<String>,
    },
```

Add to `run`'s match:

```rust
        Command::Create {
            title,
            rig,
            description,
            needs,
            labels,
            bead_type,
            assignee,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::create::run(&camp, title, rig, description, needs, labels, bead_type, assignee)
        }
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp --test cli_create`
Expected: PASS — all three tests.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/create.rs crates/camp/src/main.rs crates/camp/tests/cli_create.rs
git commit -m "feat: camp create — create a bead with an allocated per-rig id"
```

---

## Task 3.8: `camp claim` / `camp close`

**Files:**
- Create: `crates/camp/src/cmd/claim.rs`, `crates/camp/src/cmd/close.rs`
- Modify: `crates/camp/src/main.rs`
- Test: `crates/camp/tests/cli_claim_close.rs`

**Interfaces:**
- Consumes: `Ledger::append`, `EventType::{BeadClaimed, BeadClosed}`.
- Produces: `cmd::claim::run`, `cmd::close::run`.

- [ ] **Step 1: Write the failing CLI test**

Create `crates/camp/tests/cli_claim_close.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn camp_with_bead() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["create", "do the thing", "--rig", "gascity"])
        .assert()
        .success();
    dir
}

#[test]
fn claim_then_close_runs_the_full_lifecycle() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "shipped"])
        .assert()
        .success();
    // ledger stays refold-clean across the whole lifecycle
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
}

#[test]
fn double_claim_fails_fast() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/2"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn close_rejects_a_non_subset_outcome() {
    let dir = camp_with_bead();
    // clap constrains --outcome to pass|fail (usage error, exit 2)
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "skipped"])
        .assert()
        .failure()
        .code(2);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp --test cli_claim_close`
Expected: FAIL — no `claim`/`close` subcommands.

- [ ] **Step 3: Implement the two commands**

Create `crates/camp/src/cmd/claim.rs`:

```rust
use anyhow::Result;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp claim <bead> --session <name>`: open → in_progress (worker contract).
pub fn run(camp: &CampDir, bead: String, session: String) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    ledger.append(EventInput {
        kind: EventType::BeadClaimed,
        rig: None,
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data: serde_json::json!({ "session": session }),
    })?;
    println!("claimed {bead}");
    Ok(())
}
```

Create `crates/camp/src/cmd/close.rs`:

```rust
use anyhow::Result;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp close <bead> --outcome pass|fail [--reason r]`: close with outcome.
pub fn run(camp: &CampDir, bead: String, outcome: String, reason: Option<String>) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let mut data = serde_json::json!({ "outcome": outcome });
    if let Some(r) = reason {
        data["reason"] = serde_json::json!(r);
    }
    ledger.append(EventInput {
        kind: EventType::BeadClosed,
        rig: None,
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data,
    })?;
    println!("closed {bead} ({outcome})");
    Ok(())
}
```

- [ ] **Step 4: Wire the subcommands in `main.rs`**

Add `pub mod claim;` and `pub mod close;` to the `cmd` block. Add to the `Command` enum:

```rust
    /// Claim a bead for a session (open → in_progress)
    Claim {
        /// Bead id
        bead: String,
        /// Claiming session name
        #[arg(long)]
        session: String,
    },
    /// Close a bead with an outcome
    Close {
        /// Bead id
        bead: String,
        /// Outcome
        #[arg(long, value_parser = ["pass", "fail"])]
        outcome: String,
        /// Close note (searchable)
        #[arg(long)]
        reason: Option<String>,
    },
```

Add to `run`'s match:

```rust
        Command::Claim { bead, session } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::claim::run(&camp, bead, session)
        }
        Command::Close {
            bead,
            outcome,
            reason,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::close::run(&camp, bead, outcome, reason)
        }
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp --test cli_claim_close`
Expected: PASS — all three tests.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/claim.rs crates/camp/src/cmd/close.rs crates/camp/src/main.rs crates/camp/tests/cli_claim_close.rs
git commit -m "feat: camp claim/close — the worker lifecycle verbs"
```

---

## Task 3.9: `camp ls` — filtered queries with `--json` golden output

**Files:**
- Create: `crates/camp/src/cmd/ls.rs`
- Modify: `crates/camp/src/main.rs`
- Test: `crates/camp/tests/cli_ls.rs`

**Interfaces:**
- Consumes: `Ledger::{ready_beads, list_beads}`, `ListFilter`, `BeadRow`.
- Produces: `cmd::ls::run`.

- [ ] **Step 1: Write the failing CLI test (golden JSON)**

Create `crates/camp/tests/cli_ls.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;
use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// Init a camp, then seed beads directly through camp-core with a fixed clock
/// so `--json` output is byte-deterministic (the binary uses SystemClock).
fn seeded(dir: &std::path::Path) {
    camp().current_dir(dir).arg("init").assert().success();
    let mut ledger = Ledger::open_with_clock(
        &dir.join(".camp/camp.db"),
        Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
    )
    .unwrap();
    // gc-1 open (ready), gc-2 needs gc-1 (blocked)
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gascity".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"title": "one"}),
        })
        .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gascity".into()),
            actor: "cli".into(),
            bead: Some("gc-2".into()),
            data: serde_json::json!({"title": "two", "needs": ["gc-1"]}),
        })
        .unwrap();
}

const READY_JSON: &str = r#"[{"id":"gc-1","rig":"gascity","type":"task","title":"one","status":"open","assignee":null,"claimed_by":null,"outcome":null,"labels":[],"created_ts":"2026-07-05T21:14:03Z","updated_ts":"2026-07-05T21:14:03Z"}]"#;

#[test]
fn ls_ready_json_is_exactly_the_unblocked_bead() {
    let dir = tempfile::tempdir().unwrap();
    seeded(dir.path());
    camp()
        .current_dir(dir.path())
        .args(["ls", "--ready", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::diff(format!("{READY_JSON}\n")));
}

#[test]
fn ls_all_lists_both_beads() {
    let dir = tempfile::tempdir().unwrap();
    seeded(dir.path());
    let out = camp()
        .current_dir(dir.path())
        .args(["ls", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(r#""id":"gc-1""#));
    assert!(out.contains(r#""id":"gc-2""#));
}

#[test]
fn ls_rig_filter_scopes_results() {
    let dir = tempfile::tempdir().unwrap();
    seeded(dir.path());
    camp()
        .current_dir(dir.path())
        .args(["ls", "--rig", "nonesuch", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]\n"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp --test cli_ls`
Expected: FAIL — no `ls` subcommand.

- [ ] **Step 3: Implement `cmd/ls.rs`**

Create `crates/camp/src/cmd/ls.rs`:

```rust
use anyhow::Result;
use camp_core::ledger::Ledger;
use camp_core::readiness::ListFilter;

use crate::campdir::CampDir;

/// `camp ls [--ready | --mine <session>] [--rig <r>] [--json]`.
pub fn run(
    camp: &CampDir,
    ready: bool,
    mine: Option<String>,
    rig: Option<String>,
    json: bool,
) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let beads = if ready {
        ledger.ready_beads(rig.as_deref())?
    } else {
        ledger.list_beads(&ListFilter {
            rig: rig.as_deref(),
            mine: mine.as_deref(),
        })?
    };
    if json {
        println!("{}", serde_json::to_string(&beads)?);
    } else {
        for b in &beads {
            println!("{}\t{}\t{}\t{}", b.id, b.status, b.rig, b.title);
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Wire the subcommand in `main.rs`**

Add `pub mod ls;` to the `cmd` block. Add to the `Command` enum:

```rust
    /// List beads
    Ls {
        /// Only open, unblocked beads
        #[arg(long, conflicts_with = "mine")]
        ready: bool,
        /// Only beads claimed by this session
        #[arg(long)]
        mine: Option<String>,
        /// Scope to a rig
        #[arg(long)]
        rig: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
```

Add to `run`'s match:

```rust
        Command::Ls {
            ready,
            mine,
            rig,
            json,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::ls::run(&camp, ready, mine, rig, json)
        }
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp --test cli_ls`
Expected: PASS — all three tests, including the exact `READY_JSON` golden match.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/ls.rs crates/camp/src/main.rs crates/camp/tests/cli_ls.rs
git commit -m "feat: camp ls with --ready/--mine/--rig and --json golden output"
```

---

## Task 3.10: `camp show` — current state + full event history

**Files:**
- Create: `crates/camp/src/cmd/show.rs`
- Modify: `crates/camp/src/main.rs`
- Test: `crates/camp/tests/cli_show.rs`

**Interfaces:**
- Consumes: `Ledger::{get_bead, is_ready, events_for_bead}`.
- Produces: `cmd::show::run`.

- [ ] **Step 1: Write the failing CLI test**

Create `crates/camp/tests/cli_show.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn camp_with_bead() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["create", "do the thing", "--rig", "gascity"])
        .assert()
        .success();
    dir
}

#[test]
fn show_reports_state_and_history() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1"))
        .stdout(predicates::str::contains("in_progress"))
        .stdout(predicates::str::contains("bead.created"))
        .stdout(predicates::str::contains("bead.claimed"));
}

#[test]
fn show_of_unknown_bead_errors() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-999"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("no such bead"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp --test cli_show`
Expected: FAIL — no `show` subcommand.

- [ ] **Step 3: Implement `cmd/show.rs`**

Create `crates/camp/src/cmd/show.rs`:

```rust
use anyhow::{Result, anyhow};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp show <bead>`: current state plus full event history — the one
/// sanctioned history read (spec §7.4).
pub fn run(camp: &CampDir, bead: String) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let row = ledger
        .get_bead(&bead)?
        .ok_or_else(|| anyhow!("no such bead: {bead}"))?;
    let ready = ledger.is_ready(&bead)?;
    let history = ledger.events_for_bead(&bead)?;

    println!("bead     {}", row.id);
    println!("rig      {}", row.rig);
    println!("type     {}", row.kind);
    println!("title    {}", row.title);
    println!(
        "status   {}{}",
        row.status,
        if ready { "  (ready)" } else { "" }
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
    if !row.labels.is_empty() {
        println!("labels   {}", row.labels.join(", "));
    }
    println!("created  {}", row.created_ts);
    println!("updated  {}", row.updated_ts);
    println!();
    println!("history:");
    for e in &history {
        println!("  {:>4}  {}  {:<14}  {}", e.seq, e.ts, e.kind.as_str(), e.data);
    }
    Ok(())
}
```

- [ ] **Step 4: Wire the subcommand in `main.rs`**

Add `pub mod show;` to the `cmd` block. Add to the `Command` enum:

```rust
    /// Show a bead's current state and full event history
    Show {
        /// Bead id
        bead: String,
    },
```

Add to `run`'s match:

```rust
        Command::Show { bead } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::show::run(&camp, bead)
        }
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp --test cli_show`
Expected: PASS — both tests.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/show.rs crates/camp/src/main.rs crates/camp/tests/cli_show.rs
git commit -m "feat: camp show — bead state plus full event history (spec 7.4)"
```

---

## Task 3.11: Property test extension + full-lifecycle integration test

**Files:**
- Modify: `crates/camp-core/tests/refold_prop.rs`
- Create: `crates/camp/tests/cli_lifecycle.rs`

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Extend the refold property to cover `counters` and id survival**

In `crates/camp-core/tests/refold_prop.rs`, add a `counters` entry to `DUMPS` (so the two-ledger equivalence check covers it), immediately after the `"search"` entry:

```rust
    ("counters", "prefix, high"),
```

Then extend the property body: after the existing `Property 1` refold_check block and before `Property 2`, add an id-survival assertion (the counter is the allocation high-water mark, so `next_bead_id` must be identical before and after a repair):

```rust
        // Property 3: id allocation is folded state — refold_repair preserves
        // the next id for every prefix that was created ("bead").
        let before = ledger_a.next_bead_id("bead").unwrap();
        ledger_a.refold_repair().unwrap();
        let after = ledger_a.next_bead_id("bead").unwrap();
        prop_assert_eq!(before, after);
```

(`ledger_a` is already `mut`; `refold_repair` and `next_bead_id` take `&mut self`/`&self` respectively.)

- [ ] **Step 2: Run the property test**

Run: `cargo test --package camp-core --test refold_prop`
Expected: PASS (64 cases). If a counterexample appears, the fold is wrong — fix `bump_counter`/refold before proceeding, do not weaken the test.

- [ ] **Step 3: Write the full-lifecycle integration test**

Create `crates/camp/tests/cli_lifecycle.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// A bead's whole Tier-0 life through the CLI, with `doctor --refold` clean at
/// every stage (the Phase 3 exit criterion).
#[test]
fn create_claim_close_stays_refold_clean_throughout() {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();

    let refold_clean = |label: &str| {
        camp()
            .current_dir(dir.path())
            .args(["doctor", "--refold"])
            .assert()
            .success()
            .stdout(predicates::str::contains("0 drift rows"));
        let _ = label;
    };

    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    refold_clean("after rig add");

    camp()
        .current_dir(dir.path())
        .args(["create", "the whole life", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    refold_clean("after new");

    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    refold_clean("after claim");

    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "done"])
        .assert()
        .success();
    refold_clean("after close");

    // final state: closed + passed, out of the ready set
    camp()
        .current_dir(dir.path())
        .args(["ls", "--ready", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::diff("[]\n"));
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("status   closed"))
        .stdout(predicates::str::contains("outcome  pass"));
}
```

- [ ] **Step 4: Run the integration test**

Run: `cargo test --package camp --test cli_lifecycle`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/tests/refold_prop.rs crates/camp/tests/cli_lifecycle.rs
git commit -m "test: id allocation survives refold; full Tier-0 CLI lifecycle stays refold-clean"
```

---

## Task 3.12: Phase gate, push, PR

**Files:** none (verification + delivery).

- [ ] **Step 1: Run the full gate locally**

Run:
```bash
cargo fmt --all --check && \
cargo clippy --workspace --all-targets --all-features -- -D warnings && \
cargo test --workspace
```
Expected: all green. Fix any clippy findings (e.g. `is_none_or` needs a recent stable; if unavailable, rewrite `default_prefix`'s guard as `slug.chars().next().map_or(true, |c| !c.is_ascii_lowercase())`). Do not push until this triple is clean.

- [ ] **Step 2: Push the branch**

```bash
git push -u origin phase-3-beads-readiness
```

- [ ] **Step 3: Open the PR**

```bash
gh pr create --title "Phase 3: beads, rigs, readiness, queries" --body "$(cat <<'EOF'
Implements master-plan Phase 3 (docs/superpowers/plans/2026-07-06-phase-3-beads-readiness.md).

- config.rs: camp.toml model (CampConfig/RigConfig, deny_unknown_fields)
- id.rs + counters table: per-rig prefixed ids as folded state (refold-exact)
- rig.added event (camp-specific, log-only); camp.toml is the rig source of truth
- readiness.rs: is_ready / ready_beads / newly_ready per decision 6
- CLI: create, rig add/ls, claim, close, ls (--ready/--mine/--rig/--json), show
- Full Tier-0 lifecycle (create → claim → close) stays doctor --refold clean

Tests: readiness truth table + diamonds, newly_ready subgraph, id-survives-refold
property, --json golden output, rig add writes both TOML and event, lifecycle
refold-clean integration.
EOF
)"
```

- [ ] **Step 4: Watch CI to green**

Run: `gh pr checks --watch`
Expected: fmt, clippy, test (ubuntu), test (macos) all pass. Phase 3 is complete only when green.

- [ ] **Step 5: Report to the team lead**

Report the PR number, CI status, and each exit criterion quoted with its evidence (see below).

---

## Exit Criteria → Evidence Map

Quoted from the master-plan Phase 3 contract:

1. **"a bead can live its whole Tier-0 ledger life via the CLI (create → claim → close)"** → `camp create`/`claim`/`close` verbs (Tasks 3.7–3.8) + `crates/camp/tests/cli_lifecycle.rs::create_claim_close_stays_refold_clean_throughout`.
2. **"doctor --refold stays clean throughout (assert it in tests)"** → refold assertions after every lifecycle stage in `cli_lifecycle.rs`; `counters_are_refold_exact` (Task 3.2); the `refold_prop.rs` id-survival property (Task 3.11).
3. **"CI green"** → Task 3.12 `gh pr checks --watch`.

Master-plan Phase 3 test obligations:

- **readiness truth table (unmet dep, closed-fail dep, missing dep, diamond graphs)** → `readiness.rs` tests: `open_dependency_blocks`, `closed_fail_dependency_stays_blocked`, `missing_dependency_stays_blocked`, `closed_pass_dependency_unblocks`, `diamond_graph_readiness`.
- **newly_ready returns exactly the newly ready subgraph** → `diamond_graph_readiness`, `newly_ready_is_empty_for_a_fail_close`.
- **id allocation survives refold (property extension)** → `refold_prop.rs` Property 3.
- **CLI round-trips incl. --json golden output** → `cli_ls.rs::ls_ready_json_is_exactly_the_unblocked_bead`.
- **rig add writes both TOML and event** → `cli_rig.rs::rig_add_writes_toml_and_appends_event`.

## Self-Review

- **Spec coverage:** every contract file (`config`/`id`/`readiness`, the five/​six CLI verbs, extended `fold`, `rig.added`) has a task; every listed test obligation maps to a named test (above).
- **Type consistency:** `BeadRow`/`ListFilter` defined once in `readiness.rs`, re-exported at the crate root, and consumed by name in the `Ledger` wrappers and `cmd/ls.rs`/`cmd/show.rs`. `next_bead_id`/`validate_prefix`/`parse_bead_id`/`bump_counter` names are used identically in `id.rs`, `fold.rs`, and the `Ledger` wrapper. Event name `"rig.added"` matches across `event.rs`, `vocab.rs`, and the fold.
- **Placeholders:** none — every code step carries complete code; every run step carries the exact command and expected result.
- **Operator sign-off (2026-07-06):** Decision A approved with the verb named `camp create` (parity with `bd create`); Decision B acknowledged; C–G no objections.

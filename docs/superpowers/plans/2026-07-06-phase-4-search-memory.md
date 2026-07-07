# Phase 4 — Search and Memory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Spec §7.4's query surface — `camp search` (ranked FTS over everything, all time), `camp remember` (bd-style persistent memory as memory-type beads), and `camp recall` (search filtered to memory) — so the worker skill's recall-before/remember-after contract has its verbs.

**Architecture:** Phase 1's fold already writes every search row (`body` on `bead.created`/`bead.updated`, `close` on `bead.closed`), and Phase 3's `bead.created` path already accepts `type='memory'`. Phase 4 therefore adds **no new events, no fold changes, no vocab changes, and no schema changes** — only a read-side module (`camp-core/src/search.rs` ranked by `bm25(search)`), one new `CoreError` variant for malformed FTS queries, and three thin CLI verbs. `camp remember` is a delegation to the existing `cmd::create::run` with `type='memory'`; `camp recall` is `search` with `type_filter=Some("memory")`. Refold-exactness holds by construction and is re-verified via `camp doctor --refold` in the CLI tests.

**Tech Stack:** Rust 2024 workspace; `rusqlite` 0.40.1 (bundled SQLite, FTS5); `clap` 4 derive; `assert_cmd`/`predicates`/`tempfile` for CLI tests. No new dependencies in either crate.

## Global Constraints

- Never commit to main; all work on branch `phase-4-search-memory`; no co-author lines in commits.
- Gates before push: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- No panics in library code: workspace denies `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`; `#![forbid(unsafe_code)]` in both crates. Test modules carry `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- Fail fast, no fallbacks, no silenced errors (AGENTS.md invariant 5). FTS5 query syntax errors surface as a clean domain error (exit 1), never a panic and never a raw sqlite error string alone.
- Contract interface (master plan Phase 4, verbatim):
  ```rust
  pub struct SearchHit { pub bead_id: String, pub kind: String, pub snippet: String, pub rank: f64 }
  pub fn search(conn: &Connection, query: &str, type_filter: Option<&str>, limit: usize)
      -> Result<Vec<SearchHit>, CoreError>; // ORDER BY bm25(search)
  ```
- Sibling-owned files are off limits: `crates/camp-core/src/formula/**` (Phase 5), `crates/camp/src/daemon/**`, `cmd/stop.rs`, `cmd/top.rs` (Phase 7). Shared-file edits (`crates/camp/src/main.rs`) stay minimal and additive. This phase does NOT touch `event.rs`, `vocab.rs`, `fold.rs`, or either `Cargo.toml`.
- On lead notice that a sibling PR merged: rebase onto current main, resolve, re-run all gates before continuing.

## Verified facts this plan relies on

- `search` FTS5 table (Phase 1, `schema.rs:58-60`): `fts5(bead_id UNINDEXED, kind UNINDEXED, content)` — content is column index 2 for `snippet()`.
- Fold writes: `bead_created` inserts `(id, 'body', title || '\n' || description)`; `bead_updated` rewrites the body row; `bead_closed` inserts `(id, 'close', reason)` when reason is non-empty (`fold.rs:123-126, 187, 224-231, 265-276`).
- `memory` is already a legal bead type (`fold.rs:13`: `BEAD_TYPES = ["task", "mail", "memory"]`) and `camp create --type memory` already works (covered by `cli_create.rs::create_label_and_type_round_trip_through_show`).
- `rusqlite` 0.40.1's fallible `ToSql for usize` (`to_sql.rs:276`) is gated behind the off-by-default `fallible_uint` feature, which this workspace does not enable — so `search()` converts with `i64::try_from(limit)` and maps overflow to `InvalidSearchQuery` (fail fast, never truncate). *(Corrected during execution: the plan originally said `limit` binds directly; the compile failed because the impl is feature-gated.)*
- FTS5's `bm25()` clamps non-positive IDF to `1e-6` (bundled `sqlite3.c:245021`), so bm25 values are always negative-is-better and document-length normalization ranks a short adjacent-terms doc above a long scattered-terms doc even when every doc contains the query terms.
- The FTS5 query text is a **bound parameter** parsed when the statement first steps; a parse failure surfaces as plain `SQLITE_ERROR` (code 1) with fts5's message (e.g. `fts5: syntax error near "("`). Our own SQL is fixed and known-good, so `SQLITE_ERROR` from this statement can only mean a bad user query.
- `Ledger.conn` is private; core tests drive search through a new `Ledger::search` wrapper (same pattern as `is_ready`/`ready_beads` wrapping `readiness.rs` free functions).
- `cmd::create::run(camp, title, rig, description, needs, labels, bead_type, assignee)` resolves the rig (default = the only configured rig), allocates the per-rig id, appends `bead.created`, prints the id (`create.rs:13-55`). `camp remember` reuses exactly this path.

---

### Task 1: `camp-core` search module

**Files:**
- Create: `crates/camp-core/src/search.rs`
- Modify: `crates/camp-core/src/error.rs` (add one variant)
- Modify: `crates/camp-core/src/lib.rs` (add `pub mod search;` + re-export)
- Modify: `crates/camp-core/src/ledger/mod.rs` (add `Ledger::search` wrapper)
- Test: in-file `#[cfg(test)]` module of `crates/camp-core/src/search.rs`

**Interfaces:**
- Consumes: `Ledger::open_with_clock`, `Ledger::append`, `EventInput`, `EventType` (Phase 1); `search`/`beads` tables (Phase 1/3).
- Produces (Task 2 relies on these exact names):
  - `camp_core::search::SearchHit { bead_id: String, kind: String, snippet: String, rank: f64 }` (also re-exported as `camp_core::SearchHit`)
  - `camp_core::search::search(conn: &Connection, query: &str, type_filter: Option<&str>, limit: usize) -> Result<Vec<SearchHit>, CoreError>`
  - `camp_core::ledger::Ledger::search(&self, query: &str, type_filter: Option<&str>, limit: usize) -> Result<Vec<SearchHit>, CoreError>`
  - `CoreError::InvalidSearchQuery { query: String, reason: String }` displaying as `invalid search query {query:?}: {reason}`

- [ ] **Step 1: Write the failing tests**

Create `crates/camp-core/src/search.rs` containing ONLY the test module for now (plus the doc comment), so the first `cargo test` run fails to compile on the missing items — that is the observed failure for this task:

```rust
//! Ranked full-text search over the ledger's FTS5 `search` table (spec
//! §7.4): titles, descriptions, close notes, and memory. Search rows are
//! written exclusively by the fold (Phase 1); this module only reads.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::error::CoreError;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-06T12:00:00Z")),
        )
        .unwrap();
        (dir, ledger)
    }

    fn append(ledger: &mut Ledger, kind: EventType, bead: &str, data: serde_json::Value) {
        ledger
            .append(EventInput {
                kind,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(bead.into()),
                data,
            })
            .unwrap();
    }

    /// Ranking sanity (master-plan test obligation): the same terms adjacent
    /// in a short doc must outrank the same terms scattered through a long
    /// one. The expected winner is gc-2 on purpose: it sorts AFTER gc-1 in
    /// the deterministic bead_id tiebreak, so this test cannot pass by
    /// tiebreak accident — only by bm25 rank.
    #[test]
    fn exact_phrase_outranks_scattered_terms() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({
                "title": "ops notes",
                "description": "the api endpoint returns a payload and somewhere \
                 deep in the config a key rotation schedule hides among many \
                 unrelated words about deployment logging metrics and dashboards"
            }),
        );
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-2",
            serde_json::json!({"title": "rotate the api key"}),
        );

        // Both beads contain "api" and "key"; the short adjacent use wins.
        let hits = ledger.search("api key", None, 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].bead_id, "gc-2");
        assert_eq!(hits[1].bead_id, "gc-1");
        assert!(
            hits[0].rank < hits[1].rank,
            "bm25 is more-negative-is-better: {hits:?}"
        );
        assert!(
            hits[0].snippet.contains("api key"),
            "snippet: {:?}",
            hits[0].snippet
        );

        // Quoted, it is an FTS5 phrase query: only the adjacent use matches.
        let hits = ledger.search("\"api key\"", None, 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].bead_id, "gc-2");
    }

    #[test]
    fn type_filter_narrows_to_memory_beads() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "fix the deploy pipeline"}),
        );
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-2",
            serde_json::json!({"title": "deploy runs need the staging token", "type": "memory"}),
        );

        let all = ledger.search("deploy", None, 10).unwrap();
        assert_eq!(all.len(), 2, "{all:?}");

        let memories = ledger.search("deploy", Some("memory"), 10).unwrap();
        assert_eq!(memories.len(), 1, "{memories:?}");
        assert_eq!(memories[0].bead_id, "gc-2");
        assert_eq!(memories[0].kind, "body");
    }

    #[test]
    fn close_note_content_is_searchable() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "chase the flaky test"}),
        );
        append(
            &mut ledger,
            EventType::BeadClosed,
            "gc-1",
            serde_json::json!({"outcome": "pass", "reason": "root cause was a stale dispatcher cache"}),
        );

        let hits = ledger.search("dispatcher", None, 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].bead_id, "gc-1");
        assert_eq!(hits[0].kind, "close");
        assert!(
            hits[0].snippet.contains("dispatcher"),
            "snippet: {:?}",
            hits[0].snippet
        );
    }

    #[test]
    fn limit_caps_the_result_set() {
        let (_dir, mut ledger) = temp_ledger();
        for i in 1..=3 {
            append(
                &mut ledger,
                EventType::BeadCreated,
                &format!("gc-{i}"),
                serde_json::json!({"title": format!("widget number {i}")}),
            );
        }
        assert_eq!(ledger.search("widget", None, 10).unwrap().len(), 3);
        assert_eq!(ledger.search("widget", None, 2).unwrap().len(), 2);
    }

    #[test]
    fn malformed_fts_queries_are_clean_domain_errors() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "anything"}),
        );
        // Syntax error, dangling operator, unknown column filter, empty query:
        // every one must be InvalidSearchQuery, never a panic or raw Sqlite error.
        for bad in ["(", "AND", "nosuchcolumn:foo", ""] {
            match ledger.search(bad, None, 10) {
                Err(CoreError::InvalidSearchQuery { query, reason }) => {
                    assert_eq!(query, bad);
                    assert!(!reason.is_empty());
                }
                other => panic!("query {bad:?}: expected InvalidSearchQuery, got {other:?}"),
            }
        }
    }

    #[test]
    fn no_hits_is_ok_and_empty() {
        let (_dir, ledger) = temp_ledger();
        assert_eq!(ledger.search("zeppelin", None, 10).unwrap(), vec![]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp-core search 2>&1 | tail -20`
Expected: compile error — `Ledger` has no method `search` (and `CoreError::InvalidSearchQuery` does not exist). This is the failing state; do not proceed if it compiles.

- [ ] **Step 3: Implement**

3a. Add the error variant to `crates/camp-core/src/error.rs`, after the `InvalidPrefix` variant (keep the enum's existing order otherwise):

```rust
    #[error("invalid search query {query:?}: {reason}")]
    InvalidSearchQuery { query: String, reason: String },
```

3b. Add the implementation above the test module in `crates/camp-core/src/search.rs`:

```rust
use rusqlite::{Connection, params};

use crate::error::CoreError;

/// One ranked search result. `kind` is the matched row's provenance:
/// `"body"` (title + description) or `"close"` (close note). `rank` is the
/// raw SQLite `bm25(search)` value — more negative is better; results come
/// back best-first.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub bead_id: String,
    pub kind: String,
    pub snippet: String,
    pub rank: f64,
}

/// Ranked FTS5 search over everything, all time (spec §7.4). `query` is
/// FTS5 query syntax verbatim: bare terms are AND-ed, `"quoted strings"`
/// are exact phrases, `term*` is a prefix. A query FTS5 cannot parse
/// surfaces as [`CoreError::InvalidSearchQuery`] — a clean domain error,
/// never a panic. `type_filter` narrows hits to beads of one type
/// (`Some("memory")` is `camp recall`); `limit` caps the result set.
pub fn search(
    conn: &Connection,
    query: &str,
    type_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, CoreError> {
    let mut stmt = conn.prepare(
        "SELECT search.bead_id, search.kind,
                snippet(search, 2, '', '', '…', 12),
                bm25(search)
         FROM search
         JOIN beads ON beads.id = search.bead_id
         WHERE search MATCH ?1
           AND (?2 IS NULL OR beads.type = ?2)
         ORDER BY bm25(search), search.bead_id, search.kind
         LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![query, type_filter, limit], |r| {
            Ok(SearchHit {
                bead_id: r.get(0)?,
                kind: r.get(1)?,
                snippet: r.get(2)?,
                rank: r.get(3)?,
            })
        })
        .map_err(|e| translate_fts_error(query, e))?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(|e| translate_fts_error(query, e))?);
    }
    Ok(hits)
}

/// The FTS5 query text is a bound parameter, parsed when the statement
/// first steps. A parse failure there is a plain SQLITE_ERROR carrying
/// fts5's message (`fts5: syntax error near "("`, `no such column: x`, …);
/// our own SQL is fixed and known-good, so that combination can only mean
/// a bad user query. Every other error propagates unchanged — nothing is
/// silenced.
fn translate_fts_error(query: &str, e: rusqlite::Error) -> CoreError {
    match e {
        rusqlite::Error::SqliteFailure(ffi, Some(msg))
            if ffi.extended_code == rusqlite::ffi::SQLITE_ERROR =>
        {
            CoreError::InvalidSearchQuery {
                query: query.to_owned(),
                reason: msg,
            }
        }
        other => CoreError::Sqlite(other),
    }
}
```

Notes for the implementer:
- `snippet(search, 2, '', '', '…', 12)`: column 2 is `content`; empty highlight markers (plain text for the CLI); `…` ellipsis; at most 12 tokens.
- The `bead_id`/`kind` ORDER BY tail is a deterministic tiebreak for equal bm25 scores only.
- `limit: usize` is converted with `i64::try_from` (rusqlite's `ToSql for usize` is feature-gated off); overflow maps to `InvalidSearchQuery`, never a truncation.
- Deduping is deliberately absent: a bead matched in both its body and its close note yields two hits distinguished by `kind`.

3c. Register the module in `crates/camp-core/src/lib.rs` — the module list becomes (new lines marked):

```rust
pub mod clock;
pub mod config;
pub mod error;
pub mod event;
pub mod id;
pub mod ledger;
pub mod readiness;
pub mod search;   // NEW
pub mod vocab;

pub use readiness::{BeadRow, ListFilter};
pub use search::SearchHit;   // NEW
```

3d. Add the wrapper to `impl Ledger` in `crates/camp-core/src/ledger/mod.rs`, directly after the `events_for_bead` method:

```rust
    /// Ranked full-text search over titles, descriptions, close notes, and
    /// memory (spec §7.4), best match first. See [`crate::search::search`].
    pub fn search(
        &self,
        query: &str,
        type_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::search::SearchHit>, CoreError> {
        crate::search::search(&self.conn, query, type_filter, limit)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p camp-core search`
Expected: 6 tests pass (`exact_phrase_outranks_scattered_terms`, `type_filter_narrows_to_memory_beads`, `close_note_content_is_searchable`, `limit_caps_the_result_set`, `malformed_fts_queries_are_clean_domain_errors`, `no_hits_is_ok_and_empty`).

Then run the whole core suite (the refold property test and vocab pins must stay green): `cargo test -p camp-core`
Expected: all pass, zero failures.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/search.rs crates/camp-core/src/error.rs crates/camp-core/src/lib.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat: ranked FTS5 search in camp-core (spec §7.4)"
```

---

### Task 2: CLI verbs `search`, `remember`, `recall`

**Files:**
- Create: `crates/camp/src/cmd/search.rs`
- Create: `crates/camp/src/cmd/remember.rs`
- Create: `crates/camp/src/cmd/recall.rs`
- Modify: `crates/camp/src/main.rs` (three `mod` lines, three `Command` variants, three dispatch arms — additive only; this is a shared-conflict-zone file)
- Test: `crates/camp/tests/cli_search.rs`

**Interfaces:**
- Consumes (from Task 1): `Ledger::search(&self, query: &str, type_filter: Option<&str>, limit: usize) -> Result<Vec<SearchHit>, CoreError>`; `SearchHit { bead_id, kind, snippet, rank }`.
- Consumes (Phase 3): `cmd::create::run(camp: &CampDir, title: String, rig: Option<String>, description: Option<String>, needs: Vec<String>, labels: Vec<String>, bead_type: Option<String>, assignee: Option<String>) -> Result<()>` — prints the allocated bead id.
- Produces: `camp search <query> [--limit N]` (default 20), `camp remember <fact> [--rig r]`, `camp recall <query> [--limit N]`. Hit output format, one line per hit: `<bead_id>\t<kind>\t<snippet>` (same tab-separated style as `camp ls`). No hits → empty stdout, exit 0. Domain errors → `camp: …` on stderr, exit 1 (existing `main` behavior).

- [ ] **Step 1: Write the failing CLI tests**

Create `crates/camp/tests/cli_search.rs`:

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
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
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

/// The worker-skill contract: remember a fact at close, recall it in the
/// next session. Memory is beads (bead.created, type=memory), so the
/// ledger must also stay refold-exact afterwards.
#[test]
fn remember_recall_round_trip_stays_refold_clean() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["remember", "the staging deploy needs the legacy token"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));

    camp()
        .current_dir(dir.path())
        .args(["recall", "staging deploy"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1\tbody\t"))
        .stdout(predicates::str::contains("staging deploy"));

    // The memory bead is a real bead: type=memory, visible via show.
    camp()
        .current_dir(dir.path())
        .args(["show", "gc-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("type     memory"));

    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
}

#[test]
fn recall_filters_to_memory_but_search_sees_everything() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "upgrade tokio to 2.0"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args(["remember", "tokio upgrade blocked on the tracing crate"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-2\n"));

    // recall: only the memory bead.
    let recall = camp()
        .current_dir(dir.path())
        .args(["recall", "tokio"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-2"))
        .get_output()
        .stdout
        .clone();
    assert!(
        !String::from_utf8(recall).unwrap().contains("gc-1"),
        "recall must not surface non-memory beads"
    );

    // search: both.
    camp()
        .current_dir(dir.path())
        .args(["search", "tokio"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1"))
        .stdout(predicates::str::contains("gc-2"));
}

#[test]
fn close_notes_are_searchable_from_the_cli() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["create", "chase the flaky test"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--reason",
            "root cause was a stale dispatcher cache",
        ])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["search", "dispatcher"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1\tclose\t"));
}

/// Rig scoping (master-plan test obligation): memories land in the rig
/// they were remembered against — per-rig id prefix and beads.rig — and
/// with several rigs configured, remember requires --rig (same rule as
/// create).
#[test]
fn remember_scopes_memories_to_the_named_rig() {
    let dir = camp_with_rig();
    let second = dir.path().join("toolbox");
    std::fs::create_dir_all(&second).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&second)
        .args(["--prefix", "tb", "--name", "toolbox"])
        .assert()
        .success();

    camp()
        .current_dir(dir.path())
        .args(["remember", "gascity pins the gc compiler ref", "--rig", "gascity"])
        .assert()
        .success()
        .stdout(predicates::str::diff("gc-1\n"));
    camp()
        .current_dir(dir.path())
        .args(["remember", "toolbox releases cut from main", "--rig", "toolbox"])
        .assert()
        .success()
        .stdout(predicates::str::diff("tb-1\n"));

    // Ambiguous rig fails fast, exactly like create.
    camp()
        .current_dir(dir.path())
        .args(["remember", "an orphan fact"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("multiple rigs configured"));

    // Both memories are recallable; the rig is on the bead row.
    camp()
        .current_dir(dir.path())
        .args(["recall", "toolbox OR gascity"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gc-1"))
        .stdout(predicates::str::contains("tb-1"));
    camp()
        .current_dir(dir.path())
        .args(["ls", "--rig", "toolbox"])
        .assert()
        .success()
        .stdout(predicates::str::contains("tb-1"));
}

#[test]
fn malformed_fts_query_is_a_clean_exit_1() {
    let dir = camp_with_rig();
    for verb in ["search", "recall"] {
        let assert = camp()
            .current_dir(dir.path())
            .args([verb, "("])
            .assert()
            .failure()
            .code(1)
            .stderr(predicates::str::contains("invalid search query"));
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            !stderr.contains("panicked"),
            "{verb} must fail cleanly, got: {stderr}"
        );
    }
}

#[test]
fn no_hits_is_success_with_empty_output() {
    let dir = camp_with_rig();
    camp()
        .current_dir(dir.path())
        .args(["search", "zeppelin"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn search_limit_caps_output_lines() {
    let dir = camp_with_rig();
    for i in 1..=3 {
        camp()
            .current_dir(dir.path())
            .args(["create", &format!("widget number {i}")])
            .assert()
            .success();
    }
    let out = camp()
        .current_dir(dir.path())
        .args(["search", "widget", "--limit", "2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 2);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp --test cli_search 2>&1 | tail -20`
Expected: every test FAILS — clap rejects the unknown subcommands (`error: unrecognized subcommand 'remember'` on stderr, exit 2), so the `.success()`/`.code(1)` assertions fail. Do not proceed if anything passes except compilation.

- [ ] **Step 3: Implement the three verbs**

3a. Create `crates/camp/src/cmd/search.rs`:

```rust
use anyhow::Result;
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp search <query> [--limit N]`: ranked full-text search over
/// everything, all time (spec §7.4). One line per hit:
/// `<bead_id>\t<kind>\t<snippet>`; no hits prints nothing and exits 0.
pub fn run(camp: &CampDir, query: &str, limit: usize) -> Result<()> {
    run_filtered(camp, query, None, limit)
}

/// Shared engine for `search` (unfiltered) and `recall` (memory only).
pub fn run_filtered(
    camp: &CampDir,
    query: &str,
    type_filter: Option<&str>,
    limit: usize,
) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    for hit in ledger.search(query, type_filter, limit)? {
        // Snippets can span the fold's title'\n'description boundary; the
        // output is one line per hit, so flatten embedded line breaks.
        // (Added during execution: the limit test caught 2 output lines per
        // hit because FTS5 snippets carry the stored newline through.)
        let snippet = hit.snippet.replace(['\n', '\r'], " ");
        println!("{}\t{}\t{}", hit.bead_id, hit.kind, snippet.trim());
    }
    Ok(())
}
```

3b. Create `crates/camp/src/cmd/recall.rs`:

```rust
use anyhow::Result;

use crate::campdir::CampDir;

/// `camp recall <query> [--limit N]`: `camp search` narrowed to memory
/// beads — the read half of the worker skill's recall-before /
/// remember-after contract (spec §7.4).
pub fn run(camp: &CampDir, query: &str, limit: usize) -> Result<()> {
    crate::cmd::search::run_filtered(camp, query, Some("memory"), limit)
}
```

3c. Create `crates/camp/src/cmd/remember.rs`:

```rust
use anyhow::Result;

use crate::campdir::CampDir;

/// `camp remember "<fact>" [--rig r]`: persistent memory is a bead —
/// `bead.created` with `type='memory'`, title = the fact (spec §7.4). This
/// reuses the create path wholesale: same rig resolution, same per-rig id
/// allocation, same fold-written FTS row; prints the new bead id.
pub fn run(camp: &CampDir, fact: String, rig: Option<String>) -> Result<()> {
    crate::cmd::create::run(
        camp,
        fact,
        rig,
        None,
        Vec::new(),
        Vec::new(),
        Some("memory".to_owned()),
        None,
    )
}
```

3d. Wire `crates/camp/src/main.rs` (additive edits only — this file is a shared conflict zone with Phases 5 and 7):

In the `mod cmd` block, keep alphabetical order:

```rust
mod cmd {
    pub mod claim;
    pub mod close;
    pub mod create;
    pub mod doctor;
    pub mod events;
    pub mod init;
    pub mod ls;
    pub mod recall;
    pub mod remember;
    pub mod rig;
    pub mod search;
    pub mod show;
}
```

Append three variants to `enum Command` (after `Show`):

```rust
    /// Ranked full-text search over everything, all time
    Search {
        /// FTS5 query (bare terms AND; "quoted phrase"; prefix*)
        query: String,
        /// Maximum number of hits
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Store a persistent memory (a memory-type bead; title = the fact)
    Remember {
        /// The fact to remember
        fact: String,
        /// Rig (default: the only configured rig)
        #[arg(long)]
        rig: Option<String>,
    },
    /// Search memories only
    Recall {
        /// FTS5 query (bare terms AND; "quoted phrase"; prefix*)
        query: String,
        /// Maximum number of hits
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
```

Append three arms to the `match cli.command` in `fn run` (after the `Show` arm):

```rust
        Command::Search { query, limit } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::search::run(&camp, &query, limit)
        }
        Command::Remember { fact, rig } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::remember::run(&camp, fact, rig)
        }
        Command::Recall { query, limit } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::recall::run(&camp, &query, limit)
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p camp --test cli_search`
Expected: all 7 tests pass.

Then the full workspace: `cargo test --workspace`
Expected: everything green (core suite, refold property test, vocab pins, all existing CLI suites untouched and passing).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/search.rs crates/camp/src/cmd/recall.rs crates/camp/src/cmd/remember.rs crates/camp/src/main.rs crates/camp/tests/cli_search.rs
git commit -m "feat: camp search, remember, recall verbs"
```

---

### Task 3: Gates, push, PR

**Files:** none (verification and delivery only).

- [ ] **Step 1: Run the full gate battery**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Expected: all three exit 0. Fix anything that fails and re-run all three; never push red.

- [ ] **Step 2: Sanity-check the exit criteria by hand**

In a scratch directory (not the repo): `camp init`, `rig add`, then `camp remember "campfire fact" && camp recall campfire && camp search "("`; confirm the recall hit, the clean `invalid search query` error with exit 1 (`echo $?`), and `camp doctor --refold` reporting 0 drift rows.

- [ ] **Step 3: Rebase check, push, open the PR**

```bash
git fetch origin main
git rebase origin/main   # re-run the Step 1 gates if the rebase picked up anything
git push -u origin phase-4-search-memory
gh pr create --title "Phase 4: search and memory" --body "$(cat <<'EOF'
Spec §7.4 query surface: ranked FTS5 search plus bd-style persistent memory.

- camp-core/src/search.rs: search(conn, query, type_filter, limit) -> Vec<SearchHit>
  ranked by bm25(search); SearchHit { bead_id, kind, snippet, rank }.
- camp search <query> — unfiltered, all time; camp recall <query> — memory beads only;
  camp remember "<fact>" [--rig r] — bead.created with type='memory' via the existing
  create path (memory is beads, not a new table).
- Malformed FTS5 queries surface as CoreError::InvalidSearchQuery — clean exit 1, never
  a panic, nothing silenced.
- No new events, no fold/schema/vocab changes: memory rides bead.created, and every
  search row was already written through the fold (Phase 1), so refold stays exact —
  re-verified by doctor --refold in the CLI tests.

Tests: remember→recall round trip (+refold clean); ranking sanity (adjacent terms
outrank scattered, phrase query excludes non-adjacent); close-note search; rig scoping
(per-rig prefixes, ambiguous-rig fail-fast); malformed query → clean error for search
and recall; limit caps; empty result is success.
EOF
)"
gh pr checks --watch
```

Expected: CI green. Work is not complete until it is.

- [ ] **Step 4: Report to the team lead**

PR number, CI status, and the master-plan exit criteria quoted line by line with evidence:
- "worker skill's `recall before / remember after` contract has its verbs" → `camp remember` / `camp recall` shipped with the round-trip test named above.
- "CI green" → `gh pr checks` output.

---

## Self-review notes

- **Spec coverage:** §7.4 verbs match the spec's names and semantics verbatim; no spec divergence found, so no spec edit rides in this PR. The master plan's five test obligations map to: `exact_phrase_outranks_scattered_terms` (ranking sanity), `remember_recall_round_trip_stays_refold_clean` (round trip), `close_note_content_is_searchable` + `close_notes_are_searchable_from_the_cli` (close notes), `remember_scopes_memories_to_the_named_rig` (rig scoping), `malformed_fts_queries_are_clean_domain_errors` + `malformed_fts_query_is_a_clean_exit_1` (clean error).
- **Type consistency:** `Ledger::search` (Task 1 Produces) is what `cmd/search.rs` calls (Task 2 Consumes); `create::run`'s 8-parameter signature copied verbatim from `create.rs:13-22`.
- **`--limit` flag:** additive CLI convenience over the contract's `limit: usize` parameter (default 20); the contract names no flag, and a hard-coded limit would leave the parameter untestable from the CLI.
- **Deliberate non-goals:** no `--json` output (not in the contract), no dedupe across body/close hits (`kind` distinguishes them), no rig filter on `search()` (contract fixes the signature; `remember --rig` is where rig scoping lives).

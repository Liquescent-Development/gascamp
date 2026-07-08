# Phase 14 — Export Bridge Implementation Plan

> **Plan approval:** APPROVED 2026-07-07 by the automated Opus plan review (verified against gascity at the pinned ref AND beadslib v1.0.4 in the module cache), relayed by the team lead. Rulings: fully approved — D1–D6 and D8–D11 accepted (D11's targeted ruling verified against merged Phase 8 that nothing the export ships references agents by name, so the camp-local-layer-only rule can never dangle); D7 ADOPTED by the operator (relayed 2026-07-07) — the spec §15.3 amendment lands in this PR per Task 8 Step 2. Incremental reviewer notes folded in: export.md's "What is not exported" carries D11's external-pack-layer exclusion; D3's reason reads "no key binds an order to a specific named rig (gc's scope key selects city-vs-rig instantiation)"; D8's rationale avoids asserting bd's absent-field behavior as fact. The reviewer's five non-blocking notes are folded into this doc and the execution (beadslib v1.0.4 provenance, staleness-guard softening, D3 named-rig wording, serde_json key-order comment near `bd_record`, D8 phrasing in export.md).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `camp export --city <dir>` (spec §15.3): emit a directory a Gas City operator imports with standard tooling — `beads.jsonl` in the real bd import wire format, the pinned formulas from `runs/`, and a gc-convention pack (agents verbatim, generated `pack.toml`, camp orders translated to gc order TOML). Graduation is an export, not a backend; camp never writes into a live city's store, and export appends nothing to camp's own ledger either.

**Architecture:** One new camp-core module (`crates/camp-core/src/export.rs`) holds all pure logic: a full-fidelity bead query (delegated from `Ledger`, same pattern as `readiness`), bd wire-format record types (serialize-only serde structs), order translation, and the `export_city` orchestration that writes the output tree. The `camp` binary adds a thin `cmd/export.rs` + one clap variant. Order translation runs before any file is written, so the contract's untranslatable-order failure leaves no partial output.

**Tech Stack:** Rust (edition 2024), rusqlite, serde/serde_json/toml (all already camp-core deps — no new dependencies), clap derive + anyhow in `camp`, assert-free golden comparison with `tempfile` fixtures.

## Global Constraints

- Never commit to main; all work on `phase-14-export-bridge`; no co-author lines in commits (AGENTS.md).
- TDD strictly: write the failing test, run it, watch it fail, implement, watch it pass (AGENTS.md).
- No panics in library code — workspace lints deny `clippy::unwrap_used`, `expect_used`, `panic`; test files opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (AGENTS.md invariant 5).
- Fail fast, no fallbacks, no silenced errors (AGENTS.md invariant 5).
- Gates before push: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` (AGENTS.md).
- Spec and code never silently diverge — §15.3's wording is amended in this PR (Task 8, decision D7).
- Vocabulary mirror (AGENTS.md invariant 7): `gc.outcome` / `gc.final_disposition` metadata use camp's vocab values verbatim (`crates/camp-core/src/vocab.rs`; camp's values are strict subsets of the gc pin in `crates/camp-core/tests/fixtures/gc-vocab.json`).
- Timestamps are RFC3339 UTC whole seconds (`%Y-%m-%dT%H:%M:%SZ`), already the ledger format (plan decision 9).

## Research Provenance

All Gas City facts below were extracted from gascity at the pinned ref `12410301884b51131a35e101a335dbaae16cdcb0` (== `ci/gc-compat/GASCITY_REF` == the gc-vocab.json provenance):

- **There are TWO bead wire formats — do not conflate them.** `docs/reference/exec-beads-provider.md` + gc's `internal/beads/beads.go` describe gc's *internal* pluggable-store RPC (`parent`, `needs`, `ref`, `from` fields). The format `bd import` actually reads is the external beads library's `types.Issue` (beadslib **v1.0.4** at the pin — corrected by the plan review's module-cache verification). `beads.jsonl` targets `types.Issue`. Emitting gc's internal shape would import but silently drop `parent`/`needs`/`ref`/`from` — `bd import` uses plain `encoding/json` and **silently ignores unknown fields**.
- `bd import` reads one JSON object per line; **only `title` is required**; `status` defaults to `open`, `issue_type` to `task`; `metadata` is an arbitrary JSON object preserved **verbatim**; upsert keyed by `id`. (A claimed `updated_at` staleness guard could not be confirmed at v1.0.4 — not relied upon anywhere in this phase.) beadslib validation requires `closed_at` present **iff** status is `closed` — camp satisfies this by construction (the fold sets `closed_ts` exactly on close), and export.md documents the import-rejection hazard for hand-edited files.
- `types.Issue` has **no** `needs`/`parent` field: blocking edges go in a `dependencies` array of `{"issue_id","depends_on_id","type"}`; `blocks` is in bd's blocking-for-ready set.
- `priority` (int 0–4) has **no omitempty** in bd's own export and `0` means P0/critical; camp has no priority concept, so camp emits an explicit `2` (normal) on every issue line.
- **Native memory support exists in bd import** as a separate record kind, not an issue type: `{"_type":"memory","key":"<slug>","value":"<content>"}` lines are stored as `bd remember` KV entries. Issue lines may carry `"_type":"issue"` (bd's own export does); absence also means issue.
- bd `status` values: `open`, `in_progress`, `blocked`, `deferred`, `closed`, `pinned`, `hooked` — camp's three are a 1:1 subset. bd built-in `issue_type` values include `task`, `bug`, `feature`, `epic`, `message`, … — there is **no** `memory` issue type.
- gc outcome metadata keys (`internal/beadmeta/keys.go`, values in `values.go`): `gc.outcome` (`pass|fail|skipped|missing_root`), `gc.final_disposition` (`pass|hard_fail|soft_fail|controller_error|orphaned_workflow|control_quarantined`), plus `gc.work_outcome`, `gc.outcome_bead_id`. Disposition lives in the bead `metadata` map — no dedicated column.
- gc orders (`internal/orders/order.go`): **one file per order** at `orders/<name>.toml`, wrapped in an `[order]` table; the order **name comes from the filename**; keys: `trigger` (required; `cron|event|cooldown|condition|manual`), `schedule` (required for cron), `on` (required for event), `formula` XOR `exec`, and others camp does not emit. **Orders cannot be declared inside pack.toml** (`PackConfig` has no orders field).
- gc `pack.toml` (`internal/config/pack.go`): `[pack]` table with `name` (required), `schema` (required, current = 2), optional `version`/`description`. Agents, formulas, and orders are discovered by convention from `<packdir>/agents/`, `<packdir>/formulas/`, `<packdir>/orders/` — never enumerated in the manifest.

Camp-side facts (verified against merged main at `bb279b6`, this branch's base):

- `beads` table columns: `id, rig, type, title, description, status, assignee, claimed_by, outcome, close_reason, labels (JSON array string), run_id, step_id, created_ts, updated_ts, closed_ts`; `deps(bead_id, needs_id)`; bead types `task|mail|memory` (`BEAD_TYPES`, fold.rs); outcomes `pass|fail`; memory beads are ordinary beads with `type='memory'`, title = the fact.
- `BeadRow` does **not** expose `description`, `close_reason`, `closed_ts`, `run_id`, `step_id`, or `needs`, and `Ledger.conn` is private → Phase 14 adds `Ledger::export_beads()` (Task 1) following the exact `readiness` delegation pattern.
- Orders: `OrderConfig { name, on, formula, rig: Option, catch_up_window: Option }` (raw `[[order]]`), `compile_orders(&CampConfig) -> Vec<Order>` where `Order { name, trigger: Trigger, formula, rig, catch_up_window: Duration }`, `Trigger::Cron { expr } | Trigger::Event { event_type, label: Option }`; `CronExpr::source()` returns the raw cron string.
- Runs: `<camp>/runs/<run-id>/` contains the **verbatim pinned formula copy** `<formula-name>.toml` plus `manifest.json`; run-id = `{compact-ts}-{6 hex, random}` — lexicographic order is chronological, and the random suffix means golden tests must normalize run ids.
- `<camp>/agents/` is Phase 8's surface (merged as `bb279b6`, PR #14): Claude Code agent-definition markdown (YAML frontmatter + prompt; `pack.rs::parse_agent_file` reads them verbatim — zero invented formats). Export treats the directory as an opaque tree to copy and never parses agent files (invariant 4 — no role knowledge in export code). Phase 8 also added `CampConfig.packs`/`dispatch`/`root` fields and a `CoreError::Pack` variant; none intersect this phase's surfaces (`export_city` takes an explicit `camp_root`, so `config.root` is not consumed). Phase 8 left the `beads` schema untouched — the Task 1 SELECT is verified against merged main.
- No merged phase records a final disposition anywhere (the `bead.closed` payload is `{outcome, reason?}`); Phase 9 (unmerged, not among this phase's dependencies) will. See D6.

## Flagged Decisions (for plan review)

- **D1 — memory beads → native bd memory records.** The contract said `issue_type:"task"` + label `camp-memory` "unless your research finds a native memory type in bd import" — it did (the `{"_type":"memory",...}` record kind above), so the native route is taken: `key` = bead id, `value` = title (the fact). bd memories are KV — rig/timestamps/status of memory beads are not representable and are documented as such.
- **D2 — mail beads → `issue_type:"message"`.** The contract is silent on camp's third bead type. bd has a native `message` type; the vocabulary-mirror principle (match gc verbatim where the concept exists) says use it rather than flatten to `task`.
- **D3 — orders with `rig` or `catch_up_window` set are untranslatable.** Plan decision 8's principle is "failing fast on untranslatable orders" with `[label=…]` as the *example*. gc order TOML has a `scope = "city"|"rig"` key but **no named-rig binding** (which rig an order runs in comes from pack placement), and no catch-up key exists at all — so silently dropping either field would hide declared behavior. All three cases fail the export listing name + reason; `--skip-untranslatable` opts out per order. (Reason wording per plan-review note 3.)
- **D4 — `pack/formulas/` addition.** A translated order references its formula by name and gc discovers pack formulas at `<packdir>/formulas/` — without them the exported pack imports but cannot run. The exporter copies each exported order's **authored** formula (`<camp>/formulas/<name>.toml`) into `pack/formulas/`; a missing authored file is a hard error naming the order. Additive to the contract's pack/ list, required by the exit criterion ("operator could import ... with standard tooling").
- **D5 — divergent pinned copies are archived per-run, never flattened, never fatal.** `formulas/` (top level) = the pinned copies from `runs/` per the contract. A formula edited between runs pins different bytes under the same name — a healthy history that must not fail the export. Rule: newest run's copy takes `formulas/<name>.toml`; an older run's copy that differs is written as `formulas/<name>.<run-id>.toml` with a note in the report. Identical copies dedupe. Deterministic, lossless (invariant 3).
- **D6 — `gc.final_disposition` is defined by the mapping but never emitted by this phase.** No merged phase records one (Phase 9 will, and it is not a dependency of Phase 14). The mapping table pins the key and its legal values (camp's `hard_fail|soft_fail`, a subset of gc's set); the exporter emits it only when a source exists, which today is never. The golden fixture asserts its absence.
- **D7 — spec §15.3 wording amendment (same PR).** The spec says orders are "translated into the wrapper as city-order declarations"; gc's `PackConfig` cannot declare orders — they are separate `orders/<name>.toml` files. Implementation reality contradicts the wording, so §15.3 is amended in this change (AGENTS.md: spec and code never silently diverge).
- **D8 — every issue line carries `"priority":2` explicitly** (bd semantics: field not omitted, `0` = critical).
- **D9 — camp-specific provenance rides in additive `camp.*` metadata keys**: `camp.rig` (always), `camp.claimed_by`, `camp.run_id`, `camp.step_id` (when set). Additive names, never redefinitions (invariant 7).
- **D10 — export is read-only.** Like `ls`/`show`/`search`, it appends no event and needs no vocab addition. A test pins that the event count is unchanged by an export.
- **D11 — export ships the camp-local `agents/` layer only.** Phase 8 (merged) layers agent resolution across configured `[packs]` directories with `<camp>/agents/` highest. Spec §15.3(c) exports "the pack's agent definitions" — the camp's own; agent definitions contributed by external pack directories already exist as packs on the operator's disk and are not re-exported. Matches this phase's kickoff scope (`<camp>/agents/` consumed read-only, verbatim).

## Field-Level Mapping Table (bd wire)

This table also lands verbatim in `docs/reference/export.md` (Task 8).

| camp (`beads` table) | `beads.jsonl` (beadslib `types.Issue` JSON tag) | Rule |
|---|---|---|
| — | `_type` | literal `"issue"` on issue lines (matches `bd export`; absence also accepted) |
| `id` | `id` | verbatim (`{prefix}-{n}`, e.g. `gc-142`) |
| `title` | `title` | verbatim (the only field bd requires) |
| `description` | `description` | verbatim; omitted when empty |
| `status` (`open`/`in_progress`/`closed`) | `status` | 1:1 verbatim (camp's values are a subset of bd's) |
| `type = "task"` | `issue_type: "task"` | |
| `type = "mail"` | `issue_type: "message"` | bd native message type (D2) |
| `type = "memory"` | *not an issue line* | `{"_type":"memory","key":"<bead id>","value":"<title>"}` (D1) |
| — | `priority` | literal `2` on every issue line (D8) |
| `assignee` | `assignee` | omitted when NULL |
| `claimed_by` | `metadata."camp.claimed_by"` | when set (D9) |
| `rig` | `metadata."camp.rig"` | always (D9) |
| `run_id` / `step_id` | `metadata."camp.run_id"` / `metadata."camp.step_id"` | when set (D9) |
| `outcome` (`pass`/`fail`) | `metadata."gc.outcome"` | when set; camp values are valid gc vocabulary |
| *(no source until Phase 9)* | `metadata."gc.final_disposition"` | defined, never emitted by this phase (D6); legal values `hard_fail`/`soft_fail` |
| `close_reason` | `close_reason` | when set |
| `closed_ts` | `closed_at` | RFC3339, when set |
| `created_ts` | `created_at` | RFC3339 |
| `updated_ts` | `updated_at` | RFC3339 |
| `labels` | `labels` | verbatim array; omitted when empty |
| `deps(bead_id, needs_id)` | `dependencies: [{"issue_id":<bead>,"depends_on_id":<needs>,"type":"blocks"}]` | camp `needs` is a readiness-blocking edge → bd `blocks` |

Issue lines are emitted in creation order (`ORDER BY created_ts, id` — the `list_beads` ordering); memory records interleave at their creation position. All metadata values are JSON strings.

## Order Translation Table

Also lands in `docs/reference/export.md`.

| camp `[[order]]` (camp.toml) | gc `pack/orders/<name>.toml` |
|---|---|
| `name = "x"` | filename `x.toml` (gc: name comes from the filename) |
| `formula = "f"` | `formula = "f"` (+ authored `f` copied to `pack/formulas/f.toml`, D4) |
| `on = "cron:EXPR"` | `trigger = "cron"` + `schedule = "EXPR"` |
| `on = "event:TYPE"` | `trigger = "event"` + `on = "TYPE"` |
| `on = "event:TYPE[label=X]"` | **untranslatable** (gc event orders have no label filter) |
| `rig = "r"` | **untranslatable** (gc's `scope` key is `city`\|`rig` with no named-rig binding — placement picks the rig, D3) |
| `catch_up_window = "…"` | **untranslatable** (no gc key, D3) |

Untranslatable orders fail the export with every offender listed (name + reason); `--skip-untranslatable` exports without them, naming each skip on stderr. Translation runs before any output is written.

## Output Layout

```
<dir>/
  beads.jsonl               # issue + memory records, one JSON object per line
  formulas/                 # archive: pinned copies from runs/ (contract)
    <name>.toml             #   newest run's pinned copy
    <name>.<run-id>.toml    #   older divergent pinned copies (D5)
  pack/
    pack.toml               # [pack] name = <camp name>, schema = 2, description
    agents/                 # verbatim recursive copy of <camp>/agents/
    formulas/<name>.toml    # authored formulas referenced by exported orders (D4)
    orders/<name>.toml      # translated orders, [order] table each
```

All directories are created even when empty, so the output shape is deterministic. The output directory must not exist non-empty (hard error). On an I/O error mid-write the directory may be partial and is safe to delete; the untranslatable-order failure specifically happens before any write.

## File Structure

- Create: `crates/camp-core/src/export.rs` — ExportBead + query, bd record types + mapping, order translation, `export_city`.
- Modify: `crates/camp-core/src/lib.rs` — `pub mod export;`.
- Modify: `crates/camp-core/src/error.rs` — `Export(String)`, `UntranslatableOrders { count, details }` variants.
- Modify: `crates/camp-core/src/ledger/mod.rs` — `Ledger::export_beads()` delegation.
- Create: `crates/camp-core/tests/export_city.rs` — orchestration behavior tests.
- Create: `crates/camp-core/tests/export_golden.rs` + `crates/camp-core/tests/fixtures/export-golden/**` — golden tree.
- Create: `crates/camp/src/cmd/export.rs`; Modify: `crates/camp/src/main.rs` — CLI.
- Create: `crates/camp/tests/cli_export.rs` — CLI integration + `#[ignore]` bd check.
- Create: `docs/reference/export.md`; Modify: `docs/design/2026-07-05-gas-camp-design.md` §15.3 (D7).

---

### Task 1: `ExportBead` + `Ledger::export_beads()`

**Files:**
- Create: `crates/camp-core/src/export.rs`
- Modify: `crates/camp-core/src/lib.rs`
- Modify: `crates/camp-core/src/ledger/mod.rs`

**Interfaces:**
- Consumes: `Ledger` (private `conn`, same-crate delegation pattern as `readiness`), `CoreError`.
- Produces: `pub struct ExportBead { id, rig, kind, title, description, status, assignee, claimed_by, outcome, close_reason, labels, run_id, step_id, needs, created_ts, updated_ts, closed_ts }`; `pub fn Ledger::export_beads(&self) -> Result<Vec<ExportBead>, CoreError>`; crate-internal `export::export_beads(&Connection)`.

- [ ] **Step 1: Write the failing test** — new module with inline tests. Create `crates/camp-core/src/export.rs`:

```rust
//! `camp export --city <dir>` (spec §15.3): graduation is an export, not a
//! backend. Everything here is read-only — over the ledger and the camp
//! directory. Camp never writes into a live city's store, and export
//! appends nothing to camp's own ledger. Field-level mapping tables:
//! docs/reference/export.md.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::error::CoreError;

/// One bead with every column `beads.jsonl` needs — the full-fidelity
/// superset of [`crate::readiness::BeadRow`] plus the `needs` edges from
/// `deps`. Creation order (`ORDER BY created_ts, id`), read-only.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportBead {
    pub id: String,
    pub rig: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub assignee: Option<String>,
    pub claimed_by: Option<String>,
    pub outcome: Option<String>,
    pub close_reason: Option<String>,
    pub labels: Vec<String>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub needs: Vec<String>,
    pub created_ts: String,
    pub updated_ts: String,
    pub closed_ts: Option<String>,
}

pub(crate) fn export_beads(conn: &Connection) -> Result<Vec<ExportBead>, CoreError> {
    let mut needs_by_bead: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dep_stmt =
        conn.prepare("SELECT bead_id, needs_id FROM deps ORDER BY bead_id, needs_id")?;
    let dep_rows =
        dep_stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in dep_rows {
        let (bead_id, needs_id) = row?;
        needs_by_bead.entry(bead_id).or_default().push(needs_id);
    }

    let mut stmt = conn.prepare(
        "SELECT id, rig, type, title, description, status, assignee, claimed_by,
                outcome, close_reason, labels, run_id, step_id, created_ts,
                updated_ts, closed_ts
         FROM beads ORDER BY created_ts, id",
    )?;
    let rows = stmt.query_map([], |row| {
        let labels_json: String = row.get(10)?;
        let labels: Vec<String> = serde_json::from_str(&labels_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?;
        Ok(ExportBead {
            id: row.get(0)?,
            rig: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
            status: row.get(5)?,
            assignee: row.get(6)?,
            claimed_by: row.get(7)?,
            outcome: row.get(8)?,
            close_reason: row.get(9)?,
            labels,
            run_id: row.get(11)?,
            step_id: row.get(12)?,
            needs: Vec::new(),
            created_ts: row.get(13)?,
            updated_ts: row.get(14)?,
            closed_ts: row.get(15)?,
        })
    })?;
    let mut beads = Vec::new();
    for row in rows {
        let mut bead = row?;
        if let Some(needs) = needs_by_bead.remove(&bead.id) {
            bead.needs = needs;
        }
        beads.push(bead);
    }
    Ok(beads)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    pub(crate) const TS: &str = "2026-07-05T21:14:03Z";

    pub(crate) fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger =
            Ledger::open_with_clock(&dir.path().join("camp.db"), Box::new(FixedClock::new(TS)))
                .unwrap();
        (dir, ledger)
    }

    pub(crate) fn append(
        ledger: &mut Ledger,
        kind: EventType,
        bead: &str,
        data: serde_json::Value,
    ) {
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

    /// gc-1 closed with outcome+reason after a claim; gc-2 open with
    /// description/needs/labels/assignee; gc-3 mail; gc-4 memory; gc-5
    /// with run/step provenance.
    pub(crate) fn seed(ledger: &mut Ledger) {
        append(
            ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "implement widget", "labels": ["cli"]}),
        );
        append(
            ledger,
            EventType::BeadClaimed,
            "gc-1",
            serde_json::json!({"session": "camp/dev/1"}),
        );
        append(
            ledger,
            EventType::BeadClosed,
            "gc-1",
            serde_json::json!({"outcome": "pass", "reason": "shipped the widget"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-2",
            serde_json::json!({
                "title": "review widget",
                "description": "the change",
                "needs": ["gc-1"],
                "labels": ["cli", "review"],
                "assignee": "reviewer"
            }),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-3",
            serde_json::json!({"title": "ping from ci", "type": "mail"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-4",
            serde_json::json!({"title": "deploy needs the VPN profile", "type": "memory"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-5",
            serde_json::json!({"title": "step one", "run_id": "20260705T211403Z-abc123", "step_id": "s1"}),
        );
    }

    #[test]
    fn export_beads_returns_full_fidelity_rows_in_creation_order() {
        let (_dir, mut ledger) = temp_ledger();
        seed(&mut ledger);

        let beads = ledger.export_beads().unwrap();
        assert_eq!(
            beads.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec!["gc-1", "gc-2", "gc-3", "gc-4", "gc-5"]
        );

        let b1 = &beads[0];
        assert_eq!(b1.status, "closed");
        assert_eq!(b1.kind, "task");
        assert_eq!(b1.rig, "gc");
        assert_eq!(b1.claimed_by.as_deref(), Some("camp/dev/1"));
        assert_eq!(b1.outcome.as_deref(), Some("pass"));
        assert_eq!(b1.close_reason.as_deref(), Some("shipped the widget"));
        assert_eq!(b1.closed_ts.as_deref(), Some(TS));
        assert_eq!(b1.labels, vec!["cli".to_owned()]);
        assert_eq!(b1.created_ts, TS);
        assert_eq!(b1.updated_ts, TS);

        let b2 = &beads[1];
        assert_eq!(b2.description, "the change");
        assert_eq!(b2.needs, vec!["gc-1".to_owned()]);
        assert_eq!(b2.assignee.as_deref(), Some("reviewer"));
        assert_eq!(b2.status, "open");
        assert_eq!(b2.outcome, None);
        assert_eq!(b2.closed_ts, None);

        assert_eq!(beads[2].kind, "mail");
        assert_eq!(beads[3].kind, "memory");

        let b5 = &beads[4];
        assert_eq!(b5.run_id.as_deref(), Some("20260705T211403Z-abc123"));
        assert_eq!(b5.step_id.as_deref(), Some("s1"));
    }
}
```

Register the module in `crates/camp-core/src/lib.rs` (after `pub mod event;`):

```rust
pub mod export;
```

And add the delegation in `crates/camp-core/src/ledger/mod.rs`, after `list_beads` (impl block):

```rust
    /// Full-fidelity bead rows for `camp export` (spec §15.3): every
    /// `beads` column plus the `needs` edges, in creation order.
    pub fn export_beads(&self) -> Result<Vec<crate::export::ExportBead>, CoreError> {
        crate::export::export_beads(&self.conn)
    }
```

To watch the test fail first, add the module + delegation but leave `export_beads`'s body as `Ok(Vec::new())` initially — the assertion on the id list fails.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p camp-core export_beads_returns_full_fidelity_rows -- --nocapture`
Expected: FAIL — `assertion ... left: []` (empty vec vs 5 ids).

- [ ] **Step 3: Fill in the real query body** (the `export_beads` implementation shown in Step 1).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p camp-core export_beads_returns_full_fidelity_rows`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/export.rs crates/camp-core/src/lib.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat(export): full-fidelity bead query for the export bridge"
```

---

### Task 2: bd wire records + JSONL mapping

**Files:**
- Modify: `crates/camp-core/src/export.rs`
- Modify: `crates/camp-core/src/error.rs`

**Interfaces:**
- Consumes: `ExportBead` (Task 1).
- Produces: `pub struct BdIssue`, `pub struct BdDependency`, `pub struct BdMemory`, `pub enum BdRecord { Issue(Box<BdIssue>), Memory(BdMemory) }`, `pub fn bd_record(&ExportBead) -> Result<BdRecord, CoreError>`, `pub fn jsonl_line(&BdRecord) -> Result<String, CoreError>`; `CoreError::Export(String)`.

- [ ] **Step 1: Add the `Export` error variant** to `crates/camp-core/src/error.rs` (after the `Order` variant):

```rust
    /// A `camp export` failure that is not an order-translation finding:
    /// bad output directory, unreadable inputs, malformed run dirs.
    #[error("export: {0}")]
    Export(String),
```

- [ ] **Step 2: Write the failing tests** — append to the `tests` module in `export.rs`:

```rust
    fn full_bead() -> ExportBead {
        ExportBead {
            id: "gc-1".into(),
            rig: "gc".into(),
            kind: "task".into(),
            title: "implement widget".into(),
            description: "the change".into(),
            status: "closed".into(),
            assignee: Some("dev".into()),
            claimed_by: Some("camp/dev/1".into()),
            outcome: Some("pass".into()),
            close_reason: Some("shipped the widget".into()),
            labels: vec!["cli".into()],
            run_id: None,
            step_id: None,
            needs: vec!["gc-0".into()],
            created_ts: TS.into(),
            updated_ts: TS.into(),
            closed_ts: Some(TS.into()),
        }
    }

    fn minimal_bead() -> ExportBead {
        ExportBead {
            id: "gc-2".into(),
            rig: "gc".into(),
            kind: "task".into(),
            title: "review".into(),
            description: String::new(),
            status: "open".into(),
            assignee: None,
            claimed_by: None,
            outcome: None,
            close_reason: None,
            labels: vec![],
            run_id: None,
            step_id: None,
            needs: vec![],
            created_ts: TS.into(),
            updated_ts: TS.into(),
            closed_ts: None,
        }
    }

    #[test]
    fn closed_task_maps_to_a_bd_issue_line_exactly() {
        let line = jsonl_line(&bd_record(&full_bead()).unwrap()).unwrap();
        assert_eq!(
            line,
            r#"{"_type":"issue","id":"gc-1","title":"implement widget","description":"the change","status":"closed","priority":2,"issue_type":"task","assignee":"dev","created_at":"2026-07-05T21:14:03Z","updated_at":"2026-07-05T21:14:03Z","closed_at":"2026-07-05T21:14:03Z","close_reason":"shipped the widget","labels":["cli"],"dependencies":[{"issue_id":"gc-1","depends_on_id":"gc-0","type":"blocks"}],"metadata":{"camp.claimed_by":"camp/dev/1","camp.rig":"gc","gc.outcome":"pass"}}"#
        );
    }

    #[test]
    fn open_minimal_task_omits_empty_fields_and_keeps_priority() {
        let line = jsonl_line(&bd_record(&minimal_bead()).unwrap()).unwrap();
        assert_eq!(
            line,
            r#"{"_type":"issue","id":"gc-2","title":"review","status":"open","priority":2,"issue_type":"task","created_at":"2026-07-05T21:14:03Z","updated_at":"2026-07-05T21:14:03Z","metadata":{"camp.rig":"gc"}}"#
        );
    }

    #[test]
    fn mail_maps_to_the_native_bd_message_type() {
        let mut bead = minimal_bead();
        bead.kind = "mail".into();
        let line = jsonl_line(&bd_record(&bead).unwrap()).unwrap();
        assert!(line.contains(r#""issue_type":"message""#), "{line}");
    }

    #[test]
    fn memory_maps_to_a_native_bd_memory_record() {
        let mut bead = minimal_bead();
        bead.id = "gc-4".into();
        bead.kind = "memory".into();
        bead.title = "deploy needs the VPN profile".into();
        let line = jsonl_line(&bd_record(&bead).unwrap()).unwrap();
        assert_eq!(
            line,
            r#"{"_type":"memory","key":"gc-4","value":"deploy needs the VPN profile"}"#
        );
    }

    #[test]
    fn run_and_step_provenance_ride_in_camp_metadata() {
        let mut bead = minimal_bead();
        bead.run_id = Some("20260705T211403Z-abc123".into());
        bead.step_id = Some("s1".into());
        let line = jsonl_line(&bd_record(&bead).unwrap()).unwrap();
        assert!(
            line.contains(r#""camp.run_id":"20260705T211403Z-abc123""#)
                && line.contains(r#""camp.step_id":"s1""#),
            "{line}"
        );
    }

    #[test]
    fn unknown_bead_type_is_an_export_error() {
        let mut bead = minimal_bead();
        bead.kind = "vibes".into();
        match bd_record(&bead) {
            Err(CoreError::Export(msg)) => assert!(msg.contains("vibes"), "{msg}"),
            other => panic!("expected Export error, got {other:?}"),
        }
    }
```

- [ ] **Step 3: Run to verify they fail to compile** (missing types):

Run: `cargo test -p camp-core closed_task_maps 2>&1 | head -20`
Expected: compile error — `bd_record` / `jsonl_line` not found.

- [ ] **Step 4: Implement the records + mapping** — add to `export.rs` (above the tests module):

```rust
/// One issue line of `beads.jsonl` — the bd import/export wire format
/// (beadslib `types.Issue`; the format `bd import` actually reads, NOT Gas
/// City's internal exec-provider shape, whose `parent`/`needs`/`ref`
/// fields bd silently drops). Serialize-only: camp emits, bd consumes.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BdIssue {
    /// bd's own export tags issue lines; absence also means issue.
    #[serde(rename = "_type")]
    pub record: &'static str,
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub status: String,
    /// bd priority 0 means P0/critical and the field carries no omitempty
    /// in bd's export — camp has no priority, so every line says 2 (normal).
    pub priority: i64,
    pub issue_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<BdDependency>,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// One `dependencies` entry: camp's `needs` edge is a readiness-blocking
/// dependency, bd's `blocks` type (in bd's blocking-for-ready set).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BdDependency {
    pub issue_id: String,
    pub depends_on_id: String,
    #[serde(rename = "type")]
    pub dep_type: &'static str,
}

/// A native bd memory record: `bd import` stores these as `bd remember`
/// KV entries, not issues. key = the camp bead id, value = the fact.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BdMemory {
    #[serde(rename = "_type")]
    pub record: &'static str,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BdRecord {
    Issue(Box<BdIssue>),
    Memory(BdMemory),
}

/// Map one camp bead to its `beads.jsonl` record. Field-level table:
/// docs/reference/export.md.
pub fn bd_record(bead: &ExportBead) -> Result<BdRecord, CoreError> {
    let issue_type = match bead.kind.as_str() {
        "memory" => {
            return Ok(BdRecord::Memory(BdMemory {
                record: "memory",
                key: bead.id.clone(),
                value: bead.title.clone(),
            }));
        }
        "task" => "task",
        "mail" => "message",
        other => {
            return Err(CoreError::Export(format!(
                "bead {} has unknown type {other:?}",
                bead.id
            )));
        }
    };
    let mut metadata = serde_json::Map::new();
    metadata.insert("camp.rig".into(), bead.rig.clone().into());
    if let Some(claimed_by) = &bead.claimed_by {
        metadata.insert("camp.claimed_by".into(), claimed_by.clone().into());
    }
    if let Some(run_id) = &bead.run_id {
        metadata.insert("camp.run_id".into(), run_id.clone().into());
    }
    if let Some(step_id) = &bead.step_id {
        metadata.insert("camp.step_id".into(), step_id.clone().into());
    }
    if let Some(outcome) = &bead.outcome {
        metadata.insert("gc.outcome".into(), outcome.clone().into());
    }
    let dependencies = bead
        .needs
        .iter()
        .map(|needs_id| BdDependency {
            issue_id: bead.id.clone(),
            depends_on_id: needs_id.clone(),
            dep_type: "blocks",
        })
        .collect();
    Ok(BdRecord::Issue(Box::new(BdIssue {
        record: "issue",
        id: bead.id.clone(),
        title: bead.title.clone(),
        description: bead.description.clone(),
        status: bead.status.clone(),
        priority: 2,
        issue_type,
        assignee: bead.assignee.clone(),
        created_at: bead.created_ts.clone(),
        updated_at: bead.updated_ts.clone(),
        closed_at: bead.closed_ts.clone(),
        close_reason: bead.close_reason.clone(),
        labels: bead.labels.clone(),
        dependencies,
        metadata,
    })))
}

/// One `beads.jsonl` line (no trailing newline).
pub fn jsonl_line(record: &BdRecord) -> Result<String, CoreError> {
    Ok(match record {
        BdRecord::Issue(issue) => serde_json::to_string(issue)?,
        BdRecord::Memory(memory) => serde_json::to_string(memory)?,
    })
}
```

- [ ] **Step 5: Run the Task 2 tests**

Run: `cargo test -p camp-core --lib export`
Expected: all Task 1 + Task 2 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/export.rs crates/camp-core/src/error.rs
git commit -m "feat(export): bd wire-format records and bead mapping"
```

---

### Task 3: Order translation

**Files:**
- Modify: `crates/camp-core/src/export.rs`

**Interfaces:**
- Consumes: `crate::orders::{Order, Trigger}`, `crate::orders::parse::OrderConfig`, `CronExpr::source()`.
- Produces: `pub struct GcOrderFile { order: GcOrder }`, `pub struct GcOrder { formula, trigger, schedule, on }`, `pub enum OrderTranslation { Translated { name, file }, Untranslatable { name, reason } }`, `pub fn translate_order(&Order, &OrderConfig) -> OrderTranslation`.

- [ ] **Step 1: Write the failing tests** — append to the tests module:

```rust
    /// Compile a camp.toml text and hand back (compiled, raw) order pairs.
    fn orders_from(toml_text: &str) -> Vec<(crate::orders::Order, crate::orders::parse::OrderConfig)> {
        let config = crate::config::CampConfig::parse(toml_text).unwrap();
        let compiled = crate::orders::parse::compile_orders(&config).unwrap();
        compiled.into_iter().zip(config.orders).collect()
    }

    const RIGGED: &str = r#"
[camp]
name = "golden"

[[rigs]]
name = "gc"
path = "/tmp/rig"
prefix = "gc"
"#;

    #[test]
    fn cron_order_translates_to_trigger_and_schedule() {
        let text = format!(
            "{RIGGED}\n[[order]]\nname = \"nightly\"\non = \"cron:0 7 * * 1-5\"\nformula = \"one-step\"\n"
        );
        let pairs = orders_from(&text);
        match translate_order(&pairs[0].0, &pairs[0].1) {
            OrderTranslation::Translated { name, file } => {
                assert_eq!(name, "nightly");
                assert_eq!(
                    toml::to_string(&file).unwrap(),
                    "[order]\nformula = \"one-step\"\ntrigger = \"cron\"\nschedule = \"0 7 * * 1-5\"\n"
                );
            }
            other => panic!("expected Translated, got {other:?}"),
        }
    }

    #[test]
    fn event_order_translates_to_trigger_and_on() {
        let text = format!(
            "{RIGGED}\n[[order]]\nname = \"on-close\"\non = \"event:bead.closed\"\nformula = \"one-step\"\n"
        );
        let pairs = orders_from(&text);
        match translate_order(&pairs[0].0, &pairs[0].1) {
            OrderTranslation::Translated { file, .. } => assert_eq!(
                toml::to_string(&file).unwrap(),
                "[order]\nformula = \"one-step\"\ntrigger = \"event\"\non = \"bead.closed\"\n"
            ),
            other => panic!("expected Translated, got {other:?}"),
        }
    }

    #[test]
    fn label_filter_rig_and_catch_up_window_are_untranslatable() {
        let text = format!(
            concat!(
                "{}\n",
                "[[order]]\nname = \"ci-red\"\non = \"event:bead.closed[label=ci-red]\"\nformula = \"fix-ci\"\n\n",
                "[[order]]\nname = \"rigged\"\non = \"cron:0 7 * * *\"\nformula = \"one-step\"\nrig = \"gc\"\n\n",
                "[[order]]\nname = \"caught-up\"\non = \"cron:0 7 * * *\"\nformula = \"one-step\"\ncatch_up_window = \"4h\"\n"
            ),
            RIGGED
        );
        let pairs = orders_from(&text);
        let reasons: Vec<(String, String)> = pairs
            .iter()
            .map(|(order, raw)| match translate_order(order, raw) {
                OrderTranslation::Untranslatable { name, reason } => (name, reason),
                other => panic!("expected Untranslatable, got {other:?}"),
            })
            .collect();
        assert_eq!(reasons[0].0, "ci-red");
        assert!(reasons[0].1.contains("label"), "{}", reasons[0].1);
        assert_eq!(reasons[1].0, "rigged");
        assert!(reasons[1].1.contains("rig"), "{}", reasons[1].1);
        assert_eq!(reasons[2].0, "caught-up");
        assert!(
            reasons[2].1.contains("catch_up_window"),
            "{}",
            reasons[2].1
        );
    }
```

- [ ] **Step 2: Run to verify compile failure**

Run: `cargo test -p camp-core --lib cron_order_translates 2>&1 | head -10`
Expected: compile error — `translate_order` / `OrderTranslation` not found.

- [ ] **Step 3: Implement translation** — add to `export.rs`:

```rust
use crate::orders::parse::OrderConfig;
use crate::orders::{Order, Trigger};

/// A gc `orders/<name>.toml` file: an `[order]` table. gc derives the
/// order's name from the FILENAME, so the name lives beside this struct,
/// not in it. Keys per gascity `internal/orders/order.go` at the pinned
/// ref: `trigger` (required), `schedule` (cron), `on` (event), `formula`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GcOrderFile {
    pub order: GcOrder,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GcOrder {
    pub formula: String,
    pub trigger: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
}

/// The outcome of translating one camp order (plan decision 8: an explicit
/// mapping table, failing fast on anything gc order TOML cannot express).
#[derive(Debug, Clone, PartialEq)]
pub enum OrderTranslation {
    Translated { name: String, file: GcOrderFile },
    Untranslatable { name: String, reason: String },
}

/// Translate one compiled camp order to gc order TOML. Translation table:
/// docs/reference/export.md. `raw` is the same order's `[[order]]` config,
/// needed because the compiled form defaults `catch_up_window` and would
/// hide whether the camp declared one.
pub fn translate_order(order: &Order, raw: &OrderConfig) -> OrderTranslation {
    let name = order.name.clone();
    if raw.catch_up_window.is_some() {
        return OrderTranslation::Untranslatable {
            name,
            reason: "catch_up_window has no gc order-TOML equivalent".to_owned(),
        };
    }
    if let Some(rig) = &order.rig {
        return OrderTranslation::Untranslatable {
            name,
            reason: format!(
                "rig {rig:?} cannot be expressed in gc order TOML (gc's scope key is city|rig with no named-rig binding; pack placement picks the rig)"
            ),
        };
    }
    match &order.trigger {
        Trigger::Cron { expr } => OrderTranslation::Translated {
            name,
            file: GcOrderFile {
                order: GcOrder {
                    formula: order.formula.clone(),
                    trigger: "cron",
                    schedule: Some(expr.source().to_owned()),
                    on: None,
                },
            },
        },
        Trigger::Event {
            event_type,
            label: None,
        } => OrderTranslation::Translated {
            name,
            file: GcOrderFile {
                order: GcOrder {
                    formula: order.formula.clone(),
                    trigger: "event",
                    schedule: None,
                    on: Some(event_type.clone()),
                },
            },
        },
        Trigger::Event {
            event_type,
            label: Some(label),
        } => OrderTranslation::Untranslatable {
            name,
            reason: format!(
                "event trigger {event_type:?} has a [label={label}] filter — gc event orders have no label filter"
            ),
        },
    }
}
```

- [ ] **Step 4: Run the Task 3 tests**

Run: `cargo test -p camp-core --lib export`
Expected: PASS (all export tests so far).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/export.rs
git commit -m "feat(export): camp-order to gc-order-TOML translation"
```

---

### Task 4: `export_city` orchestration

**Files:**
- Modify: `crates/camp-core/src/export.rs`
- Modify: `crates/camp-core/src/error.rs`
- Create: `crates/camp-core/tests/export_city.rs`

**Interfaces:**
- Consumes: everything above, `CampConfig`, `compile_orders`, `crate::orders::formula_path`.
- Produces:
  - `pub struct ExportOptions { pub skip_untranslatable: bool }`
  - `pub struct SkippedOrder { pub name: String, pub reason: String }`
  - `pub struct ExportReport { pub issues, pub memories, pub archive_formulas, pub pack_formulas, pub agents, pub orders: usize, pub skipped_orders: Vec<SkippedOrder>, pub notes: Vec<String> }`
  - `pub fn export_city(ledger: &Ledger, config: &CampConfig, camp_root: &Path, out_dir: &Path, options: &ExportOptions) -> Result<ExportReport, CoreError>`
  - `CoreError::UntranslatableOrders { count: usize, details: String }`

Sequencing inside `export_city` (load-bearing): (1) refuse a non-empty output dir; (2) translate ALL orders — the untranslatable failure happens **before any write**; (3) write `beads.jsonl`; (4) write `formulas/` (archive from `runs/`, D5 rule); (5) write `pack/` (pack.toml, agents copy, order files, authored formulas per order, D4).

- [ ] **Step 1: Add the error variant** to `error.rs` (after `Export`):

```rust
    /// Orders that cannot be expressed as gc order TOML (spec §15.3, plan
    /// decision 8). Listed in full; the flag named here is the contract's
    /// explicit opt-out.
    #[error(
        "export: {count} order(s) cannot be translated to gc order TOML:\n{details}\npass --skip-untranslatable to export without them"
    )]
    UntranslatableOrders { count: usize, details: String },
```

- [ ] **Step 2: Write the failing integration tests** — create `crates/camp-core/tests/export_city.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 14: export_city orchestration behavior (spec §15.3). The golden
//! byte-level test lives in export_golden.rs; this file pins the rules:
//! fail-before-write on untranslatable orders, the explicit skip, the
//! non-empty-dir refusal, the runs/ archive rules, and read-only-ness.

use std::path::{Path, PathBuf};

use camp_core::clock::FixedClock;
use camp_core::config::CampConfig;
use camp_core::error::CoreError;
use camp_core::event::{EventInput, EventType};
use camp_core::export::{ExportOptions, export_city};
use camp_core::ledger::Ledger;

const TS: &str = "2026-07-05T21:14:03Z";

const NO_SKIP: ExportOptions = ExportOptions {
    skip_untranslatable: false,
};
const SKIP: ExportOptions = ExportOptions {
    skip_untranslatable: true,
};

/// A camp root with a ledger (one closed bead, one memory), an authored
/// formula, an agents dir, and the given [[order]] tables.
fn fixture_camp(dir: &Path, orders_toml: &str) -> (PathBuf, Ledger, CampConfig) {
    let camp_root = dir.join(".camp");
    std::fs::create_dir_all(&camp_root).unwrap();
    let config_text = format!(
        "[camp]\nname = \"golden\"\n\n[[rigs]]\nname = \"gc\"\npath = {:?}\nprefix = \"gc\"\n{orders_toml}",
        dir.join("repo").display()
    );
    std::fs::write(camp_root.join("camp.toml"), &config_text).unwrap();
    let config = CampConfig::parse(&config_text).unwrap();

    std::fs::create_dir_all(camp_root.join("formulas")).unwrap();
    std::fs::write(
        camp_root.join("formulas/one-step.toml"),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(camp_root.join("agents")).unwrap();
    std::fs::write(camp_root.join("agents/dev.md"), "# dev agent\n").unwrap();

    let mut ledger =
        Ledger::open_with_clock(&camp_root.join("camp.db"), Box::new(FixedClock::new(TS)))
            .unwrap();
    for (bead, data) in [
        ("gc-1", serde_json::json!({"title": "implement widget"})),
        (
            "gc-2",
            serde_json::json!({"title": "deploy needs VPN", "type": "memory"}),
        ),
    ] {
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(bead.into()),
                data,
            })
            .unwrap();
    }
    ledger
        .append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"outcome": "pass", "reason": "done"}),
        })
        .unwrap();
    (camp_root, ledger, config)
}

const TRANSLATABLE_ORDERS: &str = r#"
[[order]]
name = "nightly"
on = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name = "on-close"
on = "event:bead.closed"
formula = "one-step"
"#;

const MIXED_ORDERS: &str = r#"
[[order]]
name = "nightly"
on = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name = "ci-red"
on = "event:bead.closed[label=ci-red]"
formula = "one-step"

[[order]]
name = "rigged"
on = "cron:0 8 * * *"
formula = "one-step"
rig = "gc"
"#;

#[test]
fn untranslatable_orders_fail_listing_every_one_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), MIXED_ORDERS);
    let out = dir.path().join("city");
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::UntranslatableOrders { count, details }) => {
            assert_eq!(count, 2);
            assert!(details.contains("ci-red") && details.contains("label"), "{details}");
            assert!(details.contains("rigged") && details.contains("rig"), "{details}");
        }
        other => panic!("expected UntranslatableOrders, got {other:?}"),
    }
    // fail-before-write: the output dir exists but holds nothing
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
fn skip_untranslatable_exports_without_the_offenders() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), MIXED_ORDERS);
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &SKIP).unwrap();
    assert_eq!(report.orders, 1);
    let skipped: Vec<&str> = report
        .skipped_orders
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(skipped, vec!["ci-red", "rigged"]);
    assert!(out.join("pack/orders/nightly.toml").exists());
    assert!(!out.join("pack/orders/ci-red.toml").exists());
    assert!(!out.join("pack/orders/rigged.toml").exists());
}

#[test]
fn a_non_empty_output_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), TRANSLATABLE_ORDERS);
    let out = dir.path().join("city");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("existing.txt"), "hello").unwrap();
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::Export(msg)) => assert!(msg.contains("non-empty"), "{msg}"),
        other => panic!("expected Export error, got {other:?}"),
    }
}

#[test]
fn exported_pack_carries_manifest_agents_orders_and_their_formulas() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), TRANSLATABLE_ORDERS);
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();

    assert_eq!(
        std::fs::read_to_string(out.join("pack/pack.toml")).unwrap(),
        "[pack]\nname = \"golden\"\nschema = 2\ndescription = \"Exported from gas-camp camp golden\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("pack/agents/dev.md")).unwrap(),
        "# dev agent\n"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("pack/orders/nightly.toml")).unwrap(),
        "[order]\nformula = \"one-step\"\ntrigger = \"cron\"\nschedule = \"0 7 * * 1-5\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("pack/orders/on-close.toml")).unwrap(),
        "[order]\nformula = \"one-step\"\ntrigger = \"event\"\non = \"bead.closed\"\n"
    );
    // D4: the authored formula the orders reference ships in the pack
    assert_eq!(
        std::fs::read_to_string(out.join("pack/formulas/one-step.toml")).unwrap(),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n"
    );
    assert_eq!(
        (report.issues, report.memories, report.agents, report.orders, report.pack_formulas),
        (1, 1, 1, 2, 1)
    );
}

#[test]
fn an_order_referencing_a_missing_formula_fails_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(
        dir.path(),
        "\n[[order]]\nname = \"nightly\"\non = \"cron:0 7 * * *\"\nformula = \"ghost\"\n",
    );
    let out = dir.path().join("city");
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::Export(msg)) => {
            assert!(msg.contains("ghost") && msg.contains("nightly"), "{msg}")
        }
        other => panic!("expected Export error, got {other:?}"),
    }
}

/// D5: newest pinned copy takes the bare name; an older divergent copy is
/// archived per-run; identical copies dedupe.
#[test]
fn pinned_formula_archive_dedupes_and_suffixes_divergent_copies() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), "");
    let runs = camp_root.join("runs");
    for (run_id, content) in [
        ("20260701T080000Z-aaaaaa", "formula = \"one-step\"\n# v1\n"),
        ("20260702T080000Z-bbbbbb", "formula = \"one-step\"\n# v2\n"),
        ("20260703T080000Z-cccccc", "formula = \"one-step\"\n# v2\n"),
    ] {
        let run_dir = runs.join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("one-step.toml"), content).unwrap();
        std::fs::write(run_dir.join("manifest.json"), "{}").unwrap();
    }
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();

    assert_eq!(
        std::fs::read_to_string(out.join("formulas/one-step.toml")).unwrap(),
        "formula = \"one-step\"\n# v2\n",
        "newest run's copy takes the bare name"
    );
    assert_eq!(
        std::fs::read_to_string(
            out.join("formulas/one-step.20260701T080000Z-aaaaaa.toml")
        )
        .unwrap(),
        "formula = \"one-step\"\n# v1\n",
        "older divergent copy is archived per-run"
    );
    assert_eq!(report.archive_formulas, 2, "identical copies dedupe");
    assert!(
        report.notes.iter().any(|n| n.contains("20260701T080000Z-aaaaaa")),
        "divergence is noted: {:?}",
        report.notes
    );
}

#[test]
fn a_run_dir_without_a_pinned_formula_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), "");
    let run_dir = camp_root.join("runs/20260701T080000Z-aaaaaa");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("manifest.json"), "{}").unwrap();
    let out = dir.path().join("city");
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::Export(msg)) => {
            assert!(msg.contains("20260701T080000Z-aaaaaa"), "{msg}")
        }
        other => panic!("expected Export error, got {other:?}"),
    }
}

#[test]
fn a_missing_agents_dir_is_noted_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), "");
    std::fs::remove_dir_all(camp_root.join("agents")).unwrap();
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();
    assert_eq!(report.agents, 0);
    assert!(
        report.notes.iter().any(|n| n.contains("no agent definitions")),
        "{:?}",
        report.notes
    );
    assert!(out.join("pack/agents").is_dir(), "layout stays deterministic");
}

/// D10: export is read-only — it appends nothing to the ledger.
#[test]
fn export_appends_no_events() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), TRANSLATABLE_ORDERS);
    let before = ledger.events_range(1, None).unwrap().len();
    let out = dir.path().join("city");
    export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();
    assert_eq!(ledger.events_range(1, None).unwrap().len(), before);
}
```

- [ ] **Step 3: Run to verify compile failure**

Run: `cargo test -p camp-core --test export_city 2>&1 | head -10`
Expected: compile error — `ExportOptions` / `export_city` not found.

- [ ] **Step 4: Implement the orchestration** — add to `export.rs`:

```rust
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::CampConfig;
use crate::ledger::Ledger;

#[derive(Debug, Clone, PartialEq)]
pub struct ExportOptions {
    /// The contract's explicit opt-out: skip untranslatable orders (each
    /// one reported) instead of failing the export.
    pub skip_untranslatable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkippedOrder {
    pub name: String,
    pub reason: String,
}

/// What an export produced — the CLI renders this; camp-core prints
/// nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportReport {
    pub issues: usize,
    pub memories: usize,
    pub archive_formulas: usize,
    pub pack_formulas: usize,
    pub agents: usize,
    pub orders: usize,
    pub skipped_orders: Vec<SkippedOrder>,
    pub notes: Vec<String>,
}

/// `camp export --city <dir>` (spec §15.3). Read-only over the ledger and
/// the camp directory; refuses a non-empty output directory; translates
/// every order BEFORE writing anything, so the untranslatable-order
/// failure leaves no partial output. Output layout and mapping tables:
/// docs/reference/export.md.
pub fn export_city(
    ledger: &Ledger,
    config: &CampConfig,
    camp_root: &Path,
    out_dir: &Path,
    options: &ExportOptions,
) -> Result<ExportReport, CoreError> {
    ensure_empty_dir(out_dir)?;
    let mut report = ExportReport::default();
    let translated = translate_all_orders(config, options, &mut report)?;
    write_beads_jsonl(ledger, out_dir, &mut report)?;
    write_archive_formulas(camp_root, out_dir, &mut report)?;
    write_pack(config, camp_root, out_dir, &translated, &mut report)?;
    Ok(report)
}

fn export_io(action: &str, path: &Path, err: &std::io::Error) -> CoreError {
    CoreError::Export(format!("cannot {action} {}: {err}", path.display()))
}

fn ensure_empty_dir(dir: &Path) -> Result<(), CoreError> {
    if dir.exists() {
        let mut entries = std::fs::read_dir(dir).map_err(|e| export_io("read", dir, &e))?;
        if entries.next().is_some() {
            return Err(CoreError::Export(format!(
                "refusing to export into non-empty directory {}",
                dir.display()
            )));
        }
    } else {
        std::fs::create_dir_all(dir).map_err(|e| export_io("create", dir, &e))?;
    }
    Ok(())
}

fn create_dir(dir: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(dir).map_err(|e| export_io("create", dir, &e))
}

fn write_file(path: &Path, content: impl AsRef<[u8]>) -> Result<(), CoreError> {
    std::fs::write(path, content).map_err(|e| export_io("write", path, &e))
}

/// Translate every `[[order]]`; any untranslatable order fails the whole
/// export (before a single byte is written) unless the caller opted out.
fn translate_all_orders(
    config: &CampConfig,
    options: &ExportOptions,
    report: &mut ExportReport,
) -> Result<Vec<(String, GcOrderFile)>, CoreError> {
    let compiled = crate::orders::parse::compile_orders(config)?;
    let mut translated = Vec::new();
    let mut skipped = Vec::new();
    for (order, raw) in compiled.iter().zip(&config.orders) {
        if order.name != raw.name {
            return Err(CoreError::Corrupt(format!(
                "compiled order {:?} does not line up with config order {:?}",
                order.name, raw.name
            )));
        }
        match translate_order(order, raw) {
            OrderTranslation::Translated { name, file } => translated.push((name, file)),
            OrderTranslation::Untranslatable { name, reason } => {
                skipped.push(SkippedOrder { name, reason });
            }
        }
    }
    if !skipped.is_empty() && !options.skip_untranslatable {
        let details = skipped
            .iter()
            .map(|s| format!("  {}: {}", s.name, s.reason))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CoreError::UntranslatableOrders {
            count: skipped.len(),
            details,
        });
    }
    report.skipped_orders = skipped;
    Ok(translated)
}

fn write_beads_jsonl(
    ledger: &Ledger,
    out_dir: &Path,
    report: &mut ExportReport,
) -> Result<(), CoreError> {
    let mut lines = String::new();
    for bead in ledger.export_beads()? {
        let record = bd_record(&bead)?;
        match &record {
            BdRecord::Issue(_) => report.issues += 1,
            BdRecord::Memory(_) => report.memories += 1,
        }
        lines.push_str(&jsonl_line(&record)?);
        lines.push('\n');
    }
    write_file(&out_dir.join("beads.jsonl"), lines)
}

/// `formulas/` = the pinned copies from `runs/` (master plan Phase 14;
/// Phase 5 pins byte-fidelity copies precisely for this export). The
/// newest run's copy of each name takes `<name>.toml`; an older run whose
/// copy differs is archived as `<name>.<run-id>.toml` — nothing dropped,
/// nothing flattened silently (invariant 3, decision D5).
fn write_archive_formulas(
    camp_root: &Path,
    out_dir: &Path,
    report: &mut ExportReport,
) -> Result<(), CoreError> {
    let dest = out_dir.join("formulas");
    create_dir(&dest)?;
    let runs_dir = camp_root.join("runs");
    if !runs_dir.exists() {
        return Ok(());
    }
    let mut run_dirs = Vec::new();
    for entry in std::fs::read_dir(&runs_dir).map_err(|e| export_io("read", &runs_dir, &e))? {
        let entry = entry.map_err(|e| export_io("read", &runs_dir, &e))?;
        if entry
            .file_type()
            .map_err(|e| export_io("stat", &entry.path(), &e))?
            .is_dir()
        {
            run_dirs.push(entry.path());
        }
    }
    // Run ids start with the compact cook timestamp: lexicographically
    // descending is newest-first.
    run_dirs.sort();
    run_dirs.reverse();

    let mut bare: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for run_dir in &run_dirs {
        let run_id = run_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                CoreError::Export(format!("run dir {} has a non-UTF-8 name", run_dir.display()))
            })?
            .to_owned();
        let mut pinned = Vec::new();
        for entry in std::fs::read_dir(run_dir).map_err(|e| export_io("read", run_dir, &e))? {
            let entry = entry.map_err(|e| export_io("read", run_dir, &e))?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                pinned.push(path);
            }
        }
        if pinned.is_empty() {
            return Err(CoreError::Export(format!(
                "run dir {} has no pinned formula (*.toml)",
                run_dir.display()
            )));
        }
        pinned.sort();
        for path in pinned {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    CoreError::Export(format!("{} has a non-UTF-8 name", path.display()))
                })?
                .to_owned();
            let content = std::fs::read(&path).map_err(|e| export_io("read", &path, &e))?;
            match bare.get(&file_name) {
                None => {
                    write_file(&dest.join(&file_name), &content)?;
                    bare.insert(file_name, content);
                    report.archive_formulas += 1;
                }
                Some(existing) if *existing == content => {}
                Some(_) => {
                    let stem = file_name.trim_end_matches(".toml");
                    let alt = format!("{stem}.{run_id}.toml");
                    write_file(&dest.join(&alt), &content)?;
                    report.archive_formulas += 1;
                    report.notes.push(format!(
                        "formula {file_name} from run {run_id} differs from the newest pinned copy; archived as formulas/{alt}"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// `pack/`: generated pack.toml wrapper, agent definitions verbatim,
/// translated orders as gc `orders/<name>.toml` files, and the authored
/// formulas those orders reference (decision D4).
fn write_pack(
    config: &CampConfig,
    camp_root: &Path,
    out_dir: &Path,
    translated: &[(String, GcOrderFile)],
    report: &mut ExportReport,
) -> Result<(), CoreError> {
    #[derive(serde::Serialize)]
    struct PackToml<'a> {
        pack: PackMeta<'a>,
    }
    #[derive(serde::Serialize)]
    struct PackMeta<'a> {
        name: &'a str,
        schema: i64,
        description: String,
    }

    let pack_dir = out_dir.join("pack");
    let agents_dest = pack_dir.join("agents");
    let formulas_dest = pack_dir.join("formulas");
    let orders_dest = pack_dir.join("orders");
    for dir in [&pack_dir, &agents_dest, &formulas_dest, &orders_dest] {
        create_dir(dir)?;
    }

    let manifest = PackToml {
        pack: PackMeta {
            name: &config.camp.name,
            schema: 2,
            description: format!("Exported from gas-camp camp {}", config.camp.name),
        },
    };
    let manifest_text = toml::to_string(&manifest)
        .map_err(|e| CoreError::Export(format!("cannot serialize pack.toml: {e}")))?;
    write_file(&pack_dir.join("pack.toml"), manifest_text)?;

    let agents_src = camp_root.join("agents");
    if agents_src.is_dir() {
        report.agents = copy_tree(&agents_src, &agents_dest)?;
    }
    if report.agents == 0 {
        report.notes.push(format!(
            "no agent definitions found under {} — pack exported without agents",
            agents_src.display()
        ));
    }

    let mut formula_names = BTreeSet::new();
    for (name, file) in translated {
        let text = toml::to_string(file)
            .map_err(|e| CoreError::Export(format!("cannot serialize order {name:?}: {e}")))?;
        write_file(&orders_dest.join(format!("{name}.toml")), text)?;
        formula_names.insert((name.clone(), file.order.formula.clone()));
    }
    report.orders = translated.len();

    let mut copied = BTreeSet::new();
    for (order_name, formula) in formula_names {
        if !copied.insert(formula.clone()) {
            continue;
        }
        let src = crate::orders::formula_path(camp_root, &formula);
        let content = std::fs::read(&src).map_err(|e| {
            CoreError::Export(format!(
                "exported order {order_name:?} references formula {formula:?} but {} cannot be read: {e}",
                src.display()
            ))
        })?;
        write_file(&formulas_dest.join(format!("{formula}.toml")), content)?;
        report.pack_formulas += 1;
    }
    Ok(())
}

/// Verbatim recursive copy (agent definitions are opaque to camp —
/// invariant 4 leaves zero role knowledge in code). Returns files copied.
fn copy_tree(src: &Path, dest: &Path) -> Result<usize, CoreError> {
    let mut count = 0;
    for entry in std::fs::read_dir(src).map_err(|e| export_io("read", src, &e))? {
        let entry = entry.map_err(|e| export_io("read", src, &e))?;
        let path = entry.path();
        let ty = entry
            .file_type()
            .map_err(|e| export_io("stat", &path, &e))?;
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            create_dir(&to)?;
            count += copy_tree(&path, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&path, &to).map_err(|e| export_io("copy", &path, &e))?;
            count += 1;
        } else {
            return Err(CoreError::Export(format!(
                "{} is neither a file nor a directory",
                path.display()
            )));
        }
    }
    Ok(count)
}
```

- [ ] **Step 5: Run the orchestration tests**

Run: `cargo test -p camp-core --test export_city`
Expected: all 9 tests PASS.

- [ ] **Step 6: Run the whole camp-core suite** (refold property, vocab pin, corpus must stay green):

Run: `cargo test -p camp-core`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/camp-core/src/export.rs crates/camp-core/src/error.rs crates/camp-core/tests/export_city.rs
git commit -m "feat(export): export_city orchestration — beads.jsonl, formula archive, pack"
```

---

### Task 5: Golden export test

**Files:**
- Create: `crates/camp-core/tests/export_golden.rs`
- Create: `crates/camp-core/tests/fixtures/export-golden/**` (generated in Step 3, hand-verified, committed)

**Interfaces:**
- Consumes: `export_city`, `formula::parse_and_validate`, `formula::cook` (re-export of the private cook module's `cook(&mut Ledger, &Formula, run_dir: &Path, rig: &RigConfig, actor: &str) -> Result<CookedRun, CoreError>`), `FixedClock`.
- Produces: the checked-in golden tree; run-id normalization helper.

The fixture camp covers the contract list exactly: beads incl. closed-with-outcome history, one cooked run (cook pins the formula copy under `runs/` and creates run/step beads with `run_id`/`step_id`), and both order kinds. The cook run-id embeds 24 random bits, so both the comparison and the golden files normalize it to the literal `RUNID`.

- [ ] **Step 1: Write the test** — create `crates/camp-core/tests/export_golden.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 14 golden export (master plan: "golden export of a fixture camp —
//! beads incl. closed-with-outcome history, one cooked run, both order
//! kinds; JSONL parses line by line and field-maps exactly").
//!
//! Regenerate after an intentional output change:
//!   UPDATE_EXPORT_GOLDEN=1 cargo test -p camp-core --test export_golden
//! then eyeball `git diff crates/camp-core/tests/fixtures/export-golden/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use camp_core::clock::FixedClock;
use camp_core::config::{CampConfig, RigConfig};
use camp_core::event::{EventInput, EventType};
use camp_core::export::{ExportOptions, export_city};
use camp_core::formula;
use camp_core::ledger::Ledger;

const TS: &str = "2026-07-05T21:14:03Z";
const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/export-golden"
);
const FORMULA: &str = "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n";
const AGENT: &str = "# dev\n\nYou are the dev agent for the golden camp.\n";
const ORDERS: &str = r#"
[[order]]
name = "nightly"
on = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name = "on-close"
on = "event:bead.closed"
formula = "one-step"
"#;

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

/// Build the fixture camp and export it; returns (out_dir, run_id).
fn export_fixture(dir: &Path) -> (PathBuf, String) {
    let camp_root = dir.join(".camp");
    std::fs::create_dir_all(&camp_root).unwrap();
    let rig_path = dir.join("repo");
    std::fs::create_dir_all(&rig_path).unwrap();
    let config_text = format!(
        "[camp]\nname = \"golden\"\n\n[[rigs]]\nname = \"gc\"\npath = {:?}\nprefix = \"gc\"\n{ORDERS}",
        rig_path.display()
    );
    std::fs::write(camp_root.join("camp.toml"), &config_text).unwrap();
    let config = CampConfig::parse(&config_text).unwrap();

    std::fs::create_dir_all(camp_root.join("formulas")).unwrap();
    std::fs::write(camp_root.join("formulas/one-step.toml"), FORMULA).unwrap();
    std::fs::create_dir_all(camp_root.join("agents")).unwrap();
    std::fs::write(camp_root.join("agents/dev.md"), AGENT).unwrap();

    let mut ledger =
        Ledger::open_with_clock(&camp_root.join("camp.db"), Box::new(FixedClock::new(TS)))
            .unwrap();
    // closed-with-outcome history
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-1",
        serde_json::json!({"title": "implement widget", "description": "the change", "labels": ["cli"], "assignee": "dev"}),
    );
    append(
        &mut ledger,
        EventType::BeadClaimed,
        "gc-1",
        serde_json::json!({"session": "camp/dev/1"}),
    );
    append(
        &mut ledger,
        EventType::BeadClosed,
        "gc-1",
        serde_json::json!({"outcome": "pass", "reason": "shipped the widget"}),
    );
    // open + blocked
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-2",
        serde_json::json!({"title": "review widget", "needs": ["gc-1"]}),
    );
    // mail + memory
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-3",
        serde_json::json!({"title": "ping from ci", "type": "mail"}),
    );
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-4",
        serde_json::json!({"title": "deploy needs the VPN profile", "type": "memory"}),
    );
    // one cooked run: pins the formula copy under runs/, creates run beads
    let parsed = formula::parse_and_validate(&camp_root.join("formulas/one-step.toml")).unwrap();
    let rig = RigConfig {
        name: "gc".into(),
        path: rig_path,
        prefix: "gc".into(),
    };
    let cooked = formula::cook(
        &mut ledger,
        &parsed,
        &camp_root.join("runs"),
        &rig,
        "order:nightly:1",
    )
    .unwrap();

    let out = dir.join("city");
    export_city(
        &ledger,
        &config,
        &camp_root,
        &out,
        &ExportOptions {
            skip_untranslatable: false,
        },
    )
    .unwrap();
    (out, cooked.run_id)
}

fn walk(root: &Path) -> BTreeMap<String, String> {
    fn inner(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                inner(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_str().unwrap().to_owned();
                out.insert(rel, std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    inner(root, root, &mut out);
    out
}

#[test]
fn golden_export_matches_the_checked_in_tree() {
    let dir = tempfile::tempdir().unwrap();
    let (out, run_id) = export_fixture(dir.path());

    // normalize the one nondeterministic value (24 random bits in run ids)
    let actual: BTreeMap<String, String> = walk(&out)
        .into_iter()
        .map(|(path, content)| (path, content.replace(&run_id, "RUNID")))
        .collect();

    if std::env::var_os("UPDATE_EXPORT_GOLDEN").is_some() {
        let golden_root = Path::new(GOLDEN);
        if golden_root.exists() {
            std::fs::remove_dir_all(golden_root).unwrap();
        }
        for (rel, content) in &actual {
            let dest = golden_root.join(rel);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, content).unwrap();
        }
        panic!("golden tree regenerated under {GOLDEN} — inspect the diff and rerun without UPDATE_EXPORT_GOLDEN");
    }

    let golden = walk(Path::new(GOLDEN));
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        golden.keys().collect::<Vec<_>>(),
        "output file set differs from the golden tree"
    );
    for (rel, content) in &golden {
        assert_eq!(&actual[rel], content, "content mismatch in {rel}");
    }
}

#[test]
fn beads_jsonl_parses_line_by_line_and_field_maps_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let (out, run_id) = export_fixture(dir.path());
    let text = std::fs::read_to_string(out.join("beads.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // every line is a self-contained JSON object with a _type discriminator
    for line in &lines {
        let t = line["_type"].as_str().unwrap();
        assert!(t == "issue" || t == "memory", "unexpected _type {t}");
    }

    // gc-1: the closed-with-outcome issue, field by field (mapping table)
    let gc1 = lines
        .iter()
        .find(|l| l["id"] == "gc-1")
        .expect("gc-1 line");
    assert_eq!(gc1["title"], "implement widget");
    assert_eq!(gc1["description"], "the change");
    assert_eq!(gc1["status"], "closed");
    assert_eq!(gc1["priority"], 2);
    assert_eq!(gc1["issue_type"], "task");
    assert_eq!(gc1["assignee"], "dev");
    assert_eq!(gc1["created_at"], TS);
    assert_eq!(gc1["updated_at"], TS);
    assert_eq!(gc1["closed_at"], TS);
    assert_eq!(gc1["close_reason"], "shipped the widget");
    assert_eq!(gc1["labels"], serde_json::json!(["cli"]));
    assert_eq!(gc1["metadata"]["gc.outcome"], "pass");
    assert_eq!(gc1["metadata"]["camp.rig"], "gc");
    assert_eq!(gc1["metadata"]["camp.claimed_by"], "camp/dev/1");
    assert!(
        gc1["metadata"].get("gc.final_disposition").is_none(),
        "no merged phase records a final disposition yet (plan D6)"
    );

    // gc-2: the needs edge became a bd blocking dependency
    let gc2 = lines.iter().find(|l| l["id"] == "gc-2").unwrap();
    assert_eq!(
        gc2["dependencies"],
        serde_json::json!([{"issue_id": "gc-2", "depends_on_id": "gc-1", "type": "blocks"}])
    );

    // gc-3: mail → native bd message type
    let gc3 = lines.iter().find(|l| l["id"] == "gc-3").unwrap();
    assert_eq!(gc3["issue_type"], "message");

    // gc-4: memory → native bd memory record, not an issue
    let mem = lines
        .iter()
        .find(|l| l["_type"] == "memory")
        .expect("memory record");
    assert_eq!(mem["key"], "gc-4");
    assert_eq!(mem["value"], "deploy needs the VPN profile");
    assert!(mem.get("id").is_none());

    // cooked-run beads carry run/step provenance in camp.* metadata
    let step = lines
        .iter()
        .find(|l| l["metadata"]["camp.step_id"] == "s1")
        .expect("step bead line");
    assert_eq!(step["metadata"]["camp.run_id"], run_id);
}
```

- [ ] **Step 2: Run to watch it fail** (no golden tree yet):

Run: `cargo test -p camp-core --test export_golden`
Expected: `golden_export_matches_the_checked_in_tree` FAILS (missing `fixtures/export-golden`); the field-map test may already pass — it exercises live output.

- [ ] **Step 3: Generate the golden tree, then hand-verify it**

Run: `UPDATE_EXPORT_GOLDEN=1 cargo test -p camp-core --test export_golden golden_export 2>&1 | tail -3`
Expected: panic message "golden tree regenerated".

Now READ every generated file under `crates/camp-core/tests/fixtures/export-golden/` and check each against the mapping tables in this plan (this is the hand-verification step — the golden is only as good as this review): `beads.jsonl` (5+ issue lines + 1 memory line, `RUNID` placeholders), `formulas/one-step.toml` (byte-identical to `FORMULA`), `pack/pack.toml`, `pack/agents/dev.md`, `pack/formulas/one-step.toml`, `pack/orders/nightly.toml`, `pack/orders/on-close.toml`.

- [ ] **Step 4: Run both tests to verify they pass**

Run: `cargo test -p camp-core --test export_golden`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/tests/export_golden.rs crates/camp-core/tests/fixtures/export-golden
git commit -m "test(export): golden export tree and line-by-line jsonl field mapping"
```

---

### Task 6: CLI — `camp export --city <dir> [--skip-untranslatable]`

**Files:**
- Create: `crates/camp/src/cmd/export.rs`
- Modify: `crates/camp/src/main.rs`
- Create: `crates/camp/tests/cli_export.rs`

**Interfaces:**
- Consumes: `camp_core::export::{ExportOptions, export_city}`, `CampDir` (`root`, `db_path()`, `config_path()`).
- Produces: `cmd::export::run(camp: &CampDir, city: &Path, skip_untranslatable: bool) -> anyhow::Result<()>`; the `camp export` verb.

- [ ] **Step 1: Write the failing CLI test** — create `crates/camp/tests/cli_export.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 14: `camp export --city <dir>` against the real binary
//! (spec §15.3). The byte-level golden lives in camp-core; this file pins
//! the CLI surface: exit codes, stderr listings, the skip flag, and the
//! non-empty-dir refusal.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn camp_cmd(root: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_remove("CAMP_DIR").arg("--camp").arg(root);
    cmd
}

fn run_ok(root: &Path, args: &[&str]) -> String {
    let out = camp_cmd(root).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "camp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// camp init + one rig + agents/dev.md + seeded beads:
/// gc-1 closed-with-outcome, gc-2 open needing gc-1, gc-3 mail, gc-4 memory.
fn init_camp(dir: &Path) -> PathBuf {
    let status = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir)
        .arg("init")
        .status()
        .unwrap();
    assert!(status.success());
    let root = dir.join(".camp");
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let out = camp_cmd(&root)
        .args(["rig", "add"])
        .arg(&rig)
        .args(["--prefix", "gc", "--name", "gc"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::write(
        root.join("formulas/one-step.toml"),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(root.join("agents/dev.md"), "# dev agent\n").unwrap();

    run_ok(
        &root,
        &[
            "create",
            "implement widget",
            "--description",
            "the change",
            "--label",
            "cli",
        ],
    );
    run_ok(&root, &["claim", "gc-1", "--session", "camp/dev/1"]);
    run_ok(
        &root,
        &["close", "gc-1", "--outcome", "pass", "--reason", "shipped"],
    );
    run_ok(&root, &["create", "review widget", "--needs", "gc-1"]);
    run_ok(&root, &["create", "ping from ci", "--type", "mail"]);
    run_ok(&root, &["remember", "deploy needs the VPN profile"]);
    root
}

fn add_orders(root: &Path, table: &str) {
    let path = root.join("camp.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(table);
    std::fs::write(&path, text).unwrap();
}

const TRANSLATABLE: &str = r#"
[[order]]
name    = "nightly"
on      = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name    = "on-close"
on      = "event:bead.closed"
formula = "one-step"
"#;

const LABELED: &str = r#"
[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "one-step"
"#;

#[test]
fn export_writes_the_city_directory_and_reports_counts() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root, TRANSLATABLE);
    let city = dir.path().join("city");
    let stdout = run_ok(&root, &["export", "--city", city.to_str().unwrap()]);
    assert!(
        stdout.contains("3 issues") && stdout.contains("1 memories"),
        "{stdout}"
    );

    // every jsonl line parses; the closed bead field-maps
    let text = std::fs::read_to_string(city.join("beads.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    let gc1 = lines.iter().find(|l| l["id"] == "gc-1").unwrap();
    assert_eq!(gc1["status"], "closed");
    assert_eq!(gc1["metadata"]["gc.outcome"], "pass");
    assert_eq!(gc1["close_reason"], "shipped");
    let gc2 = lines.iter().find(|l| l["id"] == "gc-2").unwrap();
    assert_eq!(gc2["dependencies"][0]["depends_on_id"], "gc-1");
    assert!(lines.iter().any(|l| l["_type"] == "memory"));

    assert!(city.join("pack/pack.toml").exists());
    assert!(city.join("pack/agents/dev.md").exists());
    assert!(city.join("pack/orders/nightly.toml").exists());
    assert!(city.join("pack/orders/on-close.toml").exists());
    assert!(city.join("pack/formulas/one-step.toml").exists());
}

#[test]
fn untranslatable_orders_fail_the_export_listing_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root, TRANSLATABLE);
    add_orders(&root, LABELED);
    let city = dir.path().join("city");
    let out = camp_cmd(&root)
        .args(["export", "--city", city.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ci-red")
            && stderr.contains("label")
            && stderr.contains("--skip-untranslatable"),
        "{stderr}"
    );
    // fail-before-write
    assert_eq!(std::fs::read_dir(&city).unwrap().count(), 0);
}

#[test]
fn skip_untranslatable_is_the_explicit_opt_out() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root, TRANSLATABLE);
    add_orders(&root, LABELED);
    let city = dir.path().join("city");
    let out = camp_cmd(&root)
        .args([
            "export",
            "--city",
            city.to_str().unwrap(),
            "--skip-untranslatable",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("skipped untranslatable order ci-red"),
        "{stderr}"
    );
    assert!(city.join("pack/orders/nightly.toml").exists());
    assert!(!city.join("pack/orders/ci-red.toml").exists());
}

#[test]
fn a_non_empty_target_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let city = dir.path().join("city");
    std::fs::create_dir_all(&city).unwrap();
    std::fs::write(city.join("keep.txt"), "precious").unwrap();
    let out = camp_cmd(&root)
        .args(["export", "--city", city.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("non-empty"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(city.join("keep.txt")).unwrap(),
        "precious"
    );
}
```

- [ ] **Step 2: Run to verify failure** (unknown subcommand):

Run: `cargo test -p camp --test cli_export 2>&1 | tail -5`
Expected: FAIL — `camp export` is an unrecognized subcommand (clap error on stderr).

- [ ] **Step 3: Wire the CLI.** In `crates/camp/src/main.rs`: add `pub mod export;` to the `cmd { }` block (alphabetical, after `pub mod events;`); add the variant to `enum Command` (after `Search`, before `Remember` — placement is cosmetic, keep the enum readable):

```rust
    /// Export the camp for Gas City import (spec §15.3): beads.jsonl,
    /// pinned formulas, and a pack directory. Read-only — camp never
    /// writes into a live city's store.
    Export {
        /// Output directory (created; must not already contain anything)
        #[arg(long, value_name = "DIR")]
        city: PathBuf,
        /// Skip orders that cannot be translated to gc order TOML
        /// instead of failing the export
        #[arg(long)]
        skip_untranslatable: bool,
    },
```

and the match arm in `fn run`:

```rust
        Command::Export {
            city,
            skip_untranslatable,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::export::run(&camp, &city, skip_untranslatable)
        }
```

Create `crates/camp/src/cmd/export.rs`:

```rust
//! `camp export --city <dir>` (spec §15.3): graduation is an export, not a
//! backend. All logic lives in camp-core; this shim resolves paths, runs
//! the export, and renders the report (notes and skips on stderr —
//! visible degradation, never silence).

use std::path::Path;

use camp_core::config::CampConfig;
use camp_core::export::{ExportOptions, export_city};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

pub fn run(camp: &CampDir, city: &Path, skip_untranslatable: bool) -> anyhow::Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    let config = CampConfig::load(&camp.config_path())?;
    let report = export_city(
        &ledger,
        &config,
        &camp.root,
        city,
        &ExportOptions {
            skip_untranslatable,
        },
    )?;
    for note in &report.notes {
        eprintln!("camp export: {note}");
    }
    for skipped in &report.skipped_orders {
        eprintln!(
            "camp export: skipped untranslatable order {}: {}",
            skipped.name, skipped.reason
        );
    }
    println!(
        "exported to {}: {} issues, {} memories, {} archive formulas, {} pack formulas, {} agents, {} orders ({} skipped)",
        city.display(),
        report.issues,
        report.memories,
        report.archive_formulas,
        report.pack_formulas,
        report.agents,
        report.orders,
        report.skipped_orders.len()
    );
    Ok(())
}
```

- [ ] **Step 4: Run the CLI tests**

Run: `cargo test -p camp --test cli_export`
Expected: 4 tests PASS.

- [ ] **Step 5: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/main.rs crates/camp/src/cmd/export.rs crates/camp/tests/cli_export.rs
git commit -m "feat(cli): camp export --city with --skip-untranslatable"
```

---

### Task 7: Optional local bd-import check (not in CI)

**Files:**
- Modify: `crates/camp/tests/cli_export.rs`

The contract: "optional local check if a bd binary is present (not in CI)". It is `#[ignore]`, so CI never runs it; when a human runs it explicitly it fails fast if `bd` is missing (running an ignored test IS the request to use bd — no silent skip).

- [ ] **Step 1: Append the ignored test** to `cli_export.rs`:

```rust
/// Local-only (not in CI): prove a real `bd import` accepts the export.
/// Run: cargo test -p camp --test cli_export -- --ignored
#[test]
#[ignore = "requires a bd binary on PATH; local-only by contract"]
fn bd_import_accepts_the_exported_jsonl() {
    let bd_available = Command::new("bd")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(
        bd_available,
        "this ignored test was invoked explicitly but no working `bd` binary is on PATH"
    );

    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root, TRANSLATABLE);
    let city = dir.path().join("city");
    run_ok(&root, &["export", "--city", city.to_str().unwrap()]);

    let bd_home = dir.path().join("bdws");
    std::fs::create_dir_all(&bd_home).unwrap();
    let init = Command::new("bd")
        .current_dir(&bd_home)
        .args(["init"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "bd init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let import = Command::new("bd")
        .current_dir(&bd_home)
        .arg("import")
        .arg(city.join("beads.jsonl"))
        .output()
        .unwrap();
    assert!(
        import.status.success(),
        "bd import failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&import.stdout),
        String::from_utf8_lossy(&import.stderr)
    );
}
```

(If the locally installed bd's `init`/`import` flag surface differs, adjust THIS TEST to the real CLI and record the correction in the PR description — the test is the canary for exactly that drift; the wire format itself is pinned by the golden.)

- [ ] **Step 2: Verify it is ignored by default**

Run: `cargo test -p camp --test cli_export`
Expected: `4 passed; 1 ignored` (wording approximate — the point is the new test shows as ignored, not run).

- [ ] **Step 3: Run it for real if bd is installed locally** (best effort, record the outcome in the PR):

Run: `cargo test -p camp --test cli_export -- --ignored 2>&1 | tail -5`
Expected: PASS when bd exists; a clear assertion message when it does not.

- [ ] **Step 4: Commit**

```bash
git add crates/camp/tests/cli_export.rs
git commit -m "test(export): optional local bd-import round-trip check"
```

---

### Task 8: Documentation — `docs/reference/export.md` + spec §15.3 amendment

**Files:**
- Create: `docs/reference/export.md`
- Modify: `docs/design/2026-07-05-gas-camp-design.md` (§15.3, first bullet)

- [ ] **Step 1: Write `docs/reference/export.md`.** Content: title "camp export — the Gas City export bridge"; a short intro (spec §15.3 pointer; read-only; provenance line naming the pinned gascity ref `12410301884b51131a35e101a335dbaae16cdcb0`); the **Output Layout** block, the **Field-Level Mapping Table**, and the **Order Translation Table** copied verbatim from this plan (they were written to be shared); a "Wire format provenance" subsection carrying the two-wire-formats warning (types.Issue vs exec-provider shape, silent-unknown-field behavior of bd import, priority-0 hazard, native memory records); a "What is not exported" subsection (the event log itself — bd has no equivalent; per-memory timestamps/rig — bd memories are KV; `gc.final_disposition` — defined, unpopulated until camp records one; untranslatable orders when `--skip-untranslatable` was passed); and an "Importing into a city" subsection (`bd import <dir>/beads.jsonl`; install `pack/` via gc's pack import tooling (`gc import add` / `gc import install`); `formulas/` is the historical archive of run-pinned copies — the pack already contains the formulas its orders need).

- [ ] **Step 2: Amend spec §15.3** (decision D7). In `docs/design/2026-07-05-gas-camp-design.md`, replace the bullet text:

```
(c) the pack's agent definitions with a generated `pack.toml` wrapper,
including the camp's orders translated into the wrapper as city-order
declarations.
```

with:

```
(c) the pack's agent definitions with a generated `pack.toml` wrapper, the
camp's orders translated into city order files (`orders/<name>.toml` — gc
packs declare orders as files by convention; gc's `pack.toml` cannot
declare orders inline), and the authored formulas those orders reference.
Field-level mapping: `docs/reference/export.md`.
```

(Keep the rest of the bullet list intact; do not re-litigate anything else in §15.3.)

- [ ] **Step 3: Run the full gates**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add docs/reference/export.md docs/design/2026-07-05-gas-camp-design.md
git commit -m "docs: export reference with bd field mapping; spec §15.3 order-file amendment"
```

- [ ] **Step 5: Push and open the PR**

```bash
git push -u origin phase-14-export-bridge
gh pr create --title "Phase 14: export bridge — camp export --city" --body "<summary + exit criteria evidence>"
gh pr checks --watch
```

If Phase 8 (PR #14) merged meanwhile: rebase onto current main first (`git fetch origin && git rebase origin/main`), re-run the full gates, then push. Never open or update the PR from a branch not rebased on current main.

---

## Exit Criteria Mapping (master plan Phase 14)

| Contract line | Where proven |
|---|---|
| `beads.jsonl` in bd wire format with the pinned field mapping | Tasks 1–2 unit tests, Task 5 golden + field-map test, mapping table in this plan + `docs/reference/export.md` |
| statuses 1:1; memory beads via the researched native route; metadata `gc.outcome`/`gc.final_disposition` | Task 2 tests (D1, D6), Task 5 field-map test |
| `formulas/` = pinned copies from `runs/` | Task 4 archive tests (dedupe/divergence), Task 5 golden |
| `pack/` = agents verbatim + generated pack.toml + translated orders | Task 4 pack test, Task 5 golden |
| untranslatable orders fail listing them; `--skip-untranslatable` opt-out | Task 4 core tests + Task 6 CLI tests (fail-before-write pinned in both) |
| golden export of a fixture camp (closed-with-outcome, one cooked run, both order kinds) | Task 5 |
| JSONL parses line by line and field-maps exactly | Task 5 second test |
| optional local bd check, not in CI | Task 7 (`#[ignore]`) |
| a Gas City operator could import the output directory with standard tooling; CI green | Whole plan; Task 8 gates + PR checks |

## Self-Review Notes

- Spec coverage: §15.3 bullets (a)/(b)/(c) map to Tasks 1–2/4–5/3–4; the §15.3 wording divergence found in research is amended in the same PR (Task 8) per AGENTS.md.
- Type consistency: `ExportBead` (T1) is consumed by `bd_record` (T2) and `export_city` (T4); `GcOrderFile` (T3) by `write_pack` (T4); `ExportOptions`/`ExportReport` (T4) by `cmd/export.rs` (T6). Field names cross-checked.
- The plan deliberately adds no new dependencies and no new events; the refold property, vocab pin, and formula-corpus gates are untouched by construction (export is read-only) — pinned by `export_appends_no_events`.

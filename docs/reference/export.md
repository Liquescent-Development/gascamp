# camp export — the Gas City export bridge

`camp export --city <dir>` (spec §15.3) emits a directory a Gas City
operator imports with standard tooling. Graduation is an export, not a
backend: the command is read-only over the ledger and the camp directory,
appends nothing to camp's own ledger, and never writes into a live city's
store.

Provenance: every Gas City fact on this page was verified against gascity
at the pinned ref `12410301884b51131a35e101a335dbaae16cdcb0` (the
`ci/gc-compat/GASCITY_REF` pin) and the beads library (beadslib) at
**v1.0.4**, the version that gascity ref resolves to.

## Output layout

```
<dir>/
  beads.jsonl               # issue + memory records, one JSON object per line
  formulas/                 # archive: pinned copies from runs/
    <name>.toml             #   newest run's pinned copy
    <name>.<run-id>.toml    #   older divergent pinned copies (nothing dropped)
  pack/
    pack.toml               # [pack] name = <camp name>, schema = 2, description
    agents/                 # verbatim copy of <camp>/agents/
    formulas/<name>.toml    # authored formulas referenced by exported orders
    orders/<name>.toml      # translated orders, one [order] table each
```

All directories are created even when empty, so the output shape is
deterministic. The output directory must not exist non-empty (hard
error). On an I/O error mid-write the directory may be partial and is
safe to delete; the untranslatable-order failure specifically happens
before any file is written.

## Wire-format provenance (load-bearing)

There are TWO bead wire formats in the Gas City world — do not conflate
them:

- **What `bd import` reads** is the beads library's `types.Issue` — the
  schema `beads.jsonl` targets. Only `title` is required; `status`
  defaults to `open` and `issue_type` to `task`; `metadata` is an
  arbitrary JSON object preserved verbatim; import upserts by `id`.
- **Gas City's exec-beads-provider RPC** (`docs/reference/
  exec-beads-provider.md` in gascity, and gc's internal `beads.Bead`
  struct) is a different, internal format whose `parent`/`needs`/`ref`/
  `from` fields do **not** exist in `types.Issue`. `bd import` uses plain
  JSON decoding and **silently ignores unknown fields** — emitting the
  internal shape would import and quietly drop every relationship.

Consequences camp's exporter encodes:

- Blocking edges ride in the `dependencies` array
  (`{"issue_id","depends_on_id","type"}`), never in `needs`/`parent`.
- `priority` is written explicitly as `2` (normal) on every issue line:
  bd's own export never omits the field and `0` is a valid value meaning
  P0/critical, so an explicit 2 is correct under either reading of bd's
  absent-field behavior.
- beadslib validation requires `closed_at` to be present **iff** the
  status is `closed`. Camp satisfies this by construction (the fold sets
  `closed_ts` exactly when a bead closes); hand-editing an exported
  `beads.jsonl` can break that invariant and be rejected at import.
- Memories are native: `bd import` treats
  `{"_type":"memory","key":…,"value":…}` lines as `bd remember` KV
  entries, not issues. Issue lines carry `"_type":"issue"` to match bd's
  own export; a line without `_type` is also read as an issue.

## Field-level mapping (beads.jsonl)

| camp (`beads` table) | `beads.jsonl` (beadslib `types.Issue` JSON tag) | Rule |
|---|---|---|
| — | `_type` | literal `"issue"` on issue lines (matches `bd export`; absence also accepted) |
| `id` | `id` | verbatim (`{prefix}-{n}`, e.g. `gc-142`) |
| `title` | `title` | verbatim (the only field bd requires) |
| `description` | `description` | verbatim; omitted when empty |
| `status` (`open`/`in_progress`/`closed`) | `status` | 1:1 verbatim (camp's values are a subset of bd's) |
| `type = "task"` | `issue_type: "task"` | |
| `type = "mail"` | `issue_type: "message"` | bd native message type |
| `type = "memory"` | *not an issue line* | `{"_type":"memory","key":"<bead id>","value":"<title>"}` |
| — | `priority` | literal `2` on every issue line (see above) |
| `assignee` | `assignee` | omitted when NULL |
| `claimed_by` | `metadata."camp.claimed_by"` | when set |
| `rig` | `metadata."camp.rig"` | always |
| `run_id` / `step_id` | `metadata."camp.run_id"` / `metadata."camp.step_id"` | when set |
| `outcome` (`pass`/`fail`) | `metadata."gc.outcome"` | when set; camp values are valid gc vocabulary |
| *(no source yet — see below)* | `metadata."gc.final_disposition"` | defined; emitted once camp records one; legal values `hard_fail`/`soft_fail` |
| `close_reason` | `close_reason` | when set |
| `closed_ts` | `closed_at` | RFC3339, when set |
| `created_ts` | `created_at` | RFC3339 |
| `updated_ts` | `updated_at` | RFC3339 |
| `labels` | `labels` | verbatim array; omitted when empty |
| `deps(bead_id, needs_id)` | `dependencies: [{"issue_id":<bead>,"depends_on_id":<needs>,"type":"blocks"}]` | camp `needs` is a readiness-blocking edge → bd `blocks` |

Issue lines appear in true creation order (the beads table's insertion
order, which follows event-seq order — not a timestamp/id sort, which
would misorder same-second beads with double-digit ids); memory records
interleave at their creation position. All metadata values are
JSON strings; `camp.*` keys are additive camp provenance, `gc.*` keys
follow the vocabulary mirror (camp's `outcome` values are a strict subset
of gc's `pass|fail|skipped|missing_root`).

## Order translation

| camp `[[order]]` (camp.toml) | gc `pack/orders/<name>.toml` |
|---|---|
| `name = "x"` | filename `x.toml` (gc derives the order name from the filename) |
| `formula = "f"` | `formula = "f"` (+ authored `f` copied to `pack/formulas/f.toml`) |
| `on = "cron:EXPR"` | `trigger = "cron"` + `schedule = "EXPR"` |
| `on = "event:TYPE"` | `trigger = "event"` + `on = "TYPE"` |
| `on = "event:TYPE[label=X]"` | **untranslatable** (gc event orders have no label filter) |
| `rig = "r"` | **untranslatable** (no key binds an order to a specific named rig; gc's `scope` key selects city-vs-rig instantiation) |
| `catch_up_window = "…"` | **untranslatable** (no gc equivalent) |

Untranslatable orders fail the export with every offender listed (name +
reason) before anything is written; `--skip-untranslatable` is the
explicit opt-out — the export then proceeds without those orders and
names each skip on stderr.

## Formula archive rules

`formulas/` (top level) holds the byte-fidelity pinned copies from
`runs/` — the formulas as they actually ran. The newest run's copy of
each name takes `formulas/<name>.toml`; an older run whose pinned copy
differs is archived as `formulas/<name>.<run-id>.toml` and noted on
stderr — divergent history is preserved, never flattened, never fatal.
Identical copies dedupe. The pack's `pack/formulas/` directory is
separate: it carries the current *authored* formulas the exported orders
reference, which is what gc's pack discovery expects.

## What is not exported

- The event log itself — bd has no equivalent; the ledger's history stays
  in `camp.db` (current bead state, outcomes, and close reasons are all
  in `beads.jsonl`).
- Per-memory timestamps, rig, and status — bd memories are a KV store;
  each memory bead exports as its `key` (bead id) and `value` (the fact).
- `metadata."gc.final_disposition"` — the mapping is defined (values
  mirror camp's `hard_fail`/`soft_fail` vocabulary), but no camp
  component records a final disposition yet; the key is emitted once one
  exists.
- Untranslatable orders when `--skip-untranslatable` was passed — each
  skip is named on stderr.
- External `[packs]` agent layers — the export ships the camp-local
  `<camp>/agents/` layer only; packs configured from external directories
  are already packs and install in the city from their own sources.

## Importing into a city

1. `bd import <dir>/beads.jsonl` — issues and memories land in the
   city's bead store (memories become `bd remember` entries).
2. Install `pack/` with gc's pack tooling (`gc import add <source>`, then
   `gc import install`) — agents, orders, and the formulas those orders
   reference are discovered by gc's pack conventions.
3. `formulas/` is the historical archive of run-pinned copies; the pack
   already contains everything its orders need. Copy archive entries into
   a formula layer only if you want the historical versions available.

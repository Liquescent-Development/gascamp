# Compat Phase 2 — the formula key sets (rungs 2a–2e) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or
> superpowers:subagent-driven-development) to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make camp load, compile, and run the real Gas City formula corpus — 97 of the 100
formulas at `ci/gc-compat/GCPACKS_REF` — refusing the other 3 by name, with every §9 rung's
count pinned by the CI gate.

**Architecture:** camp's formula compiler today is a *strict subset* validator: it rejects every
Gas City construct by name (`parse.rs` `CITY_ONLY_TOP`/`CITY_ONLY_STEP`). Phase 2 inverts it into
a *permissive, layered compiler* that follows compat spec §4's three-rule permissiveness table.
The single-file `parse_and_validate(path)` grows a layered sibling, `compose::compile(&Layers, …)`,
which runs gc's pipeline order: parse → extends → expansion → vars → condition-prune →
description_file → route → validate → refuse. `drain` (2e) becomes a fourth arm of the existing
`GraphRuntime` beside check/retry/fanout, and gc's convoy maps onto camp's run.

**Tech Stack:** Rust (camp-core, camp), `toml` crate, SQLite ledger, Python 3 for the CI corpus
gate (`tomllib`, stdlib only — the mold `ci/gc-compat/load_corpus_packs.py` established in compat-1).

---

## Authority and provenance

| Rank | Document | What it decides here |
|---|---|---|
| 1 | `docs/design/2026-07-05-gas-camp-design.md` | The master spec. Its §4 decision record is settled. **This plan amends one line of it** (line 449) — Task 1. |
| 2 | `docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` (rev 4) | The contract: **§4** (permissiveness + three traps), **§9** (every rung's semantics), **§10** (the gate), **§12.2** (the phase). |
| 3 | `docs/superpowers/specs/2026-07-12-KNOWN-DEFECTS.md` | Its "Verified correct — do not re-litigate" list is settled. |
| 4 | `docs/superpowers/plans/2026-07-13-wave-2-compat-orchestration.md` | Branch, gates, shared-file protocol. |

`AGENTS.md` invariants bind every task. Invariant 5 (**fail fast, no fallbacks, no panics in
library code**) and invariant 6 (**camp ⊆ gc**) are the two that will bite; invariant 7
(vocabulary mirror) governs every new name.

**Every number in this plan was measured, not asserted.** Provenance: the corpus cloned at
`GCPACKS_REF = 44b2eef94f035283b70df62d3bd1fc77bce13d56` and gc's source at
`GASCITY_REF = 12410301884b51131a35e101a335dbaae16cdcb0`, walked with `tomllib` (never regex —
KNOWN-DEFECTS "two traps in measuring this corpus"). Re-derive any of them with
`ci/gc-compat/measure_corpus.py <corpus>`.

---

## Global Constraints

- **Branch:** `compat-2-formulas`. One reviewable PR. Never commit to main.
- **Gates before every push:** `cargo fmt --all --check` && `cargo clippy --workspace
  --all-targets --all-features -- -D warnings` && `cargo test --workspace`. Work is not complete
  until pushed and `gh pr checks --watch` is green.
- **TDD, strictly.** Write the failing test, run it, *watch it fail with the expected message*,
  implement, watch it pass. A task's "Run it and watch it fail" step is not ceremonial: if the
  test passes before the implementation, the test is wrong.
- **No panics in library code.** `unwrap_used` / `expect_used` / `panic` are clippy-denied in
  `camp-core` and `camp`; `unsafe_code` is forbidden. Tests may use them (the existing
  `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` on `mod tests`).
- **No network in `cargo test`.** The corpus is never vendored (compat §10: `gascity-packs` has
  no top-level LICENSE; gascamp is AGPL-3.0). Corpus assertions live **only** in the CI gate
  script, which fetches at the pinned ref. Unit tests use synthetic fixtures.
- **New events** must be added in lockstep to four places or the build/tests break:
  `EventType` enum + `EventType::ALL` + `EventType::as_str` (`crates/camp-core/src/event.rs`),
  a `match` arm in `fold::apply` (`crates/camp-core/src/ledger/fold.rs`), and
  `CAMP_SPECIFIC_EVENTS` (`crates/camp-core/src/vocab.rs`). Payload structs are private,
  `#[serde(deny_unknown_fields)]`, validated in the fold (the `check_passed` mold,
  `fold.rs:680-704`). Keep the one-transaction event+state property.
- **Shared files — keep every touch ADDITIVE and minimal, do not refactor.** A sibling stream
  (`cp-1-control-protocol`) is in flight and owns the socket/protocol/daemon-control surface.
  Contended: `crates/camp/src/daemon/dispatch.rs`, `crates/camp/src/daemon/event_loop.rs`,
  `crates/camp/src/main.rs`, `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`,
  `crates/camp-core/src/ledger/fold.rs`, `Cargo.toml`/`Cargo.lock`,
  `.github/workflows/ci.yml`. Expect a real rebase.
- **Commit style:** no co-author trailers, never mention the agent. Conventional prefixes
  (`feat(formula):`, `fix(...)`, `test(...)`, `ci(...)`, `docs(spec):`).

---

## The interpretive decisions this plan pins (read before Task 1)

Three of these will be attacked by an adversarial reviewer. Each is settled here, with its
arithmetic or its citation, so the implementer never has to guess.

### D1. "Corpus loading" (the §9 rung column) means COMPILES, not RUNNABLE. The ceiling is **97**.

compat §9: *"Permanently refused, and therefore the ceiling is below 100: `phase = "vapor"`
(2 formulas) and `scope-check` / `gc.scope_*` (1). The ceiling is 97–98, and the gate will say
exactly which."*

Measured at `GCPACKS_REF`:

| permanently refused | count | files |
|---|---|---|
| `phase = "vapor"` | 2 | `gastown/formulas/mol-digest-generate.toml`, `gastown/formulas/mol-pr-from-issue.formula.toml` |
| `scope-check` / `gc.scope_*` | 1 | `gascity/formulas/design-review.formula.toml` |
| **union (disjoint)** | **3** | ⇒ **ceiling = 100 − 3 = 97** |

**The gate says 97.** The spec's "97–98" was uncertainty about whether the two sets overlap.
They do not.

This forces the reading of "loading". The 21 no-contract formulas (§9's last bullet — *"Camp must
not run them under graph.v2 semantics by default. Refuse…"*) **must still be counted as loaded**,
because 100 − 3 − 21 = 76, and no reading that excludes them can reach a 97 ceiling. Therefore:

- **Compile** (what the rung count measures): camp parses the file, resolves `extends`, prunes
  conditions, resolves `description_file`, and reports every key it ignored and every key it
  refused. A no-contract formula **compiles**.
- **Runnable** (a separate, stricter verdict): only a formula declaring `contract = "graph.v2"`
  may be cooked and dispatched. **A no-contract formula is refused at `camp sling` / order-fire
  time, naming the missing `contract` key, and the refusal lands as a ledger event.** This is the
  phase block's *"the 21 no-contract formulas are refused, not assumed"* — refused from
  **running**, not from loading.

`camp doctor --formula` reports both verdicts (`ok` and `runnable`). Task 4 builds it; Task 4's
gate step asserts both.

### D2. The permissiveness rule is applied by KEY, not by ORIGIN.

compat §4 is unqualified: *"A consumer that refuses unknown keys is stricter than the reference
implementation and rejects packs that work."* Its three rules key off what the key *means in gc*,
not where the file came from:

1. **Semantics camp does not implement** → **refuse the formula, naming the key** (a ledger event).
2. **No semantics in Gas City** → **ignore, warn once.**
3. **Pure annotation** (`notes`, `catalog`, formula-level `metadata`) → **ignore silently.**

Camp cannot distinguish "a key gc has no semantics for" from "a typo" — they are the same thing to
any consumer. So rule 2 subsumes typos: **an unrecognised key is ignored with a warning, not a
hard error.** This is a deliberate, spec-mandated loosening of camp's current behaviour (the
existing test `parse.rs::unknown_keys_are_rejected_everywhere_gc_would_silently_ignore_them` is
**inverted** in Task 2 — see its "Interfaces / Breaking changes" block). Loudness is preserved,
not lost: ignored keys are reported by `camp doctor --formula`, and aggregated per import into
compat-1's existing `import.added` event field `ignored_keys` (one line per import, one event —
compat §5.4: *"Warnings are aggregated, not per-key"*).

### D3. gc's **convoy** is camp's **run**; a drain's members are the run's member beads.

gc: *"Drain scatters the input convoy into one-member unit convoys and runs an item formula for
each unit"* (`internal/formula/types.go:317-319`), and its member set is literally
`convoycore.Members(store, parentConvoyID, …)` (`internal/dispatch/drain.go:211`). camp's run is
camp's instantiation of a formula — the same object. So:

> **A run member** is a bead with `run_id = <the drain's run>` **and** `step_id IS NULL`, which is
> **not** the run root, and carries **no** `bond:` or `drain:` label.

That definition reuses machinery that already exists (`beads.run_id`, `beads.step_id`,
`beads.labels`, `flow::run_bead_ids`) and adds no column. Members are put into the run by a worker
that creates a bead into it — Task 3 adds `camp create --run <run_id>` (additive; compat-3's
`bd create` shim maps onto it, which is why the flag is named for the run and not for the drain).

---

## What this plan deliberately DEFERS (named, so nothing is a silent gap)

| Deferred | Why, with citation |
|---|---|
| `drain.max_units` | A gc key (`DrainSpec.MaxUnits`) whose semantics camp does not implement. **0 uses in the corpus.** §4 rule 1 ⇒ **refused by name**, not ignored. Task 8 pins the refusal. |
| `drain.continuation_group` | Valid only with `context = "shared"`, which camp refuses (§6.2, §11.4: *"camp does not honour `gc.continuation_group`"*). **Refused by name.** Task 8. |
| `context = "shared"` drains | §9, explicit: *"REFUSED, loudly"*. All 13 corpus shared drains sit behind `condition = "{{drain_policy}} == same-session"`, whose default is `separate` (declared in gascity's `build-base.formula.toml`), so the default path runs. Task 8. |
| `advice` / `pointcuts` | §9's `extends` bullet: *"`advice`/`pointcuts` are dropped entirely."* **0 corpus uses**, so the rung counts are identical either way. Dropped (ignored + warned), per the spec's own word. |
| `gate`, `loop`, `pour`, `compose`, `tally` | Not in §9's rung table and **0 corpus uses** each. `tally` was hard-removed from gc formula-v2 as well — its existing camp violation message (`parse.rs:324-329`) is preserved verbatim. The rest are §4 rule 1 refusals. |
| `commands/` | compat §5.3 — out of scope, reported as ignored at import (compat-1 already does this). |
| The bead's `gc.routed_to` / `gc.work_branch` stamping, `hook --claim`, the shims | **compat-3's contract** (§6.1, §6.2). Phase 2 builds the *bead metadata store* those need (Task 3) and routes the step to an `assignee`; it does not build the claim invariant. |
| `bd update --set-metadata` (the CLI verb) | compat-3 (§6 verb table). Task 3 ships the *event and fold* it will use, not the verb. |

---

## File Structure

**Created:**

| File | Responsibility |
|---|---|
| `crates/camp-core/src/formula/keys.rs` | The ONE key-classification table: accepted / refused / dead / annotation, **scoped by nesting** (§4's trap 1). Also the §9 rung table (`RUNGS`), which the CI gate reads. |
| `crates/camp-core/src/formula/layers.rs` | `FormulaLayers` — the ordered (lowest→highest) formula/asset search path, built from `CampConfig`. Wraps compat-1's `transitive_layers()` < `import_layers()` < local tiers. |
| `crates/camp-core/src/formula/compose.rs` | The layered compile pipeline: extends → expansion → vars → condition → description_file → route. The heart of 2a–2d. |
| `crates/camp-core/src/formula/drain.rs` | The `Drain` AST + its parse/validate/refusal rules (2e, compile side). |
| `crates/camp-core/tests/compose.rs` | Integration tests for the pipeline over synthetic multi-layer fixtures. |
| `crates/camp-core/tests/fixtures/compose/**` | Synthetic pack layers (a `bmad`-shaped child + a `gascity`-shaped parent). |
| `crates/camp/tests/cli_doctor_corpus.rs` | `camp doctor --formula --json` contract tests. |
| `crates/camp/tests/daemon_drain.rs` | The drain runtime end-to-end against a fake worker. |
| `ci/gc-compat/formula_rungs.py` | **The §10 gate.** Drives the real `camp` binary over the real corpus; asserts the per-rung counts and the 97/3 split. |

**Modified:**

| File | Change |
|---|---|
| `crates/camp-core/src/formula/parse.rs` | Permissive walk driven by `keys.rs`; new raw fields (`vars`, `extends`, `condition`, `metadata`, `description_file`, `expand*`, `children`, `type`, `template`, `contract`, `drain`). |
| `crates/camp-core/src/formula/ast.rs` | `Formula` gains `contract`, `vars`, `ignored_keys`; `Step` gains `metadata`, `route`, `drain`. New `Drain`, `DrainItem`, `Refusal`. |
| `crates/camp-core/src/formula/validate.rs` | S2 amended (`.formula.toml` stem); S11 amended (`contract` satisfies it); new S14–S18 for drain/condition/expansion. |
| `crates/camp-core/src/formula/mod.rs` | Re-exports; `compile` entry point. |
| `crates/camp-core/src/formula/cook.rs` | Writes step `metadata` and the resolved `assignee` (route) onto the step bead. |
| `crates/camp-core/src/formula/runtime.rs` | `run_members()`, `drain_label()`/`parse_drain_label()`, drain finalization — pure, write-free (the file's stated contract). |
| `crates/camp-core/src/ledger/schema.rs` | `bead_meta` table. |
| `crates/camp-core/src/ledger/fold.rs` | `bead.created.metadata`; `bead.updated.metadata` (set/unset) with the exclusive-reservation compare-and-set; `formula.refused` (log-only, validated). |
| `crates/camp-core/src/ledger/mod.rs` | `bead_metadata()` / `run_members()` read wrappers. |
| `crates/camp-core/src/event.rs`, `vocab.rs` | `EventType::FormulaRefused` → `"formula.refused"` (additive). |
| `crates/camp-core/src/readiness.rs` | `BeadRow` unchanged; add `run_members(conn, run_id)` query. **Do not touch `dispatchable_beads`** — `single_lane` is a `needs` edge, not a new predicate (see Task 9). |
| `crates/camp-core/src/orders/mod.rs` | `resolve_formula` keeps its signature; its doc comment's "does NOT compile extends/drain (phase 2)" line is now false — update it and route callers through `formula::compile`. |
| `crates/camp/src/cmd/doctor.rs`, `crates/camp/src/cmd/sling.rs`, `crates/camp/src/main.rs` | `--json` on doctor; the runnability refusal on sling; `camp create --run`. |
| `crates/camp/src/daemon/dispatch.rs` | **ADDITIVE ONLY:** `PendingDrain`, `pending_drains`, `queue_drain`, an `execute` loop, a `reconcile` pass. |
| `crates/camp-core/tests/refold_prop.rs` | `DUMPS` gains `bead_meta`. |
| `.github/workflows/ci.yml` | The `gcpacks-compat` job gains the `formula_rungs.py` step. |
| `docs/design/2026-07-05-gas-camp-design.md` | Line 449 — the S11 amendment (Task 1). |
| `docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` | §9 addendum — the ceiling is 97; S2/S11; D1/D2/D3 (Task 1). |

---

## The measured seed table (what the gate will assert)

Cumulative formulas whose **entire key set** is within the rung's cumulative accepted set, at
`GCPACKS_REF`, excluding the 3 permanently-refused:

| rung | key set added (§9) | **count** | still-blocked by |
|---|---|---|---|
| 2a | dead keys ignored; annotations; `contract`; `description_file` (53); step `metadata` (53, incl. `gc.run_target`) | **2** | `vars` (79), `extends` (48), `expand` (15), `type`/`template` (14), `drain` (13), `condition` (13) |
| 2b | `vars`, `condition` (13) | **31** | `extends` (48), `expand` (15), `type`/`template` (14), `drain` (13) |
| 2c | `extends` (48) | **58** | `expand` (15), `expand_vars` (14), `type`/`template` (14), `drain` (13), `children` (2) |
| 2d | `type`, `template`, `expand`, `expand_vars`, `children` | **84** | `drain` (13) |
| **2e** | **`drain` (13)** | **97** | — (**the ceiling**) |

These are **seeds**, in the spec's own sense (§10: *"The claimed numbers become tests, seeded by
`measure_corpus.py`"*). The gate derives them from **camp's own declared rung table**
(`formula::keys::RUNGS`, surfaced by `camp doctor --formula-rungs --json`) applied to the real
corpus — so the table cannot drift from the loader, because the gate **also** drives the real
binary over all 100 files and asserts the real per-file verdicts match the table's prediction at
the final rung. If a measured count differs from a seed, **stop and report to the lead** — do not
edit the seed to match. A seed that moves means either the pin moved or a rule is wrong.

Corroborating measurements (all from `measure_corpus.py` at `GCPACKS_REF`):
`contract = "graph.v2"` 79 / none 21 · `version` 93 · `target_required` 64 · `internal` 40 ·
top-level `mode` 7 · top-level `single_lane` 6 · `sling_container_mode` 1 · `catalog` 17 ·
formula-level `metadata` 16 · `gc.run_target` 187 occurrences across 53 formulas · **step
`assignee`: 0 occurrences** (routing is *exclusively* step metadata) · **0 bare route values,
corpus-wide** · 25 drain steps (12 `separate`, 13 `shared`; all 25 `member_access = "exclusive"`;
`on_item_failure` and `item.single_lane` appear **only** on the 13 shared ones) ·
all 10 formula-bearing packs have **no duplicate formula basenames** (so one camp root can import
all of them without a within-tier collision).

---

# Tasks

## Task 1: The spec amendments and the two rules that refuse 92% of the corpus

Camp's *existing* semantic rules refuse the corpus before a single Gas City key is considered.
This task fixes both, and — because both are stated at spec level — amends the specs in the same
change (`AGENTS.md`: *"spec and code never silently diverge"*).

**Measured:**
- **S2** (`validate.rs:34-50`, "formula name must equal the file stem"): the corpus names files
  `<name>.formula.toml`, so the stem is `bmad-build.formula` but the declared name is
  `bmad-build`. **92 of 100 formulas violate S2.** (compat-1's `orders::resolve_formula`
  *already* accepts both `<name>.toml` and `<name>.formula.toml` — the resolver and the
  validator disagree today.)
- **S11** (`validate.rs:178-191`, "a graph-only construct requires `[requires] formula_compiler`"):
  **only 4 of 100 formulas declare `[requires]` at all**, while **36 use `check`/`retry`/
  `on_complete`.** Master spec line 449 states this rule: *"camp requires the same contract
  declaration for graph-only constructs."* **Measured: all 36 declare `contract = "graph.v2"`** —
  gc's own compiler declaration. So the amendment is exact and costs nothing: `contract =
  "graph.v2"` satisfies S11, as does `[requires] formula_compiler`.

**Files:**
- Modify: `crates/camp-core/src/formula/validate.rs:34-50` (S2), `:178-191` (S11)
- Modify: `docs/design/2026-07-05-gas-camp-design.md:449`
- Modify: `docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` (§9 addendum)
- Test: `crates/camp-core/src/formula/validate.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub(crate) fn formula_stem(path: &Path) -> Option<&str>` in `validate.rs` — strips
  `.toml`, then an optional trailing `.formula`. Used by `validate::check` and, from Task 4, by
  `compose`.

- [ ] **Step 1: Write the failing tests**

In `crates/camp-core/src/formula/validate.rs`, inside `mod tests`:

```rust
#[test]
fn gc_corpus_file_naming_satisfies_the_stem_rule() {
    // 92 of the 100 corpus formulas are named `<name>.formula.toml` while
    // declaring `formula = "<name>"`. compat §9; orders::resolve_formula
    // already accepts both spellings.
    assert_eq!(formula_stem(std::path::Path::new("/p/bmad-build.formula.toml")), Some("bmad-build"));
    assert_eq!(formula_stem(std::path::Path::new("/p/mol-digest-generate.toml")), Some("mol-digest-generate"));
    // `.formula` is only stripped as a suffix, never as the whole stem
    assert_eq!(formula_stem(std::path::Path::new("/p/formula.toml")), Some("formula"));
}

#[test]
fn contract_graph_v2_satisfies_the_compiler_declaration_rule() {
    // S11 amended: master spec line 449. gc declares graph.v2 via `contract`;
    // 36 corpus formulas use check/retry and NONE declare [requires].
    let text = "formula = \"x\"\ncontract = \"graph.v2\"\n\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n\n[steps.retry]\nmax_attempts = 2\n";
    let (raw, mut v) = crate::formula::parse::walk(text);
    check(&raw, Some("x"), &mut v);
    assert!(
        !v.iter().any(|v| v.message.contains("formula_compiler")),
        "contract = \"graph.v2\" must satisfy S11: {v:?}"
    );
}

#[test]
fn a_graph_only_construct_with_neither_declaration_is_still_a_violation() {
    let text = "formula = \"x\"\n\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n\n[steps.retry]\nmax_attempts = 2\n";
    let (raw, mut v) = crate::formula::parse::walk(text);
    check(&raw, Some("x"), &mut v);
    assert!(
        v.iter().any(|v| v.message.contains("formula_compiler")),
        "S11 still fires when neither `contract` nor [requires] is declared: {v:?}"
    );
}
```

`walk` must return the `contract` value for this to work; if `RawFormula` has no `contract` field
yet, add `pub contract: Option<String>` to it and populate it in `parse::walk` in **this** task
(it is one line, and S11 cannot be tested without it). `check`'s signature is unchanged.

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test -p camp-core --lib formula::validate 2>&1 | tail -20
```
Expected: `gc_corpus_file_naming_satisfies_the_stem_rule` fails to compile (`formula_stem` not
found); after adding the stub, it fails on the `.formula.toml` case; and
`contract_graph_v2_satisfies_the_compiler_declaration_rule` fails with an S11 violation present.

- [ ] **Step 3: Implement**

In `validate.rs`:

```rust
/// The formula's canonical stem: the file name minus `.toml`, minus an
/// optional trailing `.formula`. The Gas City corpus names 92 of its 100
/// formulas `<name>.formula.toml` while declaring `formula = "<name>"`
/// (compat §9); compat-1's `orders::resolve_formula` already resolves both
/// spellings, so the validator must accept both too.
pub(crate) fn formula_stem(path: &Path) -> Option<&str> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".toml")?;
    Some(stem.strip_suffix(".formula").unwrap_or(stem))
}
```

Route S2's caller through it (the `stem: Option<&str>` argument to `check` is already computed by
`parse_and_validate` in `formula/mod.rs:24` — change *that* call site to use `formula_stem`).

Amend S11 (`validate.rs:178-191`): the "declares the compiler" predicate becomes

```rust
let declares_compiler =
    raw.formula_compiler.is_some() || raw.contract.as_deref() == Some("graph.v2");
```

and the violation message becomes:

```
"a graph-only construct (`check`/`retry`/`on_complete`/`drain`) requires a compiler declaration: \
 either `contract = \"graph.v2\"` (Gas City's spelling) or `[requires] formula_compiler` \
 (master spec §8.2 as amended by compat §9)"
```

(`drain` is named now; Task 8 adds `has_drain` to the predicate that triggers S11.)

- [ ] **Step 4: Run and watch them pass**

```bash
cargo test -p camp-core --lib formula:: 2>&1 | tail -10
```
Expected: PASS. Existing `validate.rs` tests that assert the old S11 message must be updated to
the new message — do that here, not later.

- [ ] **Step 5: Amend the specs**

`docs/design/2026-07-05-gas-camp-design.md` line 449 — replace the cell

> `| `formula`, `description`, `[requires] formula_compiler = ">=2.0.0"` | file header; camp requires the same contract declaration for graph-only constructs |`

with

> `| `formula`, `description`, `contract = "graph.v2"` or `[requires] formula_compiler = ">=2.0.0"` | file header; camp requires a compiler declaration for graph-only constructs — **either** Gas City's `contract = "graph.v2"` **or** the `[requires]` form. *(Amended 2026-07-13 by compat phase 2: 36 of the 100 corpus formulas use graph-only constructs and none declares `[requires]`; all 36 declare `contract`.)* |`

`docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` — append to §9, before
the "Semantics an implementer must get right" list:

```markdown
**§9 addendum (compat phase 2, 2026-07-13) — measured at `GCPACKS_REF`:**

- **The ceiling is 97, and this is the gate's number.** `phase = "vapor"` (2:
  `mol-digest-generate.toml`, `mol-pr-from-issue.formula.toml`) and `scope-check` /
  `gc.scope_*` (1: `design-review.formula.toml`) are disjoint sets, so 100 − 3 = 97.
- **"Corpus loading" means COMPILES, not RUNNABLE.** The 21 no-contract formulas compile (they
  are inside the 97) and are refused at *run* time — `camp sling` and order-fire refuse them,
  naming the missing `contract` key, with a `formula.refused` ledger event. Any reading that
  excluded them from the load count would cap the ceiling at 76 and contradict this section's
  own 97–98.
- **Two camp-local rules were refusing the corpus and are amended:** the file-stem rule now
  strips an optional trailing `.formula` (92/100 corpus files are `<name>.formula.toml`), and
  the compiler-declaration rule is satisfied by `contract = "graph.v2"` as well as by
  `[requires] formula_compiler` (master spec line 449, amended in the same change; all 36
  corpus formulas using graph-only constructs declare `contract` and none declares `[requires]`).
- **The permissiveness rule (§4) is applied by KEY, not by origin.** An unrecognised key is
  ignored-and-warned (rule 2) for camp-local formulas too: camp cannot distinguish "a key gc has
  no semantics for" from a typo. Ignored keys surface via `camp doctor --formula` and, per
  import, in `import.added`'s `ignored_keys` (§5.4's aggregation).
- **gc's convoy is camp's run.** A drain's members (`convoycore.Members`, gc
  `internal/dispatch/drain.go:211`) are the run's member beads: `run_id = <run>`,
  `step_id IS NULL`, not the run root, no `bond:`/`drain:` label. `camp create --run <run_id>`
  puts a bead into a run.
- **Deferred, by name:** `drain.max_units` and `drain.continuation_group` are refused (§4 rule 1;
  0 corpus uses); `advice`/`pointcuts` are dropped (§9's own word; 0 corpus uses).
```

- [ ] **Step 6: Full gates and commit**

```bash
cargo fmt --all && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "fix(formula): the stem and compiler-declaration rules accept the gc corpus (compat 2)"
```
Expected: all green. If any *other* test broke, it is asserting the old rules — fix it here.

---

## Task 2: The key-classification table — §4's permissiveness rule and its three traps

**Files:**
- Create: `crates/camp-core/src/formula/keys.rs`
- Modify: `crates/camp-core/src/formula/parse.rs` (replace `CITY_ONLY_TOP`/`CITY_ONLY_STEP`/
  `ACCEPTED_TOP`/`ACCEPTED_STEP`, lines 42-87, and the `sorted_keys` loops at :228-236 and :318-335)
- Modify: `crates/camp-core/src/formula/ast.rs` (`Refusal`; `Formula.ignored_keys`)
- Modify: `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`,
  `crates/camp-core/src/ledger/fold.rs` (the `formula.refused` event)
- Test: `crates/camp-core/src/formula/keys.rs` (`mod tests`), `crates/camp-core/src/formula/parse.rs`

**Interfaces:**
- Consumes: Task 1's `RawFormula.contract`.
- Produces:

```rust
// crates/camp-core/src/formula/keys.rs

/// Where a key sits. §4 trap 1 — "Key off *nesting*, never name":
/// top-level `mode`/`single_lane` are DEAD, while `steps.check.check.mode`
/// and `steps.drain.item.single_lane` are load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site { Top, Step, Check, CheckInner, Retry, OnComplete, Drain, DrainItem }

/// §4's three rules, plus the keys camp implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// camp implements it.
    Accepted,
    /// gc has semantics camp does not implement → REFUSE, naming the key (rule 1).
    Refused,
    /// gc has no semantics for it → ignore, warn once (rule 2). Includes typos.
    Dead,
    /// Pure annotation → ignore silently (rule 3).
    Annotation,
}

/// The §9 rungs, in order. `RUNGS` is the single source of truth for which
/// rung introduced which key; `camp doctor --formula-rungs --json` emits it
/// and `ci/gc-compat/formula_rungs.py` asserts the corpus counts against it.
pub const RUNGS: &[Rung] = &[ /* 2a..2e — see Step 3 */ ];

#[derive(Debug, Clone, Copy)]
pub struct Rung { pub id: &'static str, pub top: &'static [&'static str], pub step: &'static [&'static str] }

pub fn classify(site: Site, key: &str) -> Class;
```

```rust
// crates/camp-core/src/formula/ast.rs

/// A key camp refuses (§4 rule 1) — the formula does not compile, and the
/// refusal names the key. Distinct from `Violation`, which is a shape or
/// semantic error.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    /// The key's location, e.g. "steps.implement.drain.context".
    pub construct: String,
    /// The key itself, e.g. "context" — what the ledger event names.
    pub key: String,
    pub reason: String,
}
```

**Breaking changes to existing tests — deliberate, per D2:**
- `parse.rs::unknown_keys_are_rejected_everywhere_gc_would_silently_ignore_them` is **inverted**
  into `unknown_keys_are_ignored_and_reported_never_fatal` (§4 rule 2). Keep the `dependson` typo
  case — it now asserts the key appears in `ignored_keys` and produces **no** violation.
- `parse.rs::every_city_only_key_is_rejected_by_name_with_a_city_pointer` is **replaced** by
  `keys.rs::classify_matches_the_permissiveness_table`, which asserts the *class* of every key in
  §4's tables. The `tally` message (`parse.rs:324-329`) survives verbatim as a `Refused` reason.
- `parse.rs::walk_collects_all_violations_not_just_the_first` uses `vars`/`pour`/`tags`; rewrite
  its input to use keys that are still violations (a wrong *type*, e.g. `steps = 3`).

- [ ] **Step 1: Write the failing test**

New file `crates/camp-core/src/formula/keys.rs`, with `mod tests`:

```rust
#[test]
fn classify_matches_the_permissiveness_table() {
    use Class::*;
    use Site::*;
    // §4 rule 2 — DEAD in gc (93/100 formulas name at least one). Ignore, warn.
    for k in ["version", "target_required", "internal", "mode", "single_lane", "sling_container_mode"] {
        assert_eq!(classify(Top, k), Dead, "top {k}");
    }
    // §4 trap 1 — the SAME names are load-bearing when nested.
    assert_eq!(classify(CheckInner, "mode"), Accepted, "steps.check.check.mode is 49 uses of exec");
    assert_eq!(classify(DrainItem, "single_lane"), Accepted, "steps.drain.item.single_lane throttles");
    // §4 trap 3 — step metadata is ROUTING, not annotation.
    assert_eq!(classify(Step, "metadata"), Accepted);
    // ...but formula-level metadata IS an annotation (rule 3).
    assert_eq!(classify(Top, "metadata"), Annotation);
    for k in ["notes", "catalog"] {
        assert_eq!(classify(Top, k), Annotation, "top {k}");
    }
    // §4 rule 1 — gc semantics camp does not implement.
    for k in ["gate", "loop", "pour", "compose", "tally"] {
        assert_eq!(classify(Step, k), Refused, "step {k}");
    }
    // Rule 2 subsumes typos: gc silently ignores `dependson`, so camp warns.
    assert_eq!(classify(Step, "dependson"), Dead);
    assert_eq!(classify(Top, "wat"), Dead);
}

#[test]
fn every_rung_key_is_accepted_and_the_rungs_cover_section_9() {
    // The rung table is the gate's source of truth — it may never contain a
    // key the classifier does not accept.
    let ids: Vec<&str> = RUNGS.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec!["2a", "2b", "2c", "2d", "2e"]);
    for r in RUNGS {
        for k in r.top { assert_eq!(classify(Site::Top, k), Class::Accepted, "rung {} top {k}", r.id); }
        for k in r.step { assert_eq!(classify(Site::Step, k), Class::Accepted, "rung {} step {k}", r.id); }
    }
    // §9's key sets, verbatim.
    let rung = |id| RUNGS.iter().find(|r| r.id == id).expect("rung");
    assert!(rung("2a").step.contains(&"description_file") && rung("2a").step.contains(&"metadata"));
    assert!(rung("2a").top.contains(&"contract"));
    assert!(rung("2b").top.contains(&"vars") && rung("2b").step.contains(&"condition"));
    assert!(rung("2c").top.contains(&"extends"));
    assert!(rung("2d").top.contains(&"type") && rung("2d").top.contains(&"template"));
    assert!(rung("2d").step.contains(&"expand") && rung("2d").step.contains(&"expand_vars")
            && rung("2d").step.contains(&"children"));
    assert_eq!(rung("2e").step, &["drain"]);
}
```

And in `parse.rs`'s `mod tests`:

```rust
#[test]
fn unknown_keys_are_ignored_and_reported_never_fatal() {
    // §4 rule 2 (D2): gc silently drops a `dependson` typo. camp ignores it
    // too — and REPORTS it. A consumer stricter than the reference
    // implementation rejects packs that work.
    let text = "formula = \"x\"\nbogus = 1\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ndependson = [\"b\"]\n";
    let (raw, v) = walk(text);
    assert!(v.is_empty(), "unknown keys must not be violations: {v:?}");
    assert!(raw.ignored_keys.contains(&"bogus".to_owned()), "{:?}", raw.ignored_keys);
    assert!(raw.ignored_keys.contains(&"dependson".to_owned()), "{:?}", raw.ignored_keys);
}

#[test]
fn dead_keys_from_the_corpus_are_ignored_not_refused() {
    // 93 of 100 corpus formulas name at least one dead key (§4).
    let text = "formula = \"x\"\nversion = \"1\"\ntarget_required = true\ninternal = true\n\
                mode = \"solo\"\nsingle_lane = true\nsling_container_mode = \"x\"\n\
                [[steps]]\nid = \"a\"\ntitle = \"t\"\n";
    let (raw, v) = walk(text);
    assert!(v.is_empty(), "{v:?}");
    assert!(raw.refusals.is_empty(), "{:?}", raw.refusals);
    assert_eq!(raw.ignored_keys.len(), 6, "{:?}", raw.ignored_keys);
}

#[test]
fn a_refused_key_names_itself() {
    // §4 rule 1 + §5.4: "Every refusal names the pack, the agent, and the key".
    let text = "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ngate = { path = \"x\" }\n";
    let (raw, _) = walk(text);
    let r = raw.refusals.iter().find(|r| r.key == "gate").expect("gate refusal");
    assert!(r.construct.contains("steps.a"), "{}", r.construct);
    assert!(r.reason.contains("gate"), "{}", r.reason);
}

#[test]
fn annotations_are_silent() {
    let text = "formula = \"x\"\nnotes = \"hi\"\n[catalog]\nx = 1\n[metadata]\ny = \"z\"\n\
                [[steps]]\nid = \"a\"\ntitle = \"t\"\n";
    let (raw, v) = walk(text);
    assert!(v.is_empty(), "{v:?}");
    assert!(raw.ignored_keys.is_empty(), "annotations are ignored SILENTLY: {:?}", raw.ignored_keys);
}
```

- [ ] **Step 2: Run and watch them fail**

```bash
cargo test -p camp-core --lib formula:: 2>&1 | tail -20
```
Expected: compile errors (`keys` module missing, `RawFormula.ignored_keys` / `.refusals` missing).

- [ ] **Step 3: Implement `keys.rs`**

```rust
//! The ONE key-classification table (compat §4). Three rules and three traps:
//!  1. Semantics camp does not implement       → Refused (name the key).
//!  2. No semantics in Gas City                → Dead (ignore, warn once).
//!  3. Pure annotation                         → Annotation (ignore silently).
//! Trap 1: key off NESTING, never name (`mode` is dead at the top and
//!         load-bearing at `steps.check.check`).
//! Trap 2: `target_required` looks semantic and is not — it is never read.
//! Trap 3: step `metadata` is ROUTING (`gc.run_target`), not annotation.

// (Site, Class, Rung, RUNGS as in the Interfaces block.)

/// gc parses formulas with plain `toml.Unmarshal` — no unknown-field check
/// (`parser.go:233`). These keys appear in the corpus and are in NO gc
/// struct: 93 of 100 formulas name at least one.
const DEAD_TOP: &[&str] = &["version", "target_required", "internal", "mode", "single_lane", "sling_container_mode"];
const ANNOTATION_TOP: &[&str] = &["notes", "catalog", "metadata"];
const ANNOTATION_STEP: &[&str] = &["notes", "tags", "priority"];

/// gc HAS semantics for these; camp does not implement them (§4 rule 1).
/// 0 corpus uses each — see the plan's "deliberately DEFERS" table.
const REFUSED_TOP: &[&str] = &["pour", "compose", "advice", "pointcuts"];
const REFUSED_STEP: &[&str] = &["gate", "loop", "tally", "waits_for", "depends_on"];

pub fn classify(site: Site, key: &str) -> Class {
    match site {
        Site::Top => {
            if accepted_top(key) { Class::Accepted }
            else if REFUSED_TOP.contains(&key) { Class::Refused }
            else if ANNOTATION_TOP.contains(&key) { Class::Annotation }
            else { Class::Dead }   // includes DEAD_TOP and every unknown key
        }
        Site::Step => {
            if accepted_step(key) { Class::Accepted }
            else if REFUSED_STEP.contains(&key) { Class::Refused }
            else if ANNOTATION_STEP.contains(&key) { Class::Annotation }
            else { Class::Dead }
        }
        Site::CheckInner => if ["mode", "path", "timeout"].contains(&key) { Class::Accepted } else { Class::Dead },
        Site::Check      => if ["check", "max_attempts"].contains(&key) { Class::Accepted } else { Class::Dead },
        Site::Retry      => if ["max_attempts", "on_exhausted"].contains(&key) { Class::Accepted } else { Class::Dead },
        Site::OnComplete => if ["bond", "for_each", "parallel", "sequential", "vars"].contains(&key) { Class::Accepted } else { Class::Dead },
        // Task 8 fills these; until then both are `Class::Dead` for every key.
        Site::Drain      => Class::Dead,
        Site::DrainItem  => Class::Dead,
    }
}
```

`accepted_top` / `accepted_step` are the base camp keys (`description`, `formula`, `requires`,
`steps` / `assignee`, `check`, `description`, `id`, `needs`, `on_complete`, `retry`, `timeout`,
`title`) **plus every key in every `RUNGS` entry**. Build `RUNGS` now with the full §9 table, even
though the *semantics* land in Tasks 4–8 — the table is data, and Task 4's gate needs it:

```rust
pub const RUNGS: &[Rung] = &[
    Rung { id: "2a", top: &["contract"],               step: &["description_file", "metadata"] },
    Rung { id: "2b", top: &["vars"],                   step: &["condition"] },
    Rung { id: "2c", top: &["extends"],                step: &[] },
    Rung { id: "2d", top: &["type", "template"],       step: &["expand", "expand_vars", "children"] },
    Rung { id: "2e", top: &[],                         step: &["drain"] },
];
```

**Warning — a key accepted by the table but not yet implemented by the pipeline must not silently
compile to nothing.** Until its rung's task lands, `parse::walk` records the key in
`RawFormula.unimplemented` and `validate::check` turns that into a hard `Violation`
("`vars` is accepted at rung 2b and not yet implemented"). Each of Tasks 4–8 removes its own key
from `unimplemented` as it implements it. Task 8's final state: `unimplemented` is empty and the
field is deleted. This is what keeps every intermediate commit honest and every intermediate rung
count real.

- [ ] **Step 4: Implement the parse-side changes**

`RawFormula` gains:

```rust
pub contract: Option<String>,               // Task 1
pub ignored_keys: Vec<String>,              // Dead keys, deduped, sorted — rule 2's "warn once"
pub refusals: Vec<crate::formula::ast::Refusal>,   // rule 1
pub unimplemented: Vec<String>,             // accepted-by-table, not-yet-implemented (temporary)
```

Replace the two key loops (`parse.rs:228-236`, `:318-335`) with a shared helper:

```rust
fn walk_keys(site: keys::Site, table: &toml::Table, at: &dyn Fn(&str) -> String,
             ignored: &mut Vec<String>, refusals: &mut Vec<Refusal>) {
    for key in sorted_keys(table) {
        match keys::classify(site, key) {
            keys::Class::Accepted => {}
            keys::Class::Annotation => {}                       // rule 3: silent
            keys::Class::Dead => ignored.push(key.to_owned()),  // rule 2: warn once
            keys::Class::Refused => refusals.push(Refusal {     // rule 1: name the key
                construct: at(key),
                key: key.to_owned(),
                reason: refusal_reason(site, key),
            }),
        }
    }
}
```

`refusal_reason` returns the `tally` message verbatim from `parse.rs:324-329` for `tally`, and
otherwise: `"`{key}` is a Gas City construct camp does not implement; camp refuses rather than
silently approximating (compat §4 rule 1)"`. Dedupe+sort `ignored_keys` at the end of `walk`.

- [ ] **Step 5: Add the `formula.refused` event**

Verified: gc's pinned vocabulary (`crates/camp-core/tests/fixtures/gc-vocab.json`, 71 events) has
**no** `formula.*` event, so this name is safely camp-specific and cannot collide
(`ci/gc-compat/check_vocab.sh` enforces that). Verified: `vocab.rs`'s
`no_reservation_vocabulary_exists` scans **event names only** — so no event may ever be named
`drain.reserved` (Task 9 depends on this).

- `event.rs`: `EventType::FormulaRefused` in the enum, in `ALL`, and `as_str` → `"formula.refused"`.
- `vocab.rs`: add `"formula.refused"` to `CAMP_SPECIFIC_EVENTS`.
- `fold.rs`: a log-only validating arm on the `check_passed` mold:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormulaRefused {
    formula: String,
    path: String,
    key: String,
    construct: String,
    reason: String,
}

/// Log-only (no state fold): a formula camp refused to compile or to run,
/// naming the key. compat §4 rule 1, §5.4 ("Every refusal ... appends a
/// ledger event. Never silently skipped").
fn formula_refused(_conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: FormulaRefused = payload(event)?;
    non_empty(event, "formula", &p.formula)?;
    non_empty(event, "key", &p.key)?;
    non_empty(event, "reason", &p.reason)?;
    Ok(())
}
```

Add the `EventType::FormulaRefused => formula_refused(conn, event)` arm to `fold::apply`. **No
`beads`/`DUMPS` change** — a log-only event keeps the refold property trivially green.

- [ ] **Step 6: Run every test and watch them pass**

```bash
cargo test -p camp-core 2>&1 | tail -20
cargo test -p camp-core --test vocab_pin 2>&1 | tail -5
cargo test -p camp-core --test refold_prop 2>&1 | tail -5
```
Expected: PASS. `vocab_pin::every_event_type_is_declared_mirrored_or_camp_specific_never_both` is
the one that catches a missed `vocab.rs` entry; `no_reservation_vocabulary_exists` must stay green.

- [ ] **Step 7: Full gates and commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "feat(formula): the permissiveness rule — one key table, three rules, three traps (compat §4)"
```

---

## Task 3: Bead metadata — the store `gc.exclusive_drain_reservation` and `gc.run_target` need

camp has **no bead metadata** today (the `beads` table has 20 columns and none is a metadata blob).
§9 requires the drain reservation to be stored *"where gc stores it: as metadata on the member
bead (`gc.exclusive_drain_reservation`, `beadmeta/keys.go:93`)"*, verbatim. §4's trap 3 requires
step `metadata` to survive onto the bead. compat-3 will need `gc.routed_to` and `gc.work_branch`
through the same door. Build the door once.

**Files:**
- Modify: `crates/camp-core/src/ledger/schema.rs` (the `bead_meta` table)
- Modify: `crates/camp-core/src/ledger/fold.rs` (`BeadCreated.metadata`; `BeadUpdated.metadata`)
- Modify: `crates/camp-core/src/ledger/mod.rs` (`bead_metadata` read wrapper)
- Modify: `crates/camp-core/tests/refold_prop.rs` (`DUMPS`)
- Modify: `crates/camp/src/main.rs`, `crates/camp/src/cmd/create.rs` (`camp create --run`)
- Test: `crates/camp-core/src/ledger/fold.rs` (`mod tests`), `crates/camp-core/tests/refold_prop.rs`

**Interfaces:**
- Consumes: nothing from Task 2.
- Produces:

```rust
// crates/camp-core/src/ledger/mod.rs
/// Every metadata key/value on a bead (gc's `bd show --json` metadata map).
pub fn bead_metadata(&self, bead: &str) -> Result<BTreeMap<String, String>, CoreError>;

// crates/camp-core/src/readiness.rs — pure, connection-scoped (used inside the cursor txn)
pub fn bead_metadata(conn: &Connection, bead: &str) -> Result<BTreeMap<String, String>, CoreError>;

/// gc's `gc.exclusive_drain_reservation` (beadmeta/keys.go:93), mirrored
/// VERBATIM (invariant 7). The value is the reserving drain's anchor bead id.
pub const EXCLUSIVE_DRAIN_RESERVATION: &str = "gc.exclusive_drain_reservation";
```

Event shape (additive; both events already exist):

```jsonc
// bead.created  — `metadata` is new, defaults to {}
{ "title": "...", "type": "task", "metadata": { "gc.run_target": "gc.run-operator" }, ... }

// bead.updated  — `metadata` is new. A null value UNSETS the key.
// The existing "must set title and/or description" check becomes
// "must set at least one of title, description, metadata".
{ "metadata": { "gc.exclusive_drain_reservation": "cmp-12" } }   // set
{ "metadata": { "gc.exclusive_drain_reservation": null } }        // release
```

**The compare-and-set lives in the FOLD, not in a read-then-write.** §9: *"A second drain
reserving a held member **fails the reserving drain loudly** — never two drains mutating one
bead."* A read-then-append is a TOCTOU race. The fold runs inside the same transaction as the
event insert (invariant 3, `Ledger::append` / `append_on`), so putting the guard there makes the
reservation atomic by construction, and keeps `append(input).is_ok()` a deterministic function of
prior state — which is exactly what `refold_prop`'s acceptance-determinism property demands.

- [ ] **Step 1: Write the failing tests**

In `crates/camp-core/src/ledger/fold.rs`'s `mod tests`:

```rust
#[test]
fn bead_created_carries_metadata_and_bead_updated_sets_and_unsets_it() {
    let (mut l, _t) = ledger();
    l.append(bead_created_input("b-1", json!({ "gc.run_target": "gc.run-operator" }))).unwrap();
    assert_eq!(l.bead_metadata("b-1").unwrap().get("gc.run_target").map(String::as_str),
               Some("gc.run-operator"));
    l.append(EventInput { kind: EventType::BeadUpdated, rig: None, actor: "t".into(),
        bead: Some("b-1".into()),
        data: json!({ "metadata": { "gc.work_branch": "camp/b-1" } }) }).unwrap();
    let m = l.bead_metadata("b-1").unwrap();
    assert_eq!(m.get("gc.work_branch").map(String::as_str), Some("camp/b-1"));
    assert_eq!(m.get("gc.run_target").map(String::as_str), Some("gc.run-operator"), "set must not clobber");
    // null unsets
    l.append(EventInput { kind: EventType::BeadUpdated, rig: None, actor: "t".into(),
        bead: Some("b-1".into()),
        data: json!({ "metadata": { "gc.work_branch": null } }) }).unwrap();
    assert!(!l.bead_metadata("b-1").unwrap().contains_key("gc.work_branch"));
}

#[test]
fn a_second_drain_cannot_reserve_a_held_member() {
    // compat §9: "A second drain reserving a held member fails the reserving
    // drain loudly — never two drains mutating one bead."
    let (mut l, _t) = ledger();
    l.append(bead_created_input("m-1", json!({}))).unwrap();
    let reserve = |who: &str| EventInput {
        kind: EventType::BeadUpdated, rig: None, actor: "campd".into(), bead: Some("m-1".into()),
        data: json!({ "metadata": { EXCLUSIVE_DRAIN_RESERVATION: who } }),
    };
    l.append(reserve("drain-a")).unwrap();
    let err = l.append(reserve("drain-b")).unwrap_err();
    assert!(err.to_string().contains("gc.exclusive_drain_reservation"), "{err}");
    assert!(err.to_string().contains("drain-a"), "the holder must be named: {err}");
    // Re-reserving by the SAME holder is idempotent (crash-recovery replays it).
    l.append(reserve("drain-a")).unwrap();
    // Release, then a second drain may take it.
    l.append(EventInput { kind: EventType::BeadUpdated, rig: None, actor: "campd".into(),
        bead: Some("m-1".into()),
        data: json!({ "metadata": { EXCLUSIVE_DRAIN_RESERVATION: null } }) }).unwrap();
    l.append(reserve("drain-b")).unwrap();
}

#[test]
fn bead_updated_still_requires_at_least_one_field() {
    let (mut l, _t) = ledger();
    l.append(bead_created_input("b-1", json!({}))).unwrap();
    let err = l.append(EventInput { kind: EventType::BeadUpdated, rig: None, actor: "t".into(),
        bead: Some("b-1".into()), data: json!({}) }).unwrap_err();
    assert!(err.to_string().contains("title"), "{err}");
}
```

(`ledger()` and `bead_created_input` follow the existing helpers in that `mod tests`; add
`bead_created_input(id, metadata)` beside them.)

In `crates/camp-core/tests/refold_prop.rs`, extend `DUMPS`:

```rust
("bead_meta", "bead_id, key, value"),
```

- [ ] **Step 2: Run and watch them fail**

```bash
cargo test -p camp-core --lib ledger::fold 2>&1 | tail -20
cargo test -p camp-core --test refold_prop 2>&1 | tail -5
```
Expected: FAIL — `bead_metadata` not found; `no such table: bead_meta`.

- [ ] **Step 3: Implement the schema**

In `schema.rs`, after the `beads` table in `STATE_DDL`:

```sql
CREATE TABLE bead_meta (
  bead_id TEXT NOT NULL REFERENCES beads(id),
  key     TEXT NOT NULL,
  value   TEXT NOT NULL,
  PRIMARY KEY (bead_id, key)
) STRICT;
CREATE INDEX bead_meta_key ON bead_meta(key, value);
```

The index exists for one query that Task 9 runs on every drain expansion: "is this member reserved
by anyone?" — and, in `camp doctor`, "who holds reservations?".

- [ ] **Step 4: Implement the fold**

`BeadCreated` gains `#[serde(default)] metadata: BTreeMap<String, String>` — inserted after the
bead row, in the same transaction. `BeadUpdated` gains
`#[serde(default)] metadata: BTreeMap<String, Option<String>>`; the emptiness check becomes
`if p.title.is_none() && p.description.is_none() && p.metadata.is_empty()`. Then:

```rust
for (key, value) in &p.metadata {
    match value {
        None => { conn.execute("DELETE FROM bead_meta WHERE bead_id = ?1 AND key = ?2", (id, key))?; }
        Some(v) => {
            // compat §9: an exclusive drain reservation is a compare-and-set,
            // enforced HERE so it is atomic with the event insert — a
            // read-then-append would be a TOCTOU race between two drains.
            if key == crate::readiness::EXCLUSIVE_DRAIN_RESERVATION {
                if let Some(holder) = current_meta(conn, id, key)? {
                    if holder != *v {
                        return Err(CoreError::InvalidEventData {
                            event_type: event.kind.as_str().to_owned(),
                            reason: format!(
                                "bead {id}: {key} is already held by {holder:?}; \
                                 a second drain may not reserve a held member (compat §9)"
                            ),
                        });
                    }
                }
            }
            conn.execute(
                "INSERT INTO bead_meta (bead_id, key, value) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(bead_id, key) DO UPDATE SET value = excluded.value",
                (id, key, v),
            )?;
        }
    }
}
```

`current_meta` is a two-line private helper. Note the guard is keyed on the **reservation key
alone** — every other metadata key is a plain upsert, which is what compat-3's
`bd update --set-metadata` needs.

- [ ] **Step 5: `camp create --run <run_id>` (the member door)**

`bead.created` already carries `run_id` (`fold.rs:94`) — only the CLI cannot set it. In
`main.rs`'s `Create` variant add:

```rust
/// Add this bead to a formula run as a MEMBER (gc's convoy member). A
/// drain step scatters the run's members (compat §9).
#[arg(long)]
run: Option<String>,
```

and thread it into the `bead.created` payload in `cmd/create.rs`. **Fail fast** if the run does not
exist: `bail!("no such run: {run} — camp create --run takes a run id (see camp events --type run.cooked)")`.

- [ ] **Step 6: Run every test and watch them pass**

```bash
cargo test -p camp-core 2>&1 | tail -20
cargo test -p camp-core --test refold_prop 2>&1 | tail -8   # the property must be green with bead_meta in DUMPS
cargo test -p camp 2>&1 | tail -10
```

- [ ] **Step 7: Full gates and commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "feat(ledger): bead metadata — gc's key/value store, with the exclusive-reservation compare-and-set"
```

---

## Task 4: Rung 2a — the layered compiler, `description_file`, routing, and the gate

The biggest task: it builds the pipeline every later rung plugs into, and the CI gate that pins
every rung's count.

**Files:**
- Create: `crates/camp-core/src/formula/layers.rs`, `crates/camp-core/src/formula/compose.rs`
- Create: `crates/camp-core/tests/compose.rs`, `crates/camp-core/tests/fixtures/compose/**`
- Create: `crates/camp/tests/cli_doctor_corpus.rs`
- Create: `ci/gc-compat/formula_rungs.py`
- Modify: `crates/camp-core/src/formula/mod.rs`, `ast.rs`, `parse.rs`, `cook.rs`
- Modify: `crates/camp-core/src/orders/mod.rs` (`resolve_formula`'s stale doc comment)
- Modify: `crates/camp/src/cmd/doctor.rs`, `crates/camp/src/cmd/sling.rs`, `crates/camp/src/main.rs`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `keys::{classify, RUNGS, Site, Class}` (Task 2); `Refusal` (Task 2);
  `bead_metadata` (Task 3); compat-1's `CampConfig::import_layers()` /
  `transitive_layers()` (`crates/camp-core/src/config.rs:258,273`) and
  `orders::resolve_formula(cfg, name) -> PathBuf` (`orders/mod.rs:272`);
  `pack::resolve_agent(cfg, name)` (`pack.rs:251`) for the binding check.
- Produces:

```rust
// crates/camp-core/src/formula/layers.rs

/// The formula/asset search path, ordered LOWEST → HIGHEST priority — the
/// same tiering compat-1 pinned: transitive imports < direct imports < the
/// camp-local `<root>/formulas/`. gc's `Parser.searchPaths` has this exact
/// order and its `winningAssetPath` takes the LAST match
/// (gascity `internal/formula/parser.go:855-873`).
#[derive(Debug, Clone)]
pub struct FormulaLayers { layers: Vec<(String, PathBuf)> }   // (binding, layer dir)

impl FormulaLayers {
    pub fn from_config(cfg: &CampConfig, root: &Path) -> Result<Self, CoreError>;
    /// The file for a bare formula name, highest layer wins. Accepts both
    /// `<name>.toml` and `<name>.formula.toml` (compat-1's rule).
    pub fn formula_path(&self, name: &str) -> Result<PathBuf, CoreError>;
    /// gc's asset shadowing: a `../assets/<rel>` reference resolves against
    /// EVERY layer's `assets/` dir, highest wins; anything else resolves
    /// against `base_dir` (the formula file's own directory).
    pub fn asset_path(&self, raw: &str, base_dir: &Path) -> Option<PathBuf>;
}

// crates/camp-core/src/formula/compose.rs

/// The compiled verdict on one formula file. `ok` and `runnable` are
/// DIFFERENT questions (plan D1): a no-contract formula compiles and is not
/// runnable.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub formula: Formula,
    /// Dead/unknown keys, deduped and sorted (§4 rule 2 — "warn once").
    pub ignored_keys: Vec<String>,
    /// Empty ⇒ it compiled. Non-empty ⇒ it did not (§4 rule 1).
    pub refusals: Vec<Refusal>,
    /// None ⇒ runnable. Some(refusal) ⇒ compiles, must never be cooked.
    pub not_runnable: Option<Refusal>,
}

/// Compile a formula file through the layers: parse → extends → expansion →
/// vars → condition-prune → description_file → route → validate.
pub fn compile(layers: &FormulaLayers, cfg: &CampConfig, path: &Path)
    -> Result<Compiled, FormulaError>;

/// The same, for a bare formula name resolved through the layers.
pub fn compile_named(layers: &FormulaLayers, cfg: &CampConfig, name: &str)
    -> Result<Compiled, FormulaError>;
```

`Formula` gains `pub contract: Option<String>`, `pub vars: BTreeMap<String, String>`;
`Step` gains `pub metadata: BTreeMap<String, String>` and keeps `assignee: Option<String>` —
**the route lands in `assignee`** (see Step 5).

**Pipeline order is not negotiable** — it is gc's, and every later rung slots into it:

```
1. parse::walk                      (Task 2 — keys, refusals, ignored)
2. extends resolution               (Task 6 — rung 2c)
3. expansion (type/template/expand) (Task 7 — rung 2d)
4. vars: parent defaults, child overrides win, then {{var}} substitution
                                    (Task 5 — rung 2b)
5. condition evaluation + prune     (Task 5 — rung 2b)
6. description_file resolution      (THIS task — rung 2a)
7. route resolution (step metadata `gc.run_target` → assignee)  (THIS task)
8. validate::check (S1..S18) + the runnability verdict          (THIS task)
```

In this task, steps 2/3/4/5 are **identity functions with a TODO pointing at their task**, and
`validate::check` hard-fails any formula whose `unimplemented` list is non-empty (Task 2, Step 3).
That is what makes the 2a count really 2, and not a lie.

### `description_file` — the exact gc semantics (verified in gc at `GASCITY_REF`)

From `gascity/internal/formula/parser.go`:
- `resolveDescriptionFiles` (`:808`): *"the file's contents replace the step description"*, and
  `step.DescriptionFile = ""` afterwards ("consumed").
- `readDescriptionFile` (`:840`): if the raw path is the documented `../assets/<rel>` form
  (`descriptionAssetRelPath`, `:964`, which rejects `../assets/../…`), resolve through the layers
  (`winningAssetPath`, `:855`: for each layer, `<layer>/../assets/<rel>` — i.e. the **pack dir's**
  `assets/`, since a layer is `<pack>/formulas` — and the LAST match wins because `searchPaths` is
  lowest→highest). Otherwise resolve relative to `baseDir` = the **formula file's own directory**.
- `descriptionFileInlineMaxBytes = 4 * 1024` (`parser.go:27`). Over that, the description becomes a
  **pointer prompt** (`descriptionFileReferenceDescription`, `:977`) — reproduce it byte-for-byte:

```rust
/// gc's `descriptionFileReferenceDescription` (gascity
/// internal/formula/parser.go:977) — reproduced byte-for-byte. The worker
/// reads this text; a paraphrase is a divergence.
fn pointer_prompt(raw_path: &str, resolved: &Path, size: usize, vars: &BTreeMap<String, String>) -> String {
    let mut b = String::new();
    b.push_str("# External Prompt Required\n\n");
    b.push_str("This bead still follows the normal runtime and lifecycle protocol from your startup prompt and current agent prompt, including claiming work, honoring result contracts, checking for follow-on work, and draining only when appropriate.\n\n");
    b.push_str("In addition to that protocol, this bead's task-specific instructions come from a formula `description_file` that is too large to inline safely into bead storage.\n\n");
    b.push_str("Before you start the task-specific work, you MUST read the file below and treat it as the task prompt for this bead. Do not proceed from memory, ambient skills, or prior workflow knowledge until you have read it.\n\n");
    b.push_str(&format!("- Resolved prompt file: `{}`\n", resolved.display()));
    b.push_str(&format!("- Original formula description_file: `{raw_path}`\n"));
    b.push_str(&format!("- Prompt file size: {size} bytes\n\n"));
    b.push_str("Treat the file contents as the authoritative task prompt for this bead. It augments the startup/runtime protocol; it does not replace the startup prompt, the current agent prompt, or any bead lifecycle/result-contract instructions already given to you.\n");
    b.push_str("Follow the section matching this bead's `gc.step_id` metadata and title, plus any result, closure, lifecycle, or post-close contract sections in that file.\n");
    if !vars.is_empty() {
        b.push_str("\n## Formula Variables\n\n");
        b.push_str("Use these resolved formula values when interpreting `{{...}}` placeholders in the prompt file:\n\n");
        b.push_str("```bash\n");
        for name in vars.keys() {                    // BTreeMap ⇒ sorted, matching gc's slices.Sort
            b.push_str(&format!("{name}=\"{{{{{name}}}}}\"\n"));
        }
        b.push_str("```\n");
    }
    b
}
```

**A `description_file` that resolves to nothing is a hard compile error** for a `graph.v2`
formula (gc: `strict = UsesGraphCompiler(f)`, `parser.go:186`, and
`validateResolvedGraphV2DescriptionFiles`, `:1007`). Measured: **all 188 corpus
`description_file` targets resolve**; 7 exceed 4096 bytes.

### Routing — step `metadata."gc.run_target"` → the step's assignee

Measured: **187 `gc.run_target` occurrences across 53 formulas, and ZERO step `assignee`.**
Routing in this corpus is *entirely* step metadata (§4 trap 3). So:

- `compose` sets `step.assignee = Some(<the resolved gc.run_target>)` when the step's metadata
  carries it and `assignee` is unset. An explicit `assignee` wins (0 corpus uses; it is camp's
  own spelling).
- The value is `{{var}}`-substituted **first** (46 of 99 route sites are var references — §7.1),
  **then** split at the first dot. The prefix must be a binding in `cfg.imports`.
- **An unbound binding is a hard compile error naming the remedy** (§7.1, §14's routing test):

```
route "gc.run-operator" names the import binding `gc`, which is not bound in camp.toml.
Bind it:  camp import add <source> --name gc
```

  Reuse compat-1's `pack::resolve_agent(cfg, name)` (`pack.rs:251`) — it already splits at the
  first dot, checks the binding, and produces exactly this error. Do not write a second resolver.
- **A value with no dot** resolves as a camp-local agent (0 corpus uses; §7.1).

### The runnability verdict (D1)

After validation, `not_runnable = Some(Refusal { key: "contract", … })` when
`formula.contract.as_deref() != Some("graph.v2")` — the 21. Its reason:

```
formula "<name>" declares no `contract = "graph.v2"`; camp will not run a formula under
graph.v2 semantics that does not ask for them (compat §9). It loads, and it cannot be cooked.
```

`camp sling` and the order-fire path **must** refuse a `not_runnable` formula and append
`formula.refused`.

- [ ] **Step 1: Write the failing tests**

`crates/camp-core/tests/compose.rs` — build a two-layer fixture under
`crates/camp-core/tests/fixtures/compose/` shaped like the corpus (a `child` pack whose
`pack.toml` declares `[imports.gc] source = "../parent"`, and a `parent` pack with
`formulas/` + `assets/`):

```rust
#[test]
fn description_file_contents_replace_the_step_description() {
    let (cfg, layers, root) = fixture();   // helper: writes camp.toml + both layers
    let c = compose::compile_named(&layers, &cfg, "child-build").unwrap();
    let step = c.formula.steps.iter().find(|s| s.id == "implement").unwrap();
    assert_eq!(step.description.as_deref(), Some("PARENT PROSE\n"),
               "the file's contents replace the description at parse time (compat §9)");
}

#[test]
fn an_asset_reference_resolves_through_the_layers_highest_wins() {
    // The child pack shadows the parent's prose while inheriting the step.
    let (cfg, layers, _root) = fixture_with_child_asset();
    let c = compose::compile_named(&layers, &cfg, "child-build").unwrap();
    let step = c.formula.steps.iter().find(|s| s.id == "implement").unwrap();
    assert_eq!(step.description.as_deref(), Some("CHILD PROSE\n"));
}

#[test]
fn an_oversize_description_file_becomes_gcs_pointer_prompt() {
    let (cfg, layers, _root) = fixture_with_big_asset(5000);   // > 4096
    let c = compose::compile_named(&layers, &cfg, "child-build").unwrap();
    let d = c.formula.steps[0].description.clone().unwrap();
    assert!(d.starts_with("# External Prompt Required\n\n"), "{d}");
    assert!(d.contains("- Prompt file size: 5000 bytes"), "{d}");
    assert!(d.contains("Resolved prompt file: `"), "the PATH must still resolve (compat §9)");
}

#[test]
fn a_missing_description_file_is_a_hard_error_for_a_graph_v2_formula() {
    let (cfg, layers, _root) = fixture_with_missing_asset();
    let err = compose::compile_named(&layers, &cfg, "child-build").unwrap_err();
    assert!(err.to_string().contains("description_file"), "{err}");
}

#[test]
fn a_step_metadata_run_target_becomes_the_steps_assignee() {
    // §4 trap 3: step metadata is ROUTING. 187 uses; 0 step `assignee`.
    let (cfg, layers, _root) = fixture();
    let c = compose::compile_named(&layers, &cfg, "child-build").unwrap();
    let step = c.formula.steps.iter().find(|s| s.id == "implement").unwrap();
    assert_eq!(step.assignee.as_deref(), Some("gc.run-operator"));
    assert_eq!(step.metadata.get("gc.run_target").map(String::as_str), Some("gc.run-operator"));
}

#[test]
fn a_route_to_an_unbound_binding_fails_at_compile_time_naming_the_remedy() {
    // compat §14 routing test: "never dispatch to nothing".
    let (cfg, layers, _root) = fixture_without_gc_binding();
    let err = compose::compile_named(&layers, &cfg, "child-build").unwrap_err();
    let text = err.to_string();
    assert!(text.contains("gc"), "{text}");
    assert!(text.contains("camp import add"), "the remedy must be printed: {text}");
}

#[test]
fn a_formula_with_no_contract_compiles_and_is_not_runnable() {
    // Plan D1 — the 21. They are inside the ceiling of 97 and refused at RUN time.
    let (cfg, layers, _root) = fixture_no_contract();
    let c = compose::compile_named(&layers, &cfg, "plain").unwrap();
    assert!(c.refusals.is_empty(), "it COMPILES: {:?}", c.refusals);
    let nr = c.not_runnable.expect("a no-contract formula is not runnable");
    assert_eq!(nr.key, "contract");
    assert!(nr.reason.contains("graph.v2"), "{}", nr.reason);
}

#[test]
fn phase_vapor_and_scope_check_are_refused_by_name() {
    // The 3 permanently-refused formulas — the ceiling of 97 (compat §9).
    let (cfg, layers, _root) = fixture_vapor();
    let c = compose::compile_named(&layers, &cfg, "vapor").unwrap();
    assert_eq!(c.refusals.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec!["phase"]);
    let (cfg, layers, _root) = fixture_scope_check();
    let c = compose::compile_named(&layers, &cfg, "scoped").unwrap();
    assert!(c.refusals.iter().any(|r| r.key == "gc.scope_kind" || r.key == "type"), "{:?}", c.refusals);
}
```

`crates/camp/tests/cli_doctor_corpus.rs`:

```rust
#[test]
fn doctor_formula_json_reports_ok_runnable_ignored_and_refusals() {
    // The gate's contract. Shape is pinned here so ci/gc-compat/formula_rungs.py
    // can rely on it.
    let camp = fixture_camp();     // an initialised camp with a formula that has a dead key
    let out = camp.run(&["doctor", "--formula", "formulas/x.formula.toml", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["runnable"], true);
    assert_eq!(v["formula"], "x");
    assert_eq!(v["ignored_keys"], serde_json::json!(["version"]));
    assert_eq!(v["refusals"], serde_json::json!([]));
}

#[test]
fn doctor_formula_rungs_json_emits_camps_own_rung_table() {
    let camp = fixture_camp();
    let out = camp.run(&["doctor", "--formula-rungs", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let ids: Vec<&str> = v.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["2a", "2b", "2c", "2d", "2e"]);
    assert_eq!(v[4]["step"], serde_json::json!(["drain"]));
}

#[test]
fn sling_refuses_a_formula_with_no_contract_and_events_the_refusal() {
    let camp = fixture_camp_with_no_contract_formula();
    let out = camp.run_failing(&["sling", "--formula", "plain"]);
    assert!(out.stderr.contains("contract"), "{}", out.stderr);
    let events = camp.events_of_type("formula.refused");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["data"]["key"], "contract");
}
```

- [ ] **Step 2: Run and watch them fail**

```bash
cargo test -p camp-core --test compose 2>&1 | tail -20
cargo test -p camp --test cli_doctor_corpus 2>&1 | tail -20
```
Expected: compile errors — `compose` and `layers` do not exist; `--json` / `--formula-rungs` are
not clap args.

- [ ] **Step 3: Implement `layers.rs` and `compose.rs`**

`FormulaLayers::from_config` builds `Vec<(binding, dir)>` lowest→highest:
`cfg.transitive_layers()?` then `cfg.import_layers()` then `("", root.to_path_buf())` for the
camp-local tier. `formula_path` walks it in reverse (highest first) trying
`<dir>/formulas/<name>.toml` then `<dir>/formulas/<name>.formula.toml` — **do not duplicate
`orders::resolve_formula`; call it** and keep `FormulaLayers` as the asset/extends resolver.
(`orders::resolve_formula` already implements the within-tier duplicate hard-error. Update its
doc comment, which still says "does NOT compile `extends`/`drain` (phase 2)".)

`asset_path(raw, base_dir)` implements gc's two branches exactly: the `../assets/<rel>` form
(after `Path::new(raw)` lexical clean; reject a `rel` that starts with `../`) searches every
layer's `<layer>/assets/<rel>` and takes the **last (highest)** match; anything else is
`base_dir.join(raw)`.

`compile` runs the eight-stage pipeline. Stages 2–5 are stubs in this task:

```rust
// Rung 2c — Task 6.
fn apply_extends(raw: RawFormula, _layers: &FormulaLayers) -> Result<RawFormula, FormulaError> { Ok(raw) }
```

- [ ] **Step 4: Wire the CLI**

`main.rs` `Doctor`: add `#[arg(long)] json: bool` and `#[arg(long = "formula-rungs")] formula_rungs: bool`
to the existing `ArgGroup("mode")` (`main.rs:86-99`) — **additive**, and `--formula-rungs` joins
the group's `args` list so exactly one mode is still required. `cmd/doctor.rs::run_formula` gains a
`json: bool` parameter and, when set, prints exactly:

```jsonc
{ "path": "...", "formula": "bmad-build", "ok": true, "runnable": true,
  "ignored_keys": ["internal", "target_required", "version"],
  "refusals": [], "not_runnable": null }
```

and **exits 0 even when `ok` is false** in `--json` mode (the gate reads the verdict from the JSON;
a non-zero exit on 3 of 100 files would make the gate script fight the exit code). In human mode
the exit code is unchanged (0 = ok, 1 = violations). Pin that in
`cli_doctor_corpus.rs::doctor_formula_json_exits_zero_even_when_the_formula_is_refused`.

`cmd/sling.rs:53` currently calls `orders::resolve_formula` + `formula::parse_and_validate`;
switch it to `compose::compile_named`, and refuse when `not_runnable.is_some()` — appending
`formula.refused` before bailing.

- [ ] **Step 5: `cook.rs` carries metadata and the route onto the bead**

In the step-bead `EventInput` (`cook.rs:191-248`), add `"metadata": step.metadata` when non-empty.
`assignee` is already written verbatim (`cook.rs`'s step arm) — compose has already put the
resolved route there, so cook needs **no** routing logic. Add a test in
`crates/camp-core/tests/cook.rs`:

```rust
#[test]
fn cook_stamps_the_steps_metadata_and_route_onto_the_bead() {
    // ... cook a formula whose step carries metadata.gc.run_target = "gc.run-operator"
    let bead = &cooked.step_beads["implement"];
    assert_eq!(ledger.get_bead(bead).unwrap().unwrap().assignee.as_deref(), Some("gc.run-operator"));
    assert_eq!(ledger.bead_metadata(bead).unwrap().get("gc.run_target").map(String::as_str),
               Some("gc.run-operator"));
}
```

- [ ] **Step 6: Write the gate — `ci/gc-compat/formula_rungs.py`**

```python
#!/usr/bin/env python3
"""The compat §10 formula gate: assert camp loads EXACTLY what it claims, at GCPACKS_REF.

usage: formula_rungs.py <corpus-checkout> <camp-binary>

Two independent assertions, so the rung table cannot drift from the loader:

  1. THE REAL LOADER. Set up one camp root importing every formula-bearing
     pack, then run `camp doctor --formula <path> --json` over all 100 corpus
     formulas. Assert exactly 97 compile and the 3 that do not are the named
     ones with the named keys (compat §9: the ceiling is 97).

  2. THE RUNG TABLE. Read camp's own `camp doctor --formula-rungs --json` and,
     for each rung, count the corpus formulas whose entire key set is within
     that rung's cumulative accepted set. Assert the §9 counts.

  Cross-check: the set of formulas the table predicts loadable at 2e must equal
  the set the real loader actually loaded. A tuned table fails here.

The corpus is NEVER vendored (compat §10 — gascity-packs has no LICENSE).
Never regex TOML (KNOWN-DEFECTS): tomllib, and glob `formulas/*.toml`, not
`*.formula.toml` — gastown's 8 `mol-*.toml` break the naming convention.
"""
CEILING = 97
PERMANENTLY_REFUSED = {
    "mol-digest-generate.toml":        "phase",   # phase = "vapor"
    "mol-pr-from-issue.formula.toml":  "phase",
    "design-review.formula.toml":      "gc.scope_kind",
}
RUNG_COUNTS = {"2a": 2, "2b": 31, "2c": 58, "2d": 84, "2e": 97}
```

The setup mirrors `load_corpus_packs.py`'s `camp()` helper verbatim: `camp init --no-service
--no-import`, append `[agent_defaults] tools = ["Read", "Bash", "Skill"]`, then
`camp import add <corpus>/<pack> --name <pack>` for each of the **10 formula-bearing packs**
(`bmad`, `compound-engineering`, `contributing`, `discord`, `gascity`, `gastown`, `github`,
`gstack`, `pr-pipeline`, `superpowers`) plus `camp import add <corpus>/gascity/roles --name gc`
(the corpus's own deployment recipe, §3/§7.3). **Measured: no two of the 100 formulas share a
basename, so no within-tier collision can arise.** Every `camp` call is checked; a non-zero exit
is a gate failure with the stderr printed — no fallbacks.

Base key sets (what camp accepts before any rung) are read from the binary too, so the script
holds no opinion: add `"base"` to the `--formula-rungs` JSON (`{"base": {"top": [...],
"step": [...]}, "rungs": [...]}`) and update `doctor_formula_rungs_json_emits_camps_own_rung_table`
accordingly. Dead/annotation keys are classified by the binary as well — emit them in the same
JSON (`"dead"`, `"annotation"`, `"refused"` lists) so the script's rung arithmetic uses **camp's**
classification, not its own copy.

- [ ] **Step 7: Run the gate against the real corpus, locally**

```bash
git clone -q https://github.com/gastownhall/gascity-packs /tmp/gcpacks \
  && git -C /tmp/gcpacks checkout -q "$(cat ci/gc-compat/GCPACKS_REF)"
cargo build --bin camp
python3 ci/gc-compat/formula_rungs.py /tmp/gcpacks target/debug/camp
```
Expected **at this point in the plan**: the rung-table half PASSES (2a=2 … 2e=97 — the table is
already complete data, Task 2), and the real-loader half FAILS, reporting **2** compiled, not 97,
because rungs 2b–2e are not implemented (`unimplemented` is a hard violation). That is the correct
failing signal — **the gate is now the TDD driver for Tasks 5–8.** Add `--expect-loaded N` so each
rung's task can assert its own number; CI always runs it with the default (97).

- [ ] **Step 8: Wire CI**

In `.github/workflows/ci.yml`, in the **existing** `gcpacks-compat` job (do not add a job — the
corpus checkout and the `cargo build --bin camp` step are already there), append one step after
the phase-1 gate:

```yaml
      - name: phase-2 formula gate (rung counts + the 97 ceiling)
        run: python3 ci/gc-compat/formula_rungs.py gcpacks-src target/debug/camp
```

- [ ] **Step 9: Full gates and commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
python3 ci/gc-compat/formula_rungs.py /tmp/gcpacks target/debug/camp --expect-loaded 2
git add -A && git commit -m "feat(formula): rung 2a — the layered compiler, description_file, routing, and the §10 gate"
```

---

## Task 5: Rung 2b — `vars`, the `{{var}}` substitution asymmetry, and `condition` pruning

**Files:**
- Modify: `crates/camp-core/src/formula/compose.rs` (pipeline stages 4 and 5), `parse.rs`, `ast.rs`,
  `validate.rs`
- Test: `crates/camp-core/tests/compose.rs`

**Interfaces:**
- Consumes: Task 4's pipeline.
- Produces:

```rust
// compose.rs
/// §9: substitution applies to `title`, `description`, `assignee`, metadata
/// VALUES, `notes`, `tags` — and NOT to `id`, `needs`, `check.path`, or
/// `drain.formula`. An undefined var KEEPS THE LITERAL PLACEHOLDER.
/// "Reproduce that asymmetry or diverge."
pub(crate) fn substitute(text: &str, vars: &BTreeMap<String, String>) -> String;

/// §9: `==` and `!=` only; the LHS must be a single `{{var}}`. False ⇒ the
/// step is PRUNED with its children, and dangling `needs` edges are silently
/// dropped.
pub(crate) fn eval_condition(expr: &str, vars: &BTreeMap<String, String>) -> Result<bool, Violation>;
```

Measured: the corpus contains exactly **3 distinct conditions**, all of this shape, with an
**unquoted bare-word RHS**: `{{drain_policy}} == separate`, `{{drain_policy}} == same-session`,
`{{pr_mode}} != none`. Trim both sides; do not require quotes; accept a quoted RHS too.

`[vars]` merge (load-bearing — compat §3's evidence table: *"`drain_policy = "separate"` is
declared in `build-base`, not in the children"*): **parent defaults first, child overrides win.**
The merge happens in the `extends` stage (Task 6) and the substitution reads the merged map, so
implement `vars` as a plain map on `RawFormula` here and let Task 6 merge it.

gc's `[vars]` entries may be a bare string **or** a table (`{ default = "…", … }`) — accept both;
take `default` from the table form; a var with no default stays **undefined** (its placeholder
survives — §9). Measured: 4 route sites have no default and receive a qualified value via
`expand_vars` (Task 7).

**The residual check is enforced ONLY on `title`** (§9). After substitution, a `title` still
containing `{{` is a `Violation`; a `description` still containing `{{` is **fine** (the pointer
prompt in Task 4 deliberately emits `{{var}}` placeholders for the worker to resolve).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn substitution_applies_to_title_description_assignee_metadata_notes_and_tags() {
    let vars = BTreeMap::from([("t".to_owned(), "gc.run-operator".to_owned()),
                               ("n".to_owned(), "Ship".to_owned())]);
    assert_eq!(substitute("{{n}} it", &vars), "Ship it");
    assert_eq!(substitute("{{t}}", &vars), "gc.run-operator");
}

#[test]
fn substitution_never_touches_id_needs_check_path_or_drain_formula() {
    // §9: "Not to `id`, `needs`, `check.path`, or `drain.formula`."
    let (cfg, layers, _r) = fixture_vars_in_every_field();
    let c = compose::compile_named(&layers, &cfg, "v").unwrap();
    let s = &c.formula.steps[0];
    assert_eq!(s.id, "{{id_var}}", "id is never substituted");
    assert_eq!(s.needs, vec!["{{needs_var}}".to_owned()]);
    assert_eq!(s.check.as_ref().unwrap().path, PathBuf::from(".gc/{{p}}.sh"));
}

#[test]
fn an_undefined_var_keeps_its_literal_placeholder_and_only_title_is_residual_checked() {
    let (cfg, layers, _r) = fixture_undefined_var_in_description();
    let c = compose::compile_named(&layers, &cfg, "v").unwrap();
    assert!(c.formula.steps[0].description.as_ref().unwrap().contains("{{nope}}"),
            "an undefined var keeps the literal placeholder (compat §9)");
    let err = compose::compile_named(&layers, &cfg, "v-title").unwrap_err();
    assert!(err.to_string().contains("{{nope}}"), "the residual check IS enforced on title: {err}");
}

#[test]
fn a_false_condition_prunes_the_step_with_its_children_and_drops_dangling_needs() {
    // The corpus's real shape: the two drain steps of bmad-build are mutually
    // exclusive on {{drain_policy}}, whose default is "separate".
    let (cfg, layers, _r) = fixture_drain_policy();     // vars.drain_policy default = "separate"
    let c = compose::compile_named(&layers, &cfg, "build").unwrap();
    let ids: Vec<&str> = c.formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"implement"), "the separate arm survives: {ids:?}");
    assert!(!ids.contains(&"implement-same-session"), "the shared arm is pruned: {ids:?}");
    assert!(!ids.contains(&"same-session-child"), "children are pruned with the parent");
    let publish = c.formula.steps.iter().find(|s| s.id == "publish").unwrap();
    assert_eq!(publish.needs, vec!["implement".to_owned()],
               "the dangling need on the pruned step is silently dropped (compat §9)");
}

#[test]
fn a_condition_outside_the_subset_is_a_violation_naming_the_step() {
    let (cfg, layers, _r) = fixture_bad_condition();   // condition = "{{a}} > 1"
    let err = compose::compile_named(&layers, &cfg, "v").unwrap_err();
    assert!(err.to_string().contains("condition"), "{err}");
    assert!(err.to_string().contains("=="), "the supported operators are named: {err}");
}

#[test]
fn vars_defaults_come_from_the_table_or_the_bare_string_form() {
    let (cfg, layers, _r) = fixture_vars_both_forms();
    let c = compose::compile_named(&layers, &cfg, "v").unwrap();
    assert_eq!(c.formula.vars.get("a").map(String::as_str), Some("1"));  // a = "1"
    assert_eq!(c.formula.vars.get("b").map(String::as_str), Some("2"));  // [vars.b] default = "2"
    assert!(!c.formula.vars.contains_key("c"), "a var with no default stays undefined");
}
```

- [ ] **Step 2: Run and watch them fail**
```bash
cargo test -p camp-core --test compose 2>&1 | tail -20
```
Expected: FAIL — `vars`/`condition` are still in `unimplemented`, so every one of these formulas
is a hard violation.

- [ ] **Step 3: Implement**

Remove `"vars"` and `"condition"` from the `unimplemented` list. Implement `substitute` as a
single left-to-right pass (never re-scan inserted values as template syntax — the existing
`cook::substitute` at `cook.rs:51` makes the same choice; **do not merge them**: cook's uses
`{name}` and formulas use `{{name}}`). Implement `eval_condition` and the prune (a post-order walk
that removes the step and its `children`, then filters every surviving step's `needs` against the
surviving id set).

- [ ] **Step 4: Run and watch them pass, then run the gate**
```bash
cargo test -p camp-core --test compose 2>&1 | tail -10
python3 ci/gc-compat/formula_rungs.py /tmp/gcpacks target/debug/camp --expect-loaded 31
```
Expected: PASS, and the gate reports **31** loaded — rung 2b's pinned count.

- [ ] **Step 5: Full gates and commit**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "feat(formula): rung 2b — vars, the substitution asymmetry, condition pruning (31/100)"
```

---

## Task 6: Rung 2c — `extends`

§9, verbatim: *"child seeds scalars; parents' steps **append**; a child step whose `id` matches a
parent's **replaces it whole, in place, preserving position**. No field-level merge.
`advice`/`pointcuts` are dropped entirely. Parents resolve by bare name through the formula
layers."*

Measured: **48 formulas extend**; **every parent lives in `gascity/formulas/`**; **no formula
extends more than one parent** (`extends` is nonetheless an array — gc's shape — so implement the
list, left-to-right); depth is at most 2 in the corpus but implement the chain with a **cycle
guard** (a repeated name in the chain is a hard error naming the cycle — invariant 5).

**Files:** `compose.rs`, `parse.rs` (`extends: Vec<String>`), `ast.rs`, `crates/camp-core/tests/compose.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_parents_steps_append_and_a_matching_child_id_replaces_in_place() {
    // parent: [a, b, c]; child overrides b and adds d.
    let (cfg, layers, _r) = fixture_extends();
    let c = compose::compile_named(&layers, &cfg, "child").unwrap();
    let ids: Vec<&str> = c.formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c", "d"], "position is preserved; new steps append");
    let b = c.formula.steps.iter().find(|s| s.id == "b").unwrap();
    assert_eq!(b.title, "CHILD B");
    assert_eq!(b.description, None, "replaced WHOLE — no field-level merge (compat §9)");
}

#[test]
fn the_child_seeds_scalars_and_inherits_the_parents_vars() {
    // compat §3: "drain_policy = "separate" is declared in build-base, not in
    // the children" — without this, 24 formulas lose their defaults.
    let (cfg, layers, _r) = fixture_extends();
    let c = compose::compile_named(&layers, &cfg, "child").unwrap();
    assert_eq!(c.formula.name, "child", "the child's own scalars win");
    assert_eq!(c.formula.vars.get("drain_policy").map(String::as_str), Some("separate"));
    assert_eq!(c.formula.vars.get("overridden").map(String::as_str), Some("by-child"));
}

#[test]
fn a_parent_resolves_by_bare_name_through_the_layers() {
    // The parent lives in the TRANSITIVE gascity layer; the child in the
    // direct import. This is what compat §7.2 is load-bearing for.
    let (cfg, layers, _r) = fixture_extends();
    assert!(compose::compile_named(&layers, &cfg, "child").is_ok());
}

#[test]
fn an_unresolvable_parent_is_a_hard_error_naming_it() {
    let (cfg, layers, _r) = fixture_extends_missing_parent();
    let err = compose::compile_named(&layers, &cfg, "child").unwrap_err();
    assert!(err.to_string().contains("no-such-parent"), "{err}");
}

#[test]
fn an_extends_cycle_is_a_hard_error_never_a_stack_overflow() {
    let (cfg, layers, _r) = fixture_extends_cycle();   // a -> b -> a
    let err = compose::compile_named(&layers, &cfg, "a").unwrap_err();
    assert!(err.to_string().contains("cycle"), "{err}");
}

#[test]
fn advice_and_pointcuts_are_dropped_not_applied() {
    // compat §9's own word. 0 corpus uses.
    let (cfg, layers, _r) = fixture_extends_with_advice();
    let c = compose::compile_named(&layers, &cfg, "child").unwrap();
    assert!(c.formula.steps.iter().all(|s| s.id != "advised"), "{:?}", c.formula.steps);
}
```

- [ ] **Step 2: Run and watch them fail** — `cargo test -p camp-core --test compose 2>&1 | tail -20`
- [ ] **Step 3: Implement.** Remove `"extends"` from `unimplemented`. Resolve the chain
  depth-first with a `Vec<String>` visited-stack (the cycle guard). Merge order: the **deepest
  ancestor first**, then each descendant applies its own steps (append-or-replace-in-place) and
  its own scalars/vars over the accumulator.
- [ ] **Step 4: Run and watch them pass, then the gate**
```bash
python3 ci/gc-compat/formula_rungs.py /tmp/gcpacks target/debug/camp --expect-loaded 58
```
- [ ] **Step 5: Full gates and commit** — `git commit -m "feat(formula): rung 2c — extends, append and replace-in-place (58/100)"`

---

## Task 7: Rung 2d — `type = "expansion"`, `template`, `expand`, `expand_vars`, `children`

§9: *"`type = "expansion"` — the formula is **not directly runnable**; it supplies `template` steps
for `expand`."* Measured: 14 formulas declare `type = "expansion"` **and** a top-level `template`
(the same 14); 15 steps carry `expand`; 14 carry `expand_vars`; 2 carry `children`.

gc (`internal/formula/expand.go`): an `expand` rule names a **target step** and a formula
(`with`); the target step is **replaced** by the expansion formula's `template` steps, with the
expansion formula's own `[vars]` merged under the rule's overrides resolved against the parent's
vars (`ApplyExpansionsWithVars`, `mergeVars`, `resolveOverrideVars`). `DefaultMaxExpansionDepth = 5`
bounds recursion — implement the bound and make exceeding it a hard error (invariant 5), not a
truncation.

`expand_vars` is what supplies a qualified route to the 4 sites with no `[vars]` default
(measured, §2) — so the expansion's var map must be visible to the substitution stage, which
means **expansion runs BEFORE vars/condition** (pipeline stages 3 → 4 → 5, as fixed in Task 4).

**A formula with `type = "expansion"` is `not_runnable`** — the same field Task 4 built for the
21 no-contract formulas, with `key: "type"` and a reason naming `expansion`. It still **compiles**
(it is inside the 97).

**Files:** `compose.rs`, `parse.rs`, `ast.rs`, `validate.rs`, `crates/camp-core/tests/compose.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_expansion_formula_compiles_and_is_not_runnable() {
    let (cfg, layers, _r) = fixture_expansion();
    let c = compose::compile_named(&layers, &cfg, "exp").unwrap();
    assert!(c.refusals.is_empty(), "{:?}", c.refusals);
    let nr = c.not_runnable.expect("type = \"expansion\" is not directly runnable (compat §9)");
    assert_eq!(nr.key, "type");
}

#[test]
fn expand_replaces_the_target_step_with_the_expansion_formulas_template() {
    let (cfg, layers, _r) = fixture_expansion();
    let c = compose::compile_named(&layers, &cfg, "host").unwrap();
    let ids: Vec<&str> = c.formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert!(!ids.contains(&"placeholder"), "the target is REPLACED: {ids:?}");
    assert!(ids.contains(&"tmpl-1") && ids.contains(&"tmpl-2"), "{ids:?}");
}

#[test]
fn expand_vars_supply_a_qualified_route_where_no_vars_default_exists() {
    // Measured: 4 route sites have no [vars] default and are supplied here.
    let (cfg, layers, _r) = fixture_expansion();
    let c = compose::compile_named(&layers, &cfg, "host").unwrap();
    let s = c.formula.steps.iter().find(|s| s.id == "tmpl-1").unwrap();
    assert_eq!(s.assignee.as_deref(), Some("bmad.story-implementer"));
}

#[test]
fn children_are_flattened_into_the_step_list_preserving_position() {
    let (cfg, layers, _r) = fixture_children();
    let c = compose::compile_named(&layers, &cfg, "h").unwrap();
    let ids: Vec<&str> = c.formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["parent", "kid-a", "kid-b", "after"]);
}

#[test]
fn expansion_deeper_than_five_is_a_hard_error_not_a_truncation() {
    let (cfg, layers, _r) = fixture_expansion_depth(6);
    let err = compose::compile_named(&layers, &cfg, "host").unwrap_err();
    assert!(err.to_string().contains("depth"), "{err}");
}

#[test]
fn an_expand_target_that_does_not_exist_is_a_hard_error() {
    let (cfg, layers, _r) = fixture_expansion_bad_target();
    let err = compose::compile_named(&layers, &cfg, "host").unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}
```

- [ ] **Step 2: Run and watch them fail**
- [ ] **Step 3: Implement.** Remove `type`/`template`/`expand`/`expand_vars`/`children` from
  `unimplemented`. Depth bound 5, hard error past it.
- [ ] **Step 4: Run, watch pass, then the gate**
```bash
python3 ci/gc-compat/formula_rungs.py /tmp/gcpacks target/debug/camp --expect-loaded 84
```
- [ ] **Step 5: Full gates and commit** — `git commit -m "feat(formula): rung 2d — expansion, template, expand_vars, children (84/100)"`

---

## Task 8: Rung 2e (compile side) — `drain`, and the refusals that keep camp honest

**Files:**
- Create: `crates/camp-core/src/formula/drain.rs`
- Modify: `parse.rs` (`walk_drain`, modeled on `walk_on_complete` at `parse.rs:460`), `ast.rs`,
  `keys.rs` (`Site::Drain` / `Site::DrainItem`), `validate.rs` (S14–S16)
- Test: `crates/camp-core/src/formula/drain.rs` (`mod tests`), `crates/camp-core/tests/compose.rs`

**Interfaces:**

```rust
// crates/camp-core/src/formula/ast.rs
/// gc's `DrainSpec` (gascity internal/formula/types.go:341), restricted to
/// what camp implements. Camp REFUSES `context = "shared"`,
/// `continuation_group`, and `max_units` — see the refusal table below.
#[derive(Debug, Clone, PartialEq)]
pub struct Drain {
    /// Always `Separate` — `Shared` is refused at compile time (compat §9, §6.2).
    pub context: DrainContext,
    /// The graph.v2 formula run once per member. NOT {{var}}-substituted (§9).
    pub formula: String,
    pub member_access: MemberAccess,
    pub on_item_failure: OnItemFailure,
    pub item: DrainItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum DrainContext { Separate }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum MemberAccess { Read, Exclusive }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum OnItemFailure { Continue, SkipRemaining }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub struct DrainItem { pub single_lane: bool }

impl MemberAccess   { pub fn as_str(self) -> &'static str; }   // "read" | "exclusive"
impl OnItemFailure  { pub fn as_str(self) -> &'static str; }   // "continue" | "skip_remaining"
```

**gc's compiler defaulting, verbatim** (`gascity/internal/formula/compile.go:583-611`,
`ApplyDrainControlMetadata` — *"the single shape owner for drain control metadata"*):

| field | gc's default | camp |
|---|---|---|
| `member_access` | `"read"` when unset | same. (**All 25 corpus drains set `"exclusive"`.**) |
| `on_item_failure` | `"skip_remaining"` when `context == "shared"`, else **`"continue"`** | same. Camp refuses `shared`, so camp's effective default is **`continue`** — §9 exactly. |
| `item.single_lane` | absent = false | same. |

**The refusal table (§4 rule 1 — each names its key):**

| construct | refused because | corpus uses |
|---|---|---|
| `drain.context = "shared"` | camp truncates gc's continuation loop (§6.2); a shared-context drain cannot share a worker session and camp will not pretend it does (§9). The message must name **the formula, the step, and the `drain_policy = same-session` var that selects it** (§9, verbatim). | 13 — all behind `condition = "{{drain_policy}} == same-session"`, default `separate` |
| `drain.continuation_group` | valid only with `shared`; §11.4: *"`gc.continuation_group` is not honoured"* | 0 |
| `drain.max_units` | gc semantics camp does not implement | 0 |

`on_item_failure = "skip_remaining"` with `context = "separate"` is **implemented, not refused** —
gc's enum permits it and §9 lists both values; refusing a value the spec enumerates would be camp
inventing a restriction. Semantics: the first item root that closes non-`pass` stops **further
items being materialized**; already-running items are not killed. Zero corpus uses (all 13 sit on
shared drains), so it is proven by a synthetic fixture in Task 9.

`single_lane` likewise has **zero corpus coverage on the separate path** — it is *"an authored
throttle"* for separate drains (§9) and the corpus only uses it on the shared drains camp refuses.
Task 9 proves it with a synthetic fixture. **State this plainly in the PR description**: the gate
proves the corpus counts; these two flags are proven by fixtures, because the corpus does not
exercise them.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn drain_defaults_follow_gcs_compiler() {
    // gascity internal/formula/compile.go:583-611 (ApplyDrainControlMetadata).
    let d = parse_drain(r#"formula = "item""#).unwrap();
    assert_eq!(d.member_access, MemberAccess::Read, "gc defaults member_access to \"read\"");
    assert_eq!(d.on_item_failure, OnItemFailure::Continue, "separate context ⇒ continue");
    assert!(!d.item.single_lane);
    assert_eq!(d.context, DrainContext::Separate);
}

#[test]
fn a_shared_context_drain_is_refused_naming_the_formula_the_step_and_drain_policy() {
    let (cfg, layers, _r) = fixture_drain_shared();
    let c = compose::compile_named(&layers, &cfg, "build").unwrap();
    let r = c.refusals.iter().find(|r| r.key == "context").expect("shared is refused (compat §9)");
    assert!(r.construct.contains("implement-same-session"), "the STEP: {}", r.construct);
    assert!(r.reason.contains("build"), "the FORMULA: {}", r.reason);
    assert!(r.reason.contains("drain_policy"), "the VAR that selects it: {}", r.reason);
    assert!(r.reason.contains("same-session"), "{}", r.reason);
}

#[test]
fn continuation_group_and_max_units_are_refused_by_name() {
    for (key, toml) in [("continuation_group", r#"formula = "i"
context = "separate"
continuation_group = "g""#),
                        ("max_units", "formula = \"i\"\nmax_units = 3\n")] {
        let refusals = drain_refusals(toml);
        assert!(refusals.iter().any(|r| r.key == key), "{key}: {refusals:?}");
    }
}

#[test]
fn the_corpus_shared_drain_is_pruned_by_the_default_drain_policy_so_the_build_still_compiles() {
    // THE load-bearing case: bmad-build/gstack-build/compound-build each carry
    // TWO drain steps on mutually exclusive conditions. The default
    // (drain_policy = "separate", declared in gascity's build-base) prunes the
    // shared one BEFORE the refusal can fire. compat §9.
    let (cfg, layers, _r) = fixture_corpus_shaped_build();   // both arms, default separate
    let c = compose::compile_named(&layers, &cfg, "build").unwrap();
    assert!(c.refusals.is_empty(), "the shared arm is pruned, not refused: {:?}", c.refusals);
    let d = c.formula.steps.iter().find(|s| s.id == "implement").unwrap().drain.as_ref().unwrap();
    assert_eq!(d.member_access, MemberAccess::Exclusive);
    assert_eq!(d.on_item_failure, OnItemFailure::Continue);
}

#[test]
fn setting_drain_policy_to_same_session_refuses_loudly_instead_of_approximating() {
    let (cfg, layers, _r) = fixture_corpus_shaped_build_with_var("drain_policy", "same-session");
    let c = compose::compile_named(&layers, &cfg, "build").unwrap();
    assert!(c.refusals.iter().any(|r| r.key == "context"), "{:?}", c.refusals);
}

#[test]
fn drain_formula_is_never_var_substituted() {
    // §9: substitution does NOT apply to `drain.formula`.
    let (cfg, layers, _r) = fixture_drain_formula_with_var();
    let c = compose::compile_named(&layers, &cfg, "b").unwrap();
    assert_eq!(c.formula.steps[0].drain.as_ref().unwrap().formula, "{{item_formula}}");
}
```

Ordering note for `the_corpus_shared_drain_is_pruned_...`: **condition pruning (stage 5) runs
before the drain refusal check (stage 8)**, which is why the default path compiles clean. If the
refusal fired at parse time, all three v1 build formulas would refuse and the ceiling would be 94.
Pin that ordering with this test — it is the single most load-bearing interaction in the phase.

- [ ] **Step 2: Run and watch them fail**
- [ ] **Step 3: Implement.** `walk_drain` mirrors `walk_on_complete` (`parse.rs:460`) — same key
  whitelist discipline, same "presence, not parse success" rule (`RawStep.has_drain`, review
  finding 5). Add `has_drain` to S9's combination bans (`check` + `drain` and `retry` + `drain` are
  both incoherent — a drain step is campd's, not a worker's) and to S11's `uses_graph_only`
  predicate (`validate.rs:182`). Fill `keys::classify` for `Site::Drain` (`context`, `formula`,
  `member_access`, `on_item_failure`, `item` accepted; `continuation_group`, `max_units` refused)
  and `Site::DrainItem` (`single_lane` accepted). Remove `"drain"` from `unimplemented`; **delete
  the `unimplemented` field and its violation** — it is now empty forever.
- [ ] **Step 4: Run, watch pass, then the gate at the CEILING**
```bash
python3 ci/gc-compat/formula_rungs.py /tmp/gcpacks target/debug/camp
```
Expected: **97 loaded, 3 refused by name** — `mol-digest-generate.toml` (`phase`),
`mol-pr-from-issue.formula.toml` (`phase`), `design-review.formula.toml` (`gc.scope_kind`) — and
every rung count matching. **This is the phase's headline gate.** If it reports anything other
than 97, stop and report to the lead; do not adjust the expectation.
- [ ] **Step 5: Full gates and commit** — `git commit -m "feat(formula): rung 2e compile — drain, with shared/continuation_group/max_units refused (97/100 — the ceiling)"`

---

## Task 9: The drain runtime — members, the exclusive reservation, `single_lane`, `on_item_failure`

The last task, and the only one touching the daemon. **ADDITIVE ONLY** in `dispatch.rs` and
`event_loop.rs` — a sibling stream owns those files. Do not refactor them.

**Files:**
- Modify: `crates/camp-core/src/formula/runtime.rs` (pure reads: `run_members`, `drain_label`,
  `parse_drain_label`, `drain_verdict`), `crates/camp-core/src/readiness.rs` (`run_members` SQL,
  `bead_metadata`), `crates/camp-core/src/ledger/mod.rs` (the `&Ledger` wrappers)
- Modify: `crates/camp/src/daemon/dispatch.rs` — **additive**: `PendingDrain`, the
  `pending_drains` field, `queue_drain`, one `execute` loop, one `reconcile` pass, one
  `on_bead_closed` arm
- Test: `crates/camp/tests/daemon_drain.rs`

**Interfaces:**
- Consumes: Task 3's `bead_metadata` + `EXCLUSIVE_DRAIN_RESERVATION`; Task 8's `Drain`;
  `flow::{bond_label, parse_bond_label}` as the model.
- Produces:

```rust
// crates/camp-core/src/formula/runtime.rs  — pure, write-free (the file's contract)

/// The drain's member set — gc's `convoycore.Members(parentConvoyID)`
/// (gascity internal/dispatch/drain.go:211), mapped onto camp's run (plan D3):
/// beads in the run that are not the root, not step anchors/attempts
/// (`step_id IS NULL`), and not fan-out or drain children (no `bond:`/`drain:`
/// label). Ordered by creation seq, so the drain is deterministic.
pub fn run_members(conn: &Connection, ctx: &RunContext) -> Result<Vec<BeadRow>, CoreError>;

/// `drain:<anchor>:<index>` — the label on each drain item ROOT, the exact
/// mold of `bond_label` (runtime.rs:504). It is the idempotency ledger: what
/// exists has been materialized.
pub fn drain_label(anchor: &str, index: usize) -> String;
pub fn parse_drain_label(label: &str) -> Option<(&str, usize)>;
pub fn drain_children(conn: &Connection, anchor: &str) -> Result<BTreeMap<usize, BeadRow>, CoreError>;
```

```rust
// crates/camp/src/daemon/dispatch.rs — beside PendingFanout (dispatch.rs:1045)

#[derive(Debug, Clone, PartialEq)]
pub struct PendingDrain {
    pub run_id: String,
    pub step_id: String,
    pub anchor: String,
}
```

### How each §9 drain semantic is implemented

| §9 requirement | implementation |
|---|---|
| **runtime fan-out** | `on_bead_closed` (dispatch.rs:1844-1852): beside the existing `if outcome == "pass" && step_ref.step.on_complete.is_some() { queue_fanout(..) }`, add `if outcome == "pass" && step_ref.step.drain.is_some() { queue_drain(..) }`. `execute` (dispatch.rs:1136) gains a third loop after the fanout loop, with the **same requeue-the-unexecuted-tail-on-error** shape (dispatch.rs:1154-1162). `reconcile` (dispatch.rs:1727) gains a third pass. |
| **`member_access = "exclusive"` (25 uses)** | Before materializing item *i*, append `bead.updated { metadata: { "gc.exclusive_drain_reservation": <anchor id> } }` on member *i* — **the key verbatim** (gc `beadmeta/keys.go:93`, invariant 7). Task 3's fold makes the compare-and-set atomic. A conflict is `CoreError::InvalidEventData`, which the drain turns into a **loud failure of the reserving drain** via the existing `fanout_failure` mold (`dispatch.rs:2258`) → `dispatch.failed` on the anchor, naming the member and the holding drain. **Never two drains mutating one bead.** `member_access = "read"` reserves nothing. |
| **release at drain end** | When the drain finalizes (all items closed, or `skip_remaining` tripped), append `bead.updated { metadata: { "gc.exclusive_drain_reservation": null } }` for every member it holds, in the finalizing batch. |
| **`item.single_lane`** | *"items dispatch one at a time, never in parallel… the drain's ready items enter dispatch with concurrency 1."* Implemented as a **`needs` edge**, not a counter — `CookOptions::extra_root_needs = vec![<previous item's root bead>]` (the exact idiom sequential `on_complete` already uses, dispatch.rs:1244-1252). `readiness::dispatchable_beads`'s existing `UNMET_DEP` clause then serializes them for free, survives `kill -9`, and refolds correctly. **Do not add a predicate to `dispatchable_beads`** — the only cap that exists (`max_workers`) is global, and a new folded column would drag in `STATE_DDL`, `BEAD_COLS`, `row_to_bead`, `stuck_task_count` and `refold_prop::DUMPS`. |
| **`on_item_failure = "continue"` (the separate-context default)** | *"an item's failure does not stop the remaining items; the drain's own outcome reflects the failures at finalize."* Every member is materialized in the first `execute` pass (no gating on predecessors, unless `single_lane`); at finalize the anchor closes `pass` iff every item root closed `pass`, else `fail` with `final_disposition = "hard_fail"`. |
| **`on_item_failure = "skip_remaining"`** | Materialize lazily (item *i* only once items `0..i` have all closed `pass`, the `sequential`-bond gate at dispatch.rs:1199-1211); on the first non-`pass`, materialize nothing further and finalize `fail`. |
| **`drain.formula`** | Resolved through `FormulaLayers` (**not** `<camp>/formulas/<bond>.toml` — `execute_fanout` at dispatch.rs:1230 hardcodes that path; the drain loop must call `compose::compile_named`, because every corpus item formula (`bmad-story-development`, `gstack-work`, `compound-work`) lives in an imported pack, not in the camp root). |
| **item vars** | Each item root is cooked with `CookOptions::vars` carrying the parent's merged vars plus `{member}` = the member bead id, and `extra_root_labels = vec![drain_label(anchor, i)]`. |

**No new event type.** The reservation rides `bead.updated` (Task 3). A drain that cannot proceed
uses `dispatch.failed`, exactly as fan-out does (dispatch.rs:2258-2274 — *"`dispatch.failed` is the
honest name: campd could not dispatch the declared follow-on work"*). This is not laziness: `vocab.rs`'s
`no_reservation_vocabulary_exists` **forbids any event name containing `"reserv"`**, and inventing
`drain.materialized` would add vocabulary for no consumer.

- [ ] **Step 1: Write the failing tests** — `crates/camp/tests/daemon_drain.rs`

```rust
#[test]
fn a_drain_scatters_the_runs_members_one_item_root_each() {
    let mut c = fixture_campd_with_drain_formula();   // separate, exclusive, no single_lane
    let run = c.sling("build");
    c.create_member(&run, "story A");                 // camp create --run <run>
    c.create_member(&run, "story B");
    c.close_pass(&c.step_bead(&run, "decompose"));
    c.settle();
    let items = c.drain_children(&c.step_bead(&run, "implement"));
    assert_eq!(items.len(), 2, "one item root per member");
}

#[test]
fn an_exclusive_drain_reserves_every_member_with_gcs_verbatim_key() {
    let mut c = fixture_campd_with_drain_formula();
    let run = c.sling("build");
    let m = c.create_member(&run, "story A");
    c.close_pass(&c.step_bead(&run, "decompose"));
    c.settle();
    assert_eq!(c.bead_metadata(&m).get("gc.exclusive_drain_reservation").map(String::as_str),
               Some(c.step_bead(&run, "implement").as_str()));
}

#[test]
fn a_second_drain_reserving_a_held_member_fails_loudly_and_never_mutates_it() {
    // compat §9: "never two drains mutating one bead."
    let mut c = fixture_campd_with_two_drains_over_one_member();
    c.settle();
    let failures = c.events_of_type("dispatch.failed");
    assert_eq!(failures.len(), 1, "the SECOND drain fails; the first is untouched");
    assert!(failures[0]["data"]["reason"].as_str().unwrap().contains("gc.exclusive_drain_reservation"));
    // and the member is still held by the first drain, unchanged
    assert_eq!(c.bead_metadata(&c.member).get("gc.exclusive_drain_reservation"), Some(&c.first_drain));
}

#[test]
fn the_reservation_is_released_when_the_drain_finalizes() {
    let mut c = fixture_campd_with_drain_formula();
    let run = c.sling("build");
    let m = c.create_member(&run, "story A");
    c.close_pass(&c.step_bead(&run, "decompose"));
    c.settle();
    c.close_pass_all(&c.drain_children(&c.step_bead(&run, "implement")));
    c.settle();
    assert!(!c.bead_metadata(&m).contains_key("gc.exclusive_drain_reservation"));
}

#[test]
fn single_lane_items_never_run_concurrently() {
    // compat §14's drain test. The corpus does not exercise single_lane on a
    // SEPARATE drain (all 13 uses sit on shared drains, which camp refuses),
    // so this synthetic fixture is its only proof.
    let mut c = fixture_campd_with_single_lane_drain();     // 3 members, max_workers = 8
    let run = c.sling("build");
    c.close_pass(&c.step_bead(&run, "decompose"));
    c.settle();
    assert_eq!(c.dispatchable().len(), 1, "concurrency 1 — item 2 is gated on item 1's root");
    c.close_pass_all(&c.drain_children(&c.step_bead(&run, "implement"))[..1]);
    c.settle();
    assert_eq!(c.dispatchable().len(), 1);
}

#[test]
fn on_item_failure_continue_does_not_stop_the_remaining_items_and_the_drain_fails_at_finalize() {
    let mut c = fixture_campd_with_drain_formula();   // default: continue
    let run = c.sling("build");
    c.create_member(&run, "A"); c.create_member(&run, "B");
    c.close_pass(&c.step_bead(&run, "decompose"));
    c.settle();
    let items = c.drain_children(&c.step_bead(&run, "implement"));
    c.close_fail(&items[0]);
    c.close_pass(&items[1]);                          // B still ran — "continue"
    c.settle();
    let anchor = c.get_bead(&c.step_bead(&run, "implement"));
    assert_eq!(anchor.outcome.as_deref(), Some("fail"), "the drain's outcome reflects the failures");
}

#[test]
fn on_item_failure_skip_remaining_materializes_nothing_after_the_first_failure() {
    let mut c = fixture_campd_with_skip_remaining_drain();   // 3 members
    let run = c.sling("build");
    c.close_pass(&c.step_bead(&run, "decompose"));
    c.settle();
    let items = c.drain_children(&c.step_bead(&run, "implement"));
    assert_eq!(items.len(), 1, "lazy materialization");
    c.close_fail(&items[0]);
    c.settle();
    assert_eq!(c.drain_children(&c.step_bead(&run, "implement")).len(), 1, "nothing further");
    assert_eq!(c.get_bead(&c.step_bead(&run, "implement")).outcome.as_deref(), Some("fail"));
}

#[test]
fn a_drain_survives_a_campd_restart_without_double_materializing() {
    // reconcile's third pass; the drain: label is the idempotency ledger.
    let mut c = fixture_campd_with_drain_formula();
    let run = c.sling("build");
    c.create_member(&run, "A");
    c.close_pass(&c.step_bead(&run, "decompose"));
    c.settle();
    let before = c.drain_children(&c.step_bead(&run, "implement"));
    c.restart_campd();                                  // reconcile re-queues the drain
    c.settle();
    assert_eq!(c.drain_children(&c.step_bead(&run, "implement")), before, "idempotent");
}
```

- [ ] **Step 2: Run and watch them fail**
```bash
cargo test -p camp --test daemon_drain 2>&1 | tail -20
```
Expected: FAIL — `PendingDrain` does not exist; no item roots are materialized.

- [ ] **Step 3: Implement the pure reads in `runtime.rs`**

`run_members` SQL (in `readiness.rs`, exposed through `runtime.rs` like the other flow queries):

```sql
SELECT {BEAD_COLS} FROM beads b
 WHERE b.run_id = ?1
   AND b.step_id IS NULL
   AND b.id <> ?2                                   -- ?2 = the run root
   AND b.labels NOT LIKE '%"bond:%'
   AND b.labels NOT LIKE '%"drain:%'
 ORDER BY (SELECT MIN(e.seq) FROM events e WHERE e.bead = b.id AND e.type = 'bead.created'), b.id
```

Then re-parse the labels Rust-side and drop any decoy, exactly as `bond_children`
(`runtime.rs:514-549`) does — a `LIKE` is a prefilter, never the decision.

- [ ] **Step 4: Implement the dispatch arms (ADDITIVE)**

Six additive edits, no refactors:
1. `PendingDrain` beside `PendingFanout` (dispatch.rs:1045).
2. `pending_drains: Vec<PendingDrain>` in `GraphRuntime` (dispatch.rs:1051-1063).
3. `queue_drain` beside `queue_fanout` (dispatch.rs:2180) — dedupe with `Vec::contains`.
4. One arm in `on_bead_closed` (dispatch.rs:1846).
5. One `execute_drain` loop in `execute` (dispatch.rs:1154), same requeue-tail-on-error shape.
6. One pass in `reconcile` (dispatch.rs:1727) — for every `run.cooked`, for every step with
   `drain`, if the anchor is closed-`pass`, re-queue.

`execute_drain` mirrors `execute_fanout` (dispatch.rs:1174-1275): read the members, read
`drain_children` (the idempotency ledger), compute the due indices (all missing, or just
`children.len()` when `single_lane`/`skip_remaining` and every existing child closed `pass`),
reserve, then `cook_with`.

`finalize_if_quiescent` (dispatch.rs:2082) needs **no change**: the item roots hang off the anchor
by `needs`, so the run is not quiescent while they are open, exactly as bond children behave today.
The drain **anchor's** own close is the new bit — close it in `execute_drain` once every member's
item root is closed, using the existing `close_anchor` helper (dispatch.rs:2296), and release the
reservations in the same `append_batch`.

- [ ] **Step 5: Run and watch them pass**
```bash
cargo test -p camp --test daemon_drain 2>&1 | tail -10
cargo test --workspace 2>&1 | tail -10
```

- [ ] **Step 6: Full gates and commit**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "feat(dispatch): the drain runtime — members, exclusive reservations, single_lane, on_item_failure"
```

---

## Task 10: Invariant 6, the corpus gate, and the PR

**Files:** `crates/camp-core/tests/fixtures/formulas/valid/**`, `crates/camp-core/tests/formula_corpus.rs`,
`.github/workflows/ci.yml`

**Invariant 6 is the trap in this task.** The `gc-compat` CI job runs the **real gc compiler** over
`crates/camp-core/tests/fixtures/formulas/valid` (`camp_corpus_validate.go`). Every fixture this
phase adds to that directory must compile **in gc**. A fixture using camp's amended stem rule, a
`contract`-only compiler declaration, or a `drain` step must be a *legal Gas City formula* — which
they all are, because they are copies of the corpus's own shapes. **Any new fixture placed
elsewhere (e.g. `tests/fixtures/compose/`) is NOT covered by that gate** — that is fine and
intended (they are multi-layer fixtures, not standalone formulas), but at least one **standalone**
`drain` / `extends` / `vars` fixture must land in `formulas/valid/` so the camp ⊆ gc direction is
actually exercised on the new key sets.

- [ ] **Step 1: Add gc-valid fixtures for every new key set**

Add to `crates/camp-core/tests/fixtures/formulas/valid/`:
`vars-condition.formula.toml`, `extends-child.formula.toml` (+ its parent), `expansion.formula.toml`,
`drain-separate.formula.toml`. Each declares `contract = "graph.v2"`.

- [ ] **Step 2: Prove they pass the REAL gc compiler locally**

```bash
git clone -q --filter=blob:none https://github.com/gastownhall/gascity /tmp/gascity \
  && git -C /tmp/gascity checkout -q "$(cat ci/gc-compat/GASCITY_REF)"
mkdir -p /tmp/gascity/cmd/camp-corpus-validate
cp ci/gc-compat/camp_corpus_validate.go /tmp/gascity/cmd/camp-corpus-validate/main.go
(cd /tmp/gascity && go build -o /tmp/camp-corpus-validate ./cmd/camp-corpus-validate)
/tmp/camp-corpus-validate crates/camp-core/tests/fixtures/formulas/valid
```
Expected: `OK <name>` for every fixture, exit 0. **A `FAIL` here means camp accepts a formula gc
rejects — invariant 6 is broken and the fixture (or the rule) is wrong.**

- [ ] **Step 3: Run every gate, in the order CI runs them**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 ci/gc-compat/load_corpus_packs.py /tmp/gcpacks target/debug/camp   # compat-1's gate, still green
python3 ci/gc-compat/formula_rungs.py    /tmp/gcpacks target/debug/camp    # 97, and every rung
ci/gc-compat/check_vocab.sh /tmp/gascity "$PWD"                            # formula.refused must not collide
```

- [ ] **Step 4: Push and open the PR**

```bash
git push -u origin compat-2-formulas
gh pr create --title "compat: the formula key sets — rungs 2a–2e, 97/100 at GCPACKS_REF (compat-2)" --body "..."
gh pr checks --watch
```

The PR body must state, with evidence: the ceiling is **97** and why (the vapor/scope-check sets
are disjoint); the per-rung counts (2, 31, 58, 84, 97); that the 21 no-contract formulas **compile
and are refused at run time** (D1); that `single_lane` and `on_item_failure` have **no corpus
coverage** and are proven by synthetic fixtures; and the two spec amendments (master line 449; the
compat §9 addendum).

- [ ] **Step 5: CI green.** `gh pr checks --watch` must be green before this phase is complete.
  Not before.

---

## Exit criteria — and how each is proven

| Exit criterion (phase block, verbatim) | Proof |
|---|---|
| *"every §9 rung's count pinned by a test at GCPACKS_REF"* | `ci/gc-compat/formula_rungs.py`, run by the `gcpacks-compat` CI job (Task 4 Step 8), asserts **2a=2, 2b=31, 2c=58, 2d=84, 2e=97** from **camp's own rung table** applied to the corpus at the pinned ref — and cross-checks that table against the real binary's per-file verdicts, so it cannot be tuned. The corpus is not vendored, so this is a CI gate and not a `cargo test` — exactly the mold compat-1's `load_corpus_packs.py` established, and what §10 asks for. |
| *"refusals name their key and land as ledger events"* | `formula.refused` (Task 2) carries `{formula, path, key, construct, reason}`, is validated in the fold (`deny_unknown_fields`, the `check_passed` mold), and is appended by `camp sling` / order-fire (Task 4 Step 4) and by the drain refusals (Task 8). Tests: `a_refused_key_names_itself`, `sling_refuses_a_formula_with_no_contract_and_events_the_refusal`, `a_shared_context_drain_is_refused_naming_the_formula_the_step_and_drain_policy`. |
| *"camp ⊆ gc gate still green (invariant 6)"* | Task 10 Steps 1–2: new standalone fixtures for every new key set land in `tests/fixtures/formulas/valid/` and are compiled by the **real gc compiler** at `GASCITY_REF`. |
| *"CI green"* | Task 10 Step 5: `gh pr checks --watch`. |
| *"Ceiling is 97–98 and the gate names which"* | **97.** Measured, disjoint, and asserted as a constant in the gate (Task 4 Step 6). |
| *"The 21 no-contract formulas are refused, not assumed"* | D1: they compile (inside the 97) and are `not_runnable`; `camp sling` refuses them with a `formula.refused` event naming `contract`. |
| *"exclusive reservations as member-bead metadata (`gc.exclusive_drain_reservation`, verbatim key)"* | Task 3 (the store + the atomic compare-and-set in the fold) and Task 9 (`an_exclusive_drain_reserves_every_member_with_gcs_verbatim_key`, `a_second_drain_reserving_a_held_member_fails_loudly_and_never_mutates_it`). |
| *"same-session REFUSED"* | Task 8: `setting_drain_policy_to_same_session_refuses_loudly_instead_of_approximating`, and the ordering test proving the default path still compiles clean. |
| *"on_item_failure/single_lane per gc's compiler defaulting"* | Task 8's defaulting table, copied from `ApplyDrainControlMetadata` (gascity `compile.go:583-611`); Task 9's four runtime tests. |

## Self-review notes for the implementer

- **If a measured count moves, stop.** Every number here was measured at
  `GCPACKS_REF = 44b2eef94f035283b70df62d3bd1fc77bce13d56`. A different count means the pin moved
  or a rule is wrong — report to the lead; do not edit the seed to match the code.
- **Do not merge `cook::substitute` (`{name}`) with `compose::substitute` (`{{name}}`).** They are
  different grammars with different scopes. DRY does not mean conflating two languages.
- **The `unimplemented` scaffold (Task 2) must be gone by Task 8.** If it survives into the PR, an
  accepted key silently compiles to nothing — the exact failure mode §4's trap 3 warns about.
- **`dispatch.rs` and `event_loop.rs` are shared with `cp-1`.** Additive edits only; expect a
  rebase; re-run the full gates after it.

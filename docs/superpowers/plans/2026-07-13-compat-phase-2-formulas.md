# Compat Phase 2 — the formula key sets (rungs 2a–2e) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or
> superpowers:subagent-driven-development) to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Status:** rev 2 — after an adversarial plan gate returned **REJECT** (unanimous BLOCK, 4
panelists). Every ruling and every B-defect is addressed below; the map is the first section.

**Goal:** Make camp load and compile the real Gas City formula corpus at
`ci/gc-compat/GCPACKS_REF` — **95 of 100**, of which **62 are runnable** — refusing the other 5
by name, with every §9 rung's count pinned by a CI gate that runs **the real binary**, and every
fidelity claim verified against **the real gc compiler** rather than against a reading of its
source.

**Architecture:** camp's formula compiler today is a *strict subset* validator that rejects every
Gas City construct by name (`parse.rs` `CITY_ONLY_TOP`/`CITY_ONLY_STEP`). Phase 2 inverts it into
a *permissive, layered compiler* following compat §4's three rules — **permissive for imported
pack layers, strict for camp's own `<root>/formulas/`** (D2′). `parse_and_validate` grows a
layered sibling, `compose::compile`, running gc's pipeline order. `drain` (2e) becomes a
**campd-owned** step, the fourth arm of `GraphRuntime` beside check/retry/fanout, and gc's convoy
maps onto camp's run.

**Tech Stack:** Rust (camp-core, camp), `toml`, SQLite; Go (the gc oracle shim, in the existing
`ci/gc-compat` Go job); Python 3 stdlib (`tomllib`) for the corpus gates.

---

## What changed in this revision, and why

| Ruling / defect | What was wrong | Where it is fixed now |
|---|---|---|
| **RULING 1** — re-derive by running the loader, amend §9 to measured truth | Seeds came from **key-set containment only**. Running camp's full rule set finds 3 more unloadable formulas. **Ceiling is 95, not 97.** A **runnable** count (62) was never pinned. | Whole plan re-seeded from `ci/gc-compat/rungs.py` (new, in-repo). **§9 addendum now records 95 + 62** (Task 1). Rungs: **2a=2, 2b=31, 2c=57, 2d=83, 2e=95**. |
| **RULING 1** — third camp-local rule, **S3** | `validate.rs:52-57` requires ≥1 step. **25 corpus formulas have no `steps`**: 11 inherit them via `extends`, **14 are `type = "expansion"` and never have steps**. Task 1 was titled "the two rules" — there are three. | Task 1 now amends **S2, S3, S11**. |
| **RULING 2** — D2 too permissive | "camp cannot distinguish a dead key from a typo" was **false** — `FormulaLayers` knows the origin. Permissiveness for `<root>/formulas/` permits silent graph corruption (`dependson` ⇒ steps run out of order). | **D2′**: permissive for imported layers, **hard error for the camp-local tier**. Task 2. It also saves 2 existing fixtures (B7). |
| **RULING 3** — use the gc oracle | Every fidelity claim (pointer prompt, drain defaulting, extends merge, expand, condition-prune) was read from gc's *source*, never *run*. `camp_corpus_validate.go` already links gc's real compiler and **throws the compiled formula away**. | **New Task 11: the differential gate.** gc and camp both compile all 100; the step lists, descriptions, metadata and drain specs are diffed. Settles B15 for free. |
| **B1** — `bead_meta` escapes refold | I updated the **test's** `DUMPS`, not the **production** `refold.rs::STATE_TABLES` (:28-60). ⇒ `refold_check` never diffs it; **`refold_repair` hard-fails on the FK**; and the property test is **vacuous** (no op emits metadata). | Task 3: `STATE_TABLES` gains `bead_meta` **after `beads`**; `refold_prop::Op` gains `SetMeta`/`Reserve`/`Release`. |
| **B2** — no `SCHEMA_VERSION` bump | `bead_meta` is fold truth. Without a bump, an existing camp.db opens fine and dies later at `no such table`. | Task 3: **SCHEMA_VERSION 2 → 3**. |
| **B3** — nothing actually refused the 3 (now 5) | `classify(site, key)` is **value-blind**. `phase` was in no table (⇒ ignored, **deleting a merged refusal**); `design-review`'s scope-ness is entirely in **metadata values**; and **`gc.scope_kind` does not exist in the corpus** — I fabricated it. | Task 2: a **value-aware refusal table** (`refuse(site, key, value)`), covering `phase` (any value) and metadata `gc.kind = "scope"` / `gc.scope_*`. Real keys only. |
| **B4** — drain anchor specified 3 ways, guaranteed `InvalidTransition` | Trigger said "queue on anchor closed pass"; finalize said "close the anchor" ⇒ closing a closed bead. Tests described a different system. | **Task 9: ONE model — the drain anchor is campd-owned.** campd claims it when ready (`is_campd_held`, the `maybe_claim_looping` mold), scatters, gathers, then closes it. Tests re-derived. |
| **B5** — run finalizes while items run | My justification was false: `flow::finalization` iterates **step anchors only**; bond children never block quiescence. | Falls out of B4's fix: the campd-held anchor stays `in_progress` until gather, so the run is not quiescent and downstream `needs` stay blocked. Pinned by a test. |
| **B6** — `single_lane`'s mechanism is inert | `dispatchable_beads` **excludes run roots outright** (readiness.rs:139) ⇒ a `needs` edge on an item root gates nothing. And lazy materialization alone imports `skip_remaining` semantics. | Task 9: an explicit **4-cell materialization matrix**; `single_lane + continue` advances on the previous root **closing at all** (any outcome), `skip_remaining` on it **closing pass**. Test with a failing item. |
| **B7** — Task 2 detonates `formula_corpus.rs` | Never named. 52 invalid fixtures, a 55-row table, `assert_eq!(on_disk, in_table)`. ~27 rejections become non-rejections; a `Refusal` is not a `Violation` so even the still-refused ones would return `Ok`. | Task 2 §"Fixture disposition": exact fate of all 55 rows; `FormulaError` gains `refusals` + `names()`. |
| **B8** — seeds not re-derivable | `measure_corpus.py` computes **no rung count**. The seeds came from a throwaway script. The gate's counting rule existed only as prose. | **`ci/gc-compat/rungs.py`** (new, in-repo), implementing §9's text with the arithmetic **written as pseudocode** and independent of camp's `RUNGS` table. It is the arbiter. |
| **B9** — vacuous rung test | `for k in r.top { assert classify(Top,k) == Accepted }` is true by construction (`accepted_top` is *defined* as base ∪ RUNGS). | Task 2: replaced with an assertion against a **literal transcription of §9's table**. |
| **B10** — no test harness, no fixtures | ~12 undefined helpers; the two zero-coverage semantics rest on fixtures given nowhere. | Task 9: harness named (**copy `Daemon`/`wait_until`/`scaffold` from `daemon_dispatch.rs`**), every helper defined, and **full TOML for both zero-coverage fixtures**. |
| **B11** — reservations leak, no escape | Release specified only on clean finalize. Conflict / dead-end / kill-9 / `skip_remaining` all leak, and `bd update --set-metadata` is deferred ⇒ **no operator escape**. | Task 9: release on **every** exit path + a reconcile sweep for orphans + `camp doctor --drain-reservations [--release-orphans]`. |
| **B12** — order-fire refusal has no task | Exit criterion claimed it; only `sling.rs` was changed. A cron order could fire one of the 21 under graph.v2 with nothing refusing it (§13's money invariant). | **Task 4 Step 6** (new): the daemon order-fire path refuses `not_runnable`, with a test in `daemon_orders.rs`. |
| **B13** — `run_members` has no type filter | compat-4's `mail` beads (dispatch-excluded) would be scattered and reserved. | Task 9: `AND b.type = 'task'`. |
| **B14** — `advice`/`pointcuts` contradiction | Deferrals table said *dropped*; `REFUSED_TOP` said *refused*. | **D4**: **REFUSED** (§4 rule 1). Preserves the merged behaviour and 2 fixtures; 0 corpus uses. §9's "dropped" describes *gc's merge*, not camp's acceptance. |
| **B15** — `description_file` vs `{{var}}` unspecified | Worse: my pipeline had vars **before** description_file. gc resolves description_file at **parse** time (`parser.go:187`) and substitutes vars **later** — so inlined contents **are** substituted, which is precisely what the pointer prompt's `Formula Variables` block is for. | **Pipeline order corrected** (Task 4). Verified by Task 11's differential gate. |
| Non-blocking notes | 12 items | Folded in; see below. |

### Corrections folded in (non-blocking notes)

- **Counts, recursive (incl. `template` and `children` steps — they become real steps after
  expand):** `gc.run_target` **327** occurrences; `description_file` **328** targets in **67**
  formulas (**53** carry one on a *top-level* step — the §9 figure); **8** targets exceed 4096
  bytes. Corrected from 187/188/7.
- **There are 4 distinct conditions, not 3.** The one I missed is `{{review_mode}} != report`
  (4 uses), and it lives inside `children` — so **condition pruning must recurse into children**.
  Its default **varies by pack** (`report` in `code-review-base`/`review`/`planning-base`,
  `agent` in `build-base`, `interactive` in `gstack-build`), so it prunes on the default path.
- `mol-pr-from-issue.formula.toml` lives in **`pr-pipeline/formulas/`**, not `gastown/`.
- `reconcile` is at **dispatch.rs:1645**.
- `formula/mod.rs:1-5`'s module doc says camp "accepts no unknown keys, where gc silently ignores
  them" — **D2′ inverts that sentence**; Task 2 amends the doc.
- `DEAD_TOP` was declared and never read ⇒ `-D warnings` dead_code. It is now **read**: D2′ needs
  it, because a **known-dead gc key** and an **unrecognised key** are different classes.
- `substitute`/`eval_condition`/`parse_drain` are `pub(crate)` ⇒ their unit tests live **inside**
  `compose.rs`/`drain.rs` (`#[cfg(test)] mod tests`), not in the external `tests/compose.rs`.
- `crates/camp/tests/cli_doctor_formula.rs` asserts doctor's exit codes + the ArgGroup — adding
  `--json` / `--formula-rungs` touches it. Named in Task 4.
- **`gc.work_branch` / `gc.routed_to` must NOT be writable in `bead_meta`** — `beads` already has
  a `work_branch` column and `assignee`. **Rule (Task 3): a metadata key that has a dedicated
  column is PROJECTED at read time and REFUSED at write time, naming the column.** compat-3
  inherits one source of truth, not two.
- **`ready_task_count` (readiness.rs:160) lacks the run-root exclusion** that
  `dispatchable_beads` has, so every `camp create --run` member would be counted "ready" and never
  dispatched — a permanently non-decreasing count in `camp top`. Task 3 adds the exclusion.
- **`description_file` is an unbounded read from an untrusted pack.** The non-asset branch was a
  bare `base_dir.join(raw)`. **Task 4 contains it to the pack root** and refuses an escape.
- **Corpus-drift procedure** written down (Task 10).
- `camp_corpus_validate.go` derives a formula's name as `TrimSuffix(basename, ".toml")` ⇒ new
  fixtures must **not** be named `*.formula.toml` or gc receives the name `"x.formula"`. Task 10.

---

## Authority and provenance

| Rank | Document | What it decides |
|---|---|---|
| 1 | `docs/design/2026-07-05-gas-camp-design.md` | Master spec; §4 decision record settled. **This plan amends line 449** (Task 1). |
| 2 | `docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` (rev 4) | **§4** (permissiveness + three traps), **§9** (rung semantics), **§10** (the gate), **§12.2**. **This plan amends §9's ceiling to measured truth** (Task 1). |
| 3 | `docs/superpowers/specs/2026-07-12-KNOWN-DEFECTS.md` | "Verified correct" list is settled. |
| 4 | `docs/superpowers/plans/2026-07-13-wave-2-compat-orchestration.md` | Branch, gates, shared-file protocol. |

`AGENTS.md` invariants bind every task. Invariant 5 (**fail fast; no fallbacks; no panics in
library code**), invariant 6 (**camp ⊆ gc**), invariant 7 (**vocabulary mirror**).

**Provenance of every number:** corpus at `GCPACKS_REF = 44b2eef94f035283b70df62d3bd1fc77bce13d56`,
gc source at `GASCITY_REF = 12410301884b51131a35e101a335dbaae16cdcb0`, walked with `tomllib`
(never regex — KNOWN-DEFECTS' two traps), globbing `formulas/*.toml` (not `*.formula.toml` —
gastown's 8 `mol-*.toml` break the convention). **Re-derive with `ci/gc-compat/rungs.py`, which
this plan adds to the repo** (B8).

---

## Global Constraints

- **Branch:** `compat-2-formulas`. One reviewable PR. Never commit to main.
- **Gates before every push:** `cargo fmt --all --check` && `cargo clippy --workspace
  --all-targets --all-features -- -D warnings` && `cargo test --workspace`. Not complete until
  pushed and `gh pr checks --watch` is green.
- **TDD, strictly.** Write the failing test, run it, *watch it fail with the expected message*,
  implement, watch it pass.
- **No panics in library code.** `unwrap_used`/`expect_used`/`panic` are clippy-denied;
  `unsafe_code` forbidden. Tests carry the existing `#[allow(...)]` on `mod tests`.
- **No network in `cargo test`.** The corpus is never vendored (§10 — `gascity-packs` has no
  top-level LICENSE; gascamp is AGPL-3.0). Corpus assertions live **only** in CI gate scripts.
- **New events:** four lockstep edits — `EventType` enum + `ALL` + `as_str` (`event.rs`), a `match`
  arm in `fold::apply` (`fold.rs`), and `CAMP_SPECIFIC_EVENTS` (`vocab.rs`). Payloads: private,
  `#[serde(deny_unknown_fields)]`, validated in the fold (the `check_passed` mold, fold.rs:680).
- **New fold state:** a state table must be added to **BOTH** `refold.rs::STATE_TABLES` (production)
  **and** `refold_prop.rs::DUMPS` (test), and needs a **`SCHEMA_VERSION` bump** (B1, B2).
- **Shared files — ADDITIVE ONLY, no refactors.** `cp-1-control-protocol` is in flight and owns the
  socket/protocol/daemon-control surface. Contended: `daemon/dispatch.rs`, `daemon/event_loop.rs`,
  `camp/src/main.rs`, `event.rs`, `vocab.rs`, `ledger/fold.rs`, `Cargo.toml`/`Cargo.lock`,
  `.github/workflows/ci.yml`. Expect a real rebase.
- **Commits:** no co-author trailers, never mention the agent.

---

## Decisions this plan pins

### D1. "Corpus loading" = COMPILES, not RUNNABLE. **RATIFIED by the plan gate — unchanged.**

§9's rung column measures **compilation**. The 21 no-contract formulas (§9: *"camp must not **run**
them under graph.v2 semantics"* — *run*, not *load*) **compile**, and are refused at **run** time.
Two independent supports: §9's own wording, and the arithmetic (no reading that subtracts the 21
can reach §9's stated 97–98; 100−3−21 = 76).

- **Compile:** parse, resolve `extends`, expand, inline `description_file`, substitute vars, prune
  conditions, resolve routes; report every ignored key and every refused key.
- **Runnable:** only a formula with `contract = "graph.v2"` **and** `type != "expansion"` may be
  cooked. `camp sling` **and the daemon's order-fire path** (B12) refuse a `not_runnable` formula,
  naming the key, with a `formula.refused` ledger event.

**Both numbers are pinned and both go in the PR body: LOADABLE = 95, RUNNABLE = 62.** "95/100"
alone is misleading — 62 is the number that answers *"can camp run gc's packs?"*

### D2′. Permissive for IMPORTED layers; STRICT for camp's own `<root>/formulas/`. **(Ruling 2)**

Rev 1 argued camp *cannot* distinguish a gc-dead key from a typo. That was wrong: **`FormulaLayers`
— this plan's own new type — knows which layer a formula came from.** §4's permissiveness argument
is about *consuming someone else's pack*; extending it to the operator's own formulas is a choice,
and a bad one: `dependson = ["build"]` in a camp-local formula would be silently ignored, the step
would **run out of order**, and the only surface would be an opt-in `camp doctor`. That is silent
graph corruption — invariant 5.

| origin | unrecognised key | known-dead gc key | annotation | gc-semantics-camp-lacks |
|---|---|---|---|---|
| **imported layer** (`.camp/imports/**`, incl. transitive) | **ignored + warned**, aggregated into `import.added`'s `ignored_keys` (§5.4) | ignored + warned | silent | **refused, naming the key** |
| **camp-local** (`<root>/formulas/`) | **HARD ERROR, naming the key** | ignored + warned | silent | **refused, naming the key** |

A **known-dead** gc key (`version`, `target_required`, `internal`, top-level `mode`/`single_lane`,
`sling_container_mode`) is ignored-and-warned in **both** tiers — it is a real gc key, not a typo,
and a camp formula may legitimately carry it to stay portable. Only **unrecognised** keys differ by
tier. This is why `DEAD_TOP` is now load-bearing (it was dead code in rev 1).

### D3. gc's **convoy** is camp's **run**; a drain's members are the run's member beads. *(Unchanged; `camp create --run` ratified.)*

gc: *"Drain scatters the input convoy into one-member unit convoys"* (`types.go:317-319`), member
set = `convoycore.Members(store, parentConvoyID)` (`dispatch/drain.go:211`).

> **A run member** is a bead with `run_id = <the drain's run>`, `step_id IS NULL`,
> **`type = 'task'`** (B13), which is **not** the run root and carries **no** `bond:`/`drain:` label.

Members enter a run via `camp create --run <run_id>` (additive flag; `bead.created` already folds
`run_id`). compat-3's `bd create` maps onto it. Members are **never dispatchable** —
`dispatchable_beads` already excludes `run_id IS NOT NULL AND step_id IS NULL` (readiness.rs:139)
— which is correct: a member is *data for the drain*, not work. (Task 3 fixes `ready_task_count`,
which lacks that exclusion.)

### D4. `advice` / `pointcuts` are **REFUSED**, not dropped. **(B14)**

§9's *"`advice`/`pointcuts` are dropped entirely"* describes **gc's `extends` merge** (a parent's
advice is not inherited), not camp's *acceptance*. Camp does not implement advice ⇒ §4 rule 1 ⇒
**refuse, naming the key**. **0 corpus uses**, so no count moves; it preserves camp's merged
behaviour and two existing fixtures. If a future corpus parent carries `advice`, camp refuses
loudly rather than silently dropping semantics — invariant 5.

---

## Deliberately deferred (named, so nothing is a silent gap)

| Deferred | Why |
|---|---|
| `drain.max_units` | gc key (`DrainSpec.MaxUnits`), semantics camp does not implement. **0 corpus uses.** §4 rule 1 ⇒ **refused by name**. |
| `drain.continuation_group` | Valid only with `context = "shared"`, which camp refuses (§6.2, §11.4: *"`gc.continuation_group` is not honoured"*). **0 uses.** **Refused.** |
| `context = "shared"` drains | §9: *"REFUSED, loudly."* |
| `gate`, `loop`, `pour`, `compose`, `tally`, `waits_for`, `depends_on` | §4 rule 1 refusals; **0 corpus uses** each. `tally`'s existing message (parse.rs:324-329) survives verbatim. |
| `bd update --set-metadata` (the CLI verb) | compat-3. Task 3 ships the **event + fold** it will use. **B11's operator escape does not depend on it** — `camp doctor --drain-reservations --release-orphans` ships here. |
| `gc.routed_to` / `gc.work_branch` **stamping**, `hook --claim`, the shims | compat-3 (§6.1/§6.2). Task 3 fixes their **storage rule** now (projected, not stored) so compat-3 cannot inherit two sources of truth. |
| **compat-2's `drain` ≠ compat-3's `drain-ack`** | Three unrelated things share the word: gc's `runtime drain-ack` is a *worker-session exit handshake* (§6.2); compat-2's `drain` is the *formula scatter/gather*; "released at drain end" is the *reservation release*. **compat-2's drain is triggered entirely by ledger events and does NOT depend on compat-3.** |

---

## The measured seed table

Re-derived by simulating camp's **full rule set** (pipeline + S-rules as amended), not key-set
containment. Reproduce with `ci/gc-compat/rungs.py <corpus>` (Task 2 adds it).

| rung | key set added (§9) | **loadable** | rev-1 claim (wrong) |
|---|---|---|---|
| 2a | dead keys ignored; annotations; `contract`; `description_file`; step `metadata` (incl. `gc.run_target`) | **2** | 2 |
| 2b | `vars`, `condition` | **31** | 31 |
| 2c | `extends` | **57** | 58 |
| 2d | `type`, `template`, `expand`, `expand_vars`, `children` | **83** | 84 |
| **2e** | **`drain`** | **95** ← the ceiling | 97 |
| | **RUNNABLE** (`contract = "graph.v2"` ∧ `type != "expansion"`) | **62** | *never pinned* |

**The 5 formulas camp cannot load, each for a reason §9 did not anticipate:**

| file | refusal | note |
|---|---|---|
| `gastown/formulas/mol-digest-generate.toml` | `phase` (= `"vapor"`) | §9's 2 vapor formulas… |
| `pr-pipeline/formulas/mol-pr-from-issue.formula.toml` | `phase` (= `"vapor"`) | …in `pr-pipeline`, not gastown |
| `gascity/formulas/design-review.formula.toml` | step metadata `gc.kind = "scope"` (+ `gc.scope_name`/`gc.scope_role`/`gc.scope_ref`) | §9's scope-check formula. **`gc.scope_kind` does not exist anywhere in the corpus** — rev 1 fabricated it (B3). |
| **`gascity/formulas/same-session-implement.formula.toml`** | `drain.context = "shared"` | **NEW.** §9 and rev 1 both assert all shared drains sit behind `{{drain_policy}} == same-session`. **12 do. This one has NO `condition`** — nothing prunes it. |
| **`gastown/formulas/mol-polecat-work.toml`** | `extends` → `mol-polecat-base` **does not exist in the corpus** | **NEW.** The parent ships inside gc's binary-embedded core pack, which `gascity-packs` does not distribute. An unresolvable parent is a hard error (invariant 5). |

**95 < §9's declared 97–98. That is a real spec contradiction, and Task 1 amends the spec to the
measured truth** (`AGENTS.md`: spec and code never silently diverge).

Corroborating: `contract = "graph.v2"` 79 / absent 21 · `[requires]` in exactly 4 · 36 top-level
steps use `check`/`retry`/`on_complete` (50 counting template steps) and **all** declare
`contract` · `version` 93 · `target_required` 64 · `internal` 40 · top-level `mode` 7 ·
top-level `single_lane` 6 · `sling_container_mode` 1 · `catalog` 17 · formula-level `metadata` 16
· **step `assignee`: 0** (routing is *exclusively* step metadata) · **0 bare route values** · 25
drain steps (12 `separate`, 13 `shared`; all 25 `member_access = "exclusive"`; `on_item_failure`
and `item.single_lane` appear **only** on the 13 shared ones) · **no two of the 100 share a
basename** (so one camp root imports all 10 formula-bearing packs with no within-tier collision).

---

## File Structure

**Created:** `formula/keys.rs` (the value-aware key table + the §9 rung table) · `formula/layers.rs`
(`FormulaLayers` — search path + origin tier) · `formula/compose.rs` (the pipeline) ·
`formula/drain.rs` (the `Drain` AST + refusals) · `camp-core/tests/compose.rs` +
`tests/fixtures/compose/**` · `camp/tests/cli_doctor_corpus.rs` · `camp/tests/daemon_drain.rs` ·
**`ci/gc-compat/rungs.py`** (the independent arbiter, B8) · **`ci/gc-compat/formula_gate.py`** (the
§10 gate: runs the real binary) · **`ci/gc-compat/gc_compile_json.go`** + **`ci/gc-compat/differential.py`**
(the gc oracle, Ruling 3) · `ci/gc-compat/README.md` (the drift procedure).

**Modified:** `formula/{parse,ast,validate,mod,cook,runtime}.rs` · `ledger/{schema,fold,refold,mod}.rs`
· `event.rs` · `vocab.rs` · `readiness.rs` · `orders/mod.rs` · `camp/src/cmd/{doctor,sling,create}.rs`
· `camp/src/main.rs` · `camp/src/daemon/{dispatch,orders}.rs` (**additive**) ·
`camp-core/tests/{refold_prop,formula_corpus,cook}.rs` · `camp/tests/{cli_doctor_formula,daemon_orders}.rs`
· `tests/fixtures/formulas/{valid,invalid}/**` · `.github/workflows/ci.yml` · the two spec docs.

---

# Tasks

## Task 1: The three camp-local rules that refuse the corpus, and the spec amendments

**Measured** (all reproduced independently by the plan gate):

| rule | where | how it refuses the corpus |
|---|---|---|
| **S2** — formula name == file stem | `validate.rs:34-50` | Corpus files are `<name>.formula.toml` ⇒ stem `bmad-build.formula` ≠ name `bmad-build`. **92/100 violate.** (compat-1's `orders::resolve_formula` **already** accepts both spellings — the resolver and validator disagree today.) |
| **S3** — at least one step | `validate.rs:52-57` | **25/100 have no `steps` key.** 11 inherit steps via `extends` (fine — validate runs after extends, at stage 8). **14 are `type = "expansion"`: template-only, and they NEVER have steps.** |
| **S11** — graph-only construct requires `[requires] formula_compiler` | `validate.rs:178-191`; **master spec line 449** | **Only 4/100 declare `[requires]`**, while 36 use `check`/`retry`/`on_complete`. **All 36 declare `contract = "graph.v2"`** — gc's own compiler declaration. |

**Files:** `crates/camp-core/src/formula/validate.rs` · `docs/design/2026-07-05-gas-camp-design.md:449`
· `docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` (§9 addendum)

**Interfaces produced:**
```rust
// validate.rs
/// The canonical stem: file name minus `.toml`, minus an optional trailing
/// `.formula`. 92 of the 100 corpus formulas are `<name>.formula.toml`
/// declaring `formula = "<name>"`; compat-1's `orders::resolve_formula`
/// already resolves both spellings.
pub(crate) fn formula_stem(path: &Path) -> Option<&str>;
```
`RawFormula` gains `pub contract: Option<String>` and `pub kind: Option<String>` (the top-level
`type`) — one line each in `parse::walk`; S3 and S11 cannot be expressed without them.

- [ ] **Step 1: Write the failing tests** (in `validate.rs`'s `mod tests`)

```rust
#[test]
fn the_corpus_file_naming_satisfies_the_stem_rule() {
    assert_eq!(formula_stem(Path::new("/p/bmad-build.formula.toml")), Some("bmad-build"));
    assert_eq!(formula_stem(Path::new("/p/mol-digest-generate.toml")), Some("mol-digest-generate"));
    assert_eq!(formula_stem(Path::new("/p/formula.toml")), Some("formula"),
               "`.formula` is stripped only as a SUFFIX, never as the whole stem");
}

#[test]
fn an_expansion_formula_needs_no_steps() {
    // S3 amended: 14 corpus formulas are `type = "expansion"` — template-only,
    // and they never have `steps`. Rev 1 missed this rule entirely.
    let text = "formula = \"e\"\ntype = \"expansion\"\ncontract = \"graph.v2\"\n\
                [[template]]\nid = \"t\"\ntitle = \"T\"\n";
    let (raw, mut v) = crate::formula::parse::walk(text, Origin::CampLocal);
    check(&raw, Some("e"), &mut v);
    assert!(!v.iter().any(|v| v.message.contains("at least one step")), "{v:?}");
}

#[test]
fn a_non_expansion_formula_with_no_steps_is_still_a_violation() {
    let text = "formula = \"e\"\ncontract = \"graph.v2\"\n";
    let (raw, mut v) = crate::formula::parse::walk(text, Origin::CampLocal);
    check(&raw, Some("e"), &mut v);
    assert!(v.iter().any(|v| v.message.contains("at least one step")), "{v:?}");
}

#[test]
fn contract_graph_v2_satisfies_the_compiler_declaration_rule() {
    // S11 amended: master spec line 449. All 36 corpus formulas using
    // graph-only constructs declare `contract`; NONE declares [requires].
    let text = "formula = \"x\"\ncontract = \"graph.v2\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n\
                [steps.retry]\nmax_attempts = 2\n";
    let (raw, mut v) = crate::formula::parse::walk(text, Origin::CampLocal);
    check(&raw, Some("x"), &mut v);
    assert!(!v.iter().any(|v| v.message.contains("formula_compiler")), "{v:?}");
}

#[test]
fn a_graph_only_construct_with_neither_declaration_is_still_a_violation() {
    let text = "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n\
                [steps.retry]\nmax_attempts = 2\n";
    let (raw, mut v) = crate::formula::parse::walk(text, Origin::CampLocal);
    check(&raw, Some("x"), &mut v);
    assert!(v.iter().any(|v| v.message.contains("formula_compiler")), "{v:?}");
}
```

*(Task 1 lands before Task 2's `Origin` parameter exists. Write `walk(text)` here and add the
`Origin` argument in Task 2 — or land Task 2's `walk` signature first. The plan orders Task 1
first because its rules are what let the corpus load at all; if the implementer prefers, Tasks 1
and 2 may be merged into one commit. **Do not skip either.**)*

- [ ] **Step 2: Run and watch them fail**
  `cargo test -p camp-core --lib formula::validate 2>&1 | tail -20`
  Expected: `formula_stem` not found; then the `.formula.toml` case fails; S3 and S11 tests fail
  with the violation present.

- [ ] **Step 3: Implement**

```rust
pub(crate) fn formula_stem(path: &Path) -> Option<&str> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".toml")?;
    Some(stem.strip_suffix(".formula").unwrap_or(stem))
}
```
Route S2's call site (`formula/mod.rs:24`, which computes `stem`) through it.

**S3** — the "at least one step" check becomes:
```rust
// An `type = "expansion"` formula supplies `template` steps for `expand`; it
// declares no `steps` and is not directly runnable (compat §9). 14 corpus
// formulas — every one of them.
let is_expansion = raw.kind.as_deref() == Some("expansion");
if raw.steps.is_empty() && !is_expansion { /* the existing violation */ }
if is_expansion && raw.template.is_empty() {
    out.push(Violation { construct: "template".into(),
        message: "a `type = \"expansion\"` formula must declare at least one [[template]] step".into() });
}
```

**S11** — the predicate becomes strictly wider (so no merged formula loses its verdict):
```rust
let declares_compiler =
    raw.formula_compiler.is_some() || raw.contract.as_deref() == Some("graph.v2");
```
Message: `"a graph-only construct (check/retry/on_complete/drain) requires a compiler declaration:
either contract = \"graph.v2\" (Gas City's spelling) or [requires] formula_compiler (master spec
§8.2 as amended by compat §9)"`. Update `check-without-requires`'s expectations if the text is
asserted.

- [ ] **Step 4: Run and watch them pass** — `cargo test -p camp-core --lib formula:: 2>&1 | tail -10`

- [ ] **Step 5: Amend the specs**

`docs/design/2026-07-05-gas-camp-design.md` **line 449** — replace the cell with:

> `| `formula`, `description`, `contract = "graph.v2"` **or** `[requires] formula_compiler = ">=2.0.0"` | file header; camp requires a compiler declaration for graph-only constructs — **either** Gas City's `contract` **or** the `[requires]` form. *(Amended 2026-07-13, compat phase 2: 36 of 100 corpus formulas use graph-only constructs; all declare `contract`, none declares `[requires]`.)* |`

`docs/superpowers/specs/2026-07-12-gas-city-pack-compatibility-design.md` — append to §9:

```markdown
**§9 addendum (compat phase 2, 2026-07-13) — MEASURED at `GCPACKS_REF` by running camp's own rule
set over the corpus. It CORRECTS this section.**

- **The ceiling is 95, not 97–98.** §9's "97–98" counted only `phase = "vapor"` (2) and
  `scope-check` (1). Three further formulas cannot load, for reasons §9 did not anticipate:

  | file | why |
  |---|---|
  | `gascity/formulas/same-session-implement.formula.toml` | an **UNCONDITIONAL** `context = "shared"` drain. §9 states all shared drains sit behind `{{drain_policy}} == same-session`; **12 of the 13 do — this one has no `condition`**, so nothing prunes it and camp refuses it (§9's own "REFUSED, loudly"). |
  | `gastown/formulas/mol-polecat-work.toml` | `extends = ["mol-polecat-base"]`, a parent that **exists nowhere in `gascity-packs`** (it ships inside gc's binary-embedded core pack). An unresolvable parent is a hard error (invariant 5). |
  | `gascity/formulas/design-review.formula.toml` | the scope-check formula. Its scope-ness lives **entirely in step-metadata VALUES** — `gc.kind = "scope"`, `gc.scope_name`, `gc.scope_role`, `gc.scope_ref`. **There is no `gc.scope_kind` key in the corpus.** |

- **Per-rung LOADABLE counts, pinned by the gate:** 2a **2** · 2b **31** · 2c **57** · 2d **83** ·
  2e **95**.
- **RUNNABLE = 62**, pinned separately. "Corpus loading" means **compiles**, not **runnable**: the
  21 no-contract formulas and the 14 `type = "expansion"` formulas compile (they are inside the 95)
  and are refused at **run** time by `camp sling` **and by the daemon's order-fire path**, with a
  `formula.refused` ledger event. Any reading that excluded them from the load count would cap the
  ceiling at 76 and contradict this section's own 97–98.
- **Three camp-local rules were refusing the corpus and are amended:** the file-stem rule strips an
  optional trailing `.formula` (92/100 files are `<name>.formula.toml`); **`type = "expansion"`
  formulas declare `template`, not `steps`** (14/100 — the "at least one step" rule no longer
  applies to them); and the compiler-declaration rule is satisfied by `contract = "graph.v2"` as
  well as by `[requires] formula_compiler` (master spec line 449, amended in the same change).
- **The permissiveness rule (§4) is scoped BY ORIGIN.** Unrecognised keys are ignored-and-warned in
  **imported pack layers** and are a **hard error in camp's own `<root>/formulas/`**. camp *can*
  tell them apart — the formula's layer is known. Ignoring a `dependson` typo in an operator's own
  formula would silently reorder their graph (invariant 5). Known-dead gc keys (`version`,
  `target_required`, `internal`, top-level `mode`/`single_lane`, `sling_container_mode`) are
  ignored-and-warned in **both** tiers.
- **gc's convoy is camp's run.** A drain's members (gc `internal/dispatch/drain.go:211`,
  `convoycore.Members`) are the run's member beads: `run_id = <run>`, `step_id IS NULL`,
  `type = 'task'`, not the root, no `bond:`/`drain:` label. `camp create --run <run_id>` adds one.
- **`description_file` is resolved at PARSE time, BEFORE `{{var}}` substitution** (gc
  `parser.go:186-190`), so inlined contents **are** substituted — which is exactly what the >4096
  pointer prompt's `## Formula Variables` block is for.
- **Fidelity is VERIFIED, not asserted.** `ci/gc-compat/differential.py` compiles all 100 formulas
  in **the real gc compiler** and in camp and diffs the results (step lists and order, descriptions,
  metadata, drain specs). The pointer prompt, gc's drain defaulting, the `extends` merge, `expand`,
  and condition-pruning are checked against gc's *behaviour*, not its source.
- **Deferred, by name:** `drain.max_units`, `drain.continuation_group`, `advice`, `pointcuts` (all
  §4 rule 1 refusals; 0 corpus uses each). §9's "`advice`/`pointcuts` are dropped entirely"
  describes **gc's `extends` merge**, not camp's acceptance.
```

- [ ] **Step 6: Full gates and commit**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "fix(formula): the stem, expansion-steps and compiler-declaration rules accept the gc corpus"
```
Any *other* test that breaks is asserting the old rules — fix it here.

---

## Task 2: The key table (value-aware), D2′, the refusal event, and the fixture corpus

**Files:** create `formula/keys.rs`, `ci/gc-compat/rungs.py` · modify `parse.rs` (replace
`CITY_ONLY_*`/`ACCEPTED_*`, :42-87, and the key loops at :228-236 and :318-335), `ast.rs`,
`formula/mod.rs` (**incl. its module doc — D2′ inverts it**), `event.rs`, `vocab.rs`, `fold.rs`,
`tests/formula_corpus.rs`, `tests/fixtures/formulas/**`

**Interfaces produced:**

```rust
// formula/keys.rs

/// Where a key sits. §4 trap 1 — "key off NESTING, never name": top-level
/// `mode`/`single_lane` are DEAD; `steps.check.check.mode` and
/// `steps.drain.item.single_lane` are load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site { Top, Step, Check, CheckInner, Retry, OnComplete, Drain, DrainItem }

/// Which tier a formula came from. D2′ — the permissiveness rule is scoped by
/// ORIGIN, and `FormulaLayers` knows the origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin { Imported, CampLocal }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Accepted,
    /// gc has semantics camp does not implement → refuse, naming the key (rule 1).
    Refused,
    /// A real gc key with NO semantics in gc → ignore + warn, in BOTH tiers (rule 2).
    DeadInGc,
    /// Pure annotation → ignore silently (rule 3).
    Annotation,
    /// Recognised by nobody. Imported ⇒ ignore + warn. Camp-local ⇒ HARD ERROR (D2′).
    Unknown,
}

pub fn classify(site: Site, key: &str) -> Class;

/// The VALUE-AWARE refusal layer (B3). `classify` alone cannot express
/// `phase = "vapor"`, nor a scope-check hiding in step-metadata VALUES.
pub fn refuse(site: Site, key: &str, value: &toml::Value, at: &str) -> Option<Refusal>;

#[derive(Debug, Clone, Copy)]
pub struct Rung { pub id: &'static str, pub top: &'static [&'static str], pub step: &'static [&'static str] }
pub const RUNGS: &[Rung] = &[ /* §9's table, verbatim — Step 3 */ ];
```

```rust
// formula/ast.rs
/// A key camp REFUSES (§4 rule 1). Distinct from `Violation` (a shape or
/// semantic error). Both make a formula fail to compile; only a Refusal names
/// a Gas City construct camp deliberately does not implement.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal { pub construct: String, pub key: String, pub reason: String }

/// FormulaError now carries BOTH lists (B7 — a Refusal is not a Violation, and
/// `parse_and_validate` must Err on either).
#[derive(Debug)]
pub struct FormulaError {
    pub path: PathBuf,
    pub violations: Vec<Violation>,
    pub refusals: Vec<Refusal>,
}
impl FormulaError {
    /// True when any violation or refusal names `construct` — the predicate
    /// `formula_corpus.rs`'s REJECTIONS table uses.
    pub fn names(&self, construct: &str) -> bool;
}
```

### The value-aware refusal rules (B3) — real keys only

| site | key | condition | refusal key reported |
|---|---|---|---|
| `Top` | `phase` | **any value** | `phase` |
| `Step` | `metadata` | the map has `gc.kind = "scope"` | `gc.kind` |
| `Step` | `metadata` | the map has any `gc.scope_*` key | that key |
| `Drain` | `context` | value == `"shared"` | `context` |
| `Drain` | `continuation_group` / `max_units` | present | that key |

**`phase` refuses on the KEY, not the value.** §9 writes `phase = "vapor"` and **the corpus contains
only `vapor`** (measured), so the two are indistinguishable in practice — and refusing the key
preserves camp's *merged* behaviour (`parse.rs:44` already lists `phase` in `CITY_ONLY_TOP`; rev 1's
table silently **deleted** that refusal). The reason string names the value it saw. This cannot
regress and cannot under-refuse.

**Metadata is `Accepted` as a KEY (§4 trap 3 — it is routing) and its VALUES are inspected.**
`gc.run_target` is honoured (routing, Task 4); `gc.kind = "scope"` and `gc.scope_*` are refused;
**`gc.kind = "cleanup"` — also in `design-review` — is NOT refused.** Every other metadata key rides
along untouched (invariant 7).

Corpus-wide, the entire scope surface is (measured, verbatim): `gc.kind=scope` ×1,
`gc.scope_name=design-review` ×1, `gc.scope_role=body` ×1, `gc.scope_ref=body` ×2,
`gc.scope_role=member` ×1 — **all inside `design-review.formula.toml`**.

### Fixture disposition (B7) — the exact fate of all 55 REJECTIONS rows

`crates/camp-core/tests/formula_corpus.rs` holds a 55-row table over 52 invalid fixtures, plus
`assert_eq!(on_disk, in_table)` and a hard-coded 5-name list for `valid/`. Rewriting the parser
without rewriting this file walls the implementer at Step 9 with ~30 unexplained failures.

**(a) STILL REJECTED — row unchanged** (D2′ and D4 are what save these):
`phase`, `pour`, `compose`, `advice`, `pointcuts` (D4), `gate`, `loop`, `waits-for`, `tally`,
`depends-on` — §4 rule 1 refusals; **`unknown-key` (`dependson`) and `nested-unknown-key`** — these
are **camp-local** fixtures, and **D2′ keeps unrecognised keys a hard error in the camp-local
tier**; **`type-step-level`** — gc formula-v2 has no step `type` (0 corpus uses) ⇒ `Unknown` ⇒
camp-local hard error; and **every semantic row** (`dup-step-id`, `unknown-needs-id`, `cycle`, the
five `requires.*` rows, `check-without-requires`, `check-with-retry`, `check-with-assignee`,
`retry-with-on-complete`, `for-each-not-output`, `on-complete-missing-bond`,
`parallel-and-sequential`, `timeout-without-check`, `check-mode-not-exec`, `check-zero-attempts`,
`retry-zero-attempts`, `bad-on-exhausted`, `name-stem-mismatch`, `missing-title`,
`multi-violation`).

The assertion changes from `err.violations.iter().any(|v| v.construct == c)` to **`err.names(c)`**,
so refusals count too. `multi_violation_fixture_reports_every_problem_at_once` likewise.

**(b) DELETED from `invalid/` AND from the table — the key is now ACCEPTED (16 rows):**
`extends`, `vars`, `type-top-level`, `contract`, `catalog`, `template`, `drain`, `expand`,
`expand-vars`, `children`, `condition`, `metadata`, `description-file`, `priority`, `tags`, `notes`.
Delete the `.toml` files and the rows **together** — `assert_eq!(on_disk, in_table)` enforces the
pair.

**(c) `valid/` grows** (Task 10 adds them, so the invariant-6 gate compiles them with real gc):
`vars-condition`, `extends-parent`, `drain-separate`. Update the hard-coded list in
`every_valid_fixture_is_accepted`. **`extends-child` and `expansion` do NOT go in `valid/`** — the
child needs a parent *layer* and the expansion formula is compiled *standalone* by the gc shim; both
live in `tests/fixtures/compose/` (Task 10 explains why).

**`parse_and_validate` survives** with an unchanged signature, as the **no-layer, camp-local** entry
point:
```rust
pub fn parse_and_validate(path: &Path) -> Result<Formula, FormulaError> {
    compose::compile(&FormulaLayers::local_only(path), &CampConfig::default(), path)
        .map(|c| c.formula)          // Err when violations OR refusals is non-empty
}
```
A formula needing layers (`extends`, an `../assets/` `description_file`) fails there, naming what it
needed — correct: those fixtures live in `tests/compose.rs`.

- [ ] **Step 1: Write the failing tests** — `formula/keys.rs`'s `mod tests`

```rust
#[test]
fn classify_matches_section_4s_permissiveness_table() {
    use {Class::*, Site::*};
    // rule 2 — DEAD in gc (93/100 formulas name at least one).
    for k in ["version","target_required","internal","mode","single_lane","sling_container_mode"] {
        assert_eq!(classify(Top, k), DeadInGc, "top {k}");
    }
    // trap 1 — the SAME names are load-bearing when nested.
    assert_eq!(classify(CheckInner, "mode"), Accepted);
    assert_eq!(classify(DrainItem, "single_lane"), Accepted);
    // trap 3 — step metadata is ROUTING, not annotation.
    assert_eq!(classify(Step, "metadata"), Accepted);
    assert_eq!(classify(Top, "metadata"), Annotation);
    for k in ["notes","catalog"] { assert_eq!(classify(Top, k), Annotation); }
    // rule 1 — gc semantics camp does not implement (D4 puts advice/pointcuts here).
    for k in ["pour","compose","advice","pointcuts"] { assert_eq!(classify(Top, k), Refused); }
    for k in ["gate","loop","tally","waits_for","depends_on"] { assert_eq!(classify(Step, k), Refused); }
    // Unrecognised — a DIFFERENT class from DeadInGc. D2′ treats them differently BY ORIGIN.
    assert_eq!(classify(Step, "dependson"), Unknown);
    assert_eq!(classify(Top, "wat"), Unknown);
    assert_eq!(classify(Step, "type"), Unknown, "gc formula-v2 has no step `type`; 0 corpus uses");
}

#[test]
fn the_rung_table_is_section_9s_table_verbatim() {
    // B9: rev 1 looped `assert classify(k) == Accepted`, which was TRUE BY
    // CONSTRUCTION (accepted_top is DEFINED as base ∪ RUNGS) and could never
    // fail. Assert against a literal transcription of §9 instead.
    let expect: &[(&str, &[&str], &[&str])] = &[
        ("2a", &["contract"],         &["description_file", "metadata"]),
        ("2b", &["vars"],             &["condition"]),
        ("2c", &["extends"],          &[]),
        ("2d", &["type", "template"], &["expand", "expand_vars", "children"]),
        ("2e", &[],                   &["drain"]),
    ];
    assert_eq!(RUNGS.len(), expect.len());
    for (r, (id, top, step)) in RUNGS.iter().zip(expect) {
        assert_eq!(r.id, *id);
        assert_eq!(r.top, *top, "rung {id} top");
        assert_eq!(r.step, *step, "rung {id} step");
    }
}

#[test]
fn phase_is_refused_by_key_and_the_reason_names_the_value() {
    // B3: rev 1 put `phase` in NO table, so it fell through to the ignore
    // catch-all — DELETING a refusal merged camp already has (parse.rs:44).
    let r = refuse(Site::Top, "phase", &toml::Value::String("vapor".into()), "phase")
        .expect("phase is refused");
    assert_eq!(r.key, "phase");
    assert!(r.reason.contains("vapor"), "{}", r.reason);
}

#[test]
fn a_scope_check_hiding_in_step_metadata_values_is_refused() {
    // B3: design-review's scope-ness is ENTIRELY in metadata VALUES. There is
    // NO `gc.scope_kind` key anywhere in the corpus — rev 1 invented it.
    let md: toml::Value = toml::from_str(
        "\"gc.kind\" = \"scope\"\n\"gc.scope_name\" = \"design-review\"\n\"gc.scope_role\" = \"body\"\n"
    ).unwrap();
    let r = refuse(Site::Step, "metadata", &md, "steps.body.metadata").expect("scope is refused");
    assert_eq!(r.key, "gc.kind");
    assert!(r.reason.contains("scope"), "{}", r.reason);
}

#[test]
fn a_cleanup_kind_and_a_run_target_are_not_refused() {
    // design-review's `finalize` step also carries gc.kind = "cleanup"; ONLY
    // `scope` is refused. And gc.run_target is ROUTING (trap 3), never refused.
    let md: toml::Value = toml::from_str(
        "\"gc.kind\" = \"cleanup\"\n\"gc.run_target\" = \"gc.run-operator\"\n").unwrap();
    assert!(refuse(Site::Step, "metadata", &md, "steps.finalize.metadata").is_none());
}
```

And in `parse.rs`'s `mod tests`:

```rust
#[test]
fn an_unknown_key_is_ignored_in_an_IMPORTED_layer_and_fatal_in_the_CAMP_LOCAL_one() {
    // D2′ (Ruling 2). camp CAN tell them apart — FormulaLayers knows the origin.
    let text = "formula = \"x\"\nbogus = 1\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ndependson = [\"b\"]\n";

    let (raw, v) = walk(text, Origin::Imported);
    assert!(v.is_empty(), "an imported pack's unknown keys must not be fatal: {v:?}");
    assert!(raw.ignored_keys.contains(&"bogus".to_owned()));
    assert!(raw.ignored_keys.contains(&"dependson".to_owned()));

    let (_, v) = walk(text, Origin::CampLocal);
    // `dependson` silently ignored in the operator's OWN formula would reorder
    // their graph. Invariant 5.
    assert!(v.iter().any(|v| v.construct == "dependson"), "{v:?}");
    assert!(v.iter().any(|v| v.construct == "bogus"), "{v:?}");
}

#[test]
fn a_key_dead_in_gc_is_ignored_in_BOTH_tiers() {
    // 93/100 corpus formulas name at least one. A camp formula may carry them
    // to stay portable — they are real gc keys, not typos.
    let text = "formula = \"x\"\nversion = \"1\"\ntarget_required = true\ninternal = true\n\
                mode = \"solo\"\nsingle_lane = true\nsling_container_mode = \"x\"\n\
                [[steps]]\nid = \"a\"\ntitle = \"t\"\n";
    for origin in [Origin::Imported, Origin::CampLocal] {
        let (raw, v) = walk(text, origin);
        assert!(v.is_empty(), "{origin:?}: {v:?}");
        assert_eq!(raw.ignored_keys.len(), 6, "{origin:?}: {:?}", raw.ignored_keys);
    }
}

#[test]
fn annotations_are_silent_in_both_tiers() {
    let text = "formula = \"x\"\nnotes = \"hi\"\n[catalog]\nx = 1\n[metadata]\ny = \"z\"\n\
                [[steps]]\nid = \"a\"\ntitle = \"t\"\n";
    for origin in [Origin::Imported, Origin::CampLocal] {
        let (raw, v) = walk(text, origin);
        assert!(v.is_empty() && raw.ignored_keys.is_empty(), "{origin:?}");
    }
}

#[test]
fn a_refused_key_names_itself_in_both_tiers() {
    let text = "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ngate = { path = \"x\" }\n";
    for origin in [Origin::Imported, Origin::CampLocal] {
        let (raw, _) = walk(text, origin);
        let r = raw.refusals.iter().find(|r| r.key == "gate").expect("gate refusal");
        assert!(r.construct.contains("steps.a"), "{}", r.construct);
    }
}
```

- [ ] **Step 2: Run and watch them fail** — `cargo test -p camp-core --lib formula:: 2>&1 | tail -20`

- [ ] **Step 3: Implement `keys.rs`**

`classify` is a pure nesting-scoped lookup. **There is no `else { Dead }` catch-all** — the
fall-through is `Class::Unknown`, a **different** class from `DeadInGc`. That distinction is D2′'s
entire mechanism, and it is what makes `DEAD_TOP` load-bearing rather than dead code.

```rust
pub const RUNGS: &[Rung] = &[
    Rung { id: "2a", top: &["contract"],         step: &["description_file", "metadata"] },
    Rung { id: "2b", top: &["vars"],             step: &["condition"] },
    Rung { id: "2c", top: &["extends"],          step: &[] },
    Rung { id: "2d", top: &["type", "template"], step: &["expand", "expand_vars", "children"] },
    Rung { id: "2e", top: &[],                   step: &["drain"] },
];
```

**The `unimplemented` scaffold.** A key accepted by the table but not yet implemented by the
pipeline must NOT silently compile to nothing. `RawFormula.unimplemented: Vec<String>` records them
and `validate::check` turns each into a hard `Violation`. Tasks 5–8 each remove their own keys;
**Task 8 deletes the field.** This is what makes every intermediate rung count real.

- [ ] **Step 4: Implement the parse-side changes**

`walk(text, origin)`. `RawFormula` gains `contract`, `kind`, `template: Vec<RawStep>`, `vars`,
`extends`, `ignored_keys: Vec<String>` (deduped, sorted), `refusals: Vec<Refusal>`,
`unimplemented: Vec<String>`. One shared key loop replaces both existing ones:

```rust
fn walk_keys(site: Site, origin: Origin, table: &toml::Table, at: &dyn Fn(&str) -> String,
             ignored: &mut Vec<String>, refusals: &mut Vec<Refusal>, out: &mut Vec<Violation>) {
    for key in sorted_keys(table) {
        if let Some(r) = keys::refuse(site, key, &table[key], &at(key)) { refusals.push(r); continue; }
        match keys::classify(site, key) {
            Class::Accepted | Class::Annotation => {}
            Class::Refused  => refusals.push(Refusal { construct: at(key), key: key.into(),
                                                       reason: refusal_reason(site, key) }),
            Class::DeadInGc => ignored.push(key.to_owned()),
            Class::Unknown  => match origin {                          // D2′
                Origin::Imported  => ignored.push(key.to_owned()),
                Origin::CampLocal => out.push(unknown(&at(key), key)), // the merged message
            },
        }
    }
}
```
`refusal_reason` returns the `tally` message **verbatim** from parse.rs:324-329 for `tally`; else
`"`{key}` is a Gas City construct camp does not implement; camp refuses rather than silently
approximating (compat §4 rule 1)"`.

- [ ] **Step 5: The `formula.refused` event**

Verified: gc's pinned vocabulary (`tests/fixtures/gc-vocab.json`, 71 events) has **no** `formula.*`
event ⇒ safely camp-specific (`check_vocab.sh` enforces no collision). Verified:
`vocab.rs::no_reservation_vocabulary_exists` scans **event names only** ⇒ the metadata key
`gc.exclusive_drain_reservation` is safe, but **no event may ever be named `drain.reserved`**
(Task 9 depends on this).

`event.rs`: `EventType::FormulaRefused` → `"formula.refused"` (enum + `ALL` + `as_str`).
`vocab.rs`: add to `CAMP_SPECIFIC_EVENTS`. `fold.rs`: a log-only validating arm, `check_passed` mold:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormulaRefused { formula: String, path: String, key: String, construct: String, reason: String }

/// Log-only: a formula camp refused to COMPILE or to RUN, naming the key.
/// compat §4 rule 1; §5.4 "Every refusal ... appends a ledger event."
fn formula_refused(_conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: FormulaRefused = payload(event)?;
    non_empty(event, "formula", &p.formula)?;
    non_empty(event, "key", &p.key)?;
    non_empty(event, "reason", &p.reason)?;
    Ok(())
}
```
No state effect ⇒ the refold property stays trivially green **for this event** (unlike `bead_meta`,
Task 3).

- [ ] **Step 6: Rewrite `tests/formula_corpus.rs`** per the disposition above (delete 16 fixtures +
  rows; switch to `err.names(c)`; update the `valid/` list in Task 10).

- [ ] **Step 7: Amend `formula/mod.rs`'s module doc.** It says camp "rejects every city-only
  construct by name and accepts no unknown keys, where gc silently ignores them." That sentence is
  now **false**. Replace it with D2′'s rule. *(AGENTS.md: spec and code never silently diverge.)*

- [ ] **Step 8: Write `ci/gc-compat/rungs.py` — the independent arbiter (B8)**

It implements **§9's text**, not camp's `RUNGS` table, so the two can disagree and be caught. The
counting rule, as pseudocode — this is the rule rev 1 left as prose, and two implementers would
have written two scripts and got two numbers:

```python
# For each of the 100 corpus formulas F and each cumulative rung set R = {2a..r}:
#
#   ACCEPTED(R) = BASE_TOP ∪ BASE_STEP ∪ (⋃ rung.top ∪ rung.step for rung in R)
#
#   F is LOADABLE at r  iff ALL of:
#     (1) no VALUE-AWARE refusal fires:
#           `phase` present;  OR  a step (RECURSIVELY — incl. `template` and
#           `children`) whose metadata has gc.kind == "scope" or any gc.scope_*;
#           OR  (drain ∈ ACCEPTED(R)) AND a step whose drain.context == "shared"
#               AND that step SURVIVES condition pruning under the merged vars
#               (parent defaults first, child overrides win);
#           OR  drain.continuation_group / drain.max_units present.
#     (2) no rule-1 KEY refusal: pour/compose/advice/pointcuts (top);
#           gate/loop/tally/waits_for/depends_on (step).
#     (3) (extends ∈ ACCEPTED(R)) ⇒ every parent in the transitive chain RESOLVES
#           by bare name across all 10 packs, and the chain is acyclic.
#     (4) every REMAINING key of F — top-level, and on every step reached
#           RECURSIVELY through `steps`, `template` and `children` — is in
#           ACCEPTED(R) ∪ DEAD ∪ ANNOTATION.
#           * DEAD and ANNOTATION keys are EXCLUDED from this check. Without
#             that exclusion 2a = 0, not 2.
#           * NESTED sites (check.*, retry.*, on_complete.*, drain.item.*) are
#             NOT walked — §9's rungs are top-level and step-level keys only.
#
#   COUNT(r) = |{F : LOADABLE at r}|
#     The 5 refused formulas are IN the denominator (100) and OUT of every count.
#
#   RUNNABLE = |{F loadable at 2e : F.contract == "graph.v2" and F.type != "expansion"}|
```
Expected output: `2a 2 · 2b 31 · 2c 57 · 2d 83 · 2e 95 · RUNNABLE 62`, and the 5 refused named.

- [ ] **Step 9: Run every test; run the arbiter**
```bash
cargo test -p camp-core 2>&1 | tail -20
cargo test -p camp-core --test vocab_pin --test refold_prop 2>&1 | tail -6
git clone -q https://github.com/gastownhall/gascity-packs /tmp/gcpacks \
  && git -C /tmp/gcpacks checkout -q "$(cat ci/gc-compat/GCPACKS_REF)"
python3 ci/gc-compat/rungs.py /tmp/gcpacks
```
Expected: the seed table, exactly. **If it differs, STOP — the pin moved or a rule is wrong.
`rungs.py` is the arbiter; never edit a seed to match code.**

- [ ] **Step 10: Full gates and commit**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "feat(formula): the permissiveness rule — value-aware keys, origin-scoped strictness (compat §4)"
```

---

## Task 3: Bead metadata — the store, the refold wiring, and the schema bump

camp has **no bead metadata**. §9 requires the drain reservation *"where gc stores it: as metadata on
the member bead (`gc.exclusive_drain_reservation`, `beadmeta/keys.go:93`)"*, verbatim. §4 trap 3
requires step metadata to survive onto the bead. compat-3 needs the same door. Build it once — and
**wire it into refold**, which rev 1 did not (B1).

**Files:** `ledger/schema.rs` · `ledger/fold.rs` · **`ledger/refold.rs`** · `ledger/mod.rs` ·
`readiness.rs` · `camp/src/main.rs` + `cmd/create.rs` · `tests/refold_prop.rs`

**Interfaces produced:**
```rust
// readiness.rs — pure, connection-scoped (usable inside the cursor txn)
pub fn bead_metadata(conn: &Connection, bead: &str) -> Result<BTreeMap<String, String>, CoreError>;

/// gc's key, VERBATIM (beadmeta/keys.go:93; invariant 7). Value = the reserving
/// drain's anchor bead id.
pub const EXCLUSIVE_DRAIN_RESERVATION: &str = "gc.exclusive_drain_reservation";

/// Metadata keys that have a DEDICATED COLUMN. They are PROJECTED from the
/// column at read time and REFUSED at write time, naming the column — so
/// compat-3 (§6.1) inherits ONE source of truth, not two.
pub const PROJECTED_METADATA: &[(&str, &str)] =
    &[("gc.routed_to", "assignee"), ("gc.work_branch", "work_branch")];

// ledger/mod.rs
pub fn bead_metadata(&self, bead: &str) -> Result<BTreeMap<String, String>, CoreError>;
```

Event shapes (both events already exist — additive):
```jsonc
// bead.created — `metadata` is new, default {}
{ "title": "...", "metadata": { "gc.run_target": "gc.run-operator" }, ... }
// bead.updated — `metadata` is new; a null value UNSETS. The existing
// "must set title and/or description" check becomes "...or metadata".
{ "metadata": { "gc.exclusive_drain_reservation": "cmp-12" } }   // set
{ "metadata": { "gc.exclusive_drain_reservation": null } }        // release
```

**The compare-and-set lives in the FOLD.** *(Attacked at the gate and RATIFIED: `fold::apply` already
makes state-dependent acceptance decisions (`bead_updated` rejects on UnknownBead, fold.rs:234-236);
`append` is one transaction that rolls back on Err (ledger/mod.rs:982 — "rejections appended
nothing"); `build_shadow` (refold.rs:110-120) replays the **accepted** log in seq order through the
**same** `fold::apply`. So the CAS's outcome is a pure function of the accepted event prefix —
exactly what the refold property demands. A read-then-append would be a genuine TOCTOU race between
two drains.)*

- [ ] **Step 1: Write the failing tests**

In `fold.rs`'s `mod tests` (`meta_update(bead, json)` is a new local helper beside the existing ones):

```rust
#[test]
fn bead_created_carries_metadata_and_bead_updated_sets_and_unsets_it() {
    let (mut l, _t) = ledger();
    l.append(bead_created_input("b-1", json!({ "gc.run_target": "gc.run-operator" }))).unwrap();
    assert_eq!(l.bead_metadata("b-1").unwrap()["gc.run_target"], "gc.run-operator");
    l.append(meta_update("b-1", json!({ "x": "1" }))).unwrap();
    let m = l.bead_metadata("b-1").unwrap();
    assert_eq!(m["x"], "1");
    assert_eq!(m["gc.run_target"], "gc.run-operator", "a set must not clobber");
    l.append(meta_update("b-1", json!({ "x": null }))).unwrap();
    assert!(!l.bead_metadata("b-1").unwrap().contains_key("x"));
}

#[test]
fn a_second_drain_cannot_reserve_a_held_member() {
    // compat §9: "never two drains mutating one bead."
    let (mut l, _t) = ledger();
    l.append(bead_created_input("m-1", json!({}))).unwrap();
    let reserve = |who: &str| meta_update("m-1", json!({ EXCLUSIVE_DRAIN_RESERVATION: who }));
    l.append(reserve("drain-a")).unwrap();
    let err = l.append(reserve("drain-b")).unwrap_err();
    assert!(err.to_string().contains("gc.exclusive_drain_reservation"), "{err}");
    assert!(err.to_string().contains("drain-a"), "the holder must be named: {err}");
    l.append(reserve("drain-a")).unwrap();          // idempotent for the SAME holder
    l.append(meta_update("m-1", json!({ EXCLUSIVE_DRAIN_RESERVATION: null }))).unwrap();
    l.append(reserve("drain-b")).unwrap();          // released ⇒ takeable
}

#[test]
fn a_metadata_key_with_a_dedicated_column_is_projected_at_read_and_refused_at_write() {
    // ONE source of truth for compat-3 (§6.1): `beads` already has
    // `work_branch` and `assignee`.
    let (mut l, _t) = ledger();
    l.append(bead_created_input("b-1", json!({}))).unwrap();
    let err = l.append(meta_update("b-1", json!({ "gc.work_branch": "camp/b-1" }))).unwrap_err();
    assert!(err.to_string().contains("work_branch"), "names the column: {err}");
    l.append(work_branch_update("b-1", "camp/b-1")).unwrap();      // the existing path
    assert_eq!(l.bead_metadata("b-1").unwrap()["gc.work_branch"], "camp/b-1");
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

In `tests/refold_prop.rs` — **the vacuity fix (B1c).** Rev 1 added `bead_meta` to `DUMPS` and
declared victory; but `Op` emits **no metadata**, so both ledgers dump zero rows and the property
passes while exercising **nothing** (the PR #79 bug class, verbatim). Add ops **and** the dump:

```rust
enum Op {
    // ... existing Create/Claim/Update/Close/Woke/Stop/Crash ...
    /// Plain metadata set/unset — exercises the bead_meta fold.
    SetMeta { id: u8, key: u8, unset: bool },
    /// The exclusive-reservation CAS — the ONLY real test of its determinism.
    /// Deliberately generates CONFLICTS (two drains, one member): a rejected
    /// append must append NOTHING, and the replay must reach an identical state.
    Reserve { member: u8, drain: u8 },
    Release { member: u8 },
}
```
extend `op_strategy` to generate them, and add `("bead_meta", "bead_id, key, value")` to `DUMPS`.

- [ ] **Step 2: Run and watch them fail**
```bash
cargo test -p camp-core --lib ledger::fold 2>&1 | tail -20
cargo test -p camp-core --test refold_prop 2>&1 | tail -10
```
Expected: `bead_metadata` not found; `no such table: bead_meta`.

- [ ] **Step 3: Schema + the version bump (B2)**

`schema.rs`, in `STATE_DDL`, **after** `beads`:
```sql
CREATE TABLE bead_meta (
  bead_id TEXT NOT NULL REFERENCES beads(id),
  key     TEXT NOT NULL,
  value   TEXT NOT NULL,
  PRIMARY KEY (bead_id, key)
) STRICT;
CREATE INDEX bead_meta_key ON bead_meta(key, value);
```
(The index serves the query every drain expansion runs: *"is this member reserved?"*)

**`SCHEMA_VERSION: 2 → 3`.** `bead_meta` is **fold truth**, not consumer bookkeeping — cp-0's own
merged comment (schema.rs:97-107) states the rule verbatim: *"fold-state schema changes go through
FULL_DDL_PREFIX + a SCHEMA_VERSION bump, which makes an existing camp fail to open so the operator
re-inits (the v1 'no auto-upgrade' contract). Consumer-bookkeeping infrastructure tables like this
one evolve additively."* Without the bump an existing camp.db opens **successfully** and then dies at
the first `bead.created` with `no such table: bead_meta` — a late runtime failure where the codebase
gives a fail-fast **open-time** one (invariant 5). **State the re-init consequence in the PR body.**

- [ ] **Step 4: The fold**

`BeadCreated` gains `#[serde(default)] metadata: BTreeMap<String, String>`; `BeadUpdated` gains
`#[serde(default)] metadata: BTreeMap<String, Option<String>>`, and its emptiness check becomes
`title.is_none() && description.is_none() && metadata.is_empty()`. Per entry:

```rust
// ONE source of truth (compat-3, §6.1): a key with a dedicated column is
// projected at read time and refused here.
if let Some((_, col)) = PROJECTED_METADATA.iter().find(|(k, _)| *k == key) {
    return Err(CoreError::InvalidEventData { event_type: ..., reason: format!(
        "bead {id}: metadata key {key:?} is projected from the `{col}` column and may not be \
         written as metadata — set the column instead") });
}
match value {
    None => { conn.execute("DELETE FROM bead_meta WHERE bead_id = ?1 AND key = ?2", (id, key))?; }
    Some(v) => {
        // compat §9's compare-and-set, ATOMIC with the event insert.
        if key == EXCLUSIVE_DRAIN_RESERVATION {
            if let Some(holder) = current_meta(conn, id, key)? {
                if holder != *v {
                    return Err(CoreError::InvalidEventData { event_type: ..., reason: format!(
                        "bead {id}: {key} is already held by {holder:?}; a second drain may not \
                         reserve a held member (compat §9)") });
                }
            }
        }
        conn.execute("INSERT INTO bead_meta (bead_id, key, value) VALUES (?1, ?2, ?3) \
                      ON CONFLICT(bead_id, key) DO UPDATE SET value = excluded.value", (id, key, v))?;
    }
}
```
`bead_metadata(conn, bead)` reads `bead_meta` **and overlays the projections** from
`beads.assignee` / `beads.work_branch` when non-NULL.

- [ ] **Step 5: Wire refold (B1a, B1b) — the PRODUCTION constant, not just the test**

`refold.rs::STATE_TABLES` (:28-60) is the real list; `diff_all` (:166-185) and
`replace_state_from_shadow` (:142-163) iterate **only** it. Add, **positioned AFTER `beads`** so
`.iter().rev()` deletes the child before the parent and the FK holds (`foreign_keys = ON`,
schema.rs:126):

```rust
TableSpec { name: "bead_meta", cols: "bead_id, key, value", key: "bead_id || '/' || key" },
```
Without this, `camp doctor --refold` never diffs a drain reservation, and **`--repair` HARD-FAILS**
(`DELETE FROM main.beads` while `bead_meta` rows still reference them ⇒ `FOREIGN KEY constraint
failed`) — breaking a merged, working, tested command the moment one bead carries metadata.

- [ ] **Step 6: `camp create --run` + the `ready_task_count` fix**

`main.rs`'s `Create`: `#[arg(long)] run: Option<String>` — *"Add this bead to a formula run as a
MEMBER (gc's convoy member). A drain step scatters the run's members (compat §9)."* Thread into
`bead.created`'s `run_id` (already folded). **Fail fast** on an unknown run:
`bail!("no such run: {run}")`.

`readiness.rs::ready_task_count` (:160) lacks the run-root exclusion that `dispatchable_beads`
(:139) has, so every member would be counted "ready" forever and never dispatched — a permanently
non-decreasing count in `camp top`. Add `AND NOT (b.run_id IS NOT NULL AND b.step_id IS NULL)`.
Test: `a_run_member_is_never_ready_and_never_dispatchable`.

- [ ] **Step 7: Run and watch them pass**
```bash
cargo test -p camp-core 2>&1 | tail -20
cargo test -p camp-core --test refold_prop 2>&1 | tail -10   # NON-vacuous now: Reserve/Release replay
cargo test -p camp 2>&1 | tail -10
```

- [ ] **Step 8: Full gates and commit**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -20
git add -A && git commit -m "feat(ledger): bead metadata — refold-wired, schema 3, with the exclusive-reservation CAS"
```

---

## Task 4: Rung 2a — the layered compiler, `description_file`, routing, the runnability refusal, and the gate

**Files:** create `formula/layers.rs`, `formula/compose.rs`, `tests/compose.rs`,
`tests/fixtures/compose/**`, `camp/tests/cli_doctor_corpus.rs`, `ci/gc-compat/formula_gate.py` ·
modify `formula/{mod,ast,parse,cook}.rs`, `orders/mod.rs`, `camp/src/cmd/{doctor,sling}.rs`,
`camp/src/main.rs`, **`camp/src/daemon/orders.rs`** (B12), `camp/tests/cli_doctor_formula.rs`,
`camp/tests/daemon_orders.rs`, `.github/workflows/ci.yml`

**Interfaces produced:**
```rust
// formula/layers.rs
/// The formula/asset search path, LOWEST → HIGHEST priority — compat-1's
/// tiering: transitive imports < direct imports < camp-local `<root>/formulas/`.
/// gc's `Parser.searchPaths` has this order and `winningAssetPath` takes the
/// LAST match (gascity parser.go:855-873).
pub struct FormulaLayers { layers: Vec<Layer> }   // Layer { binding, dir, origin }

impl FormulaLayers {
    pub fn from_config(cfg: &CampConfig, root: &Path) -> Result<Self, CoreError>;
    /// A single camp-local file with no import layers — what `parse_and_validate` uses.
    pub fn local_only(path: &Path) -> Self;
    /// Which tier a formula FILE came from. D2′ keys off this.
    pub fn origin_of(&self, path: &Path) -> Origin;
    /// Bare name → file, highest layer wins. DELEGATES to compat-1's
    /// `orders::resolve_formula` — do NOT write a second resolver.
    pub fn formula_path(&self, name: &str) -> Result<PathBuf, CoreError>;
    /// gc's asset shadowing, with camp's containment rule (below).
    pub fn asset_path(&self, raw: &str, base_dir: &Path, pack_root: &Path)
        -> Result<PathBuf, CoreError>;
}

// formula/compose.rs
pub struct Compiled {
    pub formula: Formula,
    pub ignored_keys: Vec<String>,
    /// Empty ⇒ it compiled.
    pub refusals: Vec<Refusal>,
    /// None ⇒ runnable. Some ⇒ it compiles and must NEVER be cooked (D1).
    pub not_runnable: Option<Refusal>,
}
pub fn compile(layers: &FormulaLayers, cfg: &CampConfig, path: &Path) -> Result<Compiled, FormulaError>;
pub fn compile_named(layers: &FormulaLayers, cfg: &CampConfig, name: &str) -> Result<Compiled, FormulaError>;
```
`Formula` gains `contract`, `kind`, `vars`; `Step` gains `metadata: BTreeMap<String,String>` and
keeps `assignee` — **the route lands in `assignee`**.

### The pipeline order — CORRECTED (B15). It is gc's, and it is not negotiable.

```
1. parse::walk(text, origin)                                (Task 2)
2. extends resolution                                       (Task 6 — 2c)
3. expansion: type/template/expand/expand_vars/children     (Task 7 — 2d)
4. description_file resolution                              (THIS task — 2a)   ← gc: parser.go:186-190, at PARSE time
5. vars merge + {{var}} substitution                        (Task 5 — 2b)      ← therefore inlined contents ARE substituted
6. condition eval + prune (RECURSIVE into children)         (Task 5 — 2b)
7. route: step metadata gc.run_target → assignee            (THIS task)
8. validate::check (S1..S18) + refusals + runnability       (THIS task)
```

**Rev 1 had vars (4) before description_file (6) — backwards.** gc resolves `description_file` in the
parser (`parser.go:186-190`) and applies `Substitute` later, so the inlined contents pass through
substitution like any other description. That is exactly what the >4096 pointer prompt's
`## Formula Variables` block is **for**: it emits `name="{{name}}"` lines *so substitution resolves
them* for the worker. **Task 11's differential gate verifies this against real gc** — compilation
succeeds either way, so no camp-only test can catch the wrong choice.

In this task, stages 2/3/5/6 are **identity stubs** and `validate::check` hard-fails any formula with
a non-empty `unimplemented` list — which is what makes the 2a count really **2**.

### `description_file` — verified in gc at `GASCITY_REF`

- `resolveDescriptionFiles` (parser.go:808): contents **replace** `step.Description`; the key is then
  cleared ("consumed").
- `readDescriptionFile` (:840): the documented **`../assets/<rel>`** form (`descriptionAssetRelPath`,
  :964 — rejects a `rel` starting with `../`) resolves **through the layers**: for each layer,
  `<layer>/../assets/<rel>` (a layer is `<pack>/formulas`, so this is the **pack's** `assets/`), and
  the **LAST (highest)** match wins. **Anything else** resolves relative to `baseDir` = the formula
  file's own directory.
- **`descriptionFileInlineMaxBytes = 4 * 1024`** (parser.go:27). Over it, the description becomes
  gc's **pointer prompt** (`descriptionFileReferenceDescription`, :977) — reproduce it byte-for-byte.
  A mis-transcribed paragraph is a silent divergence **no camp test can detect**, which is exactly
  why Task 11 diffs it against real gc:

```rust
/// gc's `descriptionFileReferenceDescription` (gascity parser.go:977) —
/// byte-for-byte. The worker READS this text; a paraphrase is a divergence.
fn pointer_prompt(raw: &str, resolved: &Path, size: usize, vars: &BTreeMap<String, String>) -> String {
    let mut b = String::new();
    b.push_str("# External Prompt Required\n\n");
    b.push_str("This bead still follows the normal runtime and lifecycle protocol from your startup prompt and current agent prompt, including claiming work, honoring result contracts, checking for follow-on work, and draining only when appropriate.\n\n");
    b.push_str("In addition to that protocol, this bead's task-specific instructions come from a formula `description_file` that is too large to inline safely into bead storage.\n\n");
    b.push_str("Before you start the task-specific work, you MUST read the file below and treat it as the task prompt for this bead. Do not proceed from memory, ambient skills, or prior workflow knowledge until you have read it.\n\n");
    b.push_str(&format!("- Resolved prompt file: `{}`\n", resolved.display()));
    b.push_str(&format!("- Original formula description_file: `{raw}`\n"));
    b.push_str(&format!("- Prompt file size: {size} bytes\n\n"));
    b.push_str("Treat the file contents as the authoritative task prompt for this bead. It augments the startup/runtime protocol; it does not replace the startup prompt, the current agent prompt, or any bead lifecycle/result-contract instructions already given to you.\n");
    b.push_str("Follow the section matching this bead's `gc.step_id` metadata and title, plus any result, closure, lifecycle, or post-close contract sections in that file.\n");
    if !vars.is_empty() {
        b.push_str("\n## Formula Variables\n\n");
        b.push_str("Use these resolved formula values when interpreting `{{...}}` placeholders in the prompt file:\n\n");
        b.push_str("```bash\n");
        for name in vars.keys() {              // BTreeMap ⇒ sorted, matching gc's slices.Sort
            b.push_str(&format!("{name}=\"{{{{{name}}}}}\"\n"));
        }
        b.push_str("```\n");
    }
    b
}
```

**A `description_file` that resolves to nothing is a hard compile error** for a `graph.v2` formula
(gc: `strict = UsesGraphCompiler(f)`, parser.go:186; `validateResolvedGraphV2DescriptionFiles`,
:1007). Measured: **all 328 targets resolve; 8 exceed 4096 bytes.**

**SECURITY — path containment (fixed here).** gc's non-asset branch is a bare `base_dir.join(raw)`.
Camp now imports **arbitrary third-party packs** (compat-1), so a pack could set
`description_file = "../../../../../.ssh/id_rsa"` and have it inlined into a bead description that a
tool-enabled LLM worker then reads. **`asset_path` canonicalises the result and refuses any path
outside the declaring pack's root**, naming the escape. (gc doing otherwise is not a security
argument for camp.) The >4096 pointer prompt embeds the resolved **absolute path** into the ledger —
acceptable, and it stays inside the pack root by the same rule.

### Routing — step metadata `gc.run_target` → the step's assignee

Measured: **327 `gc.run_target` occurrences; ZERO step `assignee`.** Routing is *entirely* step
metadata (§4 trap 3).

- `compose` sets `step.assignee = Some(<resolved gc.run_target>)` when metadata carries it and
  `assignee` is unset. An explicit `assignee` wins (0 corpus uses).
- The value is `{{var}}`-substituted **first** (stage 5; 46 of 99 route sites are var references),
  **then** split at the first dot; the prefix must be a binding in `cfg.imports`.
- **An unbound binding is a hard compile error naming the remedy** (§7.1; §14's routing test).
  **Reuse compat-1's `pack::resolve_agent(cfg, name)` (pack.rs:251)** — it already splits at the
  first dot, checks the binding, and emits exactly `camp import add <source> --name <binding>`.
  **Do not write a second resolver.**
- A value with no dot resolves as a camp-local agent (0 corpus uses).

### The runnability verdict (D1) and its TWO enforcement points (B12)

`not_runnable = Some(Refusal { key, .. })` when **`contract != "graph.v2"`** (key `contract` — the 21)
**or `type == "expansion"`** (key `type` — the 14; §9: *"not directly runnable"*).

**Both cook entry points must refuse it:**
1. `camp sling` (`cmd/sling.rs:53`).
2. **The daemon's order-fire path** (`camp/src/daemon/orders.rs`) — rev 1 changed only `sling.rs`, so
   a cron order could fire one of the 21 under graph.v2 semantics **with nothing refusing it**: the
   exact silent assumption §9 forbids, on the path where *"an order fires a formula; a formula
   dispatches workers; workers cost real money"* (§13). Both append `formula.refused`; the order path
   also fires `order.failed`.

- [ ] **Step 1: Write the failing tests** (`tests/compose.rs`; fixtures under
  `tests/fixtures/compose/` — a `child` pack whose `pack.toml` declares
  `[imports.gc] source = "../parent"`, and a `parent` pack with `formulas/` + `assets/`)

```rust
#[test] fn description_file_contents_replace_the_step_description() { /* == "PARENT PROSE\n" */ }
#[test] fn an_asset_reference_resolves_through_the_layers_highest_wins() { /* child shadows parent */ }
#[test] fn an_oversize_description_file_becomes_gcs_pointer_prompt() {
    let d = c.formula.steps[0].description.clone().unwrap();
    assert!(d.starts_with("# External Prompt Required\n\n"), "{d}");
    assert!(d.contains("- Prompt file size: 5000 bytes"), "{d}");
    assert!(d.contains("Resolved prompt file: `"));
}
#[test] fn a_missing_description_file_is_a_hard_error_for_a_graph_v2_formula() { }
#[test] fn a_description_file_escaping_the_pack_root_is_refused() {
    // "../../../../../.ssh/id_rsa" — camp imports UNTRUSTED packs (compat-1).
    let err = compile_named(&layers, &cfg, "escapee").unwrap_err();
    assert!(err.to_string().contains("outside the pack"), "{err}");
}
#[test] fn a_step_metadata_run_target_becomes_the_steps_assignee() {
    assert_eq!(step.assignee.as_deref(), Some("gc.run-operator"));
    assert_eq!(step.metadata["gc.run_target"], "gc.run-operator");
}
#[test] fn a_route_to_an_unbound_binding_fails_at_compile_time_naming_the_remedy() {
    assert!(err.to_string().contains("camp import add"), "{err}");
}
#[test] fn a_no_contract_formula_compiles_and_is_not_runnable() {
    let c = compile_named(&layers, &cfg, "plain").unwrap();
    assert!(c.refusals.is_empty(), "it COMPILES: {:?}", c.refusals);
    assert_eq!(c.not_runnable.expect("not runnable").key, "contract");
}
#[test] fn phase_is_refused_by_name() {
    assert_eq!(c.refusals.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec!["phase"]);
}
#[test] fn a_scope_check_formula_is_refused_by_its_metadata() {
    // The REAL shape (design-review.formula.toml) — NOT rev 1's invented `gc.scope_kind`.
    assert!(c.refusals.iter().any(|r| r.key == "gc.kind"), "{:?}", c.refusals);
}
```

`camp/tests/cli_doctor_corpus.rs`:
```rust
#[test] fn doctor_formula_json_reports_ok_runnable_ignored_and_refusals() { /* pins the gate's contract */ }
#[test] fn doctor_formula_json_exits_zero_even_when_the_formula_is_refused() {
    // The gate reads the verdict from JSON; a non-zero exit on 5 of 100 files
    // would make the script fight the exit code. HUMAN mode keeps 0/1.
}
#[test] fn doctor_formula_rungs_json_emits_camps_own_key_classification() { /* base/rungs/dead/annotation/refused */ }
#[test] fn sling_refuses_a_no_contract_formula_and_events_the_refusal() {
    assert_eq!(camp.events_of_type("formula.refused")[0]["data"]["key"], "contract");
}
#[test] fn sling_refuses_an_expansion_formula() { /* key == "type" */ }
```

`camp/tests/daemon_orders.rs` (**B12**):
```rust
#[test]
fn a_due_order_naming_a_no_contract_formula_fires_nothing_and_events_the_refusal() {
    // §13's money invariant: an order fires a formula; a formula dispatches
    // workers; workers cost real money. Rev 1 left this path UNGUARDED.
    let mut d = daemon_with_enabled_order_on("plain");   // plain = no `contract`
    d.tick_until_order_due();
    assert_eq!(d.beads_created(), 0, "NOTHING cooked");
    assert_eq!(d.events_of_type("formula.refused")[0]["data"]["key"], "contract");
    assert_eq!(d.events_of_type("order.failed").len(), 1);
}
```

- [ ] **Step 2: Run and watch them fail** —
  `cargo test -p camp-core --test compose 2>&1 | tail -20`;
  `cargo test -p camp --test cli_doctor_corpus --test daemon_orders 2>&1 | tail -20`

- [ ] **Step 3: Implement `layers.rs` + `compose.rs`** (the pipeline above; stages 2/3/5/6 stubbed).

- [ ] **Step 4: Wire the CLI.** `main.rs`'s `Doctor` (:86-99) gains `--json` and `--formula-rungs`,
  the latter added to the existing **required** `ArgGroup("mode")` so exactly one mode is still
  required (`cli_doctor_formula.rs` asserts that group — update it). `cmd/doctor.rs::run_formula`
  gains `json: bool` and prints:
```jsonc
{ "path": "...", "formula": "bmad-build", "ok": true, "runnable": true,
  "ignored_keys": ["internal","target_required","version"],
  "refusals": [], "not_runnable": null }
```
  exiting **0 even when `ok` is false** in `--json` mode; human mode keeps 0/1.

- [ ] **Step 5: `cook.rs` carries metadata + the route.** Add `"metadata": step.metadata` to the
  step-bead `EventInput` when non-empty. `assignee` is already written verbatim — compose has already
  put the resolved route there, so **cook needs no routing logic.** Test in `tests/cook.rs`:
  `cook_stamps_the_steps_metadata_and_route_onto_the_bead`.

- [ ] **Step 6: The order-fire refusal (B12).** In `camp/src/daemon/orders.rs`, the order-fire path
  compiles via `compose::compile_named` and, on `not_runnable`, appends `formula.refused` +
  `order.failed` and cooks **nothing**.

- [ ] **Step 7: Write `ci/gc-compat/formula_gate.py` — the §10 gate (runs the REAL binary)**

```python
"""compat §10: assert camp loads EXACTLY what it claims, at GCPACKS_REF.

usage: formula_gate.py <corpus> <camp-binary> [--expect-loaded N]

Setup (the load_corpus_packs.py mold, verbatim): `camp init --no-service
--no-import`; append [agent_defaults] tools = ["Read","Bash","Skill"]; then
`camp import add <corpus>/<pack> --name <pack>` for each of the 10
formula-bearing packs (bmad, compound-engineering, contributing, discord,
gascity, gastown, github, gstack, pr-pipeline, superpowers) plus
`camp import add <corpus>/gascity/roles --name gc` (the corpus's own recipe,
§3/§7.3). Measured: no two of the 100 formulas share a basename, so no
within-tier collision arises. Every camp call is checked; a non-zero exit is a
gate failure with stderr printed. No fallbacks.

THREE assertions:
  1. REAL LOADER: `camp doctor --formula <path> --json` over all 100.
     Exactly CEILING compile; the NOT_LOADABLE ones refuse with the named keys.
  2. RUNNABLE: exactly RUNNABLE of them report runnable=true.
  3. CROSS-CHECK: rungs.py's per-rung counts must match camp's OWN
     classification (`camp doctor --formula-rungs --json`) applied to the
     corpus. A TUNED RUNG TABLE FAILS HERE.
"""
CEILING  = 95
RUNNABLE = 62
RUNG_COUNTS = {"2a": 2, "2b": 31, "2c": 57, "2d": 83, "2e": 95}
NOT_LOADABLE = {                          # basename -> a key the refusal must name
    "mol-digest-generate.toml":            "phase",
    "mol-pr-from-issue.formula.toml":      "phase",
    "design-review.formula.toml":          "gc.kind",   # NOT gc.scope_kind — that key does not exist
    "same-session-implement.formula.toml": "context",   # an UNCONDITIONAL shared drain
    "mol-polecat-work.toml":               "extends",   # parent ships in gc's embedded core pack
}
```

- [ ] **Step 8: Run the gate locally**
```bash
cargo build --bin camp
python3 ci/gc-compat/formula_gate.py /tmp/gcpacks target/debug/camp --expect-loaded 2
```
Expected **at this point**: the rung-table cross-check passes; the real loader reports **2** — rungs
2b–2e are `unimplemented` hard violations. **That is the correct failing signal: the gate is now the
TDD driver for Tasks 5–8.**

- [ ] **Step 9: Wire CI.** In `.github/workflows/ci.yml`'s **existing** `gcpacks-compat` job (the
  corpus checkout and `cargo build --bin camp` are already there — do **not** add a job), append:
```yaml
      - name: phase-2 formula gate (rung counts, the ceiling, and RUNNABLE)
        run: python3 ci/gc-compat/formula_gate.py gcpacks-src target/debug/camp
```

- [ ] **Step 10: Full gates and commit** —
  `git commit -m "feat(formula): rung 2a — layered compiler, description_file, routing, order-fire refusal, the §10 gate"`

---

## Task 5: Rung 2b — `vars`, the substitution asymmetry, `condition` pruning

**Files:** `compose.rs` (stages 5 & 6; **unit tests INSIDE the module** — these fns are
`pub(crate)`), `parse.rs`, `ast.rs`, `validate.rs`, `tests/compose.rs`

```rust
/// §9: substitution applies to `title`, `description`, `assignee`, metadata
/// VALUES, `notes`, `tags` — and NOT to `id`, `needs`, `check.path`, or
/// `drain.formula`. An undefined var KEEPS THE LITERAL PLACEHOLDER.
/// "Reproduce that asymmetry or diverge." gc: `Substitute` (parser.go:617),
/// `varPattern = \{\{([a-zA-Z_][a-zA-Z0-9_]*)\}\}` (parser.go:557).
pub(crate) fn substitute(text: &str, vars: &BTreeMap<String, String>) -> String;

/// §9: `==` and `!=` only; LHS a single `{{var}}`. False ⇒ the step is PRUNED
/// WITH ITS CHILDREN, and dangling `needs` edges are silently dropped.
pub(crate) fn eval_condition(expr: &str, vars: &BTreeMap<String, String>) -> Result<bool, Violation>;
```

Measured: **4 distinct conditions, 29 uses** — `{{drain_policy}} == separate` (12),
`{{drain_policy}} == same-session` (12), **`{{review_mode}} != report` (4 — inside `children`)**,
`{{pr_mode}} != none` (1). The RHS is an **unquoted bare word**; trim both sides, accept a quoted RHS
too. **Pruning must RECURSE into `children`** — that is where `review_mode` lives, and rev 1 missed
it.

`[vars]` merge (load-bearing — §3: *"`drain_policy = "separate"` is declared in `build-base`, not in
the children"*): **parent defaults first, child overrides win** (merged in Task 6's extends stage).
An entry may be a bare string **or** a table with `default`; **a var with no default stays
undefined** and its placeholder survives (the 4 route sites with no default rely on this — they get a
qualified value via `expand_vars`, Task 7). **`review_mode`'s default VARIES BY PACK** (`report` in
`code-review-base`/`review`/`planning-base`, `agent` in `build-base`, `interactive` in
`gstack-build`) — so the merged chain, not a global default, decides pruning.

**The residual check is title-only** (§9). A `description` still containing `{{` is fine.

- [ ] **Step 1: Write the failing tests**
```rust
#[test] fn substitution_never_touches_id_needs_check_path_or_drain_formula() {
    assert_eq!(s.id, "{{id_var}}");
    assert_eq!(s.needs, vec!["{{needs_var}}".to_owned()]);
    assert_eq!(s.check.as_ref().unwrap().path, PathBuf::from(".gc/{{p}}.sh"));
}
#[test] fn an_undefined_var_keeps_its_literal_placeholder_and_only_title_is_residual_checked() { }
#[test] fn a_false_condition_prunes_the_step_with_its_children_and_drops_dangling_needs() {
    // bmad-build's real shape: two drain steps, mutually exclusive on
    // {{drain_policy}}, default "separate" (declared in gascity's build-base).
    assert!(ids.contains(&"implement") && !ids.contains(&"implement-same-session"));
    assert_eq!(publish.needs, vec!["implement".to_owned()], "the dangling need is dropped");
}
#[test] fn condition_pruning_recurses_into_children() {
    // {{review_mode}} != report — 4 uses, ALL inside `children`. Rev 1 missed it.
    let c = compile_named(&layers, &cfg, "review").unwrap();   // review_mode default = "report"
    assert!(!ids(&c).contains(&"review-agent-leg"), "a CHILD step is pruned too");
}
#[test] fn vars_merge_parent_defaults_under_child_overrides() { }
#[test] fn a_condition_outside_the_subset_is_a_violation_naming_the_step() { /* "{{a}} > 1" */ }
```
- [ ] **Step 2: Run and watch them fail** (`vars`/`condition` are still `unimplemented`)
- [ ] **Step 3: Implement.** Remove `vars`/`condition` from `unimplemented`. `substitute` is a
  **single left-to-right pass** (never re-scan inserted values). **Do NOT merge it with
  `cook::substitute` (cook.rs:51)** — that one is `{name}`, this is `{{name}}`: two grammars, two
  scopes. Prune post-order, recursing into `children`, then filter every surviving step's `needs`
  against the surviving id set.
- [ ] **Step 4: Run; then the gate** —
  `python3 ci/gc-compat/formula_gate.py /tmp/gcpacks target/debug/camp --expect-loaded 31`
- [ ] **Step 5: Full gates and commit** —
  `"feat(formula): rung 2b — vars, the substitution asymmetry, condition pruning (31/100)"`

---

## Task 6: Rung 2c — `extends`

§9, verbatim: *"child seeds scalars; parents' steps **append**; a child step whose `id` matches a
parent's **replaces it whole, in place, preserving position**. No field-level merge. Parents resolve
by bare name through the formula layers."* (`advice`/`pointcuts` are **refused** — D4.)

Measured: **48 formulas extend; every resolvable parent lives in `gascity/formulas/`; no formula
extends more than one parent** (implement the list anyway — it is gc's shape — left-to-right); and
**`mol-polecat-work.toml`'s parent `mol-polecat-base` does not exist in the corpus** ⇒ a hard error,
and it is one of the 5.

**Files:** `compose.rs`, `parse.rs`, `ast.rs`, `tests/compose.rs`

- [ ] **Step 1: Write the failing tests**
```rust
#[test] fn a_parents_steps_append_and_a_matching_child_id_replaces_in_place() {
    assert_eq!(ids, vec!["a","b","c","d"], "position preserved; new steps append");
    assert_eq!(b.title, "CHILD B");
    assert_eq!(b.description, None, "replaced WHOLE — no field-level merge (§9)");
}
#[test] fn the_child_seeds_scalars_and_inherits_the_parents_vars() {
    // §3: without this, 24 formulas lose `drain_policy = "separate"`.
    assert_eq!(c.formula.vars["drain_policy"], "separate");
    assert_eq!(c.formula.vars["overridden"], "by-child");
}
#[test] fn a_parent_resolves_by_bare_name_through_the_TRANSITIVE_layer() { /* §7.2 is load-bearing */ }
#[test] fn an_unresolvable_parent_is_a_hard_error_naming_it() {
    // The REAL case: mol-polecat-work extends mol-polecat-base, which ships
    // inside gc's binary-embedded core pack and is NOT in gascity-packs.
    assert!(err.to_string().contains("mol-polecat-base"), "{err}");
}
#[test] fn an_extends_cycle_is_a_hard_error_never_a_stack_overflow() { }
```
- [ ] **Step 2: Run and watch them fail**
- [ ] **Step 3: Implement.** Remove `extends` from `unimplemented`. Resolve depth-first with a
  visited-stack cycle guard. Merge **deepest ancestor first**; each descendant then applies its steps
  (append-or-replace-in-place) and its scalars/vars over the accumulator.
- [ ] **Step 4: Gate** — `--expect-loaded 57`
- [ ] **Step 5: Commit** — `"feat(formula): rung 2c — extends, append and replace-in-place (57/100)"`

---

## Task 7: Rung 2d — `type = "expansion"`, `template`, `expand`, `expand_vars`, `children`

§9: *"`type = "expansion"` — the formula is **not directly runnable**; it supplies `template` steps
for `expand`."* Measured: **14** formulas are `type = "expansion"` **and** carry a top-level
`template` (the same 14, and **none has `steps`** — that is Task 1's S3); **15** steps carry
`expand`; **14** carry `expand_vars`; **2** carry `children`.

gc (`internal/formula/expand.go`): an `expand` rule names a **target step** and a formula; the target
is **replaced** by the expansion formula's `template` steps, with the expansion's own `[vars]` merged
under the rule's overrides resolved against the parent's vars (`ApplyExpansionsWithVars` /
`mergeVars` / `resolveOverrideVars`). **`DefaultMaxExpansionDepth = 5`** — exceeding it is a **hard
error** (invariant 5), never a truncation.

`expand_vars` supplies a qualified route to the **4 route sites with no `[vars]` default** (measured)
— so expansion (stage 3) must run **before** vars/substitution (stage 5), as pinned.

**An expansion formula is `not_runnable`** (key `type`) — the same field the 21 use. It still
**compiles** (it is inside the 95).

- [ ] **Step 1: Write the failing tests**
```rust
#[test] fn an_expansion_formula_compiles_and_is_not_runnable() {
    let c = compile_named(&layers, &cfg, "exp").unwrap();   // it COMPILES — S3 amended (Task 1)
    assert!(c.refusals.is_empty());
    assert_eq!(c.not_runnable.expect("expansion is not directly runnable").key, "type");
}
#[test] fn expand_replaces_the_target_step_with_the_expansion_formulas_template() {
    assert!(!ids.contains(&"placeholder") && ids.contains(&"tmpl-1"));
}
#[test] fn expand_vars_supply_a_qualified_route_where_no_vars_default_exists() {
    assert_eq!(s.assignee.as_deref(), Some("bmad.story-implementer"));
}
#[test] fn children_are_flattened_preserving_position() {
    assert_eq!(ids, vec!["parent","kid-a","kid-b","after"]);
}
#[test] fn expansion_deeper_than_five_is_a_hard_error_not_a_truncation() { }
#[test] fn an_expand_target_that_does_not_exist_is_a_hard_error() { }
```
- [ ] **Step 2–3: Watch them fail; implement.** Remove `type`/`template`/`expand`/`expand_vars`/
  `children` from `unimplemented`.
- [ ] **Step 4: Gate** — `--expect-loaded 83`
- [ ] **Step 5: Commit** — `"feat(formula): rung 2d — expansion, template, expand_vars, children (83/100)"`

---

## Task 8: Rung 2e (compile side) — `drain` and its refusals

**Files:** create `formula/drain.rs` (with its `mod tests` — `parse_drain` is `pub(crate)`) · modify
`parse.rs` (`walk_drain`, on the `walk_on_complete` mold at parse.rs:460), `ast.rs`, `keys.rs`
(`Site::Drain`/`DrainItem` + the value-aware rules), `validate.rs` (S14–S16), `tests/compose.rs`

```rust
/// gc's `DrainSpec` (gascity types.go:341), restricted to what camp implements.
pub struct Drain {
    pub context: DrainContext,       // always Separate — Shared is REFUSED
    pub formula: String,             // the per-member formula. NOT {{var}}-substituted (§9).
    pub member_access: MemberAccess,
    pub on_item_failure: OnItemFailure,
    pub item: DrainItem,
}
pub enum DrainContext  { Separate }
pub enum MemberAccess  { Read, Exclusive }         // as_str: "read" | "exclusive"
pub enum OnItemFailure { Continue, SkipRemaining } // as_str: "continue" | "skip_remaining"
pub struct DrainItem   { pub single_lane: bool }
```

**gc's compiler defaulting, verbatim** — `ApplyDrainControlMetadata`, gascity
`internal/formula/compile.go` (*"the single shape owner for drain control metadata"*).
*(Provenance: compat §9 cites `compile.go:579-608`; the function at `GASCITY_REF` spans **:583-611**.
Same function, same tree — §9's line numbers are off by 4. Recorded so the next reader does not think
they read a different tree.)*

| field | gc default | camp |
|---|---|---|
| `member_access` | **`"read"`** when unset | same. (All 25 corpus drains set `"exclusive"`.) |
| `on_item_failure` | `"skip_remaining"` if `context == "shared"`, else **`"continue"`** | same. camp refuses `shared` ⇒ camp's effective default is **`continue`** — §9 exactly. |
| `item.single_lane` | absent = false | same. |

**Refusals (§4 rule 1, each naming its key):** `drain.context = "shared"` (13 corpus uses),
`drain.continuation_group` (0), `drain.max_units` (0). The shared-drain message names **the formula,
the step, and the `drain_policy = same-session` var that selects it** (§9, verbatim) — **except** for
the one drain that has no condition, whose message says so.

**`on_item_failure = "skip_remaining"` with `context = "separate"` is IMPLEMENTED, not refused** —
gc's enum permits it and §9 lists both values; refusing an enumerated value would be camp inventing a
restriction. **Zero corpus coverage** (all 13 uses sit on shared drains) ⇒ proven by the Task 9
fixture. Same for **`single_lane`**. **The PR body must say this plainly.**

- [ ] **Step 1: Write the failing tests**
```rust
#[test] fn drain_defaults_follow_gcs_compiler() {
    let d = parse_drain(r#"formula = "item""#).unwrap();
    assert_eq!(d.member_access, MemberAccess::Read);          // gc defaults to "read"
    assert_eq!(d.on_item_failure, OnItemFailure::Continue);   // separate ⇒ continue
    assert!(!d.item.single_lane);
}
#[test] fn a_conditional_shared_drain_is_refused_naming_formula_step_and_drain_policy() {
    let r = c.refusals.iter().find(|r| r.key == "context").unwrap();
    assert!(r.construct.contains("implement-same-session"));
    assert!(r.reason.contains("build") && r.reason.contains("drain_policy")
            && r.reason.contains("same-session"), "{}", r.reason);
}
#[test] fn the_corpus_build_formulas_compile_clean_because_the_shared_arm_IS_PRUNED() {
    // THE load-bearing interaction. bmad-build/gstack-build/compound-build each
    // carry TWO drain steps on mutually exclusive conditions; the default
    // (drain_policy = "separate", from gascity's build-base) prunes the shared
    // one BEFORE the refusal can fire. If the refusal fired at parse time, all
    // three v1 build formulas would refuse and the ceiling would drop by 3.
    // PRUNING (stage 6) RUNS BEFORE THE REFUSAL CHECK (stage 8).
    let c = compile_named(&layers, &cfg, "build").unwrap();
    assert!(c.refusals.is_empty(), "{:?}", c.refusals);
    let d = c.formula.steps.iter().find(|s| s.id == "implement").unwrap().drain.as_ref().unwrap();
    assert_eq!(d.member_access, MemberAccess::Exclusive);
    assert_eq!(d.on_item_failure, OnItemFailure::Continue);
}
#[test] fn an_UNCONDITIONAL_shared_drain_is_refused_and_nothing_can_prune_it() {
    // same-session-implement.formula.toml — the 13th shared drain, with NO
    // `condition`. §9 and rev 1 both assumed all 13 were guarded. TWELVE are.
    // This is one of the 5 formulas camp cannot load.
    let c = compile_named(&layers, &cfg, "same-session-implement").unwrap();
    assert!(c.refusals.iter().any(|r| r.key == "context"), "{:?}", c.refusals);
}
#[test] fn setting_drain_policy_to_same_session_refuses_instead_of_approximating() { }
#[test] fn continuation_group_and_max_units_are_refused_by_name() { }
#[test] fn drain_formula_is_never_var_substituted() { assert_eq!(d.formula, "{{item_formula}}"); }
```
- [ ] **Step 2: Run and watch them fail**
- [ ] **Step 3: Implement.** `walk_drain` mirrors `walk_on_complete`, same key-whitelist discipline
  and the same **presence-not-parse-success** rule (`RawStep.has_drain`, review finding 5). Add
  `has_drain` to **S9**'s combination bans (`check`+`drain`, `retry`+`drain` — a drain step is
  campd's, not a worker's) and to **S11**'s `uses_graph_only` predicate (validate.rs:182). Fill
  `keys::classify` for `Site::Drain` / `Site::DrainItem` and the value-aware rules. **Remove `drain`
  from `unimplemented`, then DELETE the `unimplemented` field and its violation — it is empty
  forever.**
- [ ] **Step 4: The gate, at the ceiling**
```bash
python3 ci/gc-compat/formula_gate.py /tmp/gcpacks target/debug/camp
```
Expected: **95 loaded · 62 runnable · 5 refused by name** (the table above), every rung count
matching, the `rungs.py` cross-check green. **This is the phase's headline gate. If it reports
anything else, STOP and report to the lead — `rungs.py` is the arbiter; do not adjust the
expectation.**
- [ ] **Step 5: Commit** —
  `"feat(formula): rung 2e compile — drain, with shared/continuation_group/max_units refused (95/100, 62 runnable)"`

---

## Task 9: The drain runtime — ONE lifecycle model, the reservation, and no leaks

**ADDITIVE ONLY** in `dispatch.rs`; `event_loop.rs` is **not touched**. `cp-1` owns those files.

**Files:** `formula/runtime.rs` (pure reads) · `readiness.rs` · `ledger/mod.rs` ·
`camp/src/daemon/dispatch.rs` (**additive**) · `camp/src/cmd/doctor.rs` + `main.rs` (the operator
escape) · `camp/tests/daemon_drain.rs`

### THE LIFECYCLE — one model, and it is campd-owned (B4)

Rev 1 specified the anchor three mutually-exclusive ways and hit a guaranteed `InvalidTransition`: it
queued the drain when the anchor **closed pass**, then tried to **close** that already-closed anchor
at gather (`fold.rs:369-373` rejects closing a closed bead), and its own tests closed the
*predecessor*, never the anchor.

**gc settles it:** *"Drain … materializes as a **controller-owned control bead**"* (types.go:318-319).

> **The drain anchor is CAMPD-OWNED. It is never dispatched to a worker.**
> campd **claims** it when its `needs` are satisfied → **scatters** the members → **gathers** →
> **closes** it.

This reuses machinery camp already has for check/retry anchors: `maybe_claim_looping`
(dispatch.rs:1891) claims the anchor with `claimed_by = "campd"`, flipping it to `in_progress`, and
`dispatchable_beads` requires `status = 'open'` (readiness.rs:137) — so **a campd-held anchor is
never worker-dispatched.** The change is minimal and additive:

```rust
// formula/runtime.rs — beside is_looping (runtime.rs:94). NOT a rename.
/// campd holds this step's anchor itself: check/retry loops, and now drain —
/// gc's "controller-owned control bead" (gascity types.go:318).
pub fn is_campd_held(step: &Step) -> bool { is_looping(step) || step.drain.is_some() }
```
and switch **the one call site** (dispatch.rs:1909) from `is_looping` to `is_campd_held`. S9 (Task 8)
bans `check`+`drain` and `retry`+`drain`, so a drain anchor never enters the check/attempt paths.

**B5 falls out for free:** the anchor stays `in_progress` until gather, so `flow::finalization`
(which iterates step anchors) **cannot** find the run quiescent, and every downstream step that
`needs` the drain step stays blocked. *(Rev 1's justification — "the item roots hang off the anchor by
`needs`" — was false: `finalization` iterates step anchors only, and bond children never block
quiescence. The campd-held anchor is what actually blocks it.)* Pinned by
`the_run_does_not_finalize_while_drain_items_are_open`.

### THE MATERIALIZATION MATRIX (B6)

Rev 1 claimed a `needs` edge on the item root would serialize items. **It is inert:**
`dispatchable_beads` **excludes run roots outright** (readiness.rs:139,
`AND NOT (b.run_id IS NOT NULL AND b.step_id IS NULL)`) — a root is never dispatchable, so a `needs`
edge **on a root gates nothing**. And serializing purely by "materialize the next when the previous
**passes**" silently imports `skip_remaining` semantics into `continue`. The real matrix:

| `single_lane` | `on_item_failure` | materialization |
|---|---|---|
| false | `continue` (the corpus default) | **eager** — every member's item root in one pass |
| **true** | `continue` | **lazy** — materialize item *i+1* when item *i*'s root is **CLOSED (any outcome)**. Concurrency 1; a failure does **not** halt the lane. |
| any | `skip_remaining` | **lazy** — materialize item *i+1* only when item *i*'s root closed **`pass`**; on the first non-pass, materialize nothing further and gather. |

The distinction between rows 2 and 3 — *closed* vs *closed pass* — **is** the difference between
`continue` and `skip_remaining`, and it gets its own test
(`single_lane_with_a_failing_item_still_runs_the_rest`).

### THE RESERVATION, AND EVERY RELEASE PATH (B11)

Reserve member *i* **before** materializing its item root: `bead.updated { metadata: {
"gc.exclusive_drain_reservation": <anchor id> } }` — **gc's key verbatim** (beadmeta/keys.go:93,
invariant 7). Task 3's fold makes it an atomic CAS. `member_access = "read"` reserves nothing.

**Release on EVERY exit path** — rev 1 released only on a clean finalize, so a conflict, a dead-end, a
`skip_remaining` trip, or a `kill -9` between reserve and cook leaked a reservation **forever**,
blocking every future drain over that member with **no operator escape** (the metadata verb is
compat-3's):

| exit path | release |
|---|---|
| gather (all items closed) | release every member this drain holds, **in the finalizing `append_batch`** |
| reserve **conflict** on member *k+1* | release members *1..k* **in the same batch as** the `dispatch.failed` |
| `skip_remaining` trips | release every member this drain holds, at gather |
| run dead-ends (`dead_end_run`) | release every member held by any anchor of that run |
| **campd killed between reserve and cook** | **`reconcile` sweep**: any `bead_meta` row whose key is `gc.exclusive_drain_reservation` and whose value names an anchor that is **closed or absent** is **released** — an orphan by definition |
| **operator escape** | **`camp doctor --drain-reservations [--release-orphans]`** — lists every held member and its holding anchor; `--release-orphans` releases those whose anchor is closed or absent. **Ships here**, not in compat-3. |

**No new event type.** The reservation rides `bead.updated`; a drain that cannot proceed uses
`dispatch.failed`, exactly as fan-out does (dispatch.rs:2258-2274 — *"`dispatch.failed` is the honest
name: campd could not dispatch the declared follow-on work"*). Not laziness:
`vocab.rs::no_reservation_vocabulary_exists` **forbids any event name containing `"reserv"`**.

**Interfaces produced:**
```rust
// formula/runtime.rs — pure, write-free (the file's stated contract)

/// The drain's member set — gc's `convoycore.Members(parentConvoyID)`
/// (gascity dispatch/drain.go:211) mapped onto camp's run (D3). Ordered by
/// creation seq ⇒ a deterministic scatter.
pub fn run_members(conn: &Connection, ctx: &RunContext) -> Result<Vec<BeadRow>, CoreError>;

/// `drain:<anchor>:<index>` on each item ROOT — the exact mold of `bond_label`
/// (runtime.rs:504). It is the IDEMPOTENCY LEDGER: what exists is materialized.
pub fn drain_label(anchor: &str, index: usize) -> String;
pub fn parse_drain_label(label: &str) -> Option<(&str, usize)>;
pub fn drain_children(conn: &Connection, anchor: &str) -> Result<BTreeMap<usize, BeadRow>, CoreError>;

/// Members whose reservation names an anchor that is closed or absent.
pub fn orphaned_reservations(conn: &Connection) -> Result<Vec<(String, String)>, CoreError>;

// camp/src/daemon/dispatch.rs — beside PendingFanout (dispatch.rs:1045)
#[derive(Debug, Clone, PartialEq)]
pub struct PendingDrain { pub run_id: String, pub step_id: String, pub anchor: String }
```

`run_members` SQL — **note `b.type = 'task'` (B13):** compat-4 introduces dispatch-excluded
`type = "mail"` beads and camp already has `memory` beads; without the filter, a mail bead landing in
a run would be **scattered and reserved**.
```sql
SELECT {BEAD_COLS} FROM beads b
 WHERE b.run_id = ?1 AND b.step_id IS NULL AND b.type = 'task' AND b.id <> ?2   -- ?2 = the run root
   AND b.labels NOT LIKE '%"bond:%' AND b.labels NOT LIKE '%"drain:%'
 ORDER BY (SELECT MIN(e.seq) FROM events e WHERE e.bead = b.id AND e.type = 'bead.created'), b.id
```
The `LIKE`s are a **prefilter**; re-parse the labels Rust-side and drop decoys, exactly as
`bond_children` (runtime.rs:514-549) does.

### The test harness (B10)

**There is no shared daemon-test harness in this repo — every file rolls its own.** Copy
`struct Daemon`, `wait_until`, `scaffold` and `camp_ok` **from `crates/camp/tests/daemon_dispatch.rs`**
into `daemon_drain.rs` (the established mold), plus these helpers, defined here:

| helper | definition |
|---|---|
| `c.settle()` | `wait_until(\|\| c.cursor_is_caught_up() && c.pending_drains_empty())`, 10 s deadline (the `daemon_dispatch.rs` mold). |
| `c.create_member(&run, title)` | `camp_ok(&["create", title, "--run", run])` → the new bead id. |
| `c.step_bead(&run, step_id)` | read the run manifest's `steps` map. |
| `c.drain_children(anchor)` | the `drain:<anchor>:*` label query (`ledger.drain_children`). |
| `c.close_pass(b)` / `c.close_fail(b)` / `c.close_pass_all(&[..])` | `camp_ok(&["close", b, "--outcome", "pass"\|"fail"])`. |
| `c.dispatchable()` | `camp_core::readiness::dispatchable_beads(&conn)` — **there is no CLI for readiness**, so call the library directly. |
| `c.restart_campd()` | drop the `Daemon` and re-spawn on the same camp root (the `daemon_lifecycle.rs` mold). |
| `c.bead_metadata(b)` | `ledger.bead_metadata(b)`. |

### The two ZERO-COVERAGE fixtures — given in full (B10)

The corpus **provably cannot** exercise `single_lane` or `on_item_failure` on camp's path (all 13 uses
sit on shared drains camp refuses). These fixtures are their **only** proof, so they are specified
here rather than left to the implementer.

`tests/fixtures/compose/drain-single-lane/formulas/sl-build.formula.toml`:
```toml
formula = "sl-build"
contract = "graph.v2"

[[steps]]
id = "decompose"
title = "Decompose"

[[steps]]
id = "implement"
title = "Implement each member"
needs = ["decompose"]
[steps.implement.drain]
context = "separate"
formula = "sl-item"
member_access = "exclusive"
# on_item_failure OMITTED ⇒ gc's default for separate context = "continue"
[steps.implement.drain.item]
single_lane = true

[[steps]]
id = "publish"
title = "Publish"
needs = ["implement"]
```
`.../formulas/sl-item.formula.toml`:
```toml
formula = "sl-item"
contract = "graph.v2"

[[steps]]
id = "work"
title = "Work the member"
```
`fixture_campd_with_single_lane_drain()` = this pack, **`max_workers = 8`**, **3 members** created
with `camp create --run`.

`.../drain-skip-remaining/formulas/sr-build.formula.toml` — identical to `sl-build` except:
```toml
[steps.implement.drain]
context = "separate"
formula = "sl-item"
member_access = "exclusive"
on_item_failure = "skip_remaining"
```
(no `[steps.implement.drain.item]` — `skip_remaining` serializes on its own).
`fixture_campd_with_skip_remaining_drain()` = this pack, **3 members**.

- [ ] **Step 1: Write the failing tests** (`camp/tests/daemon_drain.rs`)

```rust
#[test]
fn the_drain_anchor_is_campd_held_and_never_worker_dispatched() {
    // B4: ONE lifecycle. gc: "a controller-owned control bead" (types.go:318).
    let mut c = fixture_campd_with_drain_formula();
    let run = c.sling("build");
    c.create_member(&run, "A");
    c.close_pass(&c.step_bead(&run, "decompose"));      // the PREDECESSOR, never the anchor
    c.settle();
    let anchor = c.get_bead(&c.step_bead(&run, "implement"));
    assert_eq!(anchor.status, "in_progress");
    assert_eq!(anchor.claimed_by.as_deref(), Some("campd"));
    assert!(!c.dispatchable().iter().any(|b| b.id == anchor.id), "never worker-dispatched");
}

#[test] fn a_drain_scatters_the_runs_members_one_item_root_each() { /* 2 members ⇒ 2 item roots */ }

#[test]
fn an_exclusive_drain_reserves_every_member_with_gcs_verbatim_key() {
    assert_eq!(c.bead_metadata(&m)["gc.exclusive_drain_reservation"], c.step_bead(&run, "implement"));
}

#[test]
fn a_second_drain_reserving_a_held_member_fails_loudly_and_never_mutates_it() {
    let failures = c.events_of_type("dispatch.failed");
    assert_eq!(failures.len(), 1, "the SECOND drain fails; the first is untouched");
    assert!(failures[0]["data"]["reason"].as_str().unwrap().contains("gc.exclusive_drain_reservation"));
    assert_eq!(c.bead_metadata(&c.member)["gc.exclusive_drain_reservation"], c.first_drain);
}

#[test]
fn a_conflicting_drain_releases_the_members_it_had_already_reserved() {
    // B11: it reserves 1..k, conflicts on k+1, and must NOT leak 1..k.
    let mut c = fixture_campd_conflict_on_second_member();
    c.settle();
    assert!(!c.bead_metadata(&c.first_member).contains_key("gc.exclusive_drain_reservation"));
}

#[test] fn the_reservation_is_released_when_the_drain_gathers() { }

#[test]
fn the_run_does_not_finalize_while_drain_items_are_open() {
    // B5: the campd-held anchor is what blocks quiescence.
    c.settle();
    assert!(c.events_of_type("run.finalized").is_empty());
    assert!(!c.dispatchable().iter().any(|b| b.id == c.step_bead(&run, "publish")),
            "a downstream step stays blocked on the open drain anchor");
    c.close_pass_all(&c.drain_children(&anchor));
    c.settle();
    assert_eq!(c.get_bead(&anchor).outcome.as_deref(), Some("pass"));
    assert!(c.dispatchable().iter().any(|b| b.id == c.step_bead(&run, "publish")));
}

#[test]
fn single_lane_items_never_run_concurrently() {
    let mut c = fixture_campd_with_single_lane_drain();   // 3 members, max_workers = 8
    c.settle();
    assert_eq!(c.drain_children(&anchor).len(), 1, "concurrency 1");
    c.close_pass(&c.drain_children(&anchor)[&0]);
    c.settle();
    assert_eq!(c.drain_children(&anchor).len(), 2);
}

#[test]
fn single_lane_with_a_failing_item_still_runs_the_rest() {
    // B6: `continue` ≠ `skip_remaining`. Lazy materialization ALONE would
    // silently import skip_remaining's semantics. THIS test separates them —
    // rev 1 had no such test.
    let mut c = fixture_campd_with_single_lane_drain();   // on_item_failure defaults to `continue`
    c.settle();
    c.close_fail(&c.drain_children(&anchor)[&0]);
    c.settle();
    assert_eq!(c.drain_children(&anchor).len(), 2, "the lane advances on CLOSED, not on PASSED");
    c.close_pass(&c.drain_children(&anchor)[&1]);
    c.settle();
    assert_eq!(c.drain_children(&anchor).len(), 3, "all 3 members ran");
    c.close_pass(&c.drain_children(&anchor)[&2]);
    c.settle();
    assert_eq!(c.get_bead(&anchor).outcome.as_deref(), Some("fail"),
               "the drain's outcome reflects the failures at gather (§9)");
}

#[test]
fn skip_remaining_materializes_nothing_after_the_first_failure() {
    let mut c = fixture_campd_with_skip_remaining_drain();   // 3 members
    c.settle();
    assert_eq!(c.drain_children(&anchor).len(), 1);
    c.close_fail(&c.drain_children(&anchor)[&0]);
    c.settle();
    assert_eq!(c.drain_children(&anchor).len(), 1, "nothing further");
    assert_eq!(c.get_bead(&anchor).outcome.as_deref(), Some("fail"));
    assert!(!c.bead_metadata(&c.members[1]).contains_key("gc.exclusive_drain_reservation"),
            "and it leaks nothing");
}

#[test]
fn a_drain_survives_a_campd_restart_without_double_materializing() {
    let before = c.drain_children(&anchor);
    c.restart_campd();                                  // reconcile re-queues
    c.settle();
    assert_eq!(c.drain_children(&anchor), before, "the drain: label is the idempotency ledger");
}

#[test]
fn reconcile_releases_a_reservation_orphaned_by_a_kill_9() {
    // B11: campd died between reserve and cook. The holder is closed/absent.
    let mut c = fixture_with_orphaned_reservation();
    c.restart_campd();
    c.settle();
    assert!(!c.bead_metadata(&c.member).contains_key("gc.exclusive_drain_reservation"));
}

#[test]
fn doctor_lists_and_releases_orphaned_drain_reservations() {
    // The operator escape. `bd update --set-metadata` is compat-3's; this ships HERE.
    assert!(c.camp(&["doctor", "--drain-reservations"]).stdout.contains("gc.exclusive_drain_reservation"));
    c.camp(&["doctor", "--drain-reservations", "--release-orphans"]);
    assert!(!c.bead_metadata(&c.member).contains_key("gc.exclusive_drain_reservation"));
}

#[test]
fn a_mail_bead_in_a_run_is_never_a_drain_member() {
    // B13: compat-4 introduces dispatch-excluded `type = "mail"` beads.
    c.create_mail_bead_in_run(&run);
    c.settle();
    assert_eq!(c.drain_children(&anchor).len(), 1, "only the task bead was scattered");
}
```

- [ ] **Step 2: Run and watch them fail** — `cargo test -p camp --test daemon_drain 2>&1 | tail -20`

- [ ] **Step 3: Implement the pure reads** in `runtime.rs`/`readiness.rs` (SQL above).

- [ ] **Step 4: Implement the dispatch arms — SEVEN additive edits, no refactors**

1. `PendingDrain` beside `PendingFanout` (dispatch.rs:1045).
2. `pending_drains: Vec<PendingDrain>` on `GraphRuntime` (:1051-1063).
3. `queue_drain` beside `queue_fanout` (:2180) — dedupe with `Vec::contains`.
4. **`maybe_claim_looping` (:1891)** — switch its predicate to `is_campd_held` (:1909) and, after a
   successful claim of a **drain** step, `queue_drain(...)`. *(This — not `on_bead_closed` — is the
   trigger. The anchor is claimed when READY, not when closed.)*
5. `execute_drain` loop in `execute` (:1154), after the fanout loop, with the same
   **requeue-the-unexecuted-tail-on-error** shape (:1154-1162).
6. `on_bead_closed` (:1813): when a closed bead is a **drain item root** (parse its `drain:` label),
   re-queue that anchor's `PendingDrain` — the mold of `on_root_closed` (:1864) for bond children.
7. `reconcile` (**dispatch.rs:1645**): for every `run.cooked`, for every step with `drain`, if the
   anchor is campd-held-and-open, re-queue; **plus the orphaned-reservation sweep** (B11).

`execute_drain` mirrors `execute_fanout` (:1174-1275): read members, read `drain_children` (the
idempotency ledger), compute the due indices **per the matrix**, reserve, then `cook_with` with
`extra_root_labels = vec![drain_label(anchor, i)]` and `vars` = the parent's merged vars plus
`{member}` = the member bead id. **`drain.formula` resolves through `FormulaLayers`**, not
`<camp>/formulas/<bond>.toml` (which `execute_fanout` hardcodes at :1230) — every corpus item formula
(`bmad-story-development`, `gstack-work`, `compound-work`) lives in an **imported** pack. At gather,
`close_anchor` (:2296) closes the campd-held anchor `pass` iff every item root closed `pass`, else
`fail` / `hard_fail`, **and releases every reservation in the same `append_batch`**.

- [ ] **Step 5: Run and watch them pass**
```bash
cargo test -p camp --test daemon_drain 2>&1 | tail -10
cargo test --workspace 2>&1 | tail -10
```
- [ ] **Step 6: Full gates and commit** —
  `"feat(dispatch): the drain runtime — campd-held anchors, exclusive reservations, single_lane, on_item_failure"`

---

## Task 10: Invariant 6 — camp ⊆ gc, with fixtures that actually reach the gate

**The trap:** the `gc-compat` CI job runs the **real gc compiler** over
`crates/camp-core/tests/fixtures/formulas/valid` (`camp_corpus_validate.go`), and that shim globs
`*.toml` and derives a formula's name as **`TrimSuffix(basename, ".toml")`**. So:

- **Do NOT name a fixture `*.formula.toml`** — gc would receive the name `"x.formula"` and fail its
  own name check, turning the invariant-6 gate red for a reason unrelated to camp's rules.
- **Do NOT put an `expansion` fixture in `valid/`** — the shim compiles each file **standalone**, and
  §9 says an expansion formula is *"not directly runnable"*; gc may reject it.
- **`extends-child` needs a parent LAYER**, which the shim does not provide.

⇒ `expansion` and `extends-child` live in `tests/fixtures/compose/` (covered by `tests/compose.rs`,
not by the gc gate). The **parent** goes in `valid/` — it is a complete, standalone, gc-valid formula.

- [ ] **Step 1: Add gc-valid standalone fixtures** to `tests/fixtures/formulas/valid/`:
  `vars-condition.toml`, `extends-parent.toml`, `drain-separate.toml` — each declaring
  `contract = "graph.v2"`, each named `<name>.toml`. Update the hard-coded list in
  `formula_corpus.rs::every_valid_fixture_is_accepted`.

- [ ] **Step 2: Prove they pass the REAL gc compiler locally**
```bash
git clone -q --filter=blob:none https://github.com/gastownhall/gascity /tmp/gascity \
  && git -C /tmp/gascity checkout -q "$(cat ci/gc-compat/GASCITY_REF)"
mkdir -p /tmp/gascity/cmd/camp-corpus-validate
cp ci/gc-compat/camp_corpus_validate.go /tmp/gascity/cmd/camp-corpus-validate/main.go
(cd /tmp/gascity && go build -o /tmp/camp-corpus-validate ./cmd/camp-corpus-validate)
/tmp/camp-corpus-validate crates/camp-core/tests/fixtures/formulas/valid
```
Expected: `OK <name>` for every fixture, exit 0. **A `FAIL` means camp accepts a formula gc rejects —
invariant 6 is broken.**

- [ ] **Step 3: Write the corpus-drift procedure** into a new `ci/gc-compat/README.md`: moving
  `GCPACKS_REF` requires, in ONE PR — re-run `python3 ci/gc-compat/rungs.py <new corpus>`; update
  `formula_gate.py`'s `CEILING`, `RUNNABLE`, `RUNG_COUNTS`, `NOT_LOADABLE`; re-run
  `differential.py`; update the §9 addendum's numbers. **`rungs.py` is the arbiter** — it is derived
  from §9's text, not from camp's `RUNGS` table, so it can tell you which of the two is wrong.

- [ ] **Step 4: Commit** —
  `"test(formula): gc-valid fixtures for the new key sets + the corpus-drift procedure"`

---

## Task 11: The gc ORACLE — a differential gate, not a reading of gc's source (RULING 3)

`ci/gc-compat/camp_corpus_validate.go` **already links gc's real compiler**
(`github.com/gastownhall/gascity/internal/formula`) and already runs in CI — and then **discards the
compiled formula** (`if _, err := formula.Compile…`). Meanwhile **every fidelity claim in this phase
is asserted from READING gc's source**: the >4096 pointer prompt "byte-for-byte", gc's drain
defaulting, `extends` = append + replace-in-place, `expand` replaces the target, condition-pruning
drops dangling `needs`, and **B15's inline-vs-substitute ordering**. Those claims carry rungs 2c (+26)
and 2d (+26) — the most formulas in the phase. A mis-transcribed paragraph in the pointer prompt is a
**silent divergence no camp-only test can detect**.

**Files:** create `ci/gc-compat/gc_compile_json.go`, `ci/gc-compat/differential.py` · modify
`camp/src/cmd/doctor.rs` (`--compiled`), `.github/workflows/ci.yml`

- [ ] **Step 1: The gc side — a NEW shim** (leave `camp_corpus_validate.go` untouched so a green
  invariant-6 gate cannot be broken by this work).

`ci/gc-compat/gc_compile_json.go`: `usage: gc-compile-json <layer-dir>[,<layer-dir>...] <formula-name>`.
Calls the same `formula.CompileWithoutRuntimeVarValidation(ctx, name, layers, nil)` the existing shim
calls, then marshals the **compiled** formula to JSON on stdout in this normalized shape:

```jsonc
{ "formula": "bmad-build",
  "steps": [ { "id": "implement",
               "title": "...",
               "description_sha256": "…",     // a sha, so the diff stays readable
               "needs": ["decompose"],
               "metadata": { "gc.run_target": "gc.run-operator", "gc.kind": "drain", ... },
               "drain": { "context": "separate", "formula": "bmad-story-development",
                          "member_access": "exclusive", "on_item_failure": "continue",
                          "single_lane": false } } ] }
```

- [ ] **Step 2: The camp side** — `camp doctor --formula <path> --json --compiled` emits **the same
  schema** from `Compiled.formula`.

- [ ] **Step 3: `ci/gc-compat/differential.py`** — compile all 100 in **both**, and diff.

```python
"""RULING 3: verify fidelity against the REAL gc compiler, not against a reading of it.

usage: differential.py <corpus> <camp-binary> <gc-compile-json-binary>

For each of the 100 formulas: compile in gc (layers = all 10 formula-bearing
packs) and in camp (the formula_gate.py camp root), then diff the normalized
JSON. ASSERT the ONLY differences are the KNOWN, EXPLAINED ones:

  - the 5 camp refuses (NOT_LOADABLE): camp emits nothing.
    (mol-polecat-work: gc ALSO fails — its parent lives in gc's embedded core
     pack, which this shim does not load. ASSERT gc fails it too.)
  - `assignee`: camp resolves metadata.gc.run_target INTO `assignee` (§4 trap 3);
    gc leaves it in metadata. Compare camp.assignee == gc.metadata["gc.run_target"].

EVERYTHING else — step id lists AND ORDER, titles, description_sha256, needs,
metadata maps, drain specs — must match EXACTLY. A mismatch is a camp≠gc
divergence and FAILS the gate.

This settles, by RUNNING gc rather than reading it:
  · the >4096 pointer prompt, byte-for-byte (via description_sha256)
  · whether description_file contents are {{var}}-substituted        <- B15
  · gc's drain defaulting (member_access→read, on_item_failure→continue)
  · extends: append + replace-in-place, position preserved, no field-level merge
  · expand: the target step is REPLACED by the template
  · condition pruning, and that dangling `needs` are dropped
"""
```

- [ ] **Step 4: Run it locally and FIX CAMP where it diverges.** This is the step that converts every
  source-read in this plan into a verified fact. **If gc and camp disagree on the pointer prompt's
  bytes, camp is wrong** — transcribe again. **If they disagree on whether `description_file`
  contents are substituted, gc's behaviour wins and the Task 4 pipeline order changes to match.**

- [ ] **Step 5: Wire CI.** The Go toolchain lives in the **`gc-compat`** job (it checks out gascity
  and runs `setup-go`); the corpus and the camp binary live in **`gcpacks-compat`**. The differential
  needs all four ⇒ add the corpus checkout (at `GCPACKS_REF`) and `cargo build --bin camp` to the
  **`gc-compat`** job, which already has the gc source and Go:
```yaml
      - name: build the gc oracle
        run: |
          mkdir -p gascity-src/cmd/gc-compile-json
          cp ci/gc-compat/gc_compile_json.go gascity-src/cmd/gc-compile-json/main.go
          cd gascity-src && go build -o "$RUNNER_TEMP/gc-compile-json" ./cmd/gc-compile-json
      - name: differential — camp's compiler vs the REAL gc compiler
        run: python3 ci/gc-compat/differential.py gcpacks-src target/debug/camp "$RUNNER_TEMP/gc-compile-json"
```

- [ ] **Step 6: Commit** —
  `"ci(gc-compat): the differential gate — camp's compiler diffed against the real gc compiler"`

---

## Task 12: Final gates and the PR

- [ ] **Step 1: Every gate, in CI's order**
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 ci/gc-compat/rungs.py             /tmp/gcpacks                          # the arbiter
python3 ci/gc-compat/load_corpus_packs.py /tmp/gcpacks target/debug/camp        # compat-1's gate, still green
python3 ci/gc-compat/formula_gate.py      /tmp/gcpacks target/debug/camp        # 95 · 62 · 5 refused
python3 ci/gc-compat/differential.py      /tmp/gcpacks target/debug/camp /tmp/gc-compile-json
/tmp/camp-corpus-validate crates/camp-core/tests/fixtures/formulas/valid        # invariant 6
ci/gc-compat/check_vocab.sh /tmp/gascity "$PWD"                                 # formula.refused must not collide
```
- [ ] **Step 2: Push and open the PR**
```bash
git push -u origin compat-2-formulas
gh pr create --title "compat: the formula key sets — rungs 2a–2e, 95/100 loadable, 62 runnable (compat-2)"
gh pr checks --watch
```

**The PR body MUST state, with evidence:**
- **LOADABLE 95** and **RUNNABLE 62** — *both*. "95/100" alone is misleading; 62 is the number that
  answers *"can camp run gc's packs?"*
- The **5** formulas camp refuses and why — including the **two §9 did not anticipate**
  (`same-session-implement`'s unconditional shared drain; `mol-polecat-work`'s parent, which ships in
  gc's binary-embedded core pack).
- The per-rung counts: **2 · 31 · 57 · 83 · 95**.
- **`SCHEMA_VERSION` 2 → 3: an existing camp.db will NOT open and the operator must re-init** (the v1
  no-auto-upgrade contract).
- **`single_lane` and `on_item_failure` have ZERO corpus coverage** — the corpus provably cannot
  exercise them on camp's path — and are proven by the two fixtures in Task 9.
- The **three spec amendments**: master line 449 (S11); the §9 addendum (the ceiling, S2/S3, D2′, the
  runnable count).
- That fidelity is **verified against the real gc compiler** (Task 11), not asserted from its source.

- [ ] **Step 3: CI green.** `gh pr checks --watch`. Not complete before.

---

## Exit criteria — and how each is proven

| Exit criterion (phase block, verbatim) | Proof |
|---|---|
| *"every §9 rung's count pinned by a test at GCPACKS_REF"* | `ci/gc-compat/formula_gate.py` (CI, `gcpacks-compat`) drives **the real binary** over all 100 and asserts **2 · 31 · 57 · 83 · 95**, cross-checked against `rungs.py`, the **independent arbiter** derived from §9's text. The corpus is not vendored ⇒ a CI gate, not a `cargo test` — the mold compat-1 established, and what §10 asks for. |
| *"refusals name their key and land as ledger events"* | `formula.refused` (Task 2), `deny_unknown_fields`-validated in the fold; appended by `camp sling`, **by the daemon's order-fire path** (B12), and by the drain refusals. Tests: `phase_is_refused_by_key_and_the_reason_names_the_value`, `a_scope_check_hiding_in_step_metadata_values_is_refused`, `sling_refuses_a_no_contract_formula_and_events_the_refusal`, `a_due_order_naming_a_no_contract_formula_fires_nothing_and_events_the_refusal`. |
| *"camp ⊆ gc gate still green (invariant 6)"* | Task 10 (new standalone fixtures compiled by the **real gc compiler**) **and Task 11** (all 100 corpus formulas diffed against it — a far stronger form of the same invariant). |
| *"Ceiling is 97–98 and the gate names which"* | **The measured ceiling is 95**, and §9 is amended to say so (Task 1). §9's 97–98 counted only vapor+scope; **two more formulas fail for reasons §9 did not anticipate.** The gate names all five. |
| *"The 21 no-contract formulas are refused, not assumed"* | D1: they compile (inside the 95) and are `not_runnable`; **both** cook entry points refuse them with a `formula.refused` event naming `contract`. The 14 `type = "expansion"` formulas are refused the same way. **RUNNABLE = 62 is pinned by the gate.** |
| *"exclusive reservations as member-bead metadata (`gc.exclusive_drain_reservation`, verbatim)"* | Task 3 (the store — refold-wired, schema 3, atomic CAS) + Task 9 (reserve/conflict/release on **every** exit path, the orphan sweep, the operator escape). |
| *"same-session REFUSED"* | Task 8: the conditional case (12 drains, pruned on the default path — with the ordering test proving the three v1 build formulas still compile clean) **and the unconditional case** (`same-session-implement`, which nothing prunes and which §9 missed). |
| *"on_item_failure/single_lane per gc's compiler defaulting"* | Task 8's defaulting table (from `ApplyDrainControlMetadata`) + Task 9's materialization matrix, including `single_lane_with_a_failing_item_still_runs_the_rest` — the test that separates `continue` from `skip_remaining`. |
| *"CI green"* | Task 12. |

## Notes for the implementer

- **`rungs.py` is the arbiter.** If a count moves, the pin moved or a rule is wrong. **Report to the
  lead; never edit a seed to match the code.**
- **Do not merge `cook::substitute` (`{name}`) with `compose::substitute` (`{{name}}`).** Two
  grammars, two scopes. DRY does not mean conflating two languages.
- **The `unimplemented` scaffold must be GONE by Task 8.** If it survives, an accepted key silently
  compiles to nothing — the exact failure §4's trap 3 warns about.
- **`dispatch.rs` is shared with `cp-1`.** Additive edits only (`event_loop.rs` is not touched at
  all). Expect a rebase; re-run every gate after it.
- **Task 11 outranks this plan's prose.** Where the differential gate says gc behaves differently from
  what a task's comment claims, **gc wins** — that is the entire point of building it.

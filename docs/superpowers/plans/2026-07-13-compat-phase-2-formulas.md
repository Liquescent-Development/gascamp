# Compat Phase 2 — the formula key sets (rungs 2a–2e) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or
> superpowers:subagent-driven-development) to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Status:** rev 3 — after two adversarial plan gates (REJECT, REJECT). **This revision was written
from the output of gc's real compiler, not from a reading of its source** (Ruling 5). Every fidelity
claim below is measured; the shim that measured it is Task 0 and ships in the repo.

**Goal:** camp loads and compiles the real Gas City formula corpus at `ci/gc-compat/GCPACKS_REF` —
**95 of 100 loadable, 62 runnable** — refusing the other 5 by name, with every §9 rung pinned by a
gate that runs the real binary, every fidelity claim pinned by a differential gate against the real
gc compiler, and **at least one imported corpus formula cooked and run end-to-end**.

**Architecture:** camp's formula compiler is today a *strict subset validator* that rejects every Gas
City construct by name. Phase 2 inverts it into a **permissive, layered, two-stage compiler** matching
gc's real staging:

| stage | gc | camp (this plan) |
|---|---|---|
| **COMPILE** | `extends` → `expand` → **single-brace `{name}` fully resolved** → `condition` pruned → `description_file` inlined. **`{{var}}` survives verbatim.** | identical |
| **INSTANTIATE** (gc `stepToBead`; camp `cook`) | **`{{var}}` substituted over every field and every metadata value, incl. `check.path`** | identical |

`drain` (2e) becomes a **campd-owned** step — gc's *"controller-owned control bead"* — with gc's
**real** runtime semantics: eager, all-members, all-or-nothing reservation.

**Tech Stack:** Rust (camp-core, camp); Go (the gc oracle shims, in the existing `ci/gc-compat` Go
job); Python 3 stdlib (`tomllib`).

---

## Task 0 came first, and it rewrote this plan

Ruling 5 ordered the gc shim built **before** any Rust and **before** rev 3 was finalized. It was.
`ci/gc-compat/factshim.go` compiles the corpus with all 10 formula dirs as search paths. Output at
`GCPACKS_REF` / `GASCITY_REF`:

```
layers=10 formulas=100 OK=99 FAIL=1        (mol-polecat-work: extends mol-polecat-base — not found)
steps                          1523
drain_steps                    20     (drain_ctx_separate 19 · drain_ctx_shared 1)
resid_desc                     567    ({{...}} SURVIVING in compiled Descriptions)
resid_title                    1
resid_md_gc.run_target         55     ({{...}} SURVIVING in compiled routes)
resid_md_gc.continuation_group 14
```

and, from the compiled Recipe of `bmad-build`:

```jsonc
"bmad-build.implement": { "Assignee": "",
  "Metadata": { "gc.kind": "drain", "gc.drain_context": "separate",
                "gc.drain_formula": "bmad-story-development",
                "gc.drain_member_access": "exclusive",
                "gc.drain_on_item_failure": "continue",          // DEFAULTED by the compiler
                "gc.run_target": "{{implementation_target}}" } } // UNRESOLVED, despite a default
```
and from `superpowers-code-review` — same corpus, the *other* grammar:
```jsonc
"…main.process-code-review": { "Metadata": { "gc.run_target": "superpowers.implementer" } }
// authored as  metadata = { "gc.run_target" = "{implementation_target}" }  — SINGLE brace, RESOLVED at compile
```

**That output invalidated four things rev 2 asserted**, and everything downstream of them. It is why
rev 3 exists. Do not re-derive any fidelity claim from gc's source; **re-run the shim.**

---

## What changed in this revision, and why

| Item | What was wrong in rev 2 | Fixed in |
|---|---|---|
| **F1** `{{var}}` is NOT substituted at compile (567 residual descriptions, 55 residual routes) | rev 2 substituted at **compile** | **Task 5** — `{{var}}` moves to **cook**. |
| **F4** gc has a **SECOND grammar**: single-brace `{name}`, **fully resolved at compile** | rev 2 had **no single-brace grammar at all**, and claimed *"0 bare route values"* — **there are 8** | **Task 7** — `{name}` resolution, inside expansion only. |
| **F8** gc **DOES** substitute `check.path` and `drain.formula`; there is **no exemption list** | rev 2 shipped two tests locking in the opposite | **Task 5** — the exemption list is deleted; a templated `drain.formula` is rejected at **validation** (`{{`-check), as gc does. §9's asymmetry bullet is **amended**. |
| **F2** gc's Recipe has **no `Drain` struct** — drain lives entirely in `Metadata` | rev 2's oracle demanded a `"drain": {…}` object gc **cannot emit** ⇒ all 20 drain steps fail the diff | **Task 11** — compare `gc.drain_*` metadata. |
| **F5/F6 + RULING 4** `single_lane` has **zero production readers**; `on_item_failure` is read **only** by `advanceSharedDrain` ⇒ separate drains are **always `continue`** | rev 2 built a **4-cell materialization matrix** + 2 synthetic fixtures for behavior **gc does not have** | **Tasks 8/9** — matrix **KILLED**. Parsed, validated, round-tripped, **no runtime behavior**. §9 amended. |
| **F7 + BD4** gc's separate drain reserves the **WHOLE member set FIRST**, then materializes | rev 2 reserved incrementally ⇒ on a conflict at k+1 it released m1 **while item-run 1 was live on it** — *two drains mutating one bead*, the exact thing the reservation prevents | **Task 9** — all-or-nothing reserve in **one `append_batch`**. |
| **BD1** rungs 2c/2d were still **key-set containment** | camp validates the **extends-MERGED** step list; **8 formulas inherit a late-rung key only from a parent** | **Rungs re-derived: 2 · 31 · 49 · 76 · 95.** `rungs.py` now simulates the pipeline it arbitrates. |
| **BD2** the value-aware refusal fired at **parse** ⇒ 19 formulas with a *conditional* shared drain refuse ⇒ **ceiling 76, not 95** | rev 2 asserted the right answer in a test comment and specified the opposite mechanism | **Tasks 1+2 / 8** — `Refusal`s are **step-scoped** and die with their pruned step. |
| **BD3** `is_campd_held` **re-detonated B4** | `maybe_claim_looping`'s tail calls `create_attempt` **unconditionally** ⇒ every drain step gets a **real worker**, the anchor closes early, gather hits `InvalidTransition`. **All four of rev 2's tests still passed.** | **Task 9** — `create_attempt` gated on `is_looping`; tests assert `flow::attempts(..).is_empty()` and that **nothing carrying the drain's `step_id`** is dispatchable. |
| **BD5** a reserve conflict **deadlocks the run forever** | `dispatch.failed` only appends an event; the campd-held anchor never closes ⇒ `NotQuiescent` forever | **Task 9** — the conflict **closes the anchor** `fail`/`hard_fail`. |
| **BD8** ⚠️ **the phase-killer: the pinned-formula round-trip** | `cook` pins the **raw authored source**; `load_run` **re-parses it with no layers and a default config** ⇒ **every one of the 62 runnable corpus runs dead-ends** — and no gate in rev 2 could see it | **Task 4** (pin the **compiled recipe**) + **Task 12** (a gate that **cooks and runs an imported corpus formula end-to-end**). |
| **BD6** `multi-violation.toml` detonates inside B7's own answer | its `tags` key becomes **accepted** ⇒ `violations.len() >= 5` fails | **Tasks 1+2** — fixture reworked, new counts given. (It is a **52**-row table, not 55.) |
| **BD7** Task 2 used types only Task 4 creates | no ordering remedy | **Tasks 1+2 merged**; `UNIMPLEMENTED` named with its initial contents. |
| **BD9** `SCHEMA_VERSION` bumped in **one** of **two** places | `schema.rs:78` writes the literal `'2'` ⇒ **every fresh camp fails to open on its next process** | **Task 3** — both sites + the module doc. |
| **BD10** the anti-tuning cross-check was **unimplementable or vacuous** | `--formula-rungs` takes no formula path; recomputing counts reproduces `rungs.py` by construction | **Task 4** — `--formula-rungs` JSON specified exactly; the cross-check becomes **set-vs-set** (camp's real per-file verdicts vs the arbiter's prediction) — falsifiable. |
| **BD11** the harness is not executable | `daemon_dispatch.rs` has free functions and a `Daemon` with one method; 3 of 5 fixtures undefined; the conflict fixture possibly unconstructible | **Task 9** — the `Camp` struct defined in full; every fixture given as TOML; the conflict fixture is **two drain steps in ONE run** (the only constructible shape — a bead has one `run_id`). |
| **RULING 5** | Task 11 — the thing meant to stop source-read errors — was **itself a source-read** | **Task 0**: the shim ships **first**; rev 3 is written from its output. |

**The standing instruction — *"for every fix, ask what NEW failure it permits, and write the test that
catches THAT"* — is applied explicitly.** Tasks 4, 5, 7, 9 each carry a **"What this fix could newly
break"** block naming the failure and the test that catches it.

---

## Authority, and the spec amendments this plan makes

| Rank | Document | Amended? |
|---|---|---|
| 1 | `docs/design/2026-07-05-gas-camp-design.md` | **Yes — line 449** (S11: `contract` satisfies the compiler declaration). |
| 2 | compat spec §4/§9/§10/§12.2 | **Yes — a §9 addendum**: the ceiling (95), S2/S3/S11, D2′, **§9's substitution bullet (wrong — F1/F4/F8)** and **§9's drain-runtime bullet (wrong — F5/F6/F7)**. |
| 3 | `2026-07-12-KNOWN-DEFECTS.md` | No. |
| 4 | `2026-07-13-wave-2-compat-orchestration.md` | No. |

Invariant 5 (**fail fast, no fallbacks, no panics in library code**), invariant 6 (**camp ⊆ gc**),
invariant 7 (**vocabulary mirror**) bind every task.

**Provenance:** corpus `GCPACKS_REF = 44b2eef94f035283b70df62d3bd1fc77bce13d56`; gc
`GASCITY_REF = 12410301884b51131a35e101a335dbaae16cdcb0`. Re-derivable with
`ci/gc-compat/factshim.go` (gc's real compiler) and `ci/gc-compat/rungs.py` (the arbiter). Both ship
in this phase.

---

## Global Constraints

- **Branch** `compat-2-formulas`; one PR; never commit to main.
- **Gates before every push:** `cargo fmt --all --check` && `cargo clippy --workspace --all-targets
  --all-features -- -D warnings` && `cargo test --workspace`. Not complete until pushed and
  `gh pr checks --watch` is green.
- **TDD strictly.** Write the failing test, run it, **watch it fail with the expected message**,
  implement, watch it pass.
- **No panics in library code** (`unwrap_used`/`expect_used`/`panic` denied; `unsafe_code` forbidden).
- **No network in `cargo test`.** The corpus is never vendored (§10); corpus assertions live only in
  CI gate scripts.
- **New events:** four lockstep edits — `EventType` + `ALL` + `as_str` (`event.rs`), a `fold::apply`
  arm, `CAMP_SPECIFIC_EVENTS` (`vocab.rs`). Payloads private, `deny_unknown_fields`, validated in the
  fold (`check_passed` mold, fold.rs:680).
- **New fold state:** the table goes in **BOTH** `refold.rs::STATE_TABLES` (production) **and**
  `refold_prop.rs::DUMPS` (test), **and** needs a `SCHEMA_VERSION` bump **in both literal sites**.
- **Shared files — ADDITIVE ONLY.** `cp-1` is in flight. Contended: `daemon/dispatch.rs`,
  `daemon/event_loop.rs` (**not touched at all**), `main.rs`, `event.rs`, `vocab.rs`, `fold.rs`,
  `Cargo.*`, `.github/workflows/ci.yml`. Expect a real rebase.
- **Commits:** no co-author trailers; never mention the agent.

---

## Decisions

### D1. LOADABLE ≠ RUNNABLE. **95 loadable, 62 runnable.** *(Ratified at gate 1.)*
Compile = parse, extends, expand, `{name}`, prune, inline `description_file`. Runnable = additionally
`contract = "graph.v2"` **and** `type != "expansion"`. The 21 no-contract + 14 expansion formulas
compile and are refused at **run** time by **all three cook entry points**: `camp sling`, the daemon's
**order-fire** path, and **`execute_drain`**'s item cook. **Both numbers go in the PR body.**

### D2′. Permissive for IMPORTED layers; STRICT for camp-local `<root>/formulas/`. *(Ratified.)*
Unrecognised key: **ignored+warned** when imported, **hard error** in `<root>/formulas/`. Known-dead
gc keys (`version`, `target_required`, `internal`, top-level `mode`/`single_lane`,
`sling_container_mode`) are ignored+warned in **both** tiers. Annotations (`notes`, `catalog`,
formula-level `metadata`) are silent in both. **Migration:** an operator whose camp-local formula
carries a now-fatal key must remove it. Zero known users — named, not built for.

### D3. gc's convoy is camp's run. *(Ratified.)*
A **run member** is a bead with `run_id = <the drain's run>`, `step_id IS NULL`, **`type = 'task'`**,
**`status <> 'closed'`** (gc: `convoycore.Members(store, id, includeClosed=false, …)`,
`membership.go:96-144` — *"if !includeClosed && IsTerminalStatus(b.Status) { return }"*), not the run
root, no `bond:`/`drain:` label. Added by `camp create --run <run_id>`.

### D4. `advice`/`pointcuts` are **REFUSED** (§4 rule 1; 0 corpus uses).

### D5 (**NEW — F1/F4/F8**). Two grammars, two stages. This is the heart of rev 3.

| grammar | resolved | scope | unknown token |
|---|---|---|---|
| **`{name}`** (gc `rangeVarPattern = \{(\w+)\}`, `range.go:32`; applied by `substituteVars`, `range.go:94`, **inside `expandStep`**, `expand.go:255`) | **AT COMPILE**, during expansion | every field `expand.go:265-342` touches — ID, Title, Description, Notes, Assignee, Expand, Timeout, Labels[], Needs[], **Metadata[]**, ExpandVars[], Gate.*, Loop.*, OnComplete.*, Ralph.Check.* — **but NOT `DescriptionFile`** | **left verbatim** (`range.go:103`) |
| **`{{name}}`** (`varPattern`, `parser.go:557`; applied by `Substitute`, `parser.go:617`) | **AT INSTANTIATION** — gc `stepToBead`; camp **`cook`** | **every field, and EVERY metadata value, with NO exemption list** (`molecule.go:1035-1037`) — **including `check.path`** (→ `gc.check_path`, `ralph.go:76`) **and `drain.formula`** (→ `gc.drain_formula`, `compile.go:590`) | **left verbatim** |

**Measured:** 435 single-brace occurrences — **362 are the fixed `{target}` family** (`{target}`,
`{target.id}`, `{target.title}`, `{target.description}` — `substituteTargetPlaceholders`,
`expand.go:446-464`, a plain `strings.ReplaceAll`, **not** the var grammar); the general single-brace
vars are the rest (`{implementation_target}` ×8 — **all in `children.metadata.gc.run_target`**,
`{ISSUE_NUM}` ×7, `{artifact_path_keys}` ×4, …). **Zero single-brace residuals in compiled metadata.**
Conversely **55 `{{}}` routes and 567 `{{}}` descriptions survive compilation.**

**⇒ §9's substitution-asymmetry bullet is WRONG and is amended.** Camp does **not** exempt
`check.path` or `drain.formula`. A templated `drain.formula` is instead rejected **at validation**, as
gc does: `if strings.Contains(formulaName, "{{")` → *"templated item formula names are not supported
in v0"* (`graphv2_validation.go:417-419`).

**Conditions are evaluated at COMPILE** over the merged var **values** (never by text substitution).
Proof: 13 authored shared drains → **1** in gc's compiled output; the other 12 are pruned by
`{{drain_policy}} == same-session` under the default `separate`.

### D6 (**NEW — BD8**). What is pinned in `runs/<id>/`, and how `load_run` reconstitutes it.

**The bug rev 2 shipped:** `cook.rs:176` writes `formula.source` — *"verbatim bytes of the authored
file"* (`ast.rs:15`) — and `runtime.rs:67-69`'s `load_run` **re-parses that file** with
`parse_and_validate` (**no layers, no config**). `dispatch.rs:1774-1783`'s `ctx()` turns any error into
`None`, and every caller then **dead-ends the run**. For **all 62 runnable corpus formulas** (they
carry `extends`, `description_file`, and routes needing `cfg.imports`) that re-parse **cannot succeed**
⇒ **every cooked corpus run dead-ends on campd's first event.** Rev 2's gates could not see it:
`formula_gate.py` only *compiles*; `differential.py` diffs *compilers*; and the drain fixtures were
layer-free camp-local packs that happen to re-parse cleanly.

**The fix — pin the COMPILED recipe beside the authored source:**
```
runs/<run_id>/
  manifest.json      unchanged (already carries `vars`, cook.rs:186-188)
  <formula>.toml     the authored bytes, VERBATIM — invariant 3 ("human-readable run files").
                     AUDIT ONLY. Nothing re-parses it.
  recipe.json        NEW: serde_json of the COMPILED `Formula`. THIS is what load_run reads.
```
- `Formula`/`Step`/`Check`/`Retry`/`OnComplete`/`Drain` derive `Serialize`/`Deserialize`.
- `cook` writes both. `ast.rs:15`'s doc comment is amended: `source` is the **authored** artifact and
  is no longer the reload path.
- **`load_run` deserializes `recipe.json` — no re-parse, no layers, no config, no vars.** It cannot
  fail for a corpus formula, and `Step.metadata` / `Step.assignee` / `Step.drain` — which exist **only**
  post-compose — survive.
- Condition pruning is **not re-derived at load** (rev 2's silent `Corrupt` on a differently-pruned
  step set disappears): the pinned recipe has exactly the steps cook materialized, so `load_run`'s
  *"manifest steps do not match the pinned formula"* check passes by construction.

---

## Deliberately deferred / accepted fidelity costs (named)

| Item | Disposition |
|---|---|
| `drain.max_units` (the **key**) | **Refused by name** (§4 rule 1; 0 corpus uses). **BUT gc applies a runtime default of 100 and hard-closes a drain with more members** (`drain.go:24`, `:244-255`, reason `limit_exceeded`). **Camp implements the cap at 100**: a drain over it **closes `fail`/`hard_fail` and scatters nothing.** Refusing the authored key while honouring the runtime cap is the only combination that neither invents semantics nor scatters 200 workers where gc fails. |
| `drain.continuation_group` (the **key**: 0 uses) | Refused by name. **The METADATA key `gc.continuation_group` (29 authored uses, 14 surviving compilation) is a DIFFERENT thing** — rev 2 conflated them — and is **accepted and carried verbatim**; camp does not honour it (§11.4). |
| `gc.build.artifact_schema` / `gc.build.artifact_path_keys` (74/74), `gc.on_fail` (1) | **Accepted and carried verbatim.** 148 sites; refusing is not an option. Camp does not act on them. **Named as accepted fidelity costs.** |
| `context = "shared"` drains | §9: *"REFUSED, loudly."* |
| `single_lane`, `on_item_failure` | **Parsed, validated, carried into `gc.drain_*` metadata — with NO runtime behavior**, exactly as gc (F5/F6). |
| `gate`, `loop`, `pour`, `compose`, `tally`, `waits_for`, `depends_on` | §4 rule 1 refusals; 0 corpus uses each. |
| `bd update --set-metadata` | compat-3. The operator escape (`camp doctor --drain-reservations`) does **not** depend on it. |
| `gc.routed_to` / `gc.work_branch` | compat-3 stamps them. Task 3 fixes their **storage rule now** (projected from the column, refused as metadata) so compat-3 cannot inherit two sources of truth. |
| gc's **ralph/scope loop expansion** (`.iteration.N` steps, `gc.kind: ralph`/`scope`, `gc.attempt`) | gc expands check/retry loops **at COMPILE** into namespaced steps (1523 for 99 formulas); **camp keeps them as RUNTIME loops** (`PendingCheck`, `create_attempt`). A pre-existing architectural difference, **not** changed here — and why a full step-list diff against gc is structurally impossible (Task 11 scopes around it). |

---

## The measured seed table

Re-derived by simulating the **real** pipeline (extends-**merged** key sets; value-aware refusals
evaluated **after** pruning). Arbiter: `ci/gc-compat/rungs.py`.

| rung | key set added (§9) | **loadable** | rev-2 (wrong) |
|---|---|---|---|
| 2a | dead keys ignored; annotations; `contract`; `description_file`; step `metadata` | **2** | 2 |
| 2b | `vars`, `condition` | **31** | 31 |
| 2c | `extends` | **49** | 57 |
| 2d | `type`, `template`, `expand`, `expand_vars`, `children` | **76** | 83 |
| **2e** | **`drain`** | **95** ← the ceiling | 95 |
| | **RUNNABLE** | **62** | 62 |

**Why 2c/2d moved:** camp resolves `extends` at stage 2 and validates at stage 6, so it validates the
**MERGED** step list. **Eight formulas inherit a late-rung key ONLY from a parent** —
`build-from-convoy`, `build-from-decompose-base`, `build-from-decompose`, `build-from-plan-base`,
`build-from-plan`, `build-from-requirements-base`, `build-from-requirements` (all inherit **`drain`**)
and `github-issue-fix` (inherits **`expand`** + **`expand_vars`**). **Independently corroborated by gc:**
the corpus *authors* 12 separate drain steps and gc *compiles* **19** — the seven extra are inherited.

**The 5 camp cannot load:**

| file | refusal |
|---|---|
| `gastown/formulas/mol-digest-generate.toml` | `phase` (`= "vapor"`) |
| `pr-pipeline/formulas/mol-pr-from-issue.formula.toml` | `phase` (`= "vapor"`) |
| `gascity/formulas/design-review.formula.toml` | step metadata `gc.kind = "scope"` / `gc.scope_*` (**`gc.scope_kind` does not exist in the corpus**) |
| `gascity/formulas/same-session-implement.formula.toml` | `drain.context = "shared"` — **UNCONDITIONAL**; 12 of the 13 shared drains are pruned by `{{drain_policy}}`, this one has no `condition`. **gc compiles it; camp deliberately refuses it.** |
| `gastown/formulas/mol-polecat-work.toml` | `extends → mol-polecat-base`, absent from the corpus. **gc fails it too** — gc compiles 99/100. |

---

# Tasks

## Task 0: The gc oracle shim — BUILD IT FIRST (RULING 5)

**Nothing else in this plan may be trusted until this runs.** Rev 2's Task 11 was meant to stop
source-read errors and was itself a source-read; four of its fidelity claims were false.

**Files:** create `ci/gc-compat/factshim.go`

- [ ] **Step 1: Write the shim.** `usage: factshim <corpus-root> [formula-name]`. Walk the corpus for
  every `*/formulas` dir (**all 10 — cross-pack `extends` needs them; with fewer, 33/100 fail and
  every count is wrong**, F9), sort them into `searchPaths`, and call the entry point
  `camp_corpus_validate.go` already uses:
  `formula.CompileWithoutRuntimeVarValidation(ctx, name, layers, nil)`. With a name →
  `json.MarshalIndent` the `*Recipe`. Without → compile all; print `FAIL <name>: <err>` and a summary
  (step count, drain steps by context, `{{`-residuals in Title/Description/Metadata).
- [ ] **Step 2: Run it; pin the baseline.**
```bash
mkdir -p /tmp/gascity/cmd/factshim && cp ci/gc-compat/factshim.go /tmp/gascity/cmd/factshim/main.go
(cd /tmp/gascity && go build -o /tmp/factshim ./cmd/factshim)
/tmp/factshim /tmp/gcpacks
```
  **Expected, exactly:** `layers=10 formulas=100 OK=99 FAIL=1` (`mol-polecat-work`) · `steps 1523` ·
  `drain_steps 20` (`separate 19`, `shared 1`) · `resid_desc 567` · `resid_title 1` ·
  `resid_md_gc.run_target 55` · `resid_md_gc.continuation_group 14`.
  **If any number differs, STOP and report to the lead — the pin moved.**
- [ ] **Step 3: Commit** — `"ci(gc-compat): factshim — gc's real compiler as this phase's oracle"`

---

## Task 1+2: The three camp-local rules, the value-aware key table, D2′, the fixture corpus

*(Merged — BD7: Task 2's `parse_and_validate` needs Task 4's types and Task 1's tests need Task 2's
`Origin`. One commit, one `cargo test --workspace`.)*

**Files:** `formula/validate.rs` · create `formula/keys.rs` · `parse.rs` (replace `CITY_ONLY_*` /
`ACCEPTED_*`, :42-87, and both key loops) · `ast.rs` · `formula/mod.rs` (**incl. the module doc**) ·
`event.rs`, `vocab.rs`, `fold.rs` · `tests/formula_corpus.rs`, `tests/fixtures/formulas/**` · create
`ci/gc-compat/rungs.py` · `docs/design/…:449` · the compat spec §9 addendum

### The three camp-local rules (measured)

| rule | site | corpus impact |
|---|---|---|
| **S2** name == file stem | `validate.rs:34-50` | **92/100 violate** — files are `<name>.formula.toml`. compat-1's `orders::resolve_formula` already accepts both spellings: **resolver and validator disagree today.** |
| **S3** ≥1 step | `validate.rs:52-57` | **25/100 have no `steps`** — 11 inherit via `extends` (fine: validate runs after the merge), **14 are `type = "expansion"` and never have steps**. |
| **S11** graph-only ⇒ `[requires] formula_compiler` | `validate.rs:178-191`; **master spec line 449** | only **4/100** declare `[requires]`; **36 use `check`/`retry`/`on_complete` and ALL 36 declare `contract = "graph.v2"`**. |

```rust
/// file name minus `.toml`, minus an optional trailing `.formula`.
pub(crate) fn formula_stem(path: &Path) -> Option<&str> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".toml")?;
    Some(stem.strip_suffix(".formula").unwrap_or(stem))
}
```
S3 → `if raw.steps.is_empty() && raw.kind.as_deref() != Some("expansion") { …existing violation… }`,
plus *"an `type = \"expansion\"` formula must declare at least one `[[template]]` step"*.
S11 → `raw.formula_compiler.is_some() || raw.contract.as_deref() == Some("graph.v2")` — strictly
wider, so no merged formula loses its verdict.

### `keys.rs`

```rust
/// §4 trap 1 — key off NESTING, never name. Top-level `mode`/`single_lane` are
/// DEAD; `steps.check.check.mode` and `steps.drain.item.single_lane` are load-bearing.
pub enum Site { Top, Step, Check, CheckInner, Retry, OnComplete, Drain, DrainItem }

/// D2′ — the permissiveness rule is scoped by ORIGIN, and FormulaLayers knows it.
pub enum Origin { Imported, CampLocal }

pub enum Class {
    Accepted,
    Refused,     // gc semantics camp does not implement → §4 rule 1
    DeadInGc,    // a real gc key with NO gc semantics → ignore+warn, BOTH tiers
    Annotation,  // silent, both tiers
    Unknown,     // recognised by nobody. Imported ⇒ ignore+warn. CampLocal ⇒ HARD ERROR.
}
pub fn classify(site: Site, key: &str) -> Class;

/// The VALUE-AWARE refusal layer. `classify` alone cannot express `phase = "vapor"`
/// nor a scope-check hiding in step-metadata VALUES. STEP-SCOPED when the site is a
/// step — see BD2.
pub fn refuse(site: Site, key: &str, value: &toml::Value, at: &str) -> Option<Refusal>;

pub const RUNGS: &[Rung] = &[
    Rung { id: "2a", top: &["contract"],         step: &["description_file", "metadata"] },
    Rung { id: "2b", top: &["vars"],             step: &["condition"] },
    Rung { id: "2c", top: &["extends"],          step: &[] },
    Rung { id: "2d", top: &["type", "template"], step: &["expand", "expand_vars", "children"] },
    Rung { id: "2e", top: &[],                   step: &["drain"] },
];

/// Accepted by the table, NOT YET IMPLEMENTED by the pipeline. Each of Tasks 5–8
/// removes its own keys; TASK 8 DELETES THIS CONST AND ITS VIOLATION. Without it an
/// accepted key silently compiles to nothing — §4 trap 3 — and every intermediate
/// rung count is a lie.
pub const UNIMPLEMENTED: &[&str] = &[
    "vars", "condition",                                     // Task 5 removes
    "extends",                                               // Task 6 removes
    "type", "template", "expand", "expand_vars", "children", // Task 7 removes
    "drain",                                                 // Task 8 removes
];
```

### **BD2 — refusals are STEP-SCOPED and die with their step**

Rev 2 called `keys::refuse` from `walk_keys` at **stage 1** and pushed into a flat, formula-level
`Vec<Refusal>` **nothing ever re-filtered**. Because 19 formulas carry a *conditional* `same-session`
drain arm (12 authored + 7 inherited), **every one would refuse at parse ⇒ ceiling 76, not 95** —
taking `bmad-build`, `gstack-build` and `compound-build` with it. Rev 2 asserted the correct answer in
a **test comment** and specified the opposite mechanism.

```rust
pub struct Refusal {
    pub construct: String,
    pub key: String,
    pub reason: String,
    /// Some(step_id) ⇒ belongs to a STEP; DISCARDED with it when the step is pruned
    /// (stage 5) or replaced in place by `extends` (stage 2).
    /// None ⇒ formula-level (e.g. `phase`) — never discarded.
    pub step: Option<String>,
}
```
Pruning drops every refusal whose `step` left the surviving set; **stage 6 collects only survivors**.
Corroborated by gc: it prunes the same 12 (13 authored shared drains → **1** compiled).

**What this fix could newly break:** a refusal carried from a **parent** step that the child
**replaces in place**. Test: `a_refusal_on_a_parent_step_that_the_child_replaces_is_discarded` (Task 6).

### The value-aware refusal rules — real keys only

| site | key | condition | reported key | scope |
|---|---|---|---|---|
| `Top` | `phase` | any value | `phase` | formula |
| `Step` | `metadata` | map has `gc.kind = "scope"` | `gc.kind` | step |
| `Step` | `metadata` | map has any `gc.scope_*` key | that key | step |
| `Drain` | `context` | `== "shared"` | `context` | step |
| `Drain` | `continuation_group` / `max_units` | present | that key | step |

`phase` refuses on the **key** (all corpus uses are `vapor`; this preserves the merged refusal at
`parse.rs:44`, which rev 2's table silently deleted). **`gc.kind = "cleanup"` is NOT refused** — only
`scope`. `gc.run_target`, `gc.continuation_group`, `gc.build.*`, `gc.on_fail` ride through untouched.
*(gc's **compiler** emits `gc.kind: scope` on generated ralph-loop bodies — measured in `bmad-build`'s
Recipe. Camp inspects the **AUTHORED** metadata, where only `design-review` carries it, and generates
no scope steps.)*

### Fixture disposition (B7 + **BD6**)

`tests/formula_corpus.rs` holds a **52**-row table over **52** invalid fixtures + `assert_eq!(on_disk,
in_table)` + a 5-name `valid/` list.

- **STILL REJECTED, row unchanged:** `phase`, `pour`, `compose`, `advice`, `pointcuts`, `gate`,
  `loop`, `waits-for`, `tally`, `depends-on`; **`unknown-key`, `nested-unknown-key`,
  `type-step-level`** (D2′ keeps unrecognised keys fatal in the camp-local tier); and every semantic
  row. **The assertion becomes `err.names(c)`** (a `Refusal` is not a `Violation`).
- **DELETED — file *and* row (16):** `extends`, `vars`, `type-top-level`, `contract`, `catalog`,
  `template`, `drain`, `expand`, `expand-vars`, `children`, `condition`, `metadata`,
  `description-file`, `priority`, `tags`, `notes`.
- **`multi-violation.toml` — BD6.** It carries step-level `tags = ["x"]`, which **becomes accepted**,
  so the fixture yields **3** violations + 1 refusal and both `names("tags")` and
  `violations.len() >= 5` fail — inside the answer to B7. **Replace `tags = ["x"]` with
  `gate = { path = "x" }`** (still refused) and rewrite the test:
```rust
#[test]
fn multi_violation_fixture_reports_every_problem_at_once() {
    let err = parse_and_validate(&corpus("invalid").join("multi-violation.toml")).unwrap_err();
    for construct in ["pour", "steps.a.gate", "formula", "steps.a.needs", "steps.a.timeout"] {
        assert!(err.names(construct), "missing {construct:?} in:\n{err}");
    }
    assert_eq!(err.violations.len(), 3, "{err}");   // formula-stem, needs, timeout
    assert_eq!(err.refusals.len(), 2, "{err}");     // pour, gate
}
```
- **`valid/` grows** (Task 10): `vars-condition.toml`, `extends-parent.toml`, `drain-separate.toml`.

`FormulaError` gains `refusals: Vec<Refusal>` and `names(&self, construct: &str) -> bool`.
**`FormulaError::Display` (ast.rs:116-126) must render refusals too** — it currently prints only
violations, so a refusal-only error (`phase`) would print *"0 violation(s):"* and list nothing, and
both `camp doctor --formula`'s human mode and several `err.to_string().contains(…)` assertions read
that string.

**`parse_and_validate` survives** with its signature, as the **no-layer, camp-local** entry:
`compose::compile(&FormulaLayers::local_only(path), &CampConfig::default(), path, &BTreeMap::new())`,
returning `Err` when violations **or** refusals is non-empty.

### `ci/gc-compat/rungs.py` — the arbiter (BD1), and its scope stated honestly

**It simulates the pipeline it arbitrates.** Rev 2's claim that it modelled *"camp's FULL rule set"*
was false.

> **In scope:** the extends chain (merged key sets; cycles; missing parents), the value-aware refusals
> (incl. condition-pruning of shared drains under merged vars), the §4 rule-1 key refusals, and
> cumulative rung key-set containment over the **recursively merged** step tree (`steps` + `template`
> + `children`).
> **Out of scope, and therefore NOT certified by it:** S2/S3/S11, route/binding resolution,
> `description_file` resolution, the `{name}` grammar, expansion depth, `needs` validity after
> pruning. Those are pinned by `cargo test` and by `formula_gate.py` driving the **real binary**.

The four base sets, **stated literally** — rev 2 referenced them and never defined them, and a
panelist reproduced the seeds only after supplying them from `parse.rs:74-87`; with plausible guesses
the counts collapsed to 0·0·11·25·25:
```python
BASE_TOP  = {"description", "formula", "requires", "steps"}
BASE_STEP = {"assignee", "check", "description", "id", "needs", "on_complete", "retry",
             "timeout", "title"}
DEAD_TOP  = {"version", "target_required", "internal", "mode", "single_lane",
             "sling_container_mode"}
ANNO_TOP  = {"notes", "catalog", "metadata"}
ANNO_STEP = {"notes", "tags", "priority"}
```
```python
# ACCEPTED(R) = BASE_TOP ∪ BASE_STEP ∪ (⋃ rung.top ∪ rung.step for rung in R)
# MERGED(F)   = F's keys ∪ EVERY ancestor's keys, over the RECURSIVE step tree
#               (steps + template + children).                              <-- BD1
# F LOADABLE at r iff:
#   (1) the extends chain resolves and is acyclic;
#   (2) no formula-level refusal (`phase`);
#   (3) no step-level refusal ON A SURVIVING STEP — a step whose `condition` is false
#       under the MERGED vars (parent defaults first, child overrides win) is PRUNED,
#       and its refusals die with it;                                       <-- BD2
#   (4) MERGED(F) ⊆ ACCEPTED(R) ∪ DEAD ∪ ANNOTATION.
#       DEAD/ANNOTATION are EXCLUDED from the check (else 2a = 0, not 2).
#       Nested sites (check.*, retry.*, on_complete.*, drain.item.*) are NOT walked.
# RUNNABLE = |{F loadable at 2e : contract == "graph.v2" and type != "expansion"}|
```
Expected: **`2a 2 · 2b 31 · 2c 49 · 2d 76 · 2e 95 · RUNNABLE 62`**, and the 5 refused named.

- [ ] **Steps:** failing tests → run → watch fail → implement → run → pass.
  `keys.rs`: `classify_matches_section_4s_table` · `the_rung_table_is_section_9s_table_verbatim`
  (asserted against a **literal transcription**, not by construction — rev 2's version was true by
  construction and could never fail) · `phase_is_refused_by_key_and_the_reason_names_the_value` ·
  `a_scope_check_hiding_in_step_metadata_values_is_refused` ·
  `a_cleanup_kind_and_a_run_target_are_not_refused` · `a_step_scoped_refusal_carries_its_step_id`.
  `parse.rs`: `an_unknown_key_is_ignored_in_an_IMPORTED_layer_and_fatal_in_the_CAMP_LOCAL_one` ·
  `a_key_dead_in_gc_is_ignored_in_BOTH_tiers` · `annotations_are_silent_in_both_tiers`.
  `validate.rs`: the three rule tests.
- [ ] Add `EventType::FormulaRefused` → `"formula.refused"` (+ `ALL`, `as_str`,
  `CAMP_SPECIFIC_EVENTS`, a log-only `deny_unknown_fields` fold arm). **Verified:** gc's 71-event
  vocabulary has no `formula.*`; `no_reservation_vocabulary_exists` scans **event names only** (the
  metadata key is safe; **no event may ever be named `drain.reserved`**).
- [ ] Rewrite `formula_corpus.rs` per the disposition; amend `formula/mod.rs`'s module doc (it says
  camp *"accepts no unknown keys, where gc silently ignores them"* — **D2′ inverts that sentence**).
- [ ] Write `rungs.py`; run it; expect the seed table exactly.
- [ ] Amend **master spec line 449** and append the **§9 addendum** (below).
- [ ] Gates; commit — `"feat(formula): the permissiveness rule — value-aware, step-scoped, origin-scoped (compat §4)"`

### The §9 addendum (append to the compat spec, in this task)

```markdown
**§9 addendum (compat phase 2, 2026-07-13) — MEASURED by RUNNING gc's compiler
(`ci/gc-compat/factshim.go`) and camp's own rule set over the corpus at `GCPACKS_REF`.
It CORRECTS this section.**

- **The ceiling is 95, not 97–98.** Beyond `phase = "vapor"` (2) and the scope-check formula (1), two
  more cannot load: `gascity/formulas/same-session-implement.formula.toml` (an **UNCONDITIONAL**
  `context = "shared"` drain — §9 assumes all 13 shared drains sit behind
  `{{drain_policy}} == same-session`; **12 do**), and `gastown/formulas/mol-polecat-work.toml`
  (`extends → mol-polecat-base`, absent from the corpus — **gc fails it too**; gc compiles 99/100).
  The scope-check formula's scope-ness lives entirely in step-metadata VALUES (`gc.kind = "scope"`,
  `gc.scope_*`) — **there is no `gc.scope_kind` key in the corpus.**
- **Per-rung LOADABLE counts:** 2a **2** · 2b **31** · 2c **49** · 2d **76** · 2e **95** — computed
  over the **extends-MERGED** step tree. Eight formulas inherit a late-rung key only from a parent
  (7 inherit `drain`, 1 inherits `expand`/`expand_vars`); gc corroborates — 12 authored separate drain
  steps compile to 19.
- **RUNNABLE = 62**, pinned separately. "Corpus loading" means **compiles**, not **runnable**: the 21
  no-contract and 14 `type = "expansion"` formulas compile and are refused at **run** time by all
  three cook entry points (`camp sling`, the order-fire path, the drain's item cook).
- **Three camp-local rules were refusing the corpus and are amended:** the file-stem rule strips an
  optional trailing `.formula` (92/100); `type = "expansion"` formulas declare `template`, not `steps`
  (14/100); and the compiler-declaration rule is satisfied by `contract = "graph.v2"` (master spec
  line 449, amended in the same change).
- **§4's permissiveness rule is scoped BY ORIGIN:** unrecognised keys are ignored+warned in imported
  pack layers and are a **hard error** in camp's own `<root>/formulas/`.

- **⚠️ §9's SUBSTITUTION-ASYMMETRY BULLET IS WRONG, and is replaced.** Measured in gc's compiled
  output: **`{{var}}` is NOT substituted at compile at all** — 567 residual Descriptions, 55 residual
  `gc.run_target` routes, 1 residual Title, **even where the var has a default**. Substitution happens
  at **instantiation** (`stepToBead`), over **every field and every metadata value, with NO exemption
  list** (`molecule.go:1035-1037`) — **including `check.path`** (→ `gc.check_path`, `ralph.go:76`)
  **and `drain.formula`** (→ `gc.drain_formula`, `compile.go:590`). A templated `drain.formula` is
  blocked **separately, by a validation reject** (`graphv2_validation.go:417-419`), not by
  substitution scoping.
  **AND gc has a SECOND grammar §9 never mentions:** single-brace **`{name}`** (`range.go:32`, applied
  inside `expandStep`, `expand.go:255`) is **FULLY RESOLVED AT COMPILE** — 435 corpus occurrences, of
  which 362 are the fixed `{target}` family and the rest are general vars **including 8 `gc.run_target`
  routes**. So §2's *"0 bare route values, corpus-wide"* is also wrong: **8 route sites are
  single-brace and resolve at compile.** Camp reproduces both stages exactly.
- **⚠️ §9's DRAIN RUNTIME BULLET IS WRONG, and is replaced.** *"`item.single_lane` — camp honours it
  mechanically: the drain's ready items enter dispatch with concurrency 1"* is a source-read mistake.
  Measured: **`single_lane` has ZERO production readers in gc** (`types.go:371` — *"reserved for future
  shared drains"*; its only readers are the compiler that writes it and the validator), and
  **`on_item_failure` is read ONLY by `advanceSharedDrain`** (`drain.go:467`), so **for
  `context = "separate"` gc is ALWAYS effectively `continue`**. gc's separate drain is **EAGER and
  ALL-OR-NOTHING**: `reserveDrainMembers` takes the **whole member set** before the materialize loop
  (`drain.go:113-118`, `:1212-1219`); a conflict closes the drain with **nothing materialized**. Camp
  matches gc: `single_lane` and `on_item_failure` are **parsed, validated and round-tripped into
  `gc.drain_*` metadata with no runtime behavior behind them.** Camp also honours gc's **runtime cap**:
  `max_units` defaults to **100** and a drain whose member set exceeds it **fails**
  (`drain.go:24`, `:244-255`, reason `limit_exceeded`).
- **The metadata key `gc.continuation_group` (29 authored uses) is distinct from the `drain.` key
  (0 uses).** The former is **accepted and carried verbatim**; camp does not honour it (§11.4).
  `gc.build.artifact_schema` / `gc.build.artifact_path_keys` (74/74) and `gc.on_fail` (1) likewise —
  **accepted fidelity costs**, named.
- **A run's pinned artifact is the COMPILED recipe** (`runs/<id>/recipe.json`), beside the authored
  source (`<formula>.toml`, kept verbatim for audit). campd reloads the recipe by deserialization; it
  never re-parses the authored file, which for an imported formula could not resolve its layers.
- **gc expands check/retry loops at COMPILE** into namespaced `.iteration.N` steps (1523 compiled steps
  for 99 formulas); **camp keeps them as RUNTIME loops.** A full step-list diff against gc is therefore
  structurally impossible; `ci/gc-compat/differential.py` scopes to what is comparable.
```

---

## Task 3: Bead metadata — the store, the refold wiring, the schema bump

**Files:** `ledger/{schema,fold,refold,mod}.rs` · `readiness.rs` · `main.rs` + `cmd/create.rs` ·
`tests/refold_prop.rs`

```rust
// readiness.rs
pub fn bead_metadata(conn: &Connection, bead: &str) -> Result<BTreeMap<String, String>, CoreError>;
/// gc's key VERBATIM (beadmeta/keys.go:93; invariant 7). Value = the reserving drain's anchor id.
pub const EXCLUSIVE_DRAIN_RESERVATION: &str = "gc.exclusive_drain_reservation";
/// Keys with a DEDICATED COLUMN: PROJECTED at read, REFUSED at write, naming the column —
/// so compat-3 (§6.1) inherits ONE source of truth, not two.
pub const PROJECTED_METADATA: &[(&str, &str)] =
    &[("gc.routed_to", "assignee"), ("gc.work_branch", "work_branch")];
```
`bead.created` gains `metadata: BTreeMap<String,String>` (default `{}`); `bead.updated` gains
`metadata: BTreeMap<String, Option<String>>` (null = unset), and its emptiness check becomes "title
and/or description **and/or metadata**".

**The CAS lives in the fold** *(ratified twice)*: `fold::apply` already makes state-dependent
acceptance decisions (fold.rs:234-236); `append` is one transaction that rolls back on `Err`
(ledger/mod.rs:982 — *"rejections appended nothing"*); `build_shadow` (refold.rs:110-120) replays the
**accepted** log through the **same** `fold::apply`. The CAS is therefore a pure function of the
accepted prefix. A read-then-append would be a real TOCTOU race.

- [ ] **Step 1: Failing tests.** `bead_created_carries_metadata_and_bead_updated_sets_and_unsets_it` ·
  `a_second_drain_cannot_reserve_a_held_member` (conflict names the holder; same-holder re-reserve is
  idempotent; release-then-retake works) ·
  `a_metadata_key_with_a_dedicated_column_is_projected_at_read_and_refused_at_write` ·
  `bead_updated_still_requires_at_least_one_field`.
  **`refold_prop.rs`: `Op` gains `SetMeta` / `Reserve` / `Release`** — `Reserve` **deliberately
  generates conflicts** (a rejected append must append nothing; the replay must reach an identical
  state) — **and `DUMPS` gains `("bead_meta", "bead_id, key, value")`.** Without the new ops the
  property is **vacuous** (the PR #79 class): no op emits metadata, both ledgers dump zero rows, and
  it passes while exercising nothing.
- [ ] **Step 2: Run; watch fail.**
- [ ] **Step 3: Schema + BOTH version sites (BD9).** In `STATE_DDL`, **after `beads`**:
```sql
CREATE TABLE bead_meta (
  bead_id TEXT NOT NULL REFERENCES beads(id),
  key     TEXT NOT NULL,
  value   TEXT NOT NULL,
  PRIMARY KEY (bead_id, key)
) STRICT;
CREATE INDEX bead_meta_key ON bead_meta(key, value);
```
  **`SCHEMA_VERSION` lives in TWO places and rev 2 bumped ONE:** `schema.rs:14`
  (`pub const SCHEMA_VERSION: i64 = 2;`) **and `schema.rs:78`**, inside `FULL_DDL_PREFIX`:
  `INSERT INTO meta (key, value) VALUES ('schema_version', '2');`. `init_schema` writes the **literal**
  and returns early without verifying; every later open calls `verify_schema_version`, which compares
  the **const**. With one bumped, **every freshly-initialized camp writes `'2'` and then fails to open
  on its very next process** (`UnsupportedSchema { found: 2, supported: 3 }`). **Bump both, and the
  module doc at `schema.rs:1`** (*"Schema v2 for camp.db"*). Test: `a_fresh_camp_reopens` (init, drop,
  re-open).
- [ ] **Step 4: The fold.** Projected-key refusal first; then `None ⇒ DELETE`; `Some ⇒` the reservation
  CAS (a different holder ⇒ `InvalidEventData` naming it) then upsert. `bead_metadata` reads
  `bead_meta` **and overlays** the projections from `beads.assignee` / `beads.work_branch`.
- [ ] **Step 5: Refold — the PRODUCTION constant.** `refold.rs::STATE_TABLES` (:28-60) is the real
  list; `diff_all` (:166-185) and `replace_state_from_shadow` (:142-163) iterate **only** it. Add
  **after `beads`** (so `.iter().rev()` deletes the child first and the FK holds — `foreign_keys = ON`,
  schema.rs:126):
```rust
TableSpec { name: "bead_meta", cols: "bead_id, key, value", key: "bead_id || '/' || key" },
```
  Without it, `--refold` never diffs a reservation and **`--repair` hard-fails** on the FK.
- [ ] **Step 6: `camp create --run` + `ready_task_count`.** `#[arg(long)] run: Option<String>` on
  `Create`, threaded into `bead.created`'s `run_id` (already folded); **fail fast** on an unknown run.
  `readiness.rs::ready_task_count` (:160) lacks the run-root exclusion `dispatchable_beads` has
  (**:141**), so every member would be "ready" forever and never dispatched. Add
  `AND NOT (b.run_id IS NOT NULL AND b.step_id IS NULL)`. **This changes `camp top`'s ready count for
  every existing run — say so in the PR body**; merged tests asserting a ready count after `camp sling`
  will move.
- [ ] **Step 7–8: Run; pass; gates; commit** —
  `"feat(ledger): bead metadata — refold-wired, schema 3, exclusive-reservation CAS"`

---

## Task 4: Rung 2a — the layered compiler, the pinned recipe (BD8), `description_file`, the gate

**Files:** create `formula/layers.rs`, `formula/compose.rs`, `tests/compose.rs`,
`tests/fixtures/compose/**`, `camp/tests/cli_doctor_corpus.rs`, `ci/gc-compat/formula_gate.py` ·
modify `formula/{mod,ast,parse,cook,runtime}.rs`, `orders/mod.rs`, `cmd/{doctor,sling}.rs`, `main.rs`,
`daemon/orders.rs`, `camp/tests/{cli_doctor_formula,daemon_orders}.rs`, `ci.yml`

```rust
// layers.rs
pub struct FormulaLayers { layers: Vec<Layer> }   // Layer { binding, dir, pack_root, origin }
impl FormulaLayers {
    pub fn from_config(cfg: &CampConfig, root: &Path) -> Result<Self, CoreError>;
    pub fn local_only(path: &Path) -> Self;
    pub fn origin_of(&self, path: &Path) -> Origin;                        // D2′
    pub fn formula_path(&self, name: &str) -> Result<PathBuf, CoreError>;  // DELEGATES to orders::resolve_formula
    pub fn asset_path(&self, raw: &str, base_dir: &Path) -> Result<PathBuf, CoreError>;
}

// compose.rs
pub struct Compiled {
    pub formula: Formula,              // Serialize + Deserialize (D6)
    pub ignored_keys: Vec<String>,
    pub refusals: Vec<Refusal>,        // SURVIVING refusals only (BD2)
    pub not_runnable: Option<Refusal>, // D1
}
pub fn compile(layers: &FormulaLayers, cfg: &CampConfig, path: &Path,
               vars_override: &BTreeMap<String, String>) -> Result<Compiled, FormulaError>;
pub fn compile_named(layers: &FormulaLayers, cfg: &CampConfig, name: &str,
                     vars_override: &BTreeMap<String, String>) -> Result<Compiled, FormulaError>;
```
`vars_override` exists because **gc's `Compile` takes vars** and conditions + `{name}` resolve at
compile: a sling-time `--var drain_policy=same-session` must change what is pruned. (`camp sling` has
no `--var` today; the parameter is threaded now and passed empty, so compat-3/4 can add the flag
without re-plumbing.)

### The pipeline — gc's real staging (D5)

```
1. parse::walk(text, origin)                                    Tasks 1+2
2. extends: merge (deepest ancestor first; parents' steps APPEND;
   a matching child id REPLACES IN PLACE, position preserved)   Task 6 — 2c
3. expansion: type/template/expand/expand_vars/children,
   + the {target} family, + single-brace {name} RESOLUTION      Task 7 — 2d   <- F4
4. description_file: inline, or gc's >4096 pointer prompt       THIS TASK — 2a
5. condition: evaluate over merged var VALUES; PRUNE the step
   with its children AND ITS REFUSALS (BD2); drop dangling
   `needs`. Recurses into `children` AND `template`.            Task 5 — 2b
6. validate (S1..S18) + collect SURVIVING refusals + runnability THIS TASK
```
**`{{var}}` is NOT substituted here — it is substituted in `cook` (Task 5).** Rev 2 substituted at
compile and had no single-brace grammar at all.

In this task, stages 2/3/5 are identity stubs and `validate` hard-fails any formula whose merged key
set touches `keys::UNIMPLEMENTED` — which is what makes the 2a count really **2**.

### `description_file` — measured

- Contents **replace** `step.description` at parse time; the key is consumed (`parser.go:808`).
- **`../assets/<rel>`** resolves **through the layers**, highest wins (`winningAssetPath`,
  `parser.go:855-873`; `searchPaths` is lowest→highest and the **last** match wins). Anything else
  resolves against the formula file's own directory.
- **>4096 bytes ⇒ gc's pointer prompt** (`descriptionFileInlineMaxBytes = 4*1024`, `parser.go:27`;
  `descriptionFileReferenceDescription`, `:977`). **Reproduce it byte-for-byte** — Task 11 diffs its
  sha256 against gc's, because a mis-transcribed paragraph is a divergence no camp test can see. Its
  `## Formula Variables` block emits `name="{{name}}"` lines **deliberately**: they resolve at **cook**,
  which is exactly what D5 now does.
- **All 328 targets resolve; 8 exceed 4096 bytes.** An unresolved `description_file` in a `graph.v2`
  formula is a **hard error** (`parser.go:186`, `:1007`).
- **Containment (security).** gc's non-asset branch is a bare `base_dir.join(raw)`. Camp imports
  **arbitrary third-party packs**, so a pack could set `description_file = "../../../../.ssh/id_rsa"`
  and have it inlined into a bead description a tool-enabled worker reads. Camp canonicalises and
  **refuses any path outside the pack root**. **The containment root is the WINNING LAYER's pack root,
  not the declaring formula's** — 32 cross-pack `extends` edges inherit a step whose asset lives in the
  **parent's** pack, so anchoring on the declaring formula would refuse `bmad-build` inheriting
  `gascity`'s `../assets/implement.md` as an "escape". Test:
  `an_inherited_asset_in_the_parents_pack_resolves_and_is_not_an_escape`.

### Routing (§4 trap 3) — and where it now happens

**327 `gc.run_target` occurrences; ZERO step `assignee`.** Routing is *entirely* step metadata.
- **At compile:** the value is `{name}`-resolved (stage 3) and carried **verbatim**. It is **NOT**
  `{{}}`-substituted and **NOT** binding-resolved here — 55 corpus routes are still
  `{{implementation_target}}` at this point, exactly as in gc's Recipe.
- **At cook:** `{{var}}` is substituted (Task 5), the value is split at the first dot, and the binding
  is resolved via compat-1's **`pack::resolve_agent(cfg, name)`** (pack.rs:251 — it already emits
  `camp import add <source> --name <binding>`; **do not write a second resolver**). The result is
  written to the bead's `assignee`. An unbound binding is a **hard cook error naming the remedy**.

- [ ] **Step 1: Failing tests.** `tests/compose.rs` (two-layer fixture: a `child` pack whose
  `pack.toml` declares `[imports.gc] source = "../parent"`):
  `description_file_contents_replace_the_step_description` ·
  `an_asset_reference_resolves_through_the_layers_highest_wins` ·
  `an_inherited_asset_in_the_parents_pack_resolves_and_is_not_an_escape` ·
  `an_oversize_description_file_becomes_gcs_pointer_prompt` (exact first line; the
  `- Prompt file size: 5000 bytes` line; and that the `{{var}}` lines **survive compile**) ·
  `a_missing_description_file_is_a_hard_error_for_a_graph_v2_formula` ·
  `a_description_file_escaping_the_pack_root_is_refused` ·
  **`a_run_target_is_carried_verbatim_and_NOT_substituted_at_compile`** (F1:
  `assert_eq!(step.metadata["gc.run_target"], "{{implementation_target}}")`) ·
  `a_no_contract_formula_compiles_and_is_not_runnable` · `phase_is_refused_by_name` ·
  `a_scope_check_formula_is_refused_by_its_metadata` (key `gc.kind`).
  **BD8 tests** (`tests/cook.rs`): `cook_pins_the_compiled_recipe_and_the_authored_source` ·
  `cook_pins_a_recipe_whose_step_ids_are_exactly_the_manifest_steps` ·
  **`load_run_reconstitutes_a_run_cooked_from_an_IMPORTED_formula_with_extends_and_description_file`**
  — *the test that would have caught the phase-killer*: cook from the two-layer fixture, then call
  `flow::load_run` and assert `Ok`, with `ctx.formula.steps[..].drain` / `.metadata` / `.assignee`
  surviving.
  `cli_doctor_corpus.rs`: the `--json` contract; `doctor_formula_json_exits_zero_even_when_refused`.
  `daemon_orders.rs`: `a_due_order_naming_a_no_contract_formula_fires_nothing_and_events_the_refusal`
  (§13's money invariant).
- [ ] **Step 2: Run; watch fail.**
- [ ] **Step 3: Implement `layers.rs` + `compose.rs`.**
- [ ] **Step 4: BD8 — the pinned recipe.** Derive `Serialize`/`Deserialize`; `cook` writes
  `recipe.json` beside `<formula>.toml`; rewrite `runtime.rs:67-69`'s `load_run` to **deserialize
  `recipe.json`**, deleting its `parse_and_validate` call; amend `ast.rs:15`'s doc comment.
- [ ] **Step 5: CLI.** `Doctor` gains `--json` and `--formula-rungs` (into the existing required
  `ArgGroup("mode")` — `cli_doctor_formula.rs` asserts that group and must be updated). `run_formula`
  prints `{path, formula, ok, runnable, ignored_keys, refusals, not_runnable}` and exits **0 even when
  `ok` is false** in `--json` mode (human mode keeps 0/1). **`--formula-rungs --json` (BD10) takes no
  formula path and emits exactly:**
```jsonc
{ "base":       { "top": ["description","formula","requires","steps"], "step": ["assignee", …] },
  "dead":       { "top": ["internal","mode", …], "step": [] },
  "annotation": { "top": ["catalog","metadata","notes"], "step": ["notes","priority","tags"] },
  "refused":    { "top": ["advice","compose","pointcuts","pour"], "step": ["depends_on", …] },
  "rungs":      [ { "id": "2a", "top": ["contract"], "step": ["description_file","metadata"] }, … ] }
```
- [ ] **Step 6: `cook.rs`** writes `"metadata": step.metadata` on the step bead; `assignee` comes from
  the route resolution Task 5 adds at cook.
- [ ] **Step 7: The order-fire refusal** in `daemon/orders.rs`.
- [ ] **Step 8: `ci/gc-compat/formula_gate.py`** — the §10 gate, driving the **real binary**. Setup is
  `load_corpus_packs.py`'s mold verbatim: `camp init --no-service --no-import`; append
  `[agent_defaults] tools = ["Read","Bash","Skill"]`; `camp import add <corpus>/<pack> --name <pack>`
  for the **10 formula-bearing packs** (bmad, compound-engineering, contributing, discord, gascity,
  gastown, github, gstack, pr-pipeline, superpowers) + `camp import add <corpus>/gascity/roles --name gc`.
  *(Measured: no two of the 100 share a basename ⇒ no within-tier collision.)*
```python
CEILING = 95; RUNNABLE = 62
RUNG_COUNTS = {"2a": 2, "2b": 31, "2c": 49, "2d": 76, "2e": 95}
NOT_LOADABLE = {  # basename -> a key the refusal MUST name
    "mol-digest-generate.toml": "phase",  "mol-pr-from-issue.formula.toml": "phase",
    "design-review.formula.toml": "gc.kind",            # NOT gc.scope_kind — that key does not exist
    "same-session-implement.formula.toml": "context",   # an UNCONDITIONAL shared drain
    "mol-polecat-work.toml": "extends",                 # gc fails this one too
}
```
  **Three assertions.** (1) `camp doctor --formula <path> --json` over all 100: exactly `CEILING`
  compile; the five refuse naming those keys. (2) exactly `RUNNABLE` report `runnable: true`.
  (3) **The falsifiable cross-check (BD10):** the **SET of basenames camp actually loaded** must equal
  the **SET `rungs.py` predicts loadable at 2e**. *(Rev 2's version compared counts the gate would have
  had to recompute from camp's key table — reproducing `rungs.py` by construction, so it could not
  fail. Comparing the two **sets** — one from the real binary, one from the arbiter — is a real check:
  a tuned key table changes camp's set and the comparison breaks.)*
- [ ] **Step 9: Run the gate** — `--expect-loaded 2` at this point (rungs 2b–2e are `UNIMPLEMENTED`
  hard violations). **That is the correct failing signal: the gate is the TDD driver for Tasks 5–8.**
- [ ] **Step 10: CI** — one step appended to the **existing** `gcpacks-compat` job:
```yaml
      - name: phase-2 formula gate (rungs, the ceiling, RUNNABLE)
        run: python3 ci/gc-compat/formula_gate.py gcpacks-src target/debug/camp
```
- [ ] **Step 11: Gates; commit** —
  `"feat(formula): rung 2a — layered compiler, the pinned recipe, description_file, the §10 gate"`

**What this task's fixes could newly break:** `recipe.json` is a **run-dir schema change**. A campd
started against a run cooked by an *older* camp finds none. `load_run` must **fail loudly** —
`Corrupt("run <id> has no recipe.json — cooked by an older camp; re-sling it")` — and **never** fall
back to the old re-parse. Test: `load_run_on_a_pre_recipe_run_dir_fails_loudly`.

---

## Task 5: Rung 2b — `vars`, `condition` pruning, and `{{var}}` substitution AT COOK

**Files:** `compose.rs` (stage 5; unit tests **inside** the module — `pub(crate)` fns) · **`cook.rs`**
(substitution + route resolution) · `parse.rs`, `ast.rs`, `validate.rs`, `tests/compose.rs`,
`tests/cook.rs`

```rust
// compose.rs — COMPILE
/// §9: `==` and `!=` only; LHS a single `{{var}}`. False ⇒ the step is PRUNED WITH
/// ITS CHILDREN and ITS REFUSALS (BD2); dangling `needs` are dropped. Evaluated over
/// merged var VALUES — never by text substitution.
pub(crate) fn eval_condition(expr: &str, vars: &BTreeMap<String, String>) -> Result<bool, Violation>;

// cook.rs — INSTANTIATION (gc: stepToBead)
/// gc's `Substitute` (parser.go:617); varPattern `\{\{([a-zA-Z_][a-zA-Z0-9_]*)\}\}`
/// (parser.go:557). Applied to EVERY field and EVERY metadata value, with NO exemption
/// list (molecule.go:1035-1037) — INCLUDING `check.path` and `drain.formula` (F8).
/// An unknown token is LEFT VERBATIM.
pub(crate) fn substitute_vars(text: &str, vars: &BTreeMap<String, String>) -> String;
```

**Measured: 4 distinct conditions, 29 uses** — `{{drain_policy}} == separate` (12), `== same-session`
(12), **`{{review_mode}} != report` (4 — inside `children`, on the `template` tree)**,
`{{pr_mode}} != none` (1). The RHS is an **unquoted bare word**; trim, and accept a quoted RHS too.
**Pruning must recurse into `children` AND `template`** — rev 2 said `children` only, and all four
`review_mode` conditions live at `template/children`. `review_mode`'s default **varies by pack**
(`report` in `code-review-base`/`review`/`planning-base`, `agent` in `build-base`, `interactive` in
`gstack-build`), so the merged chain decides.

`[vars]`: a bare string **or** a table with `default`; **no default ⇒ undefined**, and the placeholder
survives. Merge = **parent defaults first, child overrides win** (Task 6's stage). Load-bearing:
`drain_policy = "separate"` is declared in gascity's `build-base`, not in the children.

**The residual check is title-only** (§9) and now runs **at cook**, after substitution.

**§9's asymmetry list is DELETED** (F8): no exemption for `check.path` or `drain.formula`. Instead,
**validation rejects a templated `drain.formula`** — `if formula.contains("{{")` → *"templated item
formula names are not supported"* (gc `graphv2_validation.go:417-419`).

- [ ] **Step 1: Failing tests**
```rust
// compose.rs (compile stage)
#[test] fn a_false_condition_prunes_the_step_its_children_AND_its_refusals() { }   // BD2
#[test] fn condition_pruning_recurses_into_children_and_template() { }             // the 4 review_mode uses
#[test] fn vars_merge_parent_defaults_under_child_overrides() { }
#[test] fn a_condition_outside_the_subset_is_a_violation_naming_the_step() { }
#[test] fn a_templated_drain_formula_is_rejected_at_validation() {                 // F8 — gc's own rule
    assert!(err.to_string().contains("templated item formula"), "{err}");
}
#[test] fn compile_does_NOT_substitute_double_brace_vars_anywhere() {              // F1
    let c = compile_named(&layers, &cfg, "b", &no_overrides).unwrap();
    assert_eq!(c.formula.steps[0].metadata["gc.run_target"], "{{implementation_target}}");
    assert!(c.formula.steps[0].description.as_ref().unwrap().contains("{{"));
}

// cook.rs (instantiation stage)
#[test] fn cook_substitutes_double_brace_vars_over_every_field_INCLUDING_check_path() {   // F8
    // §9 claimed check.path is exempt. gc substitutes it (→ gc.check_path, ralph.go:76).
    assert_eq!(bead_check_path, ".gc/scripts/checks/build.sh");  // authored ".gc/scripts/checks/{{kind}}.sh"
}
#[test] fn cook_substitutes_every_metadata_value_with_no_exemption_list() { }
#[test] fn an_undefined_var_keeps_its_literal_placeholder_and_only_title_is_residual_checked() { }
#[test] fn cook_resolves_the_route_through_the_binding_namespace_into_assignee() {
    assert_eq!(bead.assignee.as_deref(), Some("superpowers.implementer"));
}
#[test] fn cook_fails_loudly_when_a_route_names_an_unbound_binding() {
    assert!(err.to_string().contains("camp import add"), "{err}");
}
```
- [ ] **Steps 2–3: Watch fail; implement.** Remove `vars`/`condition` from `UNIMPLEMENTED`.
  `substitute_vars` is a **single left-to-right pass**. **Do NOT merge it with `cook::substitute`
  (cook.rs:51)** — that one is `{name}` over `CookOptions.vars` for bond children: a different grammar
  with a different scope. **Three substitution functions, three grammars, three stages — name them and
  keep them apart:** `compose::resolve_single_brace` (Task 7), `cook::substitute_vars` (this task),
  and the existing `cook::substitute`.
- [ ] **Step 4: Gate** — `--expect-loaded 31`
- [ ] **Step 5: Gates; commit** —
  `"feat(formula): rung 2b — vars, condition pruning, and {{var}} substitution at cook (31/100)"`

**What this fix could newly break:** substituting `check.path` at cook means a check-script path can
now contain a var — while `trust_exec` (compat-1) inventories `check.path` **at import**, before
substitution. **Substitution must never turn an untrusted path into a trusted one.** Test:
`a_substituted_check_path_still_requires_trust_exec_and_the_inventory_reports_the_AUTHORED_path`.

---

## Task 6: Rung 2c — `extends`

§9: *child seeds scalars; parents' steps **append**; a child step whose `id` matches a parent's
**replaces it whole, in place, preserving position**. No field-level merge. Parents resolve by bare
name through the layers.*

Measured: **48 formulas extend**; every resolvable parent lives in `gascity/formulas/`; **none extends
more than one parent** (implement the list anyway — gc's shape — left-to-right); **`mol-polecat-work`'s
parent is absent ⇒ a hard error, and gc fails it too.**

- [ ] Tests: `a_parents_steps_append_and_a_matching_child_id_replaces_in_place` (position preserved;
  **`assert_eq!(b.description, None)` — replaced WHOLE, no field-level merge**) ·
  `the_child_seeds_scalars_and_inherits_the_parents_vars` (`drain_policy == "separate"`) ·
  `a_parent_resolves_by_bare_name_through_the_TRANSITIVE_layer` ·
  `an_unresolvable_parent_is_a_hard_error_naming_it` (`mol-polecat-base`) ·
  `an_extends_cycle_is_a_hard_error_never_a_stack_overflow` ·
  **`a_refusal_on_a_parent_step_that_the_child_replaces_is_discarded`** (BD2's new failure) ·
  **`a_formula_that_inherits_drain_ONLY_from_its_parent_is_blocked_until_rung_2e`** (BD1 — the seven
  `build-from-*` formulas; this is what moves 2c from 57 to **49**).
- [ ] Implement: depth-first with a visited-stack cycle guard; merge **deepest ancestor first**. Remove
  `extends` from `UNIMPLEMENTED`.
- [ ] **Gate — `--expect-loaded 49`** (not 57 — BD1).
- [ ] Commit — `"feat(formula): rung 2c — extends, append and replace-in-place (49/100)"`

---

## Task 7: Rung 2d — expansion, and the SINGLE-BRACE grammar (F4)

§9: *`type = "expansion"` — not directly runnable; it supplies `template` steps for `expand`.*
Measured: **14** formulas are `type = "expansion"` with a top-level `template` (and **none has
`steps`** — S3); **15** steps carry `expand`; **14** carry `expand_vars`; **`children` appears 16 times
across 15 formulas** (rev 2 said "2" — it counted only the `steps` tree; **14 are on `template`**).

gc (`expand.go`): an `expand` rule names a **target step**; the target is **REPLACED** by the expansion
formula's `template` steps, with the expansion's own `[vars]` merged under the rule's overrides
(`ApplyExpansionsWithVars` / `mergeVars` / `resolveOverrideVars`). **`DefaultMaxExpansionDepth = 5`** —
exceeding it is a **hard error**, never a truncation.

### The single-brace grammar (F4) — rev 2 had none, and 8 routes corrupt without it

Inside `expandStep` (`expand.go:255`), gc applies, in order:
1. **`substituteTargetPlaceholders`** (`expand.go:446-464`) — a plain `strings.ReplaceAll` over a
   **fixed 4-token vocabulary**: `{target}`, `{target.id}`, `{target.title}`, `{target.description}`.
   **362 of the 435 corpus single-brace occurrences are this family.** It is **not** the var grammar.
2. **`substituteVars`** (`range.go:94`, `rangeVarPattern = \{(\w+)\}`) — the general single-brace var
   grammar, over ID, Title, Description, Notes, Assignee, Expand, Timeout, Labels[], Needs[],
   **Metadata[]**, ExpandVars[], Gate.*, Loop.*, OnComplete.*, Ralph.Check.* (`expand.go:265-342`) —
   **but NOT `DescriptionFile`**. An unknown token is **left verbatim** (`range.go:103`).

**Proof it is load-bearing:** `superpowers-code-review.formula.toml:63` authors
`metadata = { "gc.run_target" = "{implementation_target}" }`, and gc's compiled Recipe carries
`gc.run_target = "superpowers.implementer"` — **resolved**. All 8 single-brace routes live in
`children.metadata.gc.run_target`. Get the stages backwards and **55 routes silently corrupt.**

```rust
/// gc's compile-stage grammar. The {target} family first (a fixed vocabulary), then
/// `\{(\w+)\}` against the merged vars. Unknown tokens are LEFT VERBATIM. Never
/// touches `description_file`. gc: expand.go:255, :446-464; range.go:94.
/// APPLIED ONLY INSIDE EXPANSION — never as a global pass. See the warning below.
pub(crate) fn resolve_single_brace(text: &str, target: Option<&Step>,
                                   vars: &BTreeMap<String, String>) -> String;
```

- [ ] Tests: `an_expansion_formula_compiles_and_is_not_runnable` (key `type`) ·
  `expand_replaces_the_target_step_with_the_expansion_formulas_template` ·
  **`a_single_brace_var_in_step_metadata_resolves_AT_COMPILE`**
  (`assert_eq!(md["gc.run_target"], "superpowers.implementer")`) ·
  **`the_target_family_is_a_fixed_vocabulary_not_the_var_grammar`** (`{target.title}` resolves with no
  such var; `{target.bogus}` is left verbatim) ·
  **`an_unknown_single_brace_token_is_left_verbatim`** (`{GC_PACK_DIR}` in prose survives) ·
  **`a_single_brace_token_in_description_file_is_NOT_resolved`** ·
  `children_are_flattened_preserving_position` · `expansion_deeper_than_five_is_a_hard_error` ·
  `an_expand_target_that_does_not_exist_is_a_hard_error`.
- [ ] Implement; remove `type`/`template`/`expand`/`expand_vars`/`children` from `UNIMPLEMENTED`.
- [ ] **Gate — `--expect-loaded 76`** (not 83 — BD1).
- [ ] Commit — `"feat(formula): rung 2d — expansion, and gc's compile-stage {name} grammar (76/100)"`

**⚠️ What this fix could newly break — the single highest-risk line in the phase.** The regex
`\{(\w+)\}` **matches `{x}` inside `{{x}}`** (at offset 1). A naive global single-brace pass would
therefore **corrupt every one of the 55 surviving `{{}}` routes and 567 `{{}}` descriptions**. The
reason gc does not corrupt them is that `substituteVars` **runs only inside `expandStep`**, on
expansion templates — never over the whole formula. **Camp must do the same: `resolve_single_brace` is
called ONLY from the expansion stage.** Pin it:
```rust
#[test]
fn resolving_single_brace_leaves_double_brace_untouched() {
    let vars = BTreeMap::from([("x".into(), "RESOLVED".into())]);
    assert_eq!(resolve_single_brace("{{x}}", None, &vars), "{{x}}");   // byte-identical
    assert_eq!(resolve_single_brace("{x}",   None, &vars), "RESOLVED");
}
#[test]
fn a_double_brace_route_outside_an_expansion_survives_compile_byte_for_byte() { /* the corpus case */ }
```

---

## Task 8: Rung 2e (compile side) — `drain`

**Files:** create `formula/drain.rs` · `parse.rs` (`walk_drain`, on `walk_on_complete`'s mold,
parse.rs:460), `ast.rs`, `keys.rs`, `validate.rs` (S14–S17), `tests/compose.rs`

```rust
/// gc's DrainSpec (types.go:341), restricted to what camp implements.
/// F2: gc's compiled Recipe has NO Drain struct — this becomes `gc.drain_*` METADATA
/// on the step bead, which is where gc keeps it.
pub struct Drain {
    pub context: DrainContext,          // always Separate — Shared is REFUSED
    pub formula: String,                // rejected at validation if it contains "{{"  (gc's rule)
    pub member_access: MemberAccess,    // default Read              (compile.go:590-598)
    pub on_item_failure: OnItemFailure, // default Continue (separate) — PARSED, NOT ACTED ON (F6)
    pub item: DrainItem,                // single_lane                — PARSED, NOT ACTED ON (F5)
}
pub enum DrainContext  { Separate }
pub enum MemberAccess  { Read, Exclusive }          // "read" | "exclusive"
pub enum OnItemFailure { Continue, SkipRemaining }  // "continue" | "skip_remaining"
pub struct DrainItem   { pub single_lane: bool }
```
**gc's compiler defaulting** (`ApplyDrainControlMetadata`, `compile.go:584-614` — §9 cites `:579-608`;
at `GASCITY_REF` it is **`:584-614`**): `member_access` → `"read"`; `on_item_failure` →
`"skip_remaining"` (shared) / **`"continue"`** (else); `single_lane` written only when true.
**Camp reproduces it exactly** — Task 11-B diffs the emitted `gc.drain_*` map against gc's.

**Refusals** (step-scoped — BD2): `context = "shared"`, `continuation_group`, `max_units`.

**S17 (new):** a `drain` step **must declare at least one `needs`** — *"a drain step must depend on the
step that creates its members"*. Without it the anchor is claimed at cook time, before any member
exists, scatters zero members and gathers `pass` immediately. Every corpus drain has `needs`.

- [ ] Tests: `drain_defaults_follow_gcs_compiler` (`Read`, `Continue`) ·
  `a_conditional_shared_drain_is_refused_naming_formula_step_and_drain_policy` ·
  **`the_corpus_build_formulas_compile_clean_because_the_shared_arm_IS_PRUNED`** — the load-bearing
  one (BD2): `bmad-build`/`gstack-build`/`compound-build` each carry **two** drain steps on mutually
  exclusive conditions, and the default `separate` prunes the shared one **and its refusal** before
  stage 6 collects. *(gc corroborates: 13 authored shared drains → **1** compiled.)* ·
  **`an_UNCONDITIONAL_shared_drain_is_refused_and_nothing_can_prune_it`** (`same-session-implement`) ·
  `setting_drain_policy_to_same_session_refuses_instead_of_approximating` (via `vars_override`) ·
  `continuation_group_and_max_units_are_refused_by_name` ·
  **`the_metadata_key_gc_continuation_group_is_ACCEPTED_and_carried`** (29 uses — distinct from the
  `drain.` key) · `a_templated_drain_formula_is_rejected_at_validation` (F8) ·
  **`a_drain_step_compiles_to_gcs_gc_drain_metadata`** (F2/F3 — assert the exact 5-key map) ·
  `a_drain_step_with_no_needs_is_a_violation` (S17).
- [ ] Implement. `walk_drain` keeps the **presence-not-parse-success** rule (`RawStep.has_drain`). Add
  `has_drain` to **S9**'s bans (`check`+`drain`, `retry`+`drain` — a drain step is campd's, not a
  worker's) and to **S11**'s `uses_graph_only`. **Remove `drain` from `UNIMPLEMENTED`, then DELETE
  `UNIMPLEMENTED` and its violation.**
- [ ] **Gate — the ceiling.** `python3 ci/gc-compat/formula_gate.py /tmp/gcpacks target/debug/camp` ⇒
  **95 loaded · 62 runnable · 5 refused by name**, every rung count matching, the set-vs-`rungs.py`
  cross-check green. **If it reports anything else, STOP and report to the lead.**
- [ ] Commit — `"feat(formula): rung 2e compile — drain (95/100 loadable, 62 runnable — the ceiling)"`

---

## Task 9: The drain runtime — gc's REAL semantics (RULING 4)

**ADDITIVE ONLY** in `dispatch.rs`; `event_loop.rs` untouched.

**Files:** `formula/runtime.rs`, `readiness.rs`, `ledger/mod.rs` · `daemon/dispatch.rs` (additive) ·
`cmd/doctor.rs` + `main.rs` (the operator escape) · `camp/tests/daemon_drain.rs`

### The lifecycle — campd-owned, and **BD3's fix**

gc: a drain *"materializes as a **controller-owned control bead**"* (types.go:318). campd **claims**
the anchor when ready → **scatters** → **gathers** → **closes** it.

```rust
// runtime.rs — beside is_looping (:94). NOT a rename.
pub fn is_campd_held(step: &Step) -> bool { is_looping(step) || step.drain.is_some() }
```

**BD3 — rev 2's "minimal and additive" one-line swap dispatched a real worker for every drain step.**
`maybe_claim_looping` (dispatch.rs:1891-1934) **does not end at the claim**:
```rust
Ledger::append_on(conn, now, EventInput { kind: EventType::BeadClaimed, /* campd */ … })?;
let step = step_ref.step.clone();
self.create_attempt(conn, now, &ctx, &step, &row, 1, None)?;   // <-- UNCONDITIONAL
```
`create_attempt` emits a `bead.created` with `run_id` + `step_id`, `type = task`, **open, no `needs`**
— **exactly the shape `dispatchable_beads` picks up.** So every drain step got a worker **plus** the
scatter (§13's money invariant, on the very path Task 4 protects); that phantom attempt's close then
fell through `on_attempt_closed`'s branches to `Ok(())` **silently**, closing the anchor early, so the
gather's `close_anchor` hit `InvalidTransition` — **B4, reintroduced through B4's own fix. And all four
of rev 2's tests still passed**, because they only checked *the anchor*, and the attempt is a
**different bead**.

**Fix** — in `maybe_claim_looping`:
```rust
if flow::is_looping(step_ref.step) {                 // attempts ARE the check/retry mechanism
    self.create_attempt(conn, now, &ctx, &step, &row, 1, None)?;
} else {                                             // a drain anchor: campd scatters, never attempts
    self.queue_drain(PendingDrain { … });
}
```
**and the tests must be able to see it:**
```rust
#[test]
fn a_drain_step_creates_NO_ATTEMPT_and_dispatches_NO_WORKER() {
    // BD3: rev 2's four tests all passed against a broken implementation because
    // they only checked THE ANCHOR. The attempt bead is a different bead id.
    c.settle();
    let anchor = c.step_bead(&run, "implement");
    assert!(flow::attempts(&c.conn(), &run, "implement", &anchor).unwrap().is_empty(),
            "a drain step has no attempts");
    assert!(!c.dispatchable().iter().any(|b| b.step_id.as_deref() == Some("implement")),
            "NOTHING carrying the drain step's step_id is dispatchable");
}
```
**B5 (verified sound, kept):** `flow::finalization` (runtime.rs:392) returns `NotQuiescent` on any
`in_progress` anchor, so the campd-held anchor blocks quiescence and every downstream `needs` stays
blocked until gather.

### Materialization — gc's REAL semantics (RULING 4; F5/F6/F7)

**The 4-cell matrix is KILLED, with both synthetic fixtures.** They built behavior gc does not have.

> **A separate drain is EAGER, ALL-MEMBERS, ALWAYS-`continue`, ALL-OR-NOTHING.**

1. Read the member set (D3 — `type = 'task'`, **`status <> 'closed'`**).
2. **If `len(members) > 100`** (gc's `defaultDrainMaxUnits`, `drain.go:24`): **close the anchor
   `fail`/`hard_fail`, reason `limit_exceeded`; materialize nothing.**
3. **Reserve EVERY member in ONE `append_batch`** (when `member_access = "exclusive"`).
4. **Then** cook one item root per member, in the same execution.

`single_lane` and `on_item_failure` are carried into `gc.drain_*` metadata and **never read** — exactly
as in gc.

### **BD4 — all-or-nothing, and why incremental was a correctness bug**

Rev 2 reserved member *i* **before** materializing item *i*, and on a conflict at *k+1* "released
1..k" — **while item-run 1 was already cooked and its workers dispatchable on m1.** m1 then carried
**no** reservation, so a second drain could reserve it and cook its own item run over it: **two drains
mutating one bead — the precise thing the reservation exists to prevent.** Rev 2's test asserted only
that the metadata key was gone; it never asserted that item-run 1 was not cooked.

**gc does not have this hole (F7):** `expandDrain` calls `reserveDrainMembers(store, bead, members,
opts)` for the **whole set** (`drain.go:113-118`, `:1212-1219`) **before** the materialize loop; a
conflict ⇒ `closeDrainReservationFailure` with **nothing materialized**.

**Camp adopts that shape.** One `append_batch` holds every reservation: a CAS rejection **rolls the
whole batch back for free** (ledger/mod.rs:982 — *"rejections appended nothing"*), so a partial
reservation state is **unrepresentable** and the compensating-release path **disappears**.

### **BD5 — a reserve conflict must CLOSE the anchor, or the run deadlocks forever**

Rev 2 emitted `dispatch.failed` and stopped. That **only appends an event**; the campd-held anchor
stays `in_progress` and `finalization` returns `NotQuiescent` **forever**. The reservation leak was
fixed and replaced with a **run leak**.

**On conflict, in one batch:** `dispatch.failed` (naming the member and the holding drain) **and the
anchor close** (`fail` / `hard_fail`). The run then finalizes `fail`, and the operator sees a closed,
named failure. Test: `a_reserve_conflict_closes_the_losing_anchor_and_the_run_FINALIZES`.

### Release paths — now short, because BD4 removed the partial-state arm

| exit | release |
|---|---|
| gather (all item roots closed) | release every member, **in the gather batch** |
| reserve conflict | **nothing to release** — the batch rolled back |
| `limit_exceeded` | nothing was reserved |
| run dead-ends (`dead_end_run`) | release every member held by any anchor of that run |
| **campd killed between the reserve batch and the cook** | **`reconcile` sweep**: a reservation naming an anchor that is **closed or absent** is an orphan ⇒ released |
| **operator escape** | **`camp doctor --drain-reservations [--release-orphans]`** — ships **here**, not compat-3 |

**No new event type.** The reservation rides `bead.updated`; failure uses `dispatch.failed` (the
fan-out mold, :2258). `no_reservation_vocabulary_exists` **forbids any event name containing
`"reserv"`**.

### Interfaces

```rust
// runtime.rs — pure, write-free
pub fn run_members(conn: &Connection, ctx: &RunContext) -> Result<Vec<BeadRow>, CoreError>;
pub fn drain_label(anchor: &str, index: usize) -> String;          // "drain:<anchor>:<i>"
pub fn parse_drain_label(label: &str) -> Option<(&str, usize)>;
pub fn drain_children(conn: &Connection, anchor: &str) -> Result<BTreeMap<usize, BeadRow>, CoreError>;
pub fn orphaned_reservations(conn: &Connection) -> Result<Vec<(String, String)>, CoreError>;
pub const DRAIN_MAX_UNITS: usize = 100;      // gc's defaultDrainMaxUnits (drain.go:24)

// dispatch.rs — beside PendingFanout (:1045)
#[derive(Debug, Clone, PartialEq)]
pub struct PendingDrain { pub run_id: String, pub step_id: String, pub anchor: String }
```
```sql
-- run_members. NOTE b.type='task' AND b.status<>'closed' (D3 — gc excludes closed members).
SELECT {BEAD_COLS} FROM beads b
 WHERE b.run_id = ?1 AND b.step_id IS NULL AND b.type = 'task' AND b.status <> 'closed'
   AND b.id <> ?2                                          -- ?2 = the run root
   AND b.labels NOT LIKE '%"bond:%' AND b.labels NOT LIKE '%"drain:%'
 ORDER BY (SELECT MIN(e.seq) FROM events e WHERE e.bead = b.id AND e.type = 'bead.created'), b.id
```
The `LIKE`s are a **prefilter**; re-parse labels Rust-side and drop decoys (the `bond_children` mold,
runtime.rs:514-549).

### The harness — **defined in full (BD11)**

`daemon_dispatch.rs` (the named mold) has **free functions** (`camp`, `camp_ok`, `scaffold`,
`wait_until`, `events_json`) and a `struct Daemon` with **one method and no accessors** — rev 2 wrote
`c.method()` against a composite type that does not exist. Define it in `daemon_drain.rs`:

```rust
struct Camp { root: TempDir, daemon: Daemon }
impl Camp {
    fn new(pack: &str) -> Self;                                  // scaffold + import the fixture pack + spawn
    fn camp(&self, args: &[&str]) -> Output;
    fn conn(&self) -> Connection;                                // camp.db, read-only
    fn sling(&self, formula: &str) -> String;                    // -> run_id
    fn create_member(&self, run: &str, title: &str) -> String;   // camp create <title> --run <run>
    fn step_bead(&self, run: &str, step: &str) -> String;        // runs/<run>/manifest.json
    fn get_bead(&self, id: &str) -> BeadRow;
    fn bead_metadata(&self, id: &str) -> BTreeMap<String, String>;
    fn drain_children(&self, anchor: &str) -> BTreeMap<usize, BeadRow>;
    fn dispatchable(&self) -> Vec<BeadRow>;                      // readiness::dispatchable_beads — no CLI exists
    fn events_of_type(&self, t: &str) -> Vec<serde_json::Value>;
    fn close_item(&self, item_root: &str);                       // see below
    fn settle(&self);          // wait_until(cursor caught up AND pending_drains empty), 10 s deadline
    fn restart_campd(&mut self);
}
```
**An item run root is NEVER closed directly** — every run root closes via `flow::finalization`, and
`camp close` on a live root would hit the same `InvalidTransition` class as B4. **`close_item` closes
the item run's `work` STEP bead** (read from that run's manifest); campd's finalization then closes the
item root, and `settle()` observes it.

### The fixtures — in full

`tests/fixtures/compose/drain/formulas/build.formula.toml`:
```toml
formula = "build"
contract = "graph.v2"

[[steps]]
id = "decompose"
title = "Decompose"

[[steps]]
id = "implement"
title = "Implement each member"
needs = ["decompose"]                    # S17 — a drain must depend on its member-producer
[steps.implement.drain]
context = "separate"
formula = "item"
member_access = "exclusive"

[[steps]]
id = "publish"
title = "Publish"
needs = ["implement"]
```
`.../formulas/item.formula.toml`:
```toml
formula = "item"
contract = "graph.v2"

[[steps]]
id = "work"
title = "Work the member"
```
**The conflict fixture — the only constructible shape.** A bead has **one** `run_id` and `run_members`
selects `WHERE b.run_id = ?1`, so two drains can contend **only as two drain steps of the SAME run**.
`.../formulas/two-drains.formula.toml`:
```toml
formula = "two-drains"
contract = "graph.v2"

[[steps]]
id = "decompose"
title = "Decompose"

[[steps]]
id = "drain-a"
title = "Drain A"
needs = ["decompose"]
[steps.drain-a.drain]
context = "separate"
formula = "item"
member_access = "exclusive"

[[steps]]
id = "drain-b"
title = "Drain B"
needs = ["decompose"]                    # PARALLEL with drain-a — both ready at once
[steps.drain-b.drain]
context = "separate"
formula = "item"
member_access = "exclusive"
```
Both anchors go ready when `decompose` closes; campd claims both; the first to execute reserves every
member and the second's reserve batch **conflicts and rolls back**.

**The orphan fixture** reuses `build.formula.toml` but points `drain.formula` at a name that does not
resolve: `execute_drain` appends the reserve batch, then the cook fails ⇒ the anchor is left holding
reservations ⇒ `restart_campd()` runs the sweep. *(That is also the honest test for "a drain whose item
formula is missing": it must `dispatch.failed` **and close the anchor**, not leak.)*

- [ ] **Step 1: Failing tests.** `a_drain_step_creates_NO_ATTEMPT_and_dispatches_NO_WORKER` (BD3) ·
  `the_drain_anchor_is_campd_held_and_never_worker_dispatched` ·
  **`a_drain_scatters_EVERY_member_in_one_pass`** (F7 — 3 members ⇒ 3 item roots after one `settle`) ·
  `an_exclusive_drain_reserves_every_member_with_gcs_verbatim_key` ·
  **`a_conflicting_drain_reserves_NOTHING_and_materializes_NOTHING`** (BD4 — the loser's
  `drain_children` is **empty**; the winner still holds every member) ·
  **`a_reserve_conflict_closes_the_losing_anchor_and_the_run_FINALIZES`** (BD5) ·
  `the_reservation_is_released_when_the_drain_gathers` ·
  `the_run_does_not_finalize_while_drain_items_are_open` (B5) ·
  **`the_drains_outcome_reflects_a_failed_item_at_gather`** (one item fails ⇒ anchor `fail`, **and the
  other items still ran** — `continue`, always, F6) ·
  **`a_drain_over_100_members_fails_the_drain_and_scatters_nothing`** (gc's cap) ·
  `a_drain_survives_a_campd_restart_without_double_materializing` ·
  `reconcile_releases_a_reservation_orphaned_by_a_kill_9` ·
  `doctor_lists_and_releases_orphaned_drain_reservations` ·
  `a_mail_bead_in_a_run_is_never_a_drain_member` ·
  **`a_CLOSED_member_is_never_scattered`** (D3 — gc excludes closed members) ·
  **`execute_drain_refuses_a_not_runnable_item_formula`** (the third cook entry point).
- [ ] **Step 2: Run; watch fail.**
- [ ] **Step 3: The pure reads** (SQL above).
- [ ] **Step 4: The dispatch arms — SEVEN additive edits, no refactors.**
  (1) `PendingDrain` beside `PendingFanout` (:1045). (2) `pending_drains` on `GraphRuntime`
  (:1051-1063). (3) `queue_drain` beside `queue_fanout` (:2180). (4) **`maybe_claim_looping` (:1891):
  the `is_campd_held` predicate at :1909 AND the `create_attempt` gate (BD3).** (5) `execute_drain` in
  `execute` (:1154), after the fanout loop, same requeue-tail-on-error shape. (6) `on_bead_closed`
  (:1813): a closed **drain item root** (by its `drain:` label) re-queues its anchor — the
  `on_root_closed` mold (:1864). (7) `reconcile` (**:1645**): re-queue open campd-held drain anchors,
  **plus the orphan sweep**.
  `execute_drain` mirrors `execute_fanout` (:1174-1275) but resolves `drain.formula` **through
  `FormulaLayers`** (not `<camp>/formulas/<bond>.toml`, which `execute_fanout` hardcodes at :1230 —
  every corpus item formula lives in an **imported** pack) and checks `not_runnable` before cooking.
  **`close_anchor` (:2296) takes `&Connection`** and uses `append_on`, while `execute_drain` holds
  `&mut Ledger` — so the gather **builds its `EventInput`s (the anchor close *and* every release) and
  submits ONE `append_batch`**; it does **not** call `close_anchor`. *(Rev 2's "call `close_anchor` and
  release in the same `append_batch`" does not typecheck.)*
- [ ] **Steps 5–6: Run; pass; gates; commit** —
  `"feat(dispatch): the drain runtime — campd-held anchors, all-or-nothing reservations, gc's real semantics"`

---

## Task 10: Invariant 6 — camp ⊆ gc fixtures

The `gc-compat` job runs the **real gc compiler** over `tests/fixtures/formulas/valid`;
`camp_corpus_validate.go` globs `*.toml` and derives the name as `TrimSuffix(basename, ".toml")`. So:
**never name a fixture `*.formula.toml`** (gc would get `"x.formula"`); **no `expansion` fixture in
`valid/`** (the shim compiles standalone, and §9 says an expansion formula is *"not directly
runnable"*); and **`extends-child` needs a parent LAYER** the shim does not provide. ⇒ `expansion` and
`extends-child` live in `tests/fixtures/compose/`; the **parent** goes in `valid/`.

- [ ] Add `vars-condition.toml`, `extends-parent.toml`, `drain-separate.toml` to `valid/`; update the
  list in `every_valid_fixture_is_accepted`.
- [ ] Prove them against the real gc compiler locally (`OK <name>`, exit 0). **A `FAIL` means camp
  accepts what gc rejects — invariant 6 is broken.**
- [ ] **`ci/gc-compat/README.md` — the corpus-drift procedure.** Moving `GCPACKS_REF` requires, in ONE
  PR: re-run `factshim` (the gc baseline) and `rungs.py` (the arbiter); update `formula_gate.py`'s
  `CEILING`/`RUNNABLE`/`RUNG_COUNTS`/`NOT_LOADABLE`; re-run `differential.py`; **and update the §9
  addendum's numbers.** *(The addendum hard-codes 95/62/the rungs into the spec. Nothing can enforce
  "spec == arbiter" mechanically; the written procedure is the enforcement.)*
- [ ] Commit.

---

## Task 11: The differential gate — scoped to what is actually comparable

**Rev 2's oracle could not have worked.** It diffed camp's **post**-substitution output against gc's
**pre**-substitution Recipe (F1 ⇒ hundreds of false diffs), demanded a `"drain": {…}` object gc
**cannot emit** (F2 ⇒ all 20 drain steps fail), and implied a step-list diff — but **gc expands
check/retry loops at compile into namespaced `.iteration.N` steps and synthesizes `gc.kind: scope`
bodies (1523 steps for 99 formulas), while camp keeps those as RUNTIME loops.** A full step-list diff
is **structurally impossible**, and always will be.

**So the oracle asserts the four things that ARE comparable**, each keyed by the **authored** step id
(gc exposes it as `Metadata["gc.step_id"]`; top-level steps are `"<formula>.<authored-id>"`):

| # | assertion | catches |
|---|---|---|
| **A** | **The compile set.** gc compiles 99/100 (`mol-polecat-work` fails); camp compiles 95. The delta is **exactly** the 4 camp deliberately refuses. | a silent over- or under-refusal |
| **B** | **Drain metadata.** For every gc step with `gc.kind = "drain"` (**20**: 19 separate, 1 shared), camp emits an identical `gc.drain_*` map. Camp yields **19** — the shared one is its deliberate refusal. | gc's **defaulting** (F3), camp's **condition-pruning** (12 of 13 shared drains vanish in **both**), and **extends propagation** (12 authored → 19 compiled) |
| **C** | **Routes.** For every gc step with `gc.run_target`, camp's value matches **byte-for-byte, pre-`{{}}`-substitution** — `{{implementation_target}}` must survive in **both**, and `{implementation_target}` must be **resolved** in both. | **F1 and F4 together** — the highest-risk pair in the phase |
| **D** | **Descriptions.** `sha256(description)` per authored step id, for steps camp materializes. | the **>4096 pointer prompt byte-for-byte**, `description_file` layering, and **whether `{{var}}` was wrongly substituted at compile** |

Excluded from every diff, and why: `FormulaSource` (an **absolute path** — environment-dependent),
`ContentHash`, and every gc step camp has no counterpart for (the `.iteration.N` / scope bodies gc
synthesizes).

**Files:** `ci/gc-compat/differential.py` (drives Task 0's `factshim` and `camp doctor --formula --json
--compiled`) · `cmd/doctor.rs` (`--compiled` emits camp's compiled formula in the same normalized
shape) · `ci.yml` (into the **`gc-compat`** job — it already has the gascity checkout and Go; add the
corpus checkout and `cargo build --bin camp` there).

- [ ] Implement; **run locally; FIX CAMP where it diverges. gc's behaviour outranks this plan's
  prose** — that is the entire point of building it.
- [ ] Commit — `"ci(gc-compat): the differential gate — camp's compiler diffed against gc's"`

---

## Task 12: The END-TO-END gate (BD8's proof), final gates, the PR

**Nothing in rev 2 cooked an imported formula.** `formula_gate.py` compiles; `differential.py` diffs
compilers; the drain fixtures were layer-free camp-local packs. That is exactly why the pinned-formula
round-trip could be dead in every corpus run with no gate able to see it.

- [ ] **Step 1: `ci/gc-compat/e2e_corpus.py`** — in the `formula_gate.py` camp root:
  `camp sling --formula bmad-story-development` (an **imported** graph.v2 formula with `extends`,
  `description_file` and a `{{}}` route), start campd with a **fake worker** (the
  `crates/camp/tests/fake-agent.sh` mold), and assert:
  1. the run **cooks** (`run.cooked`);
  2. `runs/<id>/recipe.json` exists and its step ids equal the manifest's;
  3. **campd does not dead-end the run** — *the exact failure BD8 names*: zero `dispatch.failed`
     carrying a `load_run` reason, and a step bead reaches `in_progress`;
  4. that bead's `assignee` is the **binding-resolved** route and its `metadata` carries
     `gc.run_target`.
  Wire it into the `gcpacks-compat` job.
- [ ] **Step 2: Every gate, in CI's order**
```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace
/tmp/factshim /tmp/gcpacks                                                   # the gc baseline
python3 ci/gc-compat/rungs.py             /tmp/gcpacks                       # the arbiter
python3 ci/gc-compat/load_corpus_packs.py /tmp/gcpacks target/debug/camp     # compat-1, still green
python3 ci/gc-compat/formula_gate.py      /tmp/gcpacks target/debug/camp     # 95 · 62 · 5 refused
python3 ci/gc-compat/differential.py      /tmp/gcpacks target/debug/camp /tmp/factshim
python3 ci/gc-compat/e2e_corpus.py        /tmp/gcpacks target/debug/camp     # BD8
/tmp/camp-corpus-validate crates/camp-core/tests/fixtures/formulas/valid     # invariant 6
ci/gc-compat/check_vocab.sh /tmp/gascity "$PWD"
```
- [ ] **Step 3: Push; PR; `gh pr checks --watch` green.**

**The PR body MUST state:** LOADABLE **95** and RUNNABLE **62** (both — "95/100" alone misleads); the
**5** camp refuses (incl. the two §9 did not anticipate) and that **gc itself fails one of them**; the
rungs **2 · 31 · 49 · 76 · 95**; **`SCHEMA_VERSION` 2 → 3 — an existing camp.db will NOT open; the
operator must re-init**; that **`single_lane` / `on_item_failure` have no runtime behavior in camp
because they have none in gc** (measured); that `ready_task_count`'s new exclusion **changes `camp top`'s
ready count**; the **accepted fidelity costs** (`gc.continuation_group`, `gc.build.*`, `gc.on_fail`
carried but not honoured); and the **spec amendments** (master line 449; the §9 addendum's ceiling,
S2/S3, D2′, **and §9's two corrected bullets — substitution and drain**).

---

## Exit criteria

| Criterion (phase block, verbatim) | Proof |
|---|---|
| *"every §9 rung's count pinned by a test at GCPACKS_REF"* | `formula_gate.py` drives the **real binary** over all 100: **2 · 31 · 49 · 76 · 95**, cross-checked **as a SET** against `rungs.py`. |
| *"refusals name their key and land as ledger events"* | `formula.refused`, validated in the fold; emitted by **all three** cook entry points (`camp sling`, order-fire, `execute_drain`). |
| *"camp ⊆ gc gate still green (invariant 6)"* | Task 10 (real gc compiler over `valid/`) **and Task 11** (all 100 diffed against gc). |
| *"Ceiling is 97–98 and the gate names which"* | **Measured: 95.** §9 is amended. The gate names all five — and records that **gc itself fails one**. |
| *"The 21 no-contract formulas are refused, not assumed"* | D1 — plus the 14 expansion formulas. **RUNNABLE = 62 pinned.** |
| *"exclusive reservations as member-bead metadata (verbatim key)"* | Task 3 (store, refold-wired, schema 3, atomic CAS) + Task 9 (**all-or-nothing** reserve, conflict closes the anchor, orphan sweep, operator escape). |
| *"same-session REFUSED"* | Task 8 — the 12 conditional (pruned, **with their refusals**) **and** the 1 unconditional. |
| *"on_item_failure/single_lane per gc's compiler defaulting"* | Task 8's defaulting table, **diffed against gc's emitted `gc.drain_*`** (Task 11-B). Their **runtime** behavior is nil **because it is nil in gc** (F5/F6). |
| *"CI green"* | Task 12. |

## Notes for the implementer

- **`factshim` (Task 0) and `rungs.py` are the arbiters.** If a number moves, the pin moved or a rule
  is wrong — **report to the lead; never edit a seed to match the code.**
- **Three substitution functions, three grammars, three stages. Never merge them:**
  `compose::resolve_single_brace` (`{name}`, **compile**, **inside expansion only**),
  `cook::substitute_vars` (`{{name}}`, **instantiation**, every field), and the existing
  `cook::substitute` (`{name}` over `CookOptions.vars`, for bond children).
- **`UNIMPLEMENTED` must be GONE by Task 8.** If it survives, an accepted key silently compiles to
  nothing.
- **`dispatch.rs` is shared with `cp-1`.** Additive only; `event_loop.rs` is untouched. Expect a
  rebase; re-run every gate after it.
- **Before you build a mechanism, trace it on paper against a concrete input.** BD3, BD4 and BD8 each
  took sixty seconds to falsify that way — and all three shipped in rev 2.

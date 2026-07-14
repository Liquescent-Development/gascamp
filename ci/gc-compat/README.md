# `ci/gc-compat` — the Gas City compatibility gates

Camp's claim is **invariant 6**: every valid camp formula is a valid Gas City
formula-v2 file. Compat phase 2 adds the converse-ish claim that matters
commercially: **camp reads the real Gas City corpus** — 95 of 100 formulas at
`GCPACKS_REF`, 62 of them runnable.

Neither claim is worth anything asserted. Everything here MEASURES.

## The pins

| file | what it pins |
|---|---|
| `GASCITY_REF` | the gc commit whose **real compiler** is the oracle |
| `GCPACKS_REF` | the corpus commit every count below is measured at |

The corpus is **never vendored** (compat §10). CI fetches it at the ref.

## The instruments

| script | what it does | what it would catch |
|---|---|---|
| `factshim.go` | Builds gc's **real compiler** and dumps its actual output (`--all-json`, `--authored-json`, `--corrupt-sites`). | Every fidelity claim in phase 2 was wrong at least once when taken from a source-read, and right every time it came from here. **Do not re-derive a fidelity claim by reading gc's source. Run the shim.** |
| `rungs.py` | The **independent arbiter**. Simulates camp's pipeline in stdlib Python — it does NOT import camp and does NOT shell out to the binary. Predicts the rung counts and the loadable SET. | A key table tuned until the numbers come out. It is a second, from-scratch model; if it and the binary disagree, one of them is wrong and neither gets to decide which. |
| `formula_gate.py` | Drives the **real camp binary** over all 100 corpus formulas. | A ceiling that is asserted rather than achieved. |
| `camp_corpus_validate.go` | Compiles camp's own `valid/` fixtures with the **real gc compiler**. | Camp accepting something gc rejects — invariant 6, broken. |
| `check_vocab.sh` | Event names vs gc's vocabulary. | A camp event that silently redefines a gc one (invariant 7). |
| `load_corpus_packs.py` | compat-1: the pack/agent/import loader. | A loader that refuses every real pack. |

### `formula_gate.py`'s three assertions

1. **The ceiling** — exactly 95 of 100 compile, and the 5 that do not refuse
   **naming the right key**. A refusal that fires for the wrong reason is not a
   pass.
2. **RUNNABLE** — exactly 62. Compiling is not enough to `camp sling` something,
   and "95/100" alone is a misleading headline.
3. **The falsifiable cross-check** — the **SET** camp loaded must equal the
   **SET** `rungs.py` predicts. Comparing *counts* would mean recomputing them
   from camp's own key table, reproducing the arbiter by construction so it could
   never fail. Comparing two sets — one from the real binary, one from an
   independent model — can fail.

`--expect-loaded N` overrides assertion 1 only, for driving an intermediate rung
during development. Assertions 2 and 3 bind only at the full ladder.

## ⚠️ Moving `GCPACKS_REF` — the drift procedure

The counts below are hard-coded in **four** places, and nothing can enforce
"spec == arbiter" mechanically. This written procedure is the enforcement. Do all
of it in **ONE PR**:

1. **Re-run `factshim`** (the gc baseline). It prints the compiled-step counts,
   the drain steps, the residual-`{{var}}` counts, and the `{{var}}`-corruption
   sites. If a number moved, the corpus moved — understand *why* before touching
   anything else.
2. **Re-run `rungs.py`.** It hard-codes the seeds and exits non-zero on drift, on
   purpose. **Do not edit a seed to make it pass.** If the arbiter and the binary
   disagree, that is a finding, not a chore.
3. **Update `formula_gate.py`**: `CEILING`, `RUNNABLE`, `RUNG_COUNTS`,
   `NOT_LOADABLE`.
4. **Re-run `differential.py`** against the new corpus.
5. **Update the compat spec's §9 addendum** — it hard-codes 95 / 62 / the rung
   ladder into the spec as measured fact. A spec that disagrees with the gate is
   worse than no spec.

## The counting rules (an ambiguous metric invites tuning until it agrees)

* `resid_desc_steps` counts **STEPS** with ≥1 residual `{{var}}` in a
  description (**561**). The **OCCURRENCE** count is **2396**. They are different
  numbers and conflating them cost one revision.
* The `{{var}}` corruption has **THREE** units: **52 occurrences / 49 steps / 20
  formulas.** Assertion D hashes a whole description, so its exclusion set is
  **steps** (49), not occurrences (52).
* The differential join key is `Step.ID` with the `"<formula>."` prefix stripped,
  and synthesized steps excluded by a **derived** flag — never a guessed list.
  gc's `gc.step_id` is a **back-reference stamped on the steps gc SYNTHESIZED**,
  not an authored id; keying on it inverts the join and yields **zero rows** on
  the very steps the assertion exists to check.

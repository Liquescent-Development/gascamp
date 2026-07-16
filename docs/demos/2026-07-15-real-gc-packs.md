# Demo: real Gas City v1 packs load, compile, and cook in camp ($0)

An opt-in, re-runnable local verification that camp ingests the **real** Gas
City v1 packs — `bmad` and `gstack` — off the pinned corpus, compiles their
formulas against gc's own compiler, and cooks one into a real bead graph.

It spends **no API money**. It never starts campd and never spawns a worker; it
cooks a formula into beads (durable in the ledger) and reads the graph back.
Live dispatch against a real `claude` is `make e2e` — a separate, operator-gated
step.

## Run it

```bash
make demo-pack                                  # clone the pinned corpus
make demo-pack CORPUS=/path/to/gcpacks-src      # reuse an existing checkout
# or directly:
scripts/demo-pack.sh [/path/to/gcpacks-src]
```

Requirements: `git`, `python3`, `cargo`. The gc-real-compiler **differential**
also runs when `go` is on `PATH` (it builds the gascity oracle at
`GASCITY_REF`); without Go it is skipped and the camp-side gates still bind.

## What it proves (and how)

The corpus is pinned and **never vendored** (compat §10): `GCPACKS_REF` pins
`gastownhall/gascity-packs`, `GASCITY_REF` pins the gc commit whose *real
compiler* is the oracle. Both are fetched at their SHA; the pinned gc commit is
reachable only by explicit `git fetch origin <sha>` (the server allows it), not
by a default clone.

| Step | Instrument | Claim |
|---|---|---|
| LOAD | `ci/gc-compat/load_corpus_packs.py` | camp's own loader ingests the packs, their transitive `gascity` layer, and the `gascity/roles` base — via the §3 two-command recipe |
| RUNGS | `ci/gc-compat/rungs.py` | an independent stdlib model predicts the loadable set |
| COMPILE | `ci/gc-compat/formula_gate.py` | 95/100 loadable, 65 runnable, and camp's loaded **set** == the arbiter's |
| REAL gc | `ci/gc-compat/differential.py` | camp's compiler == gc's real compiler byte-for-byte (routes, descriptions, drains, dep edges); the compile-set delta is exactly the 4 camp deliberately refuses (gc 99, camp 95) |
| COOK | `camp sling --formula bmad-build` (campd down) | the pack produces the right work graph — a 20-bead DAG with correct readiness |

### The import recipe (compat phase 1, §3)

```bash
camp init --no-service --no-import
# camp never inherits gc's unrestricted tool default (§5.2); bmad/gstack ship
# skills/, so the allowlist must carry Skill for their agents to resolve.
printf '\n[agent_defaults]\ntools = ["Read","Bash","Skill"]\n' >> .camp/camp.toml
camp import add <corpus>/bmad          --name bmad    # pulls gascity transitively as gc
camp import add <corpus>/gstack        --name gstack
camp import add <corpus>/gascity/roles --name gc      # direct import overrides the transitive binding
```

Local-path imports are **layered in place** (D7): they persist as `[imports.*]`
in `camp.toml` and materialize their transitive layer under
`imports/.transitive/`, but have no `packs.lock` entry — so `camp import list`
/ `camp import check` (which report *fetched/locked* imports) show "0". That is
by design, not a failure; the proof the imports are live is that formulas
resolve and cook.

### Per-pack result (measured at `GCPACKS_REF` 44b2eef)

| pack | formulas | load | runnable | loadable-but-not-runnable |
|---|---|---|---|---|
| bmad | 9 | 9 | 8 | 1 (`bmad-code-review-flow` — `type = "expansion"`) |
| gstack | 12 | 12 | 8 | 4 (`code-review`, `plan-review`, `qa-review`, `release-readiness` — all `type = "expansion"`) |

An `expansion` formula supplies `template` steps for another formula's `expand`
rule; it is loadable but not directly runnable (compat §9). That is a refusal
**by design** — reported, never failed.

### The cooked graph

`bmad-build` cooks a **20-bead DAG**; `camp ls --ready` shows the 5-bead
unblocked frontier (the entry `Prepare build context` step plus four parallel
BMAD reviewers), the rest blocked on their `needs`. `camp show <bead>` shows the
route resolved through the imported `gc` binding (`assignee gc.run-operator`),
the `description_file` resolved through the transitive layer, and the formula
`{{vars}}` resolved (`implementation_target = bmad.story-implementer`, a
qualified bmad route).

## Note on paths (macOS)

A camp's control socket is `<root>/campd.sock` and a Unix domain socket path
must fit `SUN_LEN` (~104 bytes). A deep temp/scratch path overflows it, so the
demo camp lives at a shallow `mktemp -d` under `$TMPDIR`. Cooking and reads
never touch the socket; only campd and the sling *poke* do.

#!/usr/bin/env python3
"""usage: differential.py <corpus-checkout> <camp-binary> <factshim-binary>

THE ORACLE: camp's compiler diffed against GC'S REAL ONE, over the whole corpus.

A full step-list diff is STRUCTURALLY IMPOSSIBLE and always will be: gc expands
check/retry loops AT COMPILE into namespaced `.iteration.N` steps and synthesizes
`gc.kind: scope` bodies (1523 steps for 99 formulas), while camp keeps those as
RUNTIME loops. So the oracle asserts the six things that ARE comparable, over an
AUTHORED-STEP projection.

THE JOIN KEY is `Step.ID` with the `"<formula>."` prefix stripped, and synthesized
steps excluded by a DERIVED flag (factshim's `synthesized()`), never a guessed
list. gc's `gc.step_id` is a BACK-REFERENCE stamped on the steps gc SYNTHESIZED,
pointing at their authored parent — keying on it INVERTS the join: 0 of the 20
drain steps carry it, so assertion B would have been unbuildable on 100% of its
subjects. (And "0 collisions" would still have been true: one back-reference per
authored parent is trivially unique. **0 collisions is a RELATIVE property. It
never asks whether the key MEANS anything.**)

Measured at GCPACKS_REF: 530 authored keys, 0 collisions, all 20 drains, 431
comparable dep edges.

  A  THE COMPILE SET     gc compiles 99/100; camp compiles 95. The delta is
                         EXACTLY the 4 camp deliberately refuses.
  B  DRAIN METADATA      every gc `gc.kind = "drain"` step's `gc.drain_*` map,
                         identical in camp. Catches gc's DEFAULTING, camp's
                         condition-pruning, and extends propagation.
  C  ROUTES              `gc.run_target` byte-for-byte, PRE-substitution.
  D  DESCRIPTIONS        sha256 per key, SKIPPING the 49 steps gc CORRUPTS (D7).
  E  ⭐ THE STEP SET     set(gc's authored ids) == set(camp's step ids), per
                         formula. Catches OVER-PRUNING — missing work, silently.
  F  ⭐ DEPENDENCY EDGES set(gc's Deps) == set(camp's needs), both endpoints
                         authored. A step left needing a PRUNED step never
                         dispatches and the run dead-ends — invisible to A-E.

THE VACUOUS-REPAIR TRAP, named so it is not walked into: the obvious "fix" for a
broken join is to INTERSECT the two key sets. That turns E into a comparison of a
set with itself. It goes green, silently, and the bug it exists for is un-fixed.
**E compares gc's 530 against camp's output. It never intersects.**
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

FORMULA_PACKS = [
    "bmad",
    "compound-engineering",
    "contributing",
    "discord",
    "gascity",
    "gastown",
    "github",
    "gstack",
    "pr-pipeline",
    "superpowers",
]

# The 4 camp deliberately refuses that gc COMPILES. (`mol-polecat-work` is the
# 5th camp refuses — and gc fails it too, which is why gc is 99 and not 100.)
CAMP_REFUSES = {
    "mol-digest-generate",
    "mol-pr-from-issue",
    "design-review",
    "same-session-implement",
}
GC_FAILS = {"mol-polecat-work"}

if len(sys.argv) != 4:
    print(__doc__)
    sys.exit(2)

corpus = os.path.abspath(sys.argv[1])
camp_bin = os.path.abspath(sys.argv[2])
shim = os.path.abspath(sys.argv[3])
here = os.path.dirname(os.path.abspath(__file__))

failures = []


def fail(assertion, msg):
    failures.append(f"[{assertion}] {msg}")


# ---- gc's side: the real compiler ------------------------------------------
gc_steps = json.loads(subprocess.run([shim, corpus, "--authored-json"],
                                     capture_output=True, text=True, check=True).stdout)
corrupt = json.loads(subprocess.run([shim, corpus, "--corrupt-sites"],
                                    capture_output=True, text=True, check=True).stdout)
# D7's exclusion set is STEPS (49), not occurrences (52). Assertion D hashes a
# WHOLE description, so conflating the two units is the 561-vs-2396 trap one level
# up.
corrupt_steps = {(c["formula"], c["step_id"]) for c in corrupt}

gc_by_formula = {}
for s in gc_steps:
    gc_by_formula.setdefault(s["formula"], {})[s["id"]] = s

# ---- the ONE structural difference the oracle must SCOPE AROUND ------------
# gc expands check/retry loops AT COMPILE, so a template step's `children` become
# the LOOP BODY and gc namespaces them under `.iteration.N.`:
#
#   bmad-build.review.bmad-code-review-loop.iteration.1.review.blind-hunter-review
#
# Those are SYNTHESIZED and are excluded from the 530. Camp keeps the loop at
# RUNTIME and FLATTENS the children into top-level steps (`review.blind-hunter-review`),
# so camp legitimately has step ids gc has only inside a loop body. This is the
# pre-existing architectural difference §9's addendum records — not a camp bug, and
# not something this phase changes.
#
# So assertion E excludes exactly those camp ids — and it DERIVES them from gc's own
# compiled output rather than guessing a list.
all_recipes = json.loads(subprocess.run([shim, corpus, "--all-json"],
                                        capture_output=True, text=True, check=True).stdout)
loop_body = {}
for fname, recipe in all_recipes.items():
    ids = set()
    for s in (recipe.get("Steps") or []):
        sid = s.get("ID") or ""
        marker = ".iteration."
        if marker not in sid:
            continue
        tail = sid.split(marker, 1)[1]
        # "<N>.<child-id>" — the child is everything after the iteration number.
        if "." in tail:
            ids.add(tail.split(".", 1)[1])
    loop_body[fname] = ids

keys = [(s["formula"], s["id"]) for s in gc_steps]
if len(keys) != len(set(keys)):
    print("DIFFERENTIAL FAIL: the join key COLLIDES — the oracle cannot be built")
    sys.exit(1)
print(
    f"join key: {len(keys)} authored steps, 0 collisions, "
    f"{sum(1 for s in gc_steps if s['kind'] == 'drain')} drains, "
    f"{len(corrupt_steps)} gc-corrupt steps excluded from assertion D"
)

# ---- camp's side: the real binary ------------------------------------------
work = tempfile.mkdtemp(prefix="differential-")
try:
    subprocess.run([camp_bin, "init", "--no-service", "--no-import"],
                   cwd=work, capture_output=True, check=True)
    root = os.path.join(work, ".camp")
    with open(os.path.join(root, "camp.toml"), "a") as fh:
        fh.write('\n[agent_defaults]\ntools = ["Read", "Bash", "Skill"]\n')

    def camp(*argv):
        return subprocess.run([camp_bin, "--camp", root, *argv],
                              capture_output=True, text=True)

    for pack in FORMULA_PACKS:
        camp("import", "add", os.path.join(corpus, pack), "--name", pack)
    camp("import", "add", os.path.join(corpus, "gascity", "roles"), "--name", "gc")

    camp_by_formula = {}
    camp_loaded = set()
    for pack in FORMULA_PACKS:
        d = os.path.join(corpus, pack, "formulas")
        if not os.path.isdir(d):
            continue
        for f in sorted(os.listdir(d)):
            if not f.endswith(".toml"):
                continue
            out = camp("doctor", "--formula", os.path.join(d, f), "--json", "--compiled").stdout
            v = json.loads(out)
            name = v["formula"]
            if not v["ok"]:
                continue
            camp_loaded.add(name)
            camp_by_formula[name] = {s["id"]: s for s in v["steps"]}
finally:
    shutil.rmtree(work, ignore_errors=True)

gc_compiled = set(gc_by_formula)

# ---- A: THE COMPILE SET ----------------------------------------------------
# Every formula gc compiles, camp must compile too — EXCEPT the 4 it deliberately
# refuses. A silent over- or under-refusal shows up here and nowhere else.
delta = gc_compiled - camp_loaded
if delta != CAMP_REFUSES:
    fail("A", f"gc compiles but camp does not: {sorted(delta)}; expected exactly {sorted(CAMP_REFUSES)}")
extra = camp_loaded - gc_compiled - GC_FAILS
if extra:
    fail("A", f"camp compiles what gc does not (and gc did not fail them): {sorted(extra)}")

# ---- B/C/D/E/F, per formula ------------------------------------------------
drains_checked = routes_checked = descs_checked = edges_checked = 0

# gc's `MaterializeExpansion` synthesizes a target step literally named `main`.
# The AUTHORED projection strips the `<formula>.` prefix, so an expansion formula
# compiled standalone shows up as `main`, `main.<child>`, …
EXPANSION_FORMULAS = {f for f, m in gc_by_formula.items() if "main" in m}

for formula, gc_map in sorted(gc_by_formula.items()):
    if formula in CAMP_REFUSES:
        continue
    # gc's `MaterializeExpansion` compiles an EXPANSION formula standalone by
    # synthesizing a target step named `main` and expanding the template against
    # it. Camp has no such notion: an expansion formula has no `steps`, is not
    # runnable, and exists only to be expanded INTO a host. Its steps are diffed
    # where they actually land — in the host.
    if formula in EXPANSION_FORMULAS:
        continue
    camp_map = camp_by_formula.get(formula)
    if camp_map is None:
        continue  # already reported by A

    # ---- E: ⭐ THE STEP SET. gc's authored ids vs camp's step ids. NEVER
    # intersected — that would compare a set with itself and go green while
    # checking nothing.
    gc_ids = set(gc_map)
    # Camp's loop-body children are excluded — gc has them only inside `.iteration.N.`
    # (derived above from gc's own output, never a guessed list).
    camp_ids = set(camp_map) - loop_body.get(formula, set())
    if gc_ids != camp_ids:
        missing = sorted(gc_ids - camp_ids)
        surplus = sorted(camp_ids - gc_ids)
        fail("E", f"{formula}: camp OVER-PRUNED {missing}; camp has extra {surplus}")
        continue

    for sid, g in sorted(gc_map.items()):
        c = camp_map[sid]

        # ---- B: DRAIN METADATA (gc's compiler defaulting, F3).
        if g["kind"] == "drain":
            gd = {k: v for k, v in (g["metadata"] or {}).items() if k.startswith("gc.drain_") or k == "gc.kind"}
            cd = {k: v for k, v in (c["metadata"] or {}).items() if k.startswith("gc.drain_") or k == "gc.kind"}
            if gd != cd:
                fail("B", f"{formula}.{sid}: drain metadata\n    gc  : {gd}\n    camp: {cd}")
            drains_checked += 1

        # ---- C: ROUTES, byte-for-byte, PRE-substitution.
        # Safe from D7: gc's corruption is confirmed DESCRIPTION-ONLY — 0 corrupted
        # titles, 0 corrupted metadata.
        g_route = (g["metadata"] or {}).get("gc.run_target")
        c_route = (c["metadata"] or {}).get("gc.run_target")
        if g_route != c_route:
            fail("C", f"{formula}.{sid}: route\n    gc  : {g_route!r}\n    camp: {c_route!r}")
        if g_route is not None:
            routes_checked += 1

        # ---- D: DESCRIPTIONS, skipping the steps gc CORRUPTS (D7).
        if (formula, sid) not in corrupt_steps:
            if g["description_sha256_norm"] != c["description_sha256_norm"]:
                fail("D", f"{formula}.{sid}: description sha256 differs")
            descs_checked += 1

        # ---- F: ⭐ DEPENDENCY EDGES, both endpoints authored.
        # BD2 rewrote condition-pruning to "drop dangling needs". A step camp
        # leaves needing a PRUNED step NEVER DISPATCHES — the run dead-ends — and
        # it is invisible to A-E.
        g_needs = {n for n in (g["needs"] or []) if n in gc_ids}
        c_needs = {n for n in (c["needs"] or []) if n in gc_ids}
        if g_needs != c_needs:
            fail("F", f"{formula}.{sid}: needs\n    gc  : {sorted(g_needs)}\n    camp: {sorted(c_needs)}")
        edges_checked += len(g_needs)

if failures:
    print(f"\nDIFFERENTIAL FAIL: {len(failures)} divergence(s) from gc's real compiler\n")
    for f in failures[:40]:
        print(" ", f)
    if len(failures) > 40:
        print(f"  … and {len(failures) - 40} more")
    sys.exit(1)

ref = open(os.path.join(here, "GCPACKS_REF")).read().strip()
print(
    f"\nDIFFERENTIAL gate OK at {ref}\n"
    f"  A  compile set : gc {len(gc_compiled)}, camp {len(camp_loaded)}, "
    f"delta == the {len(CAMP_REFUSES)} camp deliberately refuses\n"
    f"  B  drain md    : {drains_checked} drain step(s), gc's defaulting reproduced exactly\n"
    f"  C  routes      : {routes_checked} route(s) byte-for-byte, pre-substitution\n"
    f"  D  descriptions: {descs_checked} of {len(gc_steps)} step(s) "
    f"({len(corrupt_steps)} skipped — gc CORRUPTS them and camp does not, D7)\n"
    f"  E  step set    : equal, per formula (over-pruning would show here)\n"
    f"  F  dep edges   : {edges_checked} edge(s) equal (a dangling need would show here)"
)

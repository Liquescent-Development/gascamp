#!/usr/bin/env python3
"""usage: formula_gate.py <corpus-checkout> <camp-binary> [--expect-loaded N]

The §10 compat gate for phase 2's formula rungs, DRIVING THE REAL BINARY over
the real Gas City corpus at GCPACKS_REF.

It asserts three things:

  1. THE CEILING. `camp doctor --formula <path> --json` over all 100 formulas:
     exactly CEILING compile, and the five camp cannot load refuse NAMING THE
     RIGHT KEY. (A refusal that fires for the wrong reason is not a pass.)
  2. RUNNABLE. Exactly RUNNABLE report `runnable: true`. Compiling is NOT enough
     to `camp sling` something, and "95/100" alone is a misleading headline.
  3. THE FALSIFIABLE CROSS-CHECK. The SET of formulas camp actually loaded must
     equal the SET `rungs.py` independently predicts. Comparing COUNTS would
     have meant recomputing them from camp's own key table — reproducing the
     arbiter by construction, so it could never fail. Comparing two SETS, one
     from the real binary and one from an independent model, is a real check: a
     tuned key table changes camp's set and the comparison breaks.

`--expect-loaded N` overrides the CEILING assertion ONLY, for driving an
intermediate rung during development. The RUNNABLE assertion and the
set-vs-arbiter cross-check bind only at the full ladder and are SKIPPED when it
is passed — an intermediate rung has no meaningful runnable count.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

# ---- the pins. Moving GCPACKS_REF means re-running factshim AND rungs.py and
# updating these AND the §9 addendum, in ONE PR. See README.md.
CEILING = 95
RUNNABLE = 65
RUNG_COUNTS = {"2a": 2, "2b": 31, "2c": 49, "2d": 76, "2e": 95}

# basename -> a key the refusal MUST name.
NOT_LOADABLE = {
    "mol-digest-generate.toml": "phase",
    "mol-pr-from-issue.formula.toml": "phase",
    # NOT `gc.scope_kind` — that key does not exist anywhere in the corpus.
    "design-review.formula.toml": "gc.kind",
    # An UNCONDITIONAL shared drain: 12 of the 13 sit behind
    # `{{drain_policy}} == same-session` and PRUNE. This one has no condition.
    "same-session-implement.formula.toml": "context",
    # gc fails this one too — gc compiles 99/100.
    "mol-polecat-work.toml": "extends",
}

# The 10 formula-bearing packs. Measured: no two of the 100 formulas share a
# basename, so there is no within-tier collision.
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

args = [a for a in sys.argv[1:] if not a.startswith("--")]
expect_loaded = None
if "--expect-loaded" in sys.argv:
    expect_loaded = int(sys.argv[sys.argv.index("--expect-loaded") + 1])
    args = [a for a in args if a != str(expect_loaded)]
if len(args) != 2:
    print(__doc__)
    sys.exit(2)

corpus = os.path.abspath(args[0])
camp_bin = os.path.abspath(args[1])
here = os.path.dirname(os.path.abspath(__file__))


def die(msg):
    print("FORMULA gate FAIL:", msg)
    sys.exit(1)


def camp(root, *argv):
    r = subprocess.run(
        [camp_bin, "--camp", root, *argv], capture_output=True, text=True
    )
    if r.returncode != 0:
        die(f"camp {' '.join(argv)} exited {r.returncode}: {r.stderr.strip()}")
    return r.stdout


work = tempfile.mkdtemp(prefix="formula-gate-")
try:
    subprocess.run(
        [camp_bin, "init", "--no-service", "--no-import"],
        cwd=work,
        capture_output=True,
        text=True,
        check=True,
    )
    camp_root = os.path.join(work, ".camp")
    camp_toml = os.path.join(camp_root, "camp.toml")
    if not os.path.isfile(camp_toml):
        die("camp init produced no camp.toml")
    # camp never inherits gc's unrestricted tool default (§5.2), and bmad ships
    # skills/, so the allowlist must carry Skill for its agents to resolve.
    with open(camp_toml, "a") as fh:
        fh.write('\n[agent_defaults]\ntools = ["Read", "Bash", "Skill"]\n')

    for pack in FORMULA_PACKS:
        camp(camp_root, "import", "add", os.path.join(corpus, pack), "--name", pack)
    # The roles pack, bound as `gc` — the binding every corpus route resolves
    # through (`gc.run-operator`, `gc.implementation-worker`, …).
    camp(camp_root, "import", "add", os.path.join(corpus, "gascity", "roles"), "--name", "gc")

    # ---- drive the real binary over all 100.
    formulas = []
    for pack in FORMULA_PACKS:
        d = os.path.join(corpus, pack, "formulas")
        if not os.path.isdir(d):
            continue
        for f in sorted(os.listdir(d)):
            if f.endswith(".toml"):
                formulas.append((f, os.path.join(d, f)))
    if len(formulas) != 100:
        die(f"expected 100 corpus formulas, found {len(formulas)}")

    loaded, runnable, refused = set(), set(), {}
    for basename, path in formulas:
        out = camp(camp_root, "doctor", "--formula", path, "--json")
        v = json.loads(out)
        if v["ok"]:
            loaded.add(basename)
            if v["runnable"]:
                runnable.add(basename)
        else:
            keys = [r["key"] for r in v.get("refusals", [])]
            keys += [x["construct"] for x in v.get("violations", [])]
            refused[basename] = keys

    # ---- 1. the ceiling.
    want = CEILING if expect_loaded is None else expect_loaded
    if len(loaded) != want:
        missing = sorted(set(NOT_LOADABLE) - set(refused))
        die(
            f"{len(loaded)} of 100 formulas loaded, expected {want}\n"
            f"  refused ({len(refused)}): {sorted(refused)}\n"
            f"  expected-refused but LOADED: {missing}"
        )

    if expect_loaded is None:
        # The five, and each must refuse for the RIGHT REASON.
        if set(refused) != set(NOT_LOADABLE):
            die(
                f"the refused SET is wrong\n"
                f"  camp refused : {sorted(refused)}\n"
                f"  expected     : {sorted(NOT_LOADABLE)}"
            )
        for basename, key in NOT_LOADABLE.items():
            if key not in refused[basename]:
                die(
                    f"{basename} must refuse naming {key!r}, but named "
                    f"{refused[basename]!r} — a refusal that fires for the wrong "
                    f"reason is not a pass"
                )

        # ---- 2. RUNNABLE.
        if len(runnable) != RUNNABLE:
            die(
                f"{len(runnable)} runnable, expected {RUNNABLE} "
                f"(of the {CEILING} loadable, 16 declare NO graph compiler at all — "
                f"neither `contract` nor `[requires] formula_compiler` — and 14 are "
                f"expansions, and the two sets are disjoint: 95 - 16 - 14 = 65)"
            )

        # ---- 3. the falsifiable cross-check: SET vs SET.
        arb = subprocess.run(
            [sys.executable, os.path.join(here, "rungs.py"), corpus, "--json"],
            capture_output=True,
            text=True,
        )
        if arb.returncode != 0:
            die(f"the arbiter (rungs.py) failed: {arb.stderr.strip()}")
        predicted = set(json.loads(arb.stdout)["loadable_2e"])
        if loaded != predicted:
            die(
                "camp's loaded SET does not match the arbiter's prediction\n"
                f"  camp loaded, arbiter did not: {sorted(loaded - predicted)}\n"
                f"  arbiter predicted, camp did not: {sorted(predicted - loaded)}"
            )

    ref = open(os.path.join(here, "GCPACKS_REF")).read().strip()
    if expect_loaded is None:
        print(
            f"FORMULA gate OK at {ref}: {len(loaded)}/100 LOADABLE, "
            f"{len(runnable)} RUNNABLE, 5 refused by name, "
            f"set == rungs.py (rungs {RUNG_COUNTS})"
        )
    else:
        print(f"FORMULA gate (rung driver) OK: {len(loaded)}/100 loaded")
finally:
    shutil.rmtree(work, ignore_errors=True)

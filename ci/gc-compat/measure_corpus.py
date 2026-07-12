#!/usr/bin/env python3
"""Measure the real Gas City pack corpus. Every number in the pack-compatibility
specs must come from here, not from a person's recollection.

Three revisions of that spec quoted numbers derived by ad-hoc greps; every
revision had wrong ones. Two traps produced them, and both are guarded below:

  1. NEVER regex TOML. Real formulas carry multi-line `description = \"\"\"...\"\"\"`
     blocks; a regex happily matches keys *inside the prose*. We use tomllib.

  2. Glob `formulas/*.toml`, NOT `*.formula.toml`. The 8 gastown `mol-*.toml`
     files break the naming convention; the narrow glob yields 92 formulas and
     makes every downstream number wrong.

This is also the seed of the compatibility gate (compat spec §10): the claimed
"N of 100 formulas load" figures become a test, not a boast.

Usage:
    git clone --depth 1 https://github.com/gastownhall/gascity-packs /tmp/gcpacks
    python3 ci/gc-compat/measure_corpus.py /tmp/gcpacks
"""

import collections
import glob
import os
import sys
import tomllib

# What camp's formula parser accepts today (crates/camp-core/src/formula/parse.rs).
CAMP_TOP = {"formula", "description", "requires", "steps"}
CAMP_STEP = {
    "assignee", "check", "description", "id", "needs",
    "on_complete", "retry", "timeout", "title",
}

# Keys Gas City's own engine silently DROPS — absent from its Formula struct.
# A consumer that refuses these is STRICTER than the reference implementation
# and rejects packs Gas City runs fine. 93/100 formulas name at least one.
DEAD_TOP = {
    "version", "target_required", "internal", "mode",
    "single_lane", "sling_container_mode",
}


def load_formulas(root):
    # NOT *.formula.toml — see the docstring.
    paths = sorted(glob.glob(os.path.join(root, "*", "formulas", "*.toml")))
    out = []
    for p in paths:
        with open(p, "rb") as fh:
            out.append((p, tomllib.load(fh)))
    return out


def extra_keys(doc):
    """Keys a formula uses that camp does not accept, top-level and step-level.

    Walks `children` recursively — a step nested in children is still a step.
    """
    top = {k for k in doc if k not in CAMP_TOP}
    step = set()

    def walk(steps):
        for s in steps or []:
            if not isinstance(s, dict):
                continue
            step.update(k for k in s if k not in CAMP_STEP)
            walk(s.get("children"))

    walk(doc.get("steps"))
    return top, step


def main(root):
    formulas = load_formulas(root)
    print(f"formulas: {len(formulas)}")

    contracts = collections.Counter(d.get("contract", "(none)") for _, d in formulas)
    print("\ncontract declared:")
    for k, n in contracts.most_common():
        print(f"  {k:12} {n}")

    top = collections.Counter()
    step = collections.Counter()
    for _, d in formulas:
        t, s = extra_keys(d)
        for k in t:
            top[k] += 1
        for k in s:
            step[k] += 1

    print("\ntop-level keys camp does not accept:")
    for k, n in top.most_common():
        dead = "  (DEAD in gc — ignore, do not refuse)" if k in DEAD_TOP else ""
        print(f"  {k:22} {n}{dead}")

    print("\nstep keys camp does not accept:")
    for k, n in step.most_common():
        print(f"  {k:22} {n}")

    # Routing: every gc.run_target in the corpus is a QUALIFIED <binding>.<agent>
    # name. This is Critical 1 in KNOWN-DEFECTS.md, and the compat spec's §7.1.
    #
    # Counted TWO ways, because 46 of the raw values are {{var}} references:
    #   raw       — the literal metadata value ({{implementation_target}} et al.)
    #   resolved  — {{var}} references replaced by the formula's own [vars]
    #               default. Every default in the corpus is itself qualified;
    #               the 4 sites with no default receive a qualified value via
    #               expand_vars from their caller (verified by hand, not here).
    # The spec's load-bearing claim is the last line: ZERO bare route values.
    routes = collections.Counter()
    resolved = collections.Counter()
    unresolved = collections.Counter()
    for _, d in formulas:
        formula_vars = d.get("vars") or {}

        def resolve(rt):
            if rt.startswith("{{") and rt.endswith("}}"):
                v = formula_vars.get(rt[2:-2].strip())
                if isinstance(v, dict) and "default" in v:
                    return v["default"], True
                return rt, False
            return rt, True

        def walk(steps):
            for s in steps or []:
                if not isinstance(s, dict):
                    continue
                rt = (s.get("metadata") or {}).get("gc.run_target")
                if rt:
                    routes[rt] += 1
                    r, ok = resolve(rt)
                    (resolved if ok else unresolved)[r] += 1
                walk(s.get("children"))
        walk(d.get("steps"))
    print("\ngc.run_target values, raw:")
    for k, n in routes.most_common(8):
        print(f"  {k:44} {n}")
    print("\ngc.run_target values, [vars] defaults resolved:")
    for k, n in resolved.most_common(12):
        print(f"  {k:44} {n}")
    if unresolved:
        print("  (no default — supplied qualified via expand_vars:)")
        for k, n in unresolved.most_common():
            print(f"  {k:44} {n}")
    bare = sorted(k for k in resolved if "." not in k)
    print(f"\nBARE (unqualified) resolved route values: {bare or 'NONE'}")

    # Agents: a directory per agent, per pack. gascity has NO agents/ — its 12
    # roles live in the NESTED pack gascity/roles/. This is Critical 4.
    print("\nagents per pack (dirs under <pack>/agents/):")
    for pack in sorted(
        p for p in os.listdir(root) if os.path.isdir(os.path.join(root, p))
    ):
        agents = glob.glob(os.path.join(root, pack, "agents", "*", ""))
        nested = glob.glob(os.path.join(root, pack, "*", "pack.toml"))
        note = f"   NESTED PACK: {[os.path.dirname(n).split('/')[-1] for n in nested]}" if nested else ""
        if agents or nested:
            print(f"  {pack:22} {len(agents):3}{note}")

    # Pack-level imports. Depth 1 covers the whole corpus — verified.
    print("\npack.toml [imports.*] (depth-1 is sufficient — verified):")
    for pack in sorted(os.listdir(root)):
        pt = os.path.join(root, pack, "pack.toml")
        if not os.path.isfile(pt):
            continue
        with open(pt, "rb") as fh:
            d = tomllib.load(fh)
        imports = d.get("imports") or {}
        if imports:
            for binding, spec in imports.items():
                print(f"  {pack:22} [imports.{binding}] source = {spec.get('source')!r}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])

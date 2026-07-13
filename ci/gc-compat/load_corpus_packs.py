#!/usr/bin/env python3
# usage: python3 ci/gc-compat/load_corpus_packs.py <corpus-checkout> <camp-binary>
# Phase-1 compat gate (compat spec §10/§14): assert the Gas City pack corpus
# loads the way phase 1 needs — pack/agent/transitive LOADING, not formula
# compilation (the §9 rung table is phase 2). Pinned to GCPACKS_REF. Exit
# non-zero on any drift. The corpus tree is NEVER vendored (umbrella §10):
# CI fetches at GCPACKS_REF.
#
# TWO halves, and the second is the one with teeth:
#   1. SHAPE — the corpus still looks the way the design read it (tomllib).
#   2. LOAD  — camp's OWN loader actually ingests it (the camp binary).
# Half 1 alone was the whole gate once, and it could not see a loader that
# refused every real pack: a local import of any corpus pack was rejected
# outright (its `[imports.gc] source = "../gascity"` "escaped the repo root")
# while this gate stayed green, because Python never asked camp to load
# anything. A compat gate that never runs the thing it certifies is a shape
# assertion wearing a gate's hat.
import glob
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib

if len(sys.argv) != 3:
    print(__doc__ or "", end="")
    print("usage: load_corpus_packs.py <corpus-checkout> <camp-binary>")
    sys.exit(2)

root = sys.argv[1]
camp_bin = os.path.abspath(sys.argv[2])


def die(m):
    print("GCPACKS gate FAIL:", m)
    sys.exit(1)


if not os.path.isfile(camp_bin) or not os.access(camp_bin, os.X_OK):
    die(f"camp binary {camp_bin!r} is not executable — the LOAD half cannot run")


def camp(cwd, *args, expect=0):
    """Run the camp binary, failing the gate on an unexpected exit."""
    p = subprocess.run(
        [camp_bin, *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if p.returncode != expect:
        die(
            f"`camp {' '.join(args)}` exited {p.returncode} (expected {expect})\n"
            f"--- stdout ---\n{p.stdout}\n--- stderr ---\n{p.stderr}"
        )
    return p


# The four v1 importers each declare [imports.gc] source = "../gascity".
importers = {}
for pt in glob.glob(os.path.join(root, "*", "pack.toml")):
    with open(pt, "rb") as fh:
        d = tomllib.load(fh)
    if d.get("imports"):
        importers[os.path.basename(os.path.dirname(pt))] = d["imports"]
if set(importers) != {"bmad", "gstack", "compound-engineering", "superpowers"}:
    die(f"importers {sorted(importers)} != the four v1 packs")
for p, imp in importers.items():
    if imp.get("gc", {}).get("source") != "../gascity":
        die(f"{p} gc import != ../gascity (got {imp.get('gc')!r})")

# gascity is a content layer, not a pack: no top-level agents/, and a nested
# roles pack.
if glob.glob(os.path.join(root, "gascity", "agents", "*", "")):
    die("gascity should have no top-level agents/")
if not os.path.isfile(os.path.join(root, "gascity", "roles", "pack.toml")):
    die("gascity/roles nested pack missing")


def n(p):
    return len(glob.glob(os.path.join(root, p, "agents", "*", "")))


# Agent counts per pack (immediate agents/ subdirectories).
for p, expect in {
    "bmad": 10,
    "gstack": 13,
    "compound-engineering": 28,
    "superpowers": 9,
    "gascity/roles": 12,
}.items():
    if n(p) != expect:
        die(f"{p} agents {n(p)} != {expect}")

# ---------------------------------------------------------------- LOAD half
# Drive camp's real loader over the real corpus, along the operator's own path
# (§3's two-command recipe): import an importer pack, then bind the roles pack
# directly. Both are LOCAL sources, so both are layered in place (D7).
work = tempfile.mkdtemp(prefix="gcpacks-gate-")
try:
    camp(work, "init", "--no-service", "--no-import")

    # camp never inherits an unrestricted tool default (§5.2), and bmad ships
    # skills/, so the allowlist must carry Skill for its agents to resolve.
    camp_toml = os.path.join(work, ".camp", "camp.toml")
    if not os.path.isfile(camp_toml):
        camp_toml = os.path.join(work, "camp.toml")
    if not os.path.isfile(camp_toml):
        die("camp init produced no camp.toml")
    camp_root = os.path.dirname(camp_toml)
    with open(camp_toml, "a") as fh:
        fh.write('\n[agent_defaults]\ntools = ["Read", "Bash", "Skill"]\n')

    # 1. The importer. This is the step that a "../gascity"-escapes bug fails.
    bmad = os.path.abspath(os.path.join(root, "bmad"))
    camp(camp_root, "import", "add", bmad, "--name", "bmad")

    # Its transitive gascity layer must have materialized under the sentinel,
    # DISJOINT from any binding dir (D8) — this is what the 24 corpus formulas
    # that `extends` gascity are compiled against.
    trans = os.path.join(camp_root, "imports", ".transitive", "gc")
    if not os.path.isdir(os.path.join(trans, "formulas")):
        die(f"transitive gascity layer missing at {trans}/formulas")

    # ...and the local pack itself was NOT copied (D7: layered in place).
    if os.path.exists(os.path.join(camp_root, "imports", "bmad")):
        die("a local import must be layered in place, not copied into imports/")

    # 2. The roles pack, bound DIRECTLY as `gc` — the same binding bmad pulled
    # in transitively. The direct import overrides it (§7.1) and the transitive
    # formula layer must survive underneath (D8).
    roles = os.path.abspath(os.path.join(root, "gascity", "roles"))
    camp(camp_root, "import", "add", roles, "--name", "gc")
    if not os.path.isdir(os.path.join(trans, "formulas")):
        die("the direct `gc` import clobbered the transitive gascity layer")

    # 3. Everything the camp now declares must check out, and its orders must
    # compile — which resolves formulas through the transitive layer.
    camp(camp_root, "import", "check")
    camp(camp_root, "order", "ls")
finally:
    shutil.rmtree(work, ignore_errors=True)

ref = open(os.path.join(os.path.dirname(__file__), "GCPACKS_REF")).read().strip()
print("GCPACKS gate OK at", ref, "(shape + camp-loader)")
#!/usr/bin/env python3
# usage: python3 ci/gc-compat/load_corpus_packs.py <corpus-checkout>
# Phase-1 compat gate (compat spec §10/§14): assert the Gas City pack corpus
# loads the way phase 1 needs — pack/agent/transitive LOADING, not formula
# compilation (the §9 rung table is phase 2). Pinned to GCPACKS_REF. Exit
# non-zero on any drift. The corpus tree is NEVER vendored (umbrella §10):
# CI fetches at GCPACKS_REF.
import glob
import os
import sys
import tomllib

root = sys.argv[1]


def die(m):
    print("GCPACKS gate FAIL:", m)
    sys.exit(1)


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

ref = open(os.path.join(os.path.dirname(__file__), "GCPACKS_REF")).read().strip()
print("GCPACKS gate OK at", ref)
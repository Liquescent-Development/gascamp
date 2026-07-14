#!/usr/bin/env python3
"""Independent arbiter of camp's formula "rung" counts (gascamp compat-2).

Simulates camp's formula pipeline over the real Gas City formula corpus and
prints per-rung loadable counts. Python 3 stdlib only; never imports camp and
never shells out. The seeds below are measured truth: this script exits
non-zero if the corpus (or the model) drifts away from them.

    python3 ci/gc-compat/rungs.py <corpus-root> [--json]
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path

# --- the model -------------------------------------------------------------

BASE_TOP = {"description", "formula", "requires", "steps"}
BASE_STEP = {"assignee", "check", "description", "id", "needs", "on_complete", "retry",
             "timeout", "title"}
DEAD_TOP = {"version", "target_required", "internal", "mode", "single_lane",
            "sling_container_mode"}
ANNO_TOP = {"notes", "catalog", "metadata"}
ANNO_STEP = {"notes", "tags", "priority"}

RUNGS = [
    ("2a", {"contract"},         {"description_file", "metadata"}),
    ("2b", {"vars"},             {"condition"}),
    ("2c", {"extends"},          set()),
    ("2d", {"type", "template"}, {"expand", "expand_vars", "children"}),
    ("2e", set(),                {"drain"}),
]

# Keys that never make a formula fail the subset check.
EXCLUDED = DEAD_TOP | ANNO_TOP | ANNO_STEP

SEED_RUNGS = {"2a": 2, "2b": 31, "2c": 49, "2d": 76, "2e": 95}
# Operator ruling E (2026-07-13). The PREDICATE changed — from `contract` alone
# to gc's real "contract OR [requires] formula_compiler" — and the number FOLLOWED
# it. The seed was not tuned to the code; the rule was corrected and re-derived.
# The three formulas this adds are mol-idea-to-plan, mol-refinery-patrol and
# mol-review-leg.
SEED_RUNNABLE = 65
SEED_REFUSED = {
    "mol-digest-generate.toml": "phase",
    "mol-pr-from-issue.formula.toml": "phase",
    "design-review.formula.toml": "gc.kind",
    "same-session-implement.formula.toml": "context",
    "mol-polecat-work.toml": "extends",
}

UNDEFINED = object()


def accepted_at(rung: str) -> set[str]:
    acc = set(BASE_TOP) | set(BASE_STEP)
    for name, top, step in RUNGS:
        acc |= top | step
        if name == rung:
            break
    return acc


# --- corpus ----------------------------------------------------------------

class Formula:
    def __init__(self, path: Path, data: dict):
        self.path = path
        self.basename = path.name
        self.name = path.name[: -len(".toml")]
        if self.name.endswith(".formula"):
            self.name = self.name[: -len(".formula")]
        self.data = data

    @property
    def parents(self) -> list[str]:
        ext = self.data.get("extends")
        if ext is None:
            return []
        if isinstance(ext, str):
            return [ext]
        return list(ext)


def load_corpus(root: Path) -> dict[str, Formula]:
    corpus: dict[str, Formula] = {}
    for path in sorted(root.glob("**/formulas/*.toml")):
        with path.open("rb") as fh:
            data = tomllib.load(fh)
        f = Formula(path, data)
        if f.name in corpus:
            raise SystemExit(f"duplicate formula name {f.name!r}: {path} and {corpus[f.name].path}")
        corpus[f.name] = f
    return corpus


class ChainError(Exception):
    pass


def chain(f: Formula, corpus: dict[str, Formula]) -> list[Formula]:
    """Deepest ancestor first, F last. Raises ChainError on missing/cyclic parent."""
    out: list[Formula] = []
    seen: set[str] = set()

    def visit(cur: Formula, stack: tuple[str, ...]) -> None:
        if cur.name in stack:
            raise ChainError("extends")
        for pname in cur.parents:
            parent = corpus.get(pname)
            if parent is None:
                raise ChainError("extends")
            visit(parent, stack + (cur.name,))
        if cur.name not in seen:
            seen.add(cur.name)
            out.append(cur)

    visit(f, ())
    return out


# --- step trees ------------------------------------------------------------

def walk_steps(step_list):
    """Yield every step table in the recursive step tree."""
    for step in step_list or []:
        if not isinstance(step, dict):
            continue
        yield step
        yield from walk_steps(step.get("children"))


def merge_step_lists(base, override):
    """Child step with a matching id replaces the parent step in place."""
    merged = list(base)
    index = {s.get("id"): i for i, s in enumerate(merged) if isinstance(s, dict) and s.get("id")}
    for step in override or []:
        sid = step.get("id") if isinstance(step, dict) else None
        if sid is not None and sid in index:
            merged[index[sid]] = step
        else:
            if sid is not None:
                index[sid] = len(merged)
            merged.append(step)
    return merged


def merged_view(ch: list[Formula]):
    """Return (top_keys, step_keys, top_values, vars, steps, template)."""
    top_keys: set[str] = set()
    step_keys: set[str] = set()
    top_values: dict = {}
    variables: dict = {}
    steps: list = []
    template: list = []

    for f in ch:
        d = f.data
        top_keys |= set(d.keys())
        for step in walk_steps(d.get("steps")):
            step_keys |= set(step.keys())
        for step in walk_steps(d.get("template")):
            step_keys |= set(step.keys())

        for k, v in d.items():
            top_values[k] = v

        for name, spec in (d.get("vars") or {}).items():
            if isinstance(spec, dict):
                variables[name] = spec["default"] if "default" in spec else UNDEFINED
            else:
                variables[name] = spec

        if "steps" in d:
            steps = merge_step_lists(steps, d["steps"])
        if "template" in d:
            template = merge_step_lists(template, d["template"])

    return top_keys, step_keys, top_values, variables, steps, template


# --- conditions ------------------------------------------------------------

def eval_condition(cond: str, variables: dict) -> bool:
    if not isinstance(cond, str):
        return True
    if "!=" in cond:
        lhs, rhs, negate = cond.split("!=", 1) + [True]
    elif "==" in cond:
        lhs, rhs, negate = cond.split("==", 1) + [False]
    else:
        raise SystemExit(f"unsupported condition grammar: {cond!r}")

    lhs = lhs.strip()
    if not (lhs.startswith("{{") and lhs.endswith("}}")):
        raise SystemExit(f"unsupported condition LHS: {cond!r}")
    var = lhs[2:-2].strip()

    rhs = rhs.strip().strip("\"'")

    value = variables.get(var, UNDEFINED)
    equal = False if value is UNDEFINED else str(value) == rhs
    return (not equal) if negate else equal


def surviving_steps(step_list, variables):
    for step in step_list or []:
        if not isinstance(step, dict):
            continue
        if "condition" in step and not eval_condition(step["condition"], variables):
            continue
        yield step
        yield from surviving_steps(step.get("children"), variables)


# --- refusals --------------------------------------------------------------

def step_refusal(step: dict) -> str | None:
    meta = step.get("metadata")
    if isinstance(meta, dict):
        if meta.get("gc.kind") == "scope":
            return "gc.kind"
        for key in meta:
            if key.startswith("gc.scope_"):
                return key
    drain = step.get("drain")
    if isinstance(drain, dict):
        if drain.get("context") == "shared":
            return "context"
        if "continuation_group" in drain:
            return "continuation_group"
        if "max_units" in drain:
            return "max_units"
    return None


def refusal_of(f: Formula, corpus) -> tuple[str | None, dict | None]:
    """Return (refusal-key or None, merged view or None)."""
    try:
        ch = chain(f, corpus)
    except ChainError as exc:
        return str(exc), None

    top_keys, step_keys, top_values, variables, steps, template = merged_view(ch)
    view = {
        "top_keys": top_keys,
        "step_keys": step_keys,
        "top_values": top_values,
    }

    if "phase" in top_keys:
        return "phase", view

    for step in surviving_steps(steps, variables):
        key = step_refusal(step)
        if key:
            return key, view
    for step in surviving_steps(template, variables):
        key = step_refusal(step)
        if key:
            return key, view

    return None, view


# --- rungs -----------------------------------------------------------------

def analyze(root: Path):
    corpus = load_corpus(root)

    refused: dict[str, str] = {}
    views: dict[str, dict] = {}
    for f in corpus.values():
        key, view = refusal_of(f, corpus)
        if key is not None:
            refused[f.basename] = key
        if view is not None:
            views[f.basename] = view

    counts: dict[str, int] = {}
    loadable: dict[str, list[str]] = {}
    for rung, _, _ in RUNGS:
        acc = accepted_at(rung) | EXCLUDED
        names = []
        for f in corpus.values():
            if f.basename in refused:
                continue
            view = views[f.basename]
            if (view["top_keys"] | view["step_keys"]) <= acc:
                names.append(f.basename)
        loadable[rung] = sorted(names)
        counts[rung] = len(names)

    # D1 (operator ruling E) — gc's REAL predicate: a formula declares the graph
    # compiler by EITHER spelling. gc's `directFormulaCompilerConstraints`
    # (requirements.go:137-149) emits a constraint for `contract = "graph.v2"`
    # AND for `[requires] formula_compiler`, and `UsesGraphCompiler` is true for
    # either.
    #
    # This is also camp's OWN S11 rule. Gating runnability on `contract` alone
    # would leave camp VALIDATING a formula as a graph formula (its check/retry
    # steps legal) and then REFUSING TO RUN it as one — mol-idea-to-plan,
    # mol-refinery-patrol and mol-review-leg, all of which gc runs happily.
    #
    # (Origin-scoping is the other half of ruling E: camp-LOCAL formulas are
    # exempt from the gate entirely. Every corpus formula is IMPORTED, so it does
    # not move this count.)
    runnable = 0
    for basename in loadable["2e"]:
        tv = views[basename]["top_values"]
        declares_compiler = tv.get("contract") == "graph.v2" or bool(
            (tv.get("requires") or {}).get("formula_compiler")
        )
        if declares_compiler and tv.get("type") != "expansion":
            runnable += 1

    return counts, runnable, loadable["2e"], refused


def main() -> int:
    ap = argparse.ArgumentParser(description="Independent arbiter of camp formula rung counts.")
    ap.add_argument("corpus_root", help="root of the Gas City pack corpus")
    ap.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = ap.parse_args()

    counts, runnable, loadable_2e, refused = analyze(Path(args.corpus_root))

    if args.json:
        print(json.dumps({
            "rungs": counts,
            "runnable": runnable,
            "loadable_2e": loadable_2e,
            "refused": refused,
        }, indent=2, sort_keys=True))
    else:
        print("rung  loadable")
        for rung, _, _ in RUNGS:
            print(f"  {rung}  {counts[rung]:>3}")
        print(f"\nRUNNABLE  {runnable}")
        print(f"\nrefused ({len(refused)}):")
        for basename in sorted(refused):
            print(f"  {basename:<40} {refused[basename]}")

    ok = True
    if counts != SEED_RUNGS:
        print(f"SEED MISMATCH: rungs {counts} != {SEED_RUNGS}", file=sys.stderr)
        ok = False
    if runnable != SEED_RUNNABLE:
        print(f"SEED MISMATCH: runnable {runnable} != {SEED_RUNNABLE}", file=sys.stderr)
        ok = False
    if refused != SEED_REFUSED:
        print(f"SEED MISMATCH: refused {refused} != {SEED_REFUSED}", file=sys.stderr)
        ok = False
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

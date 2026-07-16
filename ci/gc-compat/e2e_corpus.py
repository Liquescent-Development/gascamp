#!/usr/bin/env python3
"""usage: e2e_corpus.py <corpus-checkout> <camp-binary>

BD8's PROOF. Nothing else in this phase COOKS an imported corpus formula and
RUNS it: `formula_gate.py` only compiles, `differential.py` diffs compilers, and
the drain fixtures are a camp-local pack. That is exactly why the pinned-formula
round-trip could be DEAD in every one of the 65 runnable corpus runs with no gate
able to see it — `load_run` re-parsed the authored `.toml` with no layers and no
config, `ctx()` turned the error into `None`, and every caller dead-ended the run.

So this gate SLINGS TWO REAL CORPUS FORMULAS and watches campd actually move them.

  1. `bmad-build` — chosen deliberately: it is IMPORTED, it `extends` a gascity
     parent, it carries `description_file`, it has a `{{implementation_target}}`
     route, AND it has `check` steps. It is the only corpus formula that exercises
     BD8 *and* BD-A's attempt-route path in one run.

  2. `superpowers-development` — because `bmad-build` only HALF-covers the route
     claim. Its only residual `{{}}` route sits on `bmad-build.implement`, which is
     the DRAIN anchor — and BD3's own fix makes that campd-held: it creates no
     attempt and dispatches no worker. All its check/retry steps carry LITERAL
     routes, so assertion 4 there proves "the attempt bead is routed at all" (real,
     and BD-A's core) but NOT "the attempt bead carries the SUBSTITUTED route".
     `superpowers-development.implement` is a RALPH step with a residual
     `{{implementation_target}}` route, so its ATTEMPT bead must come out
     substituted AND binding-resolved.
"""

import collections
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

# The workers campd must dispatch for the two runs, counted as DISTINCT BEADS. A BD-A
# regression does not make a worker BAD — it makes a worker NOT EXIST, so the count is
# the only thing that can see it (survivorship bias: the failure removes beads from the
# population).
#
# It must be distinct BEADS, not `session.woke` EVENTS. Counting events let a duplicate
# wake SUBSTITUTE for a missing worker — 13 real + 1 duplicate = 14 = PASS — which is
# the same survivorship hole one level up, hiding inside the instrument built to close
# it. Measured, not reasoned: with a bead's wake deleted and another's duplicated, the
# event-counting gate returned exit 0.
#
# 14 is a QUIESCENT count, not a sample of a moving system: locally it is reached by
# t=5s and is stable to t=90s with zero duplicates across 12 consecutive runs.
EXPECTED_DISPATCHED = 14

# The ROUTE each of those 14 dispatches resolves to, as an agent multiset — pinned
# PER RUN (issue #102). The count above proves 14 workers EXIST; this proves each went
# to the RIGHT agent IN THE RIGHT RUN — BD-A is a routing defect, so a valid-but-wrong
# agent (same count) must fail, AND a correctly-agented dispatch attributed to the WRONG
# one of the two runs must fail too. A single global multiset is BLIND to that swap:
# `gc.run-operator` occurs in BOTH runs, so a bmad-build gc.run-operator miscounted
# under superpowers-development leaves the global multiset unchanged and slips through.
# Splitting the pin by run closes it — a swap moves a count from one run's multiset to
# the other's. DERIVED from the real per-run dispatch stream on the pinned GCPACKS_REF
# (measured 1/1 for the two gc.run-operator), identical across repeated runs, NOT
# hand-transcribed. Keyed by FORMULA (stable) rather than the ledger-assigned run_id
# (dynamic); the run_id is mapped back to its formula at check time via `results`.
EXPECTED_AGENTS_BY_FORMULA = {
    "bmad-build": collections.Counter(
        {
            "bmad.acceptance-auditor": 1,
            "bmad.blind-hunter-reviewer": 1,
            "bmad.bmad-review-synthesizer": 1,
            "bmad.edge-case-reviewer": 1,
            "bmad.prd-writer": 1,
            "bmad.story-implementer": 1,
            "bmad.story-self-checker": 1,
            "gc.run-operator": 1,
        }
    ),
    "superpowers-development": collections.Counter(
        {
            "gc.run-operator": 1,
            "superpowers.code-quality-reviewer": 1,
            "superpowers.implementer": 3,
            "superpowers.spec-reviewer": 1,
        }
    ),
}
# The global multiset is now DERIVED from the per-run pins (single source of truth), so
# the import-time count invariant still guards the pinned numbers: the two runs' routes
# must sum to exactly EXPECTED_DISPATCHED.
EXPECTED_AGENTS = sum(EXPECTED_AGENTS_BY_FORMULA.values(), collections.Counter())
assert sum(EXPECTED_AGENTS.values()) == EXPECTED_DISPATCHED, (
    f"EXPECTED_AGENTS sums to {sum(EXPECTED_AGENTS.values())}, not {EXPECTED_DISPATCHED}"
)

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

if len(sys.argv) != 3:
    print(__doc__)
    sys.exit(2)

corpus = os.path.abspath(sys.argv[1])
camp_bin = os.path.abspath(sys.argv[2])
here = os.path.dirname(os.path.abspath(__file__))
fake_agent = os.path.join(
    os.path.dirname(here), "..", "crates", "camp", "tests", "fake-agent.sh"
)
fake_agent = os.path.abspath(fake_agent)


def die(msg):
    print("E2E gate FAIL:", msg)
    sys.exit(1)


work = tempfile.mkdtemp(prefix="e2e-corpus-")
campd = None
try:
    subprocess.run(
        [camp_bin, "init", "--no-service", "--no-import"],
        cwd=work,
        capture_output=True,
        check=True,
    )
    root = os.path.join(work, ".camp")
    rig = os.path.join(work, "repo")
    os.makedirs(rig, exist_ok=True)
    # The rig must be a real git repo with a base commit: campd cuts a worktree
    # per dispatched worker.
    git_env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "e2e",
        "GIT_AUTHOR_EMAIL": "e2e@example.com",
        "GIT_COMMITTER_NAME": "e2e",
        "GIT_COMMITTER_EMAIL": "e2e@example.com",
    }
    for argv in (
        ["git", "init", "-q"],
        # `-c commit.gpgsign=false`: the gate must not depend on the developer's
        # signing setup (CI has none; a local machine may).
        [
            "git",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    ):
        r = subprocess.run(argv, cwd=rig, capture_output=True, text=True, env=git_env)
        if r.returncode != 0:
            die(f"{' '.join(argv)}: {r.stderr.strip()}")

    def camp(*argv, check=True):
        r = subprocess.run(
            [camp_bin, "--camp", root, *argv], capture_output=True, text=True
        )
        if check and r.returncode != 0:
            die(f"camp {' '.join(argv)} exited {r.returncode}: {r.stderr.strip()}")
        return r

    with open(os.path.join(root, "camp.toml"), "a") as fh:
        fh.write(
            f'\n[[rigs]]\nname = "gc"\npath = "{rig}"\nprefix = "gc"\n\n'
            f'[agent_defaults]\ntools = ["Read", "Bash", "Skill"]\n\n'
            f'[dispatch]\nmax_workers = 4\ncommand = "{fake_agent}"\n'
        )
    for pack in FORMULA_PACKS:
        camp("import", "add", os.path.join(corpus, pack), "--name", pack)
    camp("import", "add", os.path.join(corpus, "gascity", "roles"), "--name", "gc")

    # campd must be UP before `camp sling` — sling promises dispatch and pokes it.
    campd = subprocess.Popen(
        [camp_bin, "--camp", root, "daemon"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env={**os.environ, "CAMP_BIN": camp_bin},
    )
    campd.stdout.readline()  # the socket line: campd is listening

    def events(kind=None):
        out = camp("events", "--json").stdout
        evs = [json.loads(l) for l in out.splitlines() if l.strip()]
        return [e for e in evs if kind is None or e["type"] == kind]

    def settle(seconds=15):
        deadline = time.time() + seconds
        while time.time() < deadline:
            time.sleep(0.3)
        return

    def sling(name):
        r = camp("sling", "--formula", name)
        return r.stdout.split()[0]

    results = {}
    for formula in ("bmad-build", "superpowers-development"):
        run_id = sling(formula)
        results[formula] = run_id

        # (2) the recipe is pinned, versioned, and its steps ARE the manifest's.
        run_dir = os.path.join(root, "runs", run_id)
        recipe_path = os.path.join(run_dir, "recipe.json")
        if not os.path.isfile(recipe_path):
            die(f"{formula}: no recipe.json at {recipe_path}")
        recipe = json.load(open(recipe_path))
        if recipe.get("recipe_version") != 1:
            die(f"{formula}: recipe_version {recipe.get('recipe_version')!r} != 1")
        manifest = json.load(open(os.path.join(run_dir, "manifest.json")))
        rsteps = {s["id"] for s in recipe["formula"]["steps"]}
        msteps = set(manifest["steps"])
        if rsteps != msteps:
            die(f"{formula}: recipe steps {sorted(rsteps)} != manifest {sorted(msteps)}")

    # (1) both runs COOKED.
    cooked = {e["data"]["formula"] for e in events("run.cooked")}
    for formula in results:
        if formula not in cooked:
            die(f"{formula} never cooked: {sorted(cooked)}")

    settle()

    # (3) ⭐ campd DID NOT DEAD-END THE RUN, AND DID NOT FAIL A SINGLE DISPATCH.
    #
    # This used to scan `dispatch.failed` for the substrings "recipe" / "load_run" /
    # "pinned formula" — so campd's ACTUAL unrouted-dispatch reason ("no agent to
    # dispatch to: bead has no assignee …") contained NONE of them and was SILENTLY
    # SKIPPED. The gate that exists to prove BD-A could not fail on BD-A.
    #
    # There is no such thing as an acceptable `dispatch.failed` in this gate: campd
    # is driving two healthy corpus formulas against a fake agent. ANY of them is a
    # failure, and it is reported with its own reason rather than filtered by one.
    failed = events("dispatch.failed")
    if failed:
        reasons = "\n    ".join(
            str(e.get("data", {}).get("reason", "")) for e in failed
        )
        die(f"campd failed {len(failed)} dispatch(es):\n    {reasons}")

    # (3b) ⭐ NO WORKER MAY HAVE CRASHED. Same principle as `dispatch.failed` above,
    # and it was missing: campd is driving two healthy corpus formulas against a fake
    # agent that always passes, so a crashed session has no innocent explanation. It is
    # also the state most likely to make campd re-wake a bead, which is exactly the
    # anomaly (4) exists to catch — so if both fire, this one names the cause.
    crashed = events("session.crashed")
    if crashed:
        # The FULL payload, not just `reason`: an uncaused SIGKILL has no `reason` and
        # would print a literal `None`, naming the session but not the cause. The payload
        # carries `cause_seq` and the signal/exit fields that distinguish a patrol
        # restart from an uncaused kill — the very distinction this assertion exists to
        # surface when it and (4) fire together.
        detail = "\n    ".join(
            f"{e['data'].get('name')}: {json.dumps(e['data'])}" for e in crashed
        )
        die(f"campd crashed {len(crashed)} session(s):\n    {detail}")

    # (4) ⭐ THE BEAD CAMPD DISPATCHES IS ROUTED — asserted POSITIVELY, on a NAMED
    # bead, with an EXPECTED COUNT OF DISTINCT BEADS.
    #
    # The old loop iterated `session.woke` and checked that every worker that DID
    # dispatch carried an agent. That is SURVIVORSHIP BIAS: an unrouted attempt bead
    # never dispatches, so it emits no `session.woke` and is INVISIBLE. The BD-A
    # regression REMOVES beads from the inspected population instead of adding bad
    # ones — the gate counted the survivors and asked whether they looked healthy,
    # while the bug killed them before they were counted.
    #
    # So: pin the COUNT (a silently-missing worker now fails), and assert on a NAMED
    # bead whose route only resolves if BD-A is fixed.
    #
    # ⭐ AND COUNT DISTINCT BEADS, NOT EVENTS. The count above was `len(events)`, which
    # reopened the very survivorship hole this comment claims to have closed: 13 real
    # dispatches + 1 duplicate `session.woke` = 14 = PASS, with a worker GENUINELY
    # MISSING. The instrument could be satisfied by a bead being woken twice. One bead,
    # one worker — so the population is the SET of beads.
    #
    # The duplicate is then checked SEPARATELY rather than tolerated. Counting the set
    # alone would make a double-wake invisible, and a gate that hides a double dispatch
    # is the same sin one layer down: two workers on one bead is a PRODUCT bug, not a
    # rounding error. With `dispatch.failed` and `session.crashed` both asserted empty
    # above, no retry or recovery can legitimately re-wake a bead here — so a repeat is
    # an anomaly, and it FAILS, naming the bead.
    #
    # This is what a real CI failure (15 events / expected 14) could not tell us: the
    # gate printed only the distinct AGENT set, and 14 dispatches already span just 11
    # agents, so a 15th event on an existing agent was invisible. The evidence for the
    # question the gate itself raised had been discarded. It now prints the beads.
    woke = events("session.woke")
    dispatched = [e for e in woke if e["data"].get("bead")]
    # A bead-less wake is FAILED, not filtered. The old `[… if …get("bead")]` silently
    # discarded any `session.woke` with no bead — "the evidence for the question the gate
    # raised had been discarded", reproduced in the filter that builds the gate's own
    # population. A real dispatch always names its bead; a wake without one is itself the
    # anomaly, so it fails here rather than shrinking the population it is counted in.
    if len(woke) != len(dispatched):
        beadless = [e["data"] for e in woke if not e["data"].get("bead")]
        die(f"{len(beadless)} session.woke event(s) carry no bead: {beadless}")
    beads = [e["data"]["bead"] for e in dispatched]

    # Every dispatched bead must BELONG TO ONE OF THE TWO RUNS. `session.woke` carries no
    # run_id, so the map comes from `bead.created` (top-level `bead`, `data.run_id`). This
    # closes the `swap` gap — a bead relabelled to a foreign id, count and agent held —
    # which the agent-multiset check below does NOT catch (a pure bead-id relabel leaves
    # the route multiset unchanged). It is derived from the ledger, not from pinned `gc-N`
    # ids, so it is not brittle: it says "this bead is from a run we started", nothing
    # about which number it got.
    run_ids = set(results.values())
    bead_run = {
        e["bead"]: e["data"].get("run_id")
        for e in events("bead.created")
        if e.get("bead")
    }
    foreign = {b: bead_run.get(b) for b in set(beads) if bead_run.get(b) not in run_ids}
    if foreign:
        die(
            f"campd dispatched {len(foreign)} bead(s) belonging to NO run we started "
            f"(run_ids={sorted(run_ids)}): {foreign}"
        )

    repeats = {b: n for b, n in collections.Counter(beads).items() if n > 1}
    if repeats:
        die(
            f"campd woke MORE THAN ONE worker for {len(repeats)} bead(s): {repeats}. "
            f"One bead gets one worker. With no failed dispatch and no crashed session "
            f"(both asserted above), nothing can legitimately re-wake a bead — this is a "
            f"double dispatch, and counting the bead SET instead of the events would "
            f"have hidden it.\n"
            f"  woke events: {len(beads)}  distinct beads: {len(set(beads))}\n"
            f"  beads: {sorted(beads)}"
        )
    if len(set(beads)) != EXPECTED_DISPATCHED:
        agents = sorted({e["data"].get("agent") or "<none>" for e in dispatched})
        die(
            f"campd dispatched {len(set(beads))} distinct worker(s), expected "
            f"{EXPECTED_DISPATCHED}. A BD-A regression does not produce a BAD worker "
            f"— it produces NO worker, so the count is the only thing that sees it.\n"
            f"  beads: {sorted(set(beads))}\n"
            f"  agents: {agents}"
        )
    for e in dispatched:
        agent = e["data"].get("agent") or ""
        if not agent:
            die(f"campd dispatched an UNROUTED worker for bead {e['data']['bead']} (BD-A)")
        if "{{" in agent:
            die(f"campd dispatched with an unsubstituted route {agent!r} (BD-A)")

    # ⭐ PIN THE ROUTE IDENTITY PER RUN, not just its COUNT. Everything above proves 14
    # distinct workers, each with SOME non-empty, substituted route — and is BLIND to
    # WHICH agent each bead went to, and UNDER WHICH RUN. BD-A is a ROUTING defect: a
    # binding-namespace regression can resolve a var to a valid-but-WRONG agent (the
    # corpus makes `superpowers.implementer` vs `bmad.story-implementer` confusable),
    # yielding a real name, non-empty, no `{{`, count still 14 — and the count gate above
    # waves it through. Measured: re-pointing all 13 non-named beads to
    # `bmad.story-implementer` left the count-and-shape gate GREEN.
    #
    # The mapping is fully deterministic at the pinned GCPACKS_REF, so pin the per-run
    # AGENT MULTISET. Each dispatched bead is attributed to its run via its AUTHORITATIVE
    # `bead.created.run_id` (already collected in `bead_run` above, and every dispatched
    # bead was proven non-`foreign` — i.e. present in `bead_run` with one of our two
    # run_ids). The run_id is mapped back to the formula it was slung from via `results`.
    # This closes `misroute` (wrong agent — the multiset changes), `swap` (foreign bead —
    # it changes the multiset), AND the #102 residual: a correctly-agented dispatch
    # attributed to the WRONG one of the two runs (the GLOBAL multiset is unchanged, but a
    # count moves between the two per-run multisets, so the per-run check fails).
    formula_of_run = {run_id: formula for formula, run_id in results.items()}
    got_by_formula = collections.defaultdict(collections.Counter)
    for e in dispatched:
        formula = formula_of_run.get(bead_run.get(e["data"]["bead"]))
        got_by_formula[formula][e["data"]["agent"]] += 1
    # Iterate the UNION of expected and observed formulas: a dispatch attributed to an
    # unexpected run (formula not in the pin, or `None`) surfaces as an extra bucket the
    # expected side does not have, rather than being silently dropped.
    for formula in sorted(
        set(EXPECTED_AGENTS_BY_FORMULA) | set(got_by_formula), key=lambda f: f or ""
    ):
        expected = EXPECTED_AGENTS_BY_FORMULA.get(formula, collections.Counter())
        got = got_by_formula.get(formula, collections.Counter())
        if got != expected:
            die(
                f"run {formula!r}: dispatch ROUTES do not match the pinned per-run "
                f"corpus multiset. Same GLOBAL count/agents can still hide a misroute to "
                f"a valid-but-wrong agent, or a dispatch attributed to the WRONG run "
                f"(BD-A is a routing defect; #102 is the cross-run residual).\n"
                f"  unexpected/extra: {dict(got - expected)}\n"
                f"  missing:          {dict(expected - got)}"
            )

    # ⭐ THE NAMED ONE, and its scope stated HONESTLY.
    #
    # `superpowers-development.implement` is a ralph step whose route is a residual
    # `{{implementation_target}}` in gc's own Recipe. The bead campd dispatches for it
    # is the ATTEMPT — a DIFFERENT bead from the anchor cook routed — and its agent is
    # `superpowers.implementer` only if cook substituted the var into the pinned recipe
    # AND resolved it through the binding namespace.
    #
    # ⚠️ It does NOT, on its own, catch the BD-A regression: measured, the workers that
    # regression kills are the `bmad.prd-writer` attempts, and THIS bead still
    # dispatches. An earlier version of this comment claimed "if BD-A were unfixed this
    # worker would not exist at all" — that sentence was FALSE, and a false claim in a
    # gate's own failure message is the exact class of defect this wave keeps finding.
    #
    # BD-A is caught by the two prongs above (ANY `dispatch.failed`, and the exact
    # dispatched COUNT), each of which fails on it independently. This prong pins the
    # SUBSTITUTED-AND-BOUND route positively, on a named bead, and that is all it
    # claims.
    sd_run = results["superpowers-development"]
    sd_anchor = json.load(
        open(os.path.join(root, "runs", sd_run, "manifest.json"))
    )["steps"]["implement"]
    ls_all = json.loads(camp("ls", "--json").stdout or "[]")
    by_id = {b["id"]: b for b in ls_all}
    attempt_agents = {
        e["data"]["agent"]
        for e in dispatched
        if by_id.get(e["data"]["bead"], {}).get("id") != sd_anchor
        and e["data"]["agent"] == "superpowers.implementer"
    }
    if "superpowers.implementer" not in attempt_agents:
        die(
            "superpowers-development.implement's ATTEMPT bead was not dispatched to "
            "`superpowers.implementer`. Its route is a residual "
            "`{{implementation_target}}` in gc's own Recipe, so this bead's agent is "
            "correct ONLY IF cook substituted the var into the pinned recipe AND "
            "resolved it through the binding namespace (BD-A: cook routed the ANCHOR, "
            "which is never dispatched — the ATTEMPT is a different bead). "
            f"agents seen: {sorted({e['data']['agent'] for e in dispatched})}"
        )
    routed = dispatched

    # And no bead anywhere may carry an unsubstituted route.
    ls = json.loads(camp("ls", "--json").stdout or "[]")
    for b in ls:
        a = b.get("assignee") or ""
        if "{{" in a:
            die(f"bead {b['id']} carries an unsubstituted route {a!r} (BD-A)")

    print(
        f"E2E gate OK: cooked and ran {len(results)} imported corpus formulas "
        f"({', '.join(sorted(results))}); recipe_version 1; no run dead-ended on "
        f"its pinned formula; campd dispatched {len(routed)} worker(s), every one "
        f"carrying a SUBSTITUTED, binding-resolved route "
        f"({sorted({e['data']['agent'] for e in routed})})"
    )
finally:
    if campd is not None:
        subprocess.run(
            [camp_bin, "--camp", os.path.join(work, ".camp"), "stop"],
            capture_output=True,
        )
        try:
            campd.wait(timeout=10)
        except subprocess.TimeoutExpired:
            campd.kill()
    shutil.rmtree(work, ignore_errors=True)

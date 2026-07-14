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

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

# The workers campd must dispatch for the two runs. A BD-A regression does not make
# a worker BAD — it makes a worker NOT EXIST, so the count is the only thing that can
# see it (survivorship bias: the failure removes beads from the population).
EXPECTED_DISPATCHED = 14

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

    # (4) ⭐ THE BEAD CAMPD DISPATCHES IS ROUTED — asserted POSITIVELY, on a NAMED
    # bead, with an EXPECTED COUNT.
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
    woke = events("session.woke")
    dispatched = [e for e in woke if e["data"].get("bead")]
    if len(dispatched) != EXPECTED_DISPATCHED:
        agents = sorted({e["data"].get("agent") or "<none>" for e in dispatched})
        die(
            f"campd dispatched {len(dispatched)} worker(s), expected "
            f"{EXPECTED_DISPATCHED}. A BD-A regression does not produce a BAD worker "
            f"— it produces NO worker, so the count is the only thing that sees it.\n"
            f"  agents: {agents}"
        )
    for e in dispatched:
        agent = e["data"].get("agent") or ""
        if not agent:
            die(f"campd dispatched an UNROUTED worker for bead {e['data']['bead']} (BD-A)")
        if "{{" in agent:
            die(f"campd dispatched with an unsubstituted route {agent!r} (BD-A)")

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

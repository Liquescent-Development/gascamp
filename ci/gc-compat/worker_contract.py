#!/usr/bin/env python3
"""usage: worker_contract.py <corpus-checkout> <camp-binary>

THE §14 WORKER-CONTRACT GATE. Nothing else in this repo runs the REAL
gc-role-worker fragment from the corpus against camp's shims. `e2e_corpus.py`
drives a fake agent that speaks camp's OWN CLI; this gate drives the corpus's
OWN 140-line bash claim protocol — its verbs, its inline `python3` json_pick,
its assignee/route comparisons — against `.camp/bin/gc` and `.camp/bin/bd`, and
proves a gc worker CLOSES a gc bead end to end:

    claim (gc hook --claim --json) -> bd show -> bd update gc.outcome -> bd close
      -> re-hook (drains) -> gc runtime drain-ack -> campd REAPS the worker.

The worker LINGERS after drain-ack (`exec sleep 600`) exactly as a real
`claude -p` does (it does not exit on EOF, P3), so campd's drain-ack ->
KillReleased is the ONLY thing that can reap it inside the deadline. A hang is
the failing signal: if that wiring regresses, the worker sleeps past the
deadline and this gate fails.

It also RE-DERIVES ci/gc-compat/fixtures/gc-role-worker.observed.json from the
live fragment and fails on drift, so moving GCPACKS_REF cannot silently change
the measured contract (§10: we commit only our derived facts, never the
fragment's source).
"""

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

DEADLINE = 20  # seconds; < the default release_grace (30s) so the grace backstop
# cannot mask a drain-ack->KillReleased regression.

if len(sys.argv) != 3:
    print(__doc__)
    sys.exit(2)

corpus = os.path.abspath(sys.argv[1])
camp_bin = os.path.abspath(sys.argv[2])
here = os.path.dirname(os.path.abspath(__file__))
repo = os.path.dirname(os.path.dirname(here))  # ci/gc-compat -> ci -> repo root
observed_path = os.path.join(here, "fixtures", "gc-role-worker.observed.json")

FRAGMENT_REL = "gascity/roles/template-fragments/gc-role-worker.template.md"
ROUTE = "gc.run-operator"  # a real gc pack agent; the fragment is its shared body


def die(msg):
    print("WORKER-CONTRACT gate FAIL:", msg)
    sys.exit(1)


# --------------------------------------------------------------------------
# python3 is a HARD gc-worker dependency (§6.1) and must be in the container.
# --------------------------------------------------------------------------
dockerfile = os.path.join(repo, "contrib", "docker", "Dockerfile")
if "python3" not in open(dockerfile).read():
    die(
        "python3 is a hard gc-worker dependency (§6.1: every gc pack agent's "
        f"fragment parses hook --claim --json with an inline python3) and must be "
        f"in the runtime image {dockerfile}"
    )


# --------------------------------------------------------------------------
# Render the REAL fragment into a runnable worker: extract its ```bash claim
# block VERBATIM (the load-bearing contract) and wrap it with the close/drain
# steps the fragment's prose prescribes. This IS the renderer (NB1) — not a
# camp `prime`; camp reads a pack agent's prompt raw.
# --------------------------------------------------------------------------
fragment_path = os.path.join(corpus, FRAGMENT_REL)
if not os.path.isfile(fragment_path):
    die(f"fragment not found at {fragment_path}")
fragment_src = open(fragment_path).read()

# Substitute the Go-template placeholders the block does not need but which
# appear in the file, and drop the define/end wrappers.
rendered = (
    fragment_src.replace("{{ .AgentName }}", ROUTE)
    .replace("{{ .TemplateName }}", ROUTE)
)

# The runnable claim protocol is the first ```bash fenced block (the GC_CLAIM
# heredoc). It claims, verifies assignee/route, and prints CLAIMED_BEAD_ID.
blocks = re.findall(r"```bash\n(.*?)\n```", rendered, re.DOTALL)
if not blocks:
    die("no ```bash block in the fragment — its shape changed; re-measure (Task 1)")
claim_block = blocks[0]
if "gc hook --claim --json" not in claim_block or "GC_CLAIM" not in claim_block:
    die("the first bash block is not the GC_CLAIM claim protocol — re-measure")


def worker_script(mode, claim_path):
    # Run the REAL claim block (captures CLAIMED_BEAD_ID from its stdout), then
    # follow the prose: set gc.outcome, close, re-hook (which drains + drain-
    # acks). The claim block is a `bash <<'GC_CLAIM'` heredoc; running it via
    # `sh <file>` (not inlined in a `$(...)`) keeps the heredoc off the outer
    # shell's parser — the Task-1 harness shape.
    fail = mode == "fail"
    close = (
        "bd update \"$WORK_ID\" --set-metadata 'gc.outcome=fail' "
        "--set-metadata 'gc.failure_class=work_error'\n"
        "bd close \"$WORK_ID\" --reason 'work failed'\n"
        if fail
        else "bd update \"$WORK_ID\" --set-metadata 'gc.outcome=pass'\n"
        "bd close \"$WORK_ID\"\n"
    )
    return (
        "#!/bin/sh\nset +e\n"
        f'OUT="$(sh {claim_path})"\n'
        "printf '%s\\n' \"$OUT\"\n"
        "WORK_ID=\"$(printf '%s\\n' \"$OUT\" | sed -n 's/^CLAIMED_BEAD_ID=//p')\"\n"
        # No work claimed (first hook drained) — the block already drain-acked.
        'if [ -z "$WORK_ID" ]; then exit 0; fi\n'
        f"{close}"
        # Continuation: re-run the REAL claim block; the bead is closed now, so
        # it drains and runs `gc runtime drain-ack`, then exits 0.
        f"sh {claim_path}\n"
    )


# --------------------------------------------------------------------------
# Drift guard: re-derive the observed facts from the LIVE fragment and compare
# to the committed fixture (Task 1). A moved GCPACKS_REF that changes the
# contract must update the fixture, not slip through.
# --------------------------------------------------------------------------
observed = json.load(open(observed_path))
static_pairs = sorted(set(re.findall(r"\b(?:gc|bd) [a-z][a-z-]*", fragment_src)))
if static_pairs != sorted(observed["verbs_static"]["pairs"]):
    die(
        "the fragment's static verb set DRIFTED from the committed measurement.\n"
        f"  live:      {static_pairs}\n"
        f"  committed: {sorted(observed['verbs_static']['pairs'])}\n"
        "Re-run Task 1's measurement and update "
        "ci/gc-compat/fixtures/gc-role-worker.observed.json."
    )
for field in observed["hook_json_fields"]["top_level"]:
    if f"json_pick {field}" not in fragment_src and f'json_pick("{field}")' not in fragment_src:
        # the fragment reads these via `| json_pick <field>`
        if re.search(rf"json_pick\s+{re.escape(field)}\b", fragment_src) is None:
            die(f"the fragment no longer reads hook field {field!r} — re-measure")


def run_mode(mode):
    """Drive real campd + a lingering fake-claude running the REAL fragment in
    `mode`; assert closed + drain_acked + reaped within DEADLINE."""
    # A SHORT tempdir: the campd unix socket path must fit SUN_LEN (~104).
    work = tempfile.mkdtemp(prefix=f"wc-{mode}-")
    campd = None
    try:
        subprocess.run(
            [camp_bin, "init", "--no-service", "--no-import"],
            cwd=work, capture_output=True, check=True,
        )
        root = os.path.join(work, ".camp")
        rig = os.path.join(work, "repo")
        os.makedirs(rig, exist_ok=True)
        git_env = {
            **os.environ,
            "GIT_AUTHOR_NAME": "wc", "GIT_AUTHOR_EMAIL": "wc@example.com",
            "GIT_COMMITTER_NAME": "wc", "GIT_COMMITTER_EMAIL": "wc@example.com",
        }
        for argv in (
            ["git", "init", "-q"],
            ["git", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "base"],
        ):
            r = subprocess.run(argv, cwd=rig, capture_output=True, text=True, env=git_env)
            if r.returncode != 0:
                die(f"{' '.join(argv)}: {r.stderr.strip()}")

        # The fake claude: run the rendered REAL fragment, then LINGER. The
        # REAL claim block lives in its own file (run via `sh`), verbatim.
        claim_sh = os.path.join(work, "claim_block.sh")
        open(claim_sh, "w").write(claim_block + "\n")
        worker_sh = os.path.join(work, "worker.sh")
        open(worker_sh, "w").write(worker_script(mode, claim_sh))
        fake = os.path.join(work, "fake-claude.sh")
        open(fake, "w").write(f"#!/bin/sh\nsh {worker_sh} 1>&2\nexec sleep 600\n")
        os.chmod(fake, 0o755)

        with open(os.path.join(root, "camp.toml"), "a") as fh:
            fh.write(
                f'\n[[rigs]]\nname = "gc"\npath = "{rig}"\nprefix = "gc"\n\n'
                f'[agent_defaults]\ntools = ["Read", "Bash"]\n\n'
                f'[dispatch]\nmax_workers = 4\ncommand = "{fake}"\n'
            )

        def camp(*argv, check=True):
            r = subprocess.run([camp_bin, "--camp", root, *argv], capture_output=True, text=True)
            if check and r.returncode != 0:
                die(f"camp {' '.join(argv)} exited {r.returncode}: {r.stderr.strip()}")
            return r

        # Import the gc role pack (the deployment recipe, §3/§7.3) so
        # `gc.run-operator` resolves and the fragment's route check passes.
        camp("import", "add", os.path.join(corpus, "gascity", "roles"), "--name", "gc")

        # start_new_session: campd leads its own process group, so cleanup can
        # killpg the WHOLE tree — campd AND the lingering `sleep 600` worker it
        # spawned (which inherits the group). NB3: no orphaned 600s sleep.
        campd = subprocess.Popen(
            [camp_bin, "--camp", root, "daemon"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env={**os.environ, "CAMP_BIN": camp_bin},
            start_new_session=True,
        )
        campd.stdout.readline()  # the socket line: campd is listening

        def events(kind=None):
            out = camp("events", "--json").stdout
            evs = [json.loads(l) for l in out.splitlines() if l.strip()]
            return [e for e in evs if kind is None or e["type"] == kind]

        r = camp("sling", "a real gc bead", "--agent", ROUTE)
        bead = r.stdout.split()[0] if r.stdout.split() else "gc-1"

        start = time.time()
        while True:
            evs = events()
            closed = any(e["type"] == "bead.closed" and e.get("bead") == bead for e in evs)
            drain_acked = any(e["type"] == "worker.drain_acked" for e in evs)
            reaped = any(e["type"] == "session.stopped" for e in evs)
            failed = [e for e in evs if e["type"] == "dispatch.failed"]
            crashed = [e for e in evs if e["type"] == "session.crashed"]
            if failed:
                die(f"[{mode}] campd failed a dispatch: "
                    f"{[e.get('data', {}).get('reason') for e in failed]}")
            if crashed:
                die(f"[{mode}] a worker crashed: {[e['data'] for e in crashed]}")
            if closed and drain_acked and reaped:
                return
            if time.time() - start > DEADLINE:
                die(
                    f"[{mode}] campd did not reap the drained gc worker within "
                    f"{DEADLINE}s: the drain-ack->KillReleased wiring regressed, or the "
                    f"real fragment hung against the shims. closed={closed} "
                    f"drain_acked={drain_acked} reaped={reaped}"
                )
            time.sleep(0.1)
    finally:
        if campd is not None:
            subprocess.run([camp_bin, "--camp", os.path.join(work, ".camp"), "stop"],
                           capture_output=True)
            try:
                campd.wait(timeout=10)
            except subprocess.TimeoutExpired:
                campd.kill()
            # Reap the whole group — the lingering `sleep 600` worker included.
            try:
                os.killpg(campd.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        shutil.rmtree(work, ignore_errors=True)


run_mode("happy")
run_mode("fail")
print(
    "WORKER-CONTRACT gate OK: the REAL gc-role-worker fragment claimed, closed, "
    "and drain-acked a gc bead against camp's shims on BOTH the happy and "
    "fail-close branches; campd reaped the lingering worker via drain-ack in each "
    "(a hang would have failed). Static verb set matches the committed measurement."
)

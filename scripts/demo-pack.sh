#!/usr/bin/env bash
# demo-pack.sh — prove the real Gas City v1 packs (bmad, gstack) LOAD, COMPILE,
# and COOK in camp, as an opt-in local verification anyone can re-run.
#
# This is a $0 verification: it spends NO Anthropic API money. It never starts
# campd and never spawns a worker — it cooks a formula into beads (durable in
# the ledger) and reads the resulting graph back. Live dispatch (real `claude`)
# is `make e2e`, a separate, operator-gated step.
#
# Usage:
#   scripts/demo-pack.sh                 # clone the pinned corpus into a temp dir
#   scripts/demo-pack.sh /path/to/gcpacks-src   # reuse an existing checkout
#
# Requirements: git, python3, cargo, and a network reachable GitHub (only when
# the corpus is not supplied). The gc real-compiler DIFFERENTIAL is run only
# when Go is on PATH and the gascity oracle can be built; otherwise it is
# skipped with a printed note (the camp-side gates still bind).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

GCPACKS_REF="$(cat ci/gc-compat/GCPACKS_REF)"
GASCITY_REF="$(cat ci/gc-compat/GASCITY_REF)"

# A SHORT work dir: a camp's control socket is <root>/campd.sock and a Unix
# domain socket path must fit SUN_LEN (~104 bytes on macOS). A deep scratch
# path overflows it, so the demo camp lives at a shallow, throwaway location.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/gcdemo.XXXXXX")"
CORPUS="${1:-}"
CLEANUP_CORPUS=0
trap 'rm -rf "$WORK"; [ "$CLEANUP_CORPUS" = 1 ] && rm -rf "$CORPUS_CLONE" || true' EXIT

say() { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }

say "1. build camp"
cargo build --bin camp
CAMP="$REPO_ROOT/target/debug/camp"
"$CAMP" --version

if [ -z "$CORPUS" ]; then
  say "2. fetch the real corpus at GCPACKS_REF ($GCPACKS_REF) — NOT vendored"
  CORPUS_CLONE="$WORK/gcpacks-src"
  git clone --quiet https://github.com/gastownhall/gascity-packs.git "$CORPUS_CLONE"
  git -C "$CORPUS_CLONE" fetch --quiet origin "$GCPACKS_REF"
  git -C "$CORPUS_CLONE" checkout --quiet "$GCPACKS_REF"
  CORPUS="$CORPUS_CLONE"
  CLEANUP_CORPUS=1
fi
echo "corpus: $CORPUS @ $(git -C "$CORPUS" rev-parse HEAD)"

say "3. canonical gates — pack/agent LOAD + the formula rungs (camp's compiler)"
python3 ci/gc-compat/load_corpus_packs.py "$CORPUS" "$CAMP"
python3 ci/gc-compat/rungs.py "$CORPUS"
python3 ci/gc-compat/formula_gate.py "$CORPUS" "$CAMP"

say "3b. DIFFERENTIAL — camp's compiler vs gc's REAL compiler (needs Go)"
if command -v go >/dev/null 2>&1; then
  GC_SRC="$WORK/gascity-src"
  git clone --quiet https://github.com/gastownhall/gascity.git "$GC_SRC"
  git -C "$GC_SRC" fetch --quiet origin "$GASCITY_REF"
  git -C "$GC_SRC" checkout --quiet "$GASCITY_REF"
  mkdir -p "$GC_SRC/cmd/factshim"
  cp ci/gc-compat/factshim.go "$GC_SRC/cmd/factshim/main.go"
  ( cd "$GC_SRC" && GOTOOLCHAIN=auto GOFLAGS=-mod=mod go build -o "$WORK/factshim" ./cmd/factshim )
  python3 ci/gc-compat/differential.py "$CORPUS" "$CAMP" "$WORK/factshim"
else
  echo "SKIP: Go not on PATH — the camp-side gates above still bind invariant 6"
fi

say "4. per-pack breakdown for bmad + gstack (loadable vs runnable vs expansion)"
CAMP_ROOT="$WORK/demo/.camp"
mkdir -p "$WORK/demo/rig"
( cd "$WORK/demo/rig" && git init -q )
( cd "$WORK/demo" && "$CAMP" init --no-service --no-import >/dev/null )
printf '\n[agent_defaults]\ntools = ["Read", "Bash", "Skill"]\n' >> "$CAMP_ROOT/camp.toml"
"$CAMP" --camp "$CAMP_ROOT" rig add "$WORK/demo/rig" >/dev/null
for pack in bmad gstack; do
  "$CAMP" --camp "$CAMP_ROOT" import add "$CORPUS/$pack" --name "$pack" >/dev/null
done
"$CAMP" --camp "$CAMP_ROOT" import add "$CORPUS/gascity/roles" --name gc >/dev/null
for pack in bmad gstack; do
  echo "--- $pack ---"
  printf '%-42s %-6s %-8s %s\n' formula loads runnable note
  for f in "$CORPUS/$pack/formulas"/*.toml; do
    "$CAMP" --camp "$CAMP_ROOT" doctor --formula "$f" --json 2>/dev/null | \
      python3 -c 'import sys,json,os
v=json.load(sys.stdin)
note=v.get("not_runnable",{}).get("reason","") if not v.get("runnable") else ""
note=("expansion" if "expansion" in note else note.split(":")[0]) if note else ""
print("%-42s %-6s %-8s %s"%(os.path.basename(v["path"]),v["ok"],v.get("runnable"),note))'
  done
done

say "5. cook a runnable formula into beads (bmad-build) and read the graph back"
set +e
"$CAMP" --camp "$CAMP_ROOT" sling --formula bmad-build 2>&1 | sed -n '1p'
set -e
echo "--- the cooked bead graph (camp ls) ---"
"$CAMP" --camp "$CAMP_ROOT" ls
echo "--- ready frontier (camp ls --ready): what campd would dispatch first ---"
"$CAMP" --camp "$CAMP_ROOT" ls --ready
echo "--- root bead + history (camp show) ---"
ROOT_BEAD="$("$CAMP" --camp "$CAMP_ROOT" ls --json | python3 -c 'import sys,json;print(sorted(json.load(sys.stdin),key=lambda b:b["created_ts"])[0]["id"])')"
"$CAMP" --camp "$CAMP_ROOT" show "$ROOT_BEAD"

say "PASS — real Gas City packs load, compile against gc, and cook in camp (\$0)"

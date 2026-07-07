# Phase 6 — gc Compatibility Gates in CI Implementation Plan

> Plan approved by Opus 4.8 plan review, 2026-07-07 (automated plan gate per
> operator directive), as relayed by the orchestration lead. Zero blocking
> findings; Task 8's required-check scope remains operator-bound.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Spec §15.2 contracts 1 (formula subset invariant) and 2 (vocabulary mirror) become CI checks: a `gc-compat` job validates the Phase 5 formula corpus against the real Gas City compiler at a pinned ref and cross-checks the event/outcome vocabulary against gc source.

**Architecture:** Gas City has no `gc formula validate` CLI (plan decision 3), so CI checks out `gastownhall/gascity` (public) at the SHA pinned in `ci/gc-compat/GASCITY_REF`, copies camp-owned `ci/gc-compat/camp_corpus_validate.go` into `<checkout>/cmd/camp-corpus-validate/main.go` (inside the module, so it may import `internal/formula`), and runs it over `crates/camp-core/tests/fixtures/formulas/valid`. A deliberately broken `selftest-invalid.toml` must make the shim exit 1 — a shim that always passes cannot go unnoticed. `check_vocab.sh` then re-extracts the vocabulary from gc source at the pinned ref and asserts the pin (`gc-vocab.json`) and camp's `CAMP_SPECIFIC_EVENTS` partition both hold (plan decision 4). Bumping `GASCITY_REF` is a deliberate PR; drift fails loudly.

**Tech Stack:** GitHub Actions (`actions/checkout@v4`, `actions/setup-go@v5`), Go (version from gascity's `go.mod`, currently 1.26.4), bash + `jq` + `comm` (all preinstalled on `ubuntu-latest`). No Rust code changes.

## Global Constraints

- Branch `phase-6-gc-compat-ci`; never commit to main; one reviewable PR; no co-author lines, no self-mention in commits; conventional-commit style (`ci:`, `docs:`, `chore:`).
- Gates before push: `cargo fmt --all --check` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace`.
- Fail fast: no fallbacks, no silenced errors. Every check in this phase must be *observed failing* on bad input before it is trusted (the CI-infrastructure analog of "watch the test fail").
- The mechanism is fixed by plan decisions 3 and 4 — do not redesign it. `camp_corpus_validate.go` is used **verbatim** from the master plan.
- `GASCITY_REF` must equal `_provenance.gascity_ref` in `crates/camp-core/tests/fixtures/gc-vocab.json`: `12410301884b51131a35e101a335dbaae16cdcb0`. The two must reference one ref.
- Owned surface: `ci/gc-compat/**` and `.github/workflows/ci.yml`. Do not touch files owned by in-flight siblings (phase-8, phase-10). If the lead announces a sibling merge, rebase onto current main and re-run all gates before continuing.

## Facts verified at planning time (2026-07-07, local checkout of gascity @ pin)

These were all verified by actually running the real compiler locally; the executor re-verifies each in its task but should not be surprised:

1. `internal/formula/compile.go:47` at the pin defines `func CompileWithoutRuntimeVarValidation(_ context.Context, name string, searchPaths []string, vars map[string]string) (*Recipe, error)` — exactly the signature the shim calls.
2. The shim compiled inside the checkout accepts all 5 valid fixtures (`diamond`, `fan-out`, `guarded-change`, `minimal`, `retry-fetch`) — exit 0.
3. gc at the pin **rejects**: missing step title, duplicate step id, `needs` referencing an unknown step. gc **accepts** camp's `cycle.toml` (camp is stricter; only camp-side tests cover those rows). The selftest fixture therefore stacks the three gc-rejected violations, and the exact content below was verified to exit 1 with all three diagnostics.
4. Vocab extraction `grep -hoE '= *"[^"]+"'` over `internal/events/events.go` + `internal/beadmeta/values.go` yields 125 assigned string literals; all 80 pinned names (events ∪ outcome ∪ final_disposition ∪ on_exhausted) are present; none of the 6 `CAMP_SPECIFIC_EVENTS` names are.
5. gascity's `go.mod` says `go 1.26.4` and a `go.sum` exists (used for `setup-go` cache keying).
6. `crates/camp-core/tests/formula_corpus.rs` enumerates `valid/` with `read_dir` — the Task 7 demo file will fail the Rust `test` job **and** `gc-compat`; that is expected and stated in the demo PR comment.
7. `main` currently has **no branch protection** (`gh api .../branches/main/protection` → 404). "Required for merge" (Task 8) means creating it.
8. **Execution-time correction (2026-07-07):** `go run` collapses every nonzero program exit code to 1 (verified with a minimal `os.Exit(2)` probe — `go run` exits 1). Through `go run`, the self-test's exact `rc == 1` assertion could not distinguish "shim rejected the formula" (program exit 1) from "shim saw no formulas" (program exit 2) — precisely the vacuous pass the assertion exists to prevent. Therefore CI and the local mirrors `go build` the shim once and run the binary directly, preserving true exit codes. Same decision-3 mechanism (shim copied into the checkout, compiled there); only the invocation differs from the originally drafted YAML.

## File Structure

- Create: `ci/gc-compat/GASCITY_REF` — one line, the pinned gascity SHA. Read by the workflow and by `check_vocab.sh`.
- Create: `ci/gc-compat/camp_corpus_validate.go` — the shim, verbatim from the master plan. Never compiled in gascamp's own module; CI copies it into the gascity checkout.
- Create: `ci/gc-compat/selftest-invalid.toml` — deliberately broken formula proving the shim fails on bad input.
- Create: `ci/gc-compat/check_vocab.sh` — executable; spec §15.2 contract 2 as a script.
- Modify: `.github/workflows/ci.yml` — append the `gc-compat` job after the existing `test` job (lines 31–40). No changes to existing jobs.

Local test scaffolding (never committed): a gascity checkout at the pinned ref in the session scratchpad, called `$GC_SRC` throughout. Every task's verification runs against it.

---

### Task 1: Pin the ref — `ci/gc-compat/GASCITY_REF` + local gascity checkout

**Files:**
- Create: `ci/gc-compat/GASCITY_REF`

**Interfaces:**
- Consumes: `crates/camp-core/tests/fixtures/gc-vocab.json` `_provenance.gascity_ref` (existing, from Phase 1).
- Produces: `ci/gc-compat/GASCITY_REF` containing exactly `12410301884b51131a35e101a335dbaae16cdcb0` + newline; `$GC_SRC` local checkout used by Tasks 2–5.

- [ ] **Step 1: Create the pin file**

```bash
mkdir -p ci/gc-compat
printf '12410301884b51131a35e101a335dbaae16cdcb0\n' > ci/gc-compat/GASCITY_REF
```

- [ ] **Step 2: Verify the one-ref invariant against the vocab pin**

Run:
```bash
test "$(cat ci/gc-compat/GASCITY_REF)" = "$(jq -r '._provenance.gascity_ref' crates/camp-core/tests/fixtures/gc-vocab.json)" && echo ONE-REF-OK
```
Expected: `ONE-REF-OK` (this same assertion is mechanized in `check_vocab.sh`, Task 4).

- [ ] **Step 3: Stage the local gascity checkout at the pin (scratchpad, not the repo)**

```bash
export GC_SRC="<session-scratchpad>/gascity-src"   # absolute path outside the repo
rm -rf "$GC_SRC" && mkdir -p "$GC_SRC" && cd "$GC_SRC"
git init -q
git remote add origin https://github.com/gastownhall/gascity
git fetch -q --depth 1 origin "$(cat <repo-root>/ci/gc-compat/GASCITY_REF)"
git checkout -q FETCH_HEAD
git rev-parse HEAD
```
Expected: last line prints `12410301884b51131a35e101a335dbaae16cdcb0`.

- [ ] **Step 4: Commit**

```bash
git add ci/gc-compat/GASCITY_REF
git commit -m "ci: pin gascity ref for gc compatibility gates"
```

---

### Task 2: The shim — `ci/gc-compat/camp_corpus_validate.go` (verbatim) proven against the valid corpus

**Files:**
- Create: `ci/gc-compat/camp_corpus_validate.go`

**Interfaces:**
- Consumes: `$GC_SRC` (Task 1); `crates/camp-core/tests/fixtures/formulas/valid/` (Phase 5).
- Produces: the shim source CI copies to `<gascity>/cmd/camp-corpus-validate/main.go`. Exit codes: 0 = all compiled, 1 = ≥1 failed, 2 = usage / no formulas found.

- [ ] **Step 1: Write the shim — verbatim from the master plan (do not edit a character of the code)**

`ci/gc-compat/camp_corpus_validate.go`:
```go
// Validates that every formula in a directory compiles under the real Gas
// City formula-v2 compiler. Lives in gascamp; runs inside a gascity checkout.
package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/gastownhall/gascity/internal/formula"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: camp-corpus-validate <formula-dir>")
		os.Exit(2)
	}
	files, err := filepath.Glob(filepath.Join(os.Args[1], "*.toml"))
	if err != nil || len(files) == 0 {
		fmt.Fprintf(os.Stderr, "no formulas found in %s (err=%v)\n", os.Args[1], err)
		os.Exit(2)
	}
	failed := 0
	for _, path := range files {
		name := strings.TrimSuffix(filepath.Base(path), ".toml")
		if _, err := formula.CompileWithoutRuntimeVarValidation(
			context.Background(), name, []string{os.Args[1]}, nil); err != nil {
			fmt.Fprintf(os.Stderr, "FAIL %s: %v\n", name, err)
			failed++
			continue
		}
		fmt.Printf("OK   %s\n", name)
	}
	if failed > 0 {
		os.Exit(1)
	}
}
```

- [ ] **Step 2: Copy into the checkout exactly as CI will**

```bash
mkdir -p "$GC_SRC/cmd/camp-corpus-validate"
cp ci/gc-compat/camp_corpus_validate.go "$GC_SRC/cmd/camp-corpus-validate/main.go"
```

- [ ] **Step 3: Build the binary and run over the valid corpus — the positive case**

(`go build`, not `go run` — see verified fact 8: `go run` collapses program exit codes to 1.)

```bash
cd "$GC_SRC" && go build -o /tmp/camp-corpus-validate ./cmd/camp-corpus-validate
/tmp/camp-corpus-validate "<repo-root>/crates/camp-core/tests/fixtures/formulas/valid"; echo "exit=$?"
```
Expected (order alphabetical; exit 0):
```
OK   diamond
OK   fan-out
OK   guarded-change
OK   minimal
OK   retry-fetch
exit=0
```

- [ ] **Step 4: Run over an empty dir — usage guard fires with its true exit code**

```bash
mkdir -p /tmp/gc-compat-empty && /tmp/camp-corpus-validate /tmp/gc-compat-empty; echo "exit=$?"
```
Expected: `no formulas found in ...` on stderr, `exit=2`.

- [ ] **Step 5: Commit**

```bash
git add ci/gc-compat/camp_corpus_validate.go
git commit -m "ci: add camp-corpus-validate shim (compiled inside gascity checkout)"
```

---

### Task 3: The shim's failing test — `ci/gc-compat/selftest-invalid.toml`

**Files:**
- Create: `ci/gc-compat/selftest-invalid.toml`

**Interfaces:**
- Consumes: the shim (Task 2), `$GC_SRC`.
- Produces: the fixture the CI self-test step copies into a lone directory and requires exit 1 on.

- [ ] **Step 1: Write the fixture (content verified at planning time to trip all three diagnostics)**

`ci/gc-compat/selftest-invalid.toml`:
```toml
# Deliberately broken formula for the Phase 6 CI shim self-test.
# The gc-compat job runs camp-corpus-validate over a directory containing
# only this file and requires exit 1 — proving the shim actually fails on
# bad input. Broken several independent ways so no single relaxed gc rule
# can silently defuse the self-test:
#   - steps[0] has no title        (gc: "title is required")
#   - steps[1] duplicates id "a"   (gc: "duplicate id")
#   - needs references "ghost"     (gc: "unknown step")
formula = "selftest-invalid"

[[steps]]
id = "a"

[[steps]]
id = "a"
title = "duplicate id, unknown dependency"
needs = ["ghost"]
```

- [ ] **Step 2: Run the self-test exactly as CI will — watch it fail with exit 1**

```bash
rm -rf /tmp/gc-compat-selftest && mkdir -p /tmp/gc-compat-selftest
cp ci/gc-compat/selftest-invalid.toml /tmp/gc-compat-selftest/
/tmp/camp-corpus-validate /tmp/gc-compat-selftest; echo "exit=$?"
```
Expected (exit 1 — the program's own code, no `go run` remapping — all three violations reported):
```
FAIL selftest-invalid: resolving formula "selftest-invalid": formula validation failed:
  - steps[0] (a): title is required (unless using expand)
  - steps[1]: duplicate id "a" (first defined at steps[0])
  - steps[1] (a): needs references unknown step "ghost"
exit=1
```

- [ ] **Step 3: Commit**

```bash
git add ci/gc-compat/selftest-invalid.toml
git commit -m "ci: add shim self-test fixture (broken formula must exit 1)"
```

---

### Task 4: Vocabulary cross-check — `ci/gc-compat/check_vocab.sh`

**Files:**
- Create: `ci/gc-compat/check_vocab.sh` (mode 755)

**Interfaces:**
- Consumes: `$GC_SRC` (`internal/events/events.go`, `internal/beadmeta/values.go`, git HEAD); repo files `ci/gc-compat/GASCITY_REF`, `crates/camp-core/tests/fixtures/gc-vocab.json`, `crates/camp-core/src/vocab.rs`.
- Produces: `check_vocab.sh <gascity-src-dir> <gascamp-root>`; exit 0 = mirror holds, exit 1 = assertion failed (drift), exit 2 = missing input / broken extraction (never passes vacuously).

- [ ] **Step 1: Write the script**

`ci/gc-compat/check_vocab.sh`:
```bash
#!/usr/bin/env bash
# Spec §15.2 contract 2 (vocabulary mirror) as a CI check — Phase 6.
#
# Usage: check_vocab.sh <gascity-src-dir> <gascamp-root>
#
# Asserts, against the gascity checkout at the pinned ref:
#   0. One-ref invariant: ci/gc-compat/GASCITY_REF equals the vocab pin's
#      _provenance.gascity_ref, and the checkout's HEAD equals both.
#   a. Every name in gc-vocab.json's gc lists (events, outcome,
#      final_disposition, on_exhausted) appears as an assigned string
#      constant in internal/events/events.go or internal/beadmeta/values.go.
#   b. No CAMP_SPECIFIC_EVENTS name (crates/camp-core/src/vocab.rs) appears
#      there — camp-specific names are additive, never redefinitions.
#
# Exit 0 = all assertions hold. Exit 1 = an assertion failed (drift).
# Exit 2 = usage error / missing input / empty extraction (a check that
# cannot see its inputs must fail loudly, never pass vacuously).
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: check_vocab.sh <gascity-src-dir> <gascamp-root>" >&2
  exit 2
fi
gc_src="$1"
camp_root="$2"

ref_file="$camp_root/ci/gc-compat/GASCITY_REF"
pin_file="$camp_root/crates/camp-core/tests/fixtures/gc-vocab.json"
vocab_rs="$camp_root/crates/camp-core/src/vocab.rs"
events_go="$gc_src/internal/events/events.go"
values_go="$gc_src/internal/beadmeta/values.go"

for f in "$ref_file" "$pin_file" "$vocab_rs" "$events_go" "$values_go"; do
  if [ ! -f "$f" ]; then
    echo "check_vocab: missing input file: $f" >&2
    exit 2
  fi
done

pinned_ref="$(cat "$ref_file")"
provenance_ref="$(jq -r '._provenance.gascity_ref' "$pin_file")"
if [ "$pinned_ref" != "$provenance_ref" ]; then
  echo "check_vocab: GASCITY_REF ($pinned_ref) != gc-vocab.json provenance ($provenance_ref) — the two must reference one ref" >&2
  exit 1
fi

checkout_head="$(git -C "$gc_src" rev-parse HEAD)"
if [ "$checkout_head" != "$pinned_ref" ]; then
  echo "check_vocab: gascity checkout HEAD ($checkout_head) != GASCITY_REF ($pinned_ref)" >&2
  exit 1
fi

# Extract assigned string literals (Name = "value") from gc source.
# Matching '= "..."' skips names merely mentioned in comments.
gc_names="$(grep -hoE '= *"[^"]+"' "$events_go" "$values_go" | sed -E 's/^= *"([^"]+)"$/\1/' | sort -u || true)"
if [ -z "$gc_names" ]; then
  echo "check_vocab: extracted zero string constants from gc source — extraction is broken" >&2
  exit 2
fi

pin_names="$(jq -r '.events[], .outcome[], .final_disposition[], .on_exhausted[]' "$pin_file" | sort -u)"
if [ -z "$pin_names" ]; then
  echo "check_vocab: gc-vocab.json gc lists are empty — pin is broken" >&2
  exit 2
fi

camp_names="$(sed -n '/CAMP_SPECIFIC_EVENTS/,/];/p' "$vocab_rs" | grep -oE '"[^"]+"' | tr -d '"' | sort -u || true)"
if [ -z "$camp_names" ]; then
  echo "check_vocab: extracted zero CAMP_SPECIFIC_EVENTS names from vocab.rs — extraction is broken" >&2
  exit 2
fi

missing="$(comm -23 <(printf '%s\n' "$pin_names") <(printf '%s\n' "$gc_names"))"
if [ -n "$missing" ]; then
  echo "check_vocab: pinned gc names missing from gascity source at $pinned_ref:" >&2
  printf '%s\n' "$missing" >&2
  exit 1
fi

collisions="$(comm -12 <(printf '%s\n' "$camp_names") <(printf '%s\n' "$gc_names"))"
if [ -n "$collisions" ]; then
  echo "check_vocab: camp-specific event names found in gc source (must be additive, never redefinitions):" >&2
  printf '%s\n' "$collisions" >&2
  exit 1
fi

pin_count="$(printf '%s\n' "$pin_names" | wc -l | tr -d ' ')"
camp_count="$(printf '%s\n' "$camp_names" | wc -l | tr -d ' ')"
echo "check_vocab: OK — $pin_count pinned names present in gc source; $camp_count camp-specific names absent (ref $pinned_ref)"
```

```bash
chmod +x ci/gc-compat/check_vocab.sh
```

- [ ] **Step 2: Positive run — the mirror holds today**

```bash
ci/gc-compat/check_vocab.sh "$GC_SRC" "$(pwd)"; echo "exit=$?"
```
Expected:
```
check_vocab: OK — 80 pinned names present in gc source; 6 camp-specific names absent (ref 12410301884b51131a35e101a335dbaae16cdcb0)
exit=0
```

- [ ] **Step 3: Negative test A — a pinned name absent from gc source must fail**

Build a scratch copy of the camp inputs, inject a fake pinned event, run against it:
```bash
FAKE=/tmp/gc-compat-fakeroot && rm -rf "$FAKE"
mkdir -p "$FAKE/ci/gc-compat" "$FAKE/crates/camp-core/tests/fixtures" "$FAKE/crates/camp-core/src"
cp ci/gc-compat/GASCITY_REF "$FAKE/ci/gc-compat/"
cp crates/camp-core/src/vocab.rs "$FAKE/crates/camp-core/src/"
jq '.events += ["camp.fake_event"]' crates/camp-core/tests/fixtures/gc-vocab.json > "$FAKE/crates/camp-core/tests/fixtures/gc-vocab.json"
ci/gc-compat/check_vocab.sh "$GC_SRC" "$FAKE"; echo "exit=$?"
```
Expected: `check_vocab: pinned gc names missing from gascity source at 1241030188...:` then `camp.fake_event`; `exit=1`.

- [ ] **Step 4: Negative test B — a camp-specific name that exists in gc must fail**

```bash
cp crates/camp-core/tests/fixtures/gc-vocab.json "$FAKE/crates/camp-core/tests/fixtures/gc-vocab.json"
sed 's/"bead.claimed",/"bead.claimed",\n    "session.woke",/' crates/camp-core/src/vocab.rs > "$FAKE/crates/camp-core/src/vocab.rs"
grep -n 'session.woke' "$FAKE/crates/camp-core/src/vocab.rs"   # confirm the injection landed inside CAMP_SPECIFIC_EVENTS
ci/gc-compat/check_vocab.sh "$GC_SRC" "$FAKE"; echo "exit=$?"
```
Expected: `check_vocab: camp-specific event names found in gc source ...` then `session.woke`; `exit=1`.

- [ ] **Step 5: Negative test C — ref drift between GASCITY_REF and the pin must fail**

```bash
cp crates/camp-core/src/vocab.rs "$FAKE/crates/camp-core/src/vocab.rs"
printf '0000000000000000000000000000000000000000\n' > "$FAKE/ci/gc-compat/GASCITY_REF"
ci/gc-compat/check_vocab.sh "$GC_SRC" "$FAKE"; echo "exit=$?"
```
Expected: `check_vocab: GASCITY_REF (0000...) != gc-vocab.json provenance (1241030188...) — the two must reference one ref`; `exit=1`.

- [ ] **Step 6: Negative test D — unreadable extraction must exit 2, never pass**

```bash
printf '// no vocab here\n' > "$FAKE/crates/camp-core/src/vocab.rs"
cp ci/gc-compat/GASCITY_REF "$FAKE/ci/gc-compat/GASCITY_REF"
ci/gc-compat/check_vocab.sh "$GC_SRC" "$FAKE"; echo "exit=$?"
```
Expected: `check_vocab: extracted zero CAMP_SPECIFIC_EVENTS names from vocab.rs — extraction is broken`; `exit=2`.

- [ ] **Step 7: Commit**

```bash
git add ci/gc-compat/check_vocab.sh
git commit -m "ci: add vocabulary cross-check against gascity source at the pinned ref"
```

---

### Task 5: The `gc-compat` job in `.github/workflows/ci.yml`

**Files:**
- Modify: `.github/workflows/ci.yml` (append after the `test` job, lines 31–40; existing jobs untouched)

**Interfaces:**
- Consumes: all four `ci/gc-compat/` files (Tasks 1–4).
- Produces: a check named `gc-compat` on every PR and main push — the name Task 8's required-check setting references.

- [ ] **Step 1: Append the job**

Add to `.github/workflows/ci.yml` (after the `test` job, same indentation as the other jobs):
```yaml
  gc-compat:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: read pinned gascity ref
        id: gc-ref
        run: echo "ref=$(cat ci/gc-compat/GASCITY_REF)" >> "$GITHUB_OUTPUT"
      - uses: actions/checkout@v4
        with:
          repository: gastownhall/gascity
          ref: ${{ steps.gc-ref.outputs.ref }}
          path: gascity-src
      - uses: actions/setup-go@v5
        with:
          go-version-file: gascity-src/go.mod
          cache-dependency-path: gascity-src/go.sum
      - name: build shim inside the gascity checkout
        run: |
          mkdir -p gascity-src/cmd/camp-corpus-validate
          cp ci/gc-compat/camp_corpus_validate.go gascity-src/cmd/camp-corpus-validate/main.go
          cd gascity-src
          go build -o "$RUNNER_TEMP/camp-corpus-validate" ./cmd/camp-corpus-validate
      - name: validate corpus against the real gc compiler
        run: |
          "$RUNNER_TEMP/camp-corpus-validate" crates/camp-core/tests/fixtures/formulas/valid
      - name: shim self-test — a broken formula must exit 1
        run: |
          mkdir -p "$RUNNER_TEMP/selftest"
          cp ci/gc-compat/selftest-invalid.toml "$RUNNER_TEMP/selftest/"
          set +e
          "$RUNNER_TEMP/camp-corpus-validate" "$RUNNER_TEMP/selftest"
          rc=$?
          set -e
          if [ "$rc" -ne 1 ]; then
            echo "self-test: expected exit 1 on a broken formula, got $rc — the shim cannot be trusted" >&2
            exit 1
          fi
      - name: vocabulary cross-check
        run: ci/gc-compat/check_vocab.sh gascity-src "$GITHUB_WORKSPACE"
```

Notes the executor should not "fix": the second checkout goes to `path: gascity-src` and does not disturb the gascamp checkout; the shim is `go build`-compiled and the **binary** is invoked directly because `go run` collapses program exit codes to 1 (verified fact 8) — the self-test asserts `rc == 1` exactly, so exit 2 ("no formulas found" — a self-test that cannot see its fixture) fails the job as its own distinct condition; `check_vocab.sh` verifies the checkout HEAD against the pin, so a checkout of the wrong ref cannot slip through.

- [ ] **Step 2: Lint the workflow**

```bash
go run github.com/rhysd/actionlint/cmd/actionlint@latest .github/workflows/ci.yml
```
Expected: no output, exit 0.

- [ ] **Step 3: Mirror every CI step locally (same order as the job)**

```bash
cd "$GC_SRC" \
  && go build -o /tmp/camp-corpus-validate ./cmd/camp-corpus-validate \
  && /tmp/camp-corpus-validate "<repo-root>/crates/camp-core/tests/fixtures/formulas/valid" \
  && rm -rf /tmp/gc-compat-selftest && mkdir -p /tmp/gc-compat-selftest \
  && cp <repo-root>/ci/gc-compat/selftest-invalid.toml /tmp/gc-compat-selftest/ \
  && { /tmp/camp-corpus-validate /tmp/gc-compat-selftest; test $? -eq 1; } \
  && <repo-root>/ci/gc-compat/check_vocab.sh "$GC_SRC" "<repo-root>" \
  && echo LOCAL-GC-COMPAT-GREEN
```
Expected: 5 `OK` lines, the selftest `FAIL` block, `check_vocab: OK — 80 ... 6 ...`, then `LOCAL-GC-COMPAT-GREEN`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add gc-compat job — corpus vs real gc compiler + vocabulary cross-check"
```

---

### Task 6: Gates, push, PR

**Files:** none new.

- [ ] **Step 1: Run the repo gates (no Rust changed, but the gates are unconditional)**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace
```
Expected: fmt silent; clippy no warnings; all tests pass.

- [ ] **Step 2: Push and open the PR**

```bash
git push -u origin phase-6-gc-compat-ci
gh pr create --title "Phase 6: gc compatibility gates in CI" --body "<summary: what the job does, the pinned ref, the self-test, the vocab cross-check; note that a demo commit adding a deliberately-bad corpus file will follow and be reverted as part of review (exit criterion)>"
```

- [ ] **Step 3: Watch CI**

```bash
gh pr checks --watch
```
Expected: `fmt`, `clippy`, `test (ubuntu-latest)`, `test (macos-latest)`, `gc-compat` all pass. If `gc-compat` diverges from the local mirror run, debug the job — do not weaken a check to get green.

---

### Task 7: Exit-criterion demo — a deliberately-bad corpus file fails in CI, then is removed

**Files (transient, inside the PR):**
- Create then revert: `crates/camp-core/tests/fixtures/formulas/valid/demo-gc-reject.toml`

This file lives in `valid/`, which Phase 5's `formula_corpus.rs` also enumerates — so the Rust `test` jobs fail alongside `gc-compat`. Expected and stated in the PR comment; the demonstrated failure is `gc-compat`'s `FAIL demo-gc-reject`.

- [ ] **Step 1: Add the bad file and push**

`crates/camp-core/tests/fixtures/formulas/valid/demo-gc-reject.toml`:
```toml
# PR-REVIEW DEMO ONLY — deliberately gc-invalid; reverted before merge.
# Proves the gc-compat gate fails on a bad corpus file (Phase 6 exit
# criterion).
formula = "demo-gc-reject"

[[steps]]
id = "demo"
title = "references a step that does not exist"
needs = ["nonexistent"]
```

```bash
git add crates/camp-core/tests/fixtures/formulas/valid/demo-gc-reject.toml
git commit -m "ci: DEMO — deliberately bad corpus file (reverted before merge)"
git push
```

- [ ] **Step 2: Watch CI fail on gc-compat; capture evidence**

```bash
gh pr checks --watch
```
Expected: `gc-compat` FAIL (job log shows `FAIL demo-gc-reject: ... needs references unknown step "nonexistent"`); `test (…)` jobs also fail (corpus test — expected). Record the failing run URL:
```bash
gh run list --branch phase-6-gc-compat-ci --limit 1 --json databaseId,url,conclusion
```

- [ ] **Step 3: Revert the demo commit and push**

```bash
git revert --no-edit HEAD
git push
gh pr checks --watch
```
Expected: all checks green again. Record the green run URL.

- [ ] **Step 4: Comment the demo on the PR**

```bash
gh pr comment --body "Exit-criterion demo: <failing-run-url> shows gc-compat rejecting demo-gc-reject.toml via the real gc compiler (the Rust corpus test fails on the same file, as expected — camp is stricter than gc). Reverted in <revert-sha>; <green-run-url> is green."
```

---

### Task 8: Mark `gc-compat` required for merge (post-merge, coordinated)

`main` has no branch protection today (verified 404). Creating it affects sibling PRs (phase-8, phase-10): a required `gc-compat` context can only be reported by branches whose `ci.yml` has the job, i.e., branches rebased onto main **after** this PR merges. Therefore this task runs only after the operator merges the PR, and the lead must tell siblings to rebase (they are required to rebase on sibling merges anyway).

- [ ] **Step 1: After the merge, create the protection**

```bash
gh api -X PUT repos/richardkiene/gascamp/branches/main/protection --input - <<'JSON'
{
  "required_status_checks": { "strict": false, "contexts": ["gc-compat"] },
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": null
}
JSON
```

- [ ] **Step 2: Verify**

```bash
gh api repos/richardkiene/gascamp/branches/main/protection/required_status_checks
```
Expected: JSON with `"contexts": ["gc-compat"]`.

- [ ] **Step 3: Report to the lead**

PR number, CI status, both demo run URLs, and each master-plan exit criterion quoted with its evidence.

**Open question for the operator (answer at plan approval):** should the required-check set be `gc-compat` only (the contract's letter) or all five checks (`fmt`, `clippy`, `test (ubuntu-latest)`, `test (macos-latest)`, `gc-compat` — formalizing the existing "gates green before push" rule)? The plan defaults to `gc-compat` only; say the word and Step 1's `contexts` array becomes the five-name list.

---

## Self-Review

1. **Spec coverage.** Contract files: `GASCITY_REF` (Task 1), `camp_corpus_validate.go` verbatim (Task 2), `selftest-invalid.toml` (Task 3), `check_vocab.sh` (Task 4), `gc-compat` job (Task 5) — all present. CI job outline items: gascamp checkout, gascity checkout at `$(cat GASCITY_REF)`, `setup-go` with `go-version-file` from the checkout, shim copy, valid-corpus run, self-test requiring exit 1, vocab extraction with assertions (a) and (b) — all in Task 5's YAML. One-ref invariant with the vocab pin — Task 1 Step 2 and mechanized in `check_vocab.sh`. Exit criteria: job green on the Phase 5 corpus (Task 6), required for merge thereafter (Task 8), deliberately-bad corpus file demonstrated then removed inside the PR (Task 7). Decision 4's "re-extracts and cross-checks the pin itself" — `check_vocab.sh` assertions 0/a/b.
2. **Placeholder scan.** Every code/config block is complete and was executed at planning time against the real pinned checkout, except the PR body/comment templates (angle-bracket slots the executor fills with real URLs/SHAs at run time — data that cannot exist before the run).
3. **Type consistency.** The check name `gc-compat` (Task 5 job id) matches Task 8's `contexts` entry; `check_vocab.sh`'s CLI (`<gascity-src-dir> <gascamp-root>`) matches its invocations in Tasks 4, 5; shim exit codes (0/1/2) match the self-test's `rc -ne 1` assertion and Task 2 Step 4's expected `exit=2`.

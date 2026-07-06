# Gas Camp Phase 2 — §17 Assumption Verification Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verify spec §17 assumptions A1–A4 against current Claude Code behavior and docs, pin the `claude -p` dispatch mechanics Phase 8 needs as fixture facts, and land `docs/design/2026-07-06-assumption-findings.md` (plus spec §17 edits if any verdict diverges) in one PR on `phase-2-assumptions`.

**Architecture:** This is a verification phase, not a code phase, so the TDD cycle adapts to research: every probe states its **expected observation before running**, runs against a throwaway scratch repo (never inside gascamp's working tree), captures raw output as evidence, and only then records a fact. Work is ordered so all docs research and scripted experiments complete first; everything that needs the operator's eyes on the Claude Code TUI is batched into one manual sitting at a single STOP checkpoint. No verdict is written for an operator-gated assumption until the operator reports.

**Tech Stack:** `claude` CLI (headless `-p` mode), `jq`, `bash`, `git`, claude-code-guide agent (docs research), `gh` (PR).

## Global Constraints

Copied from AGENTS.md, the master plan, and the operator's standing rules. Every task's requirements implicitly include this section.

- **Spec is authoritative:** `docs/design/2026-07-05-gas-camp-design.md`; §4 decision record is settled. If findings contradict a §17 assumption, update the spec **in the same PR** — spec and code never silently diverge.
- **Master plan contract:** `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md`, "Phase 2" section. Deliverable: `docs/design/2026-07-06-assumption-findings.md` with, per assumption: Assumed / Observed / Evidence (doc citations + experiment transcripts) / Verdict (`holds` | `weaker` | `stronger`) / Spec impact.
- **Never experiment inside gascamp's working tree.** All probes run from throwaway scratch repos under the session scratchpad. `cd` into a scratch rig before every `claude -p` invocation.
- **Manual verification protocol:** A1 (and the teammate portion of A2/A3) can only be verified by the operator driving the TUI. STOP at Task 8, hand over the batched protocol, and do not write those verdicts until the operator reports back.
- **Record `claude --version`** in the findings doc, plus the platform.
- **Observation over assumption:** the `claude` flag names in this plan are hypotheses to verify against `claude --help` output first. If a flag named here does not exist, that is a finding — record it, cite `--help`, and adapt the probe to the real surface. Never fake a result; never skip a probe because the first command errored.
- **Fail fast, no fallbacks, no silenced errors.** A probe that errors is evidence; record the error verbatim.
- **Git:** never commit to main; branch `phase-2-assumptions`; no co-author lines, no self-mention in commits. Conventional-commit style (`docs:` for everything in this phase).
- **Gates before push:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` (no code changes in this phase, but the gates must still be green).
- **Nothing is complete until pushed, CI green, and every claim in the PR description verified.**

## Key Paths and Conventions

- `GASCAMP=/Users/kiener/code/gascamp` — the repo. Only docs are written here.
- `EXP=<session scratchpad>/phase2` — experiment root. For the current session that is
  `/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2`.
  A different executing session must substitute its own scratchpad path **consistently in every script** (shell state does not persist between tool calls, so every script re-declares `EXP` as a literal).
- `$EXP/rig-a`, `$EXP/rig-b` — two throwaway git repos standing in for two rigs.
- `$EXP/evidence/` — one file per probe, named `<probe-id>-*.{json,txt,jsonl}`. Evidence excerpts get embedded in the findings doc (the scratchpad is ephemeral; the findings doc is the durable record).
- `$EXP/scripts/` — probe scripts that contain `sleep`/timing (run via Bash `run_in_background`; the harness blocks foreground sleeps).
- Nonces: every probe that checks context or persistence uses a greppable nonce (`GASCAMP-ZEBRA-42`, `A3-PROBE-GASCAMP`, …) so evidence can't be confused with ambient text.

## What Phase 8 needs pinned (the fixture-fact checklist)

The findings doc must answer each of these with evidence, in a "Fixture facts" section:

1. How a session id is assigned/captured for `claude -p` — can campd pre-assign it (registry-at-birth **before** exec) or only capture it after from output?
2. The `--output-format json` result envelope: exact field list Phase 8 will parse.
3. The transcript file path scheme: directory + filename as a function of cwd and session id, including the exact path-munging rule (patrol watches this path; worktree spawns change it).
4. Exit-code behavior: success, CLI usage error, invalid resume target, SIGTERM, SIGKILL, and what a denied tool does to the exit code — the SIGCHLD → `session.stopped`/`session.crashed` mapping depends on these.
5. Whether `claude -p` requires stdin closed/handled (campd must know what to do with the child's stdin).
6. Whether `--resume` preserves or forks the session id, and whether it creates a new transcript file — patrol's nudge action and the registry row depend on this.

## Findings Document Template

Task 7 creates `docs/design/2026-07-06-assumption-findings.md` from exactly this skeleton:

```markdown
# Gas Camp — Spec §17 Assumption Findings (Phase 2)

| Field | Value |
|---|---|
| Date | 2026-07-06 |
| Spec under test | docs/design/2026-07-05-gas-camp-design.md §17 |
| claude --version | <exact output> |
| Platform | <uname -sr output> |
| Method | claude-code-guide docs research + scripted experiments in throwaway scratch repos (master plan, Phase 2). Raw probe outputs were session artifacts; the load-bearing excerpts are embedded verbatim below. |

## Fixture facts for Phase 8 (dispatch mechanics)

| # | Fact | Value | Evidence |
|---|---|---|---|
| F1 | Session id assignment for `claude -p` | … | probe D2 |
| F2 | Result envelope fields (`--output-format json`) | … | probe D1 |
| F3 | Transcript path scheme + munging rule | … | probe D3 |
| F4 | Exit codes (success / usage error / bad resume / SIGTERM / SIGKILL / denied tool) | … | probe D4 |
| F5 | stdin handling for a campd-spawned child | … | probe D5 |
| F6 | `--resume` session-id and transcript behavior | … | probe A4-2 |

## A1 — Teammate interaction mechanics

**Assumed (spec §17):** the user can select an attended teammate in the Claude Code TUI and converse mid-run. Decided fallback if weaker: Tier-0 spawns headless + instant attach.

**Observed:** …

**Evidence:** …

**Verdict:** holds | weaker | stronger

**Spec impact:** …

## A2 — Teammate working directory across repos

(same five fields)

## A3 — No dependence on harness team persistence

(same five fields)

## A4 — Headless mid-run conversation

(same five fields)
```

Rules for filling it in: Observed states only what a probe or the operator actually showed; Evidence pairs each claim with the exact command + trimmed verbatim output (or the operator's report, quoted); a Verdict line is written only when its evidence is complete — Task 7 leaves A1 and A2 with `**Verdict:** PENDING OPERATOR` and Task 9 replaces it.

---

### Task 1: Branch, scratch rigs, environment capture

**Files:**
- Create: `$EXP/{evidence,scripts}/`, `$EXP/rig-a/`, `$EXP/rig-b/` (scratchpad, not committed)
- Commit: `docs/superpowers/plans/2026-07-06-phase-2-assumptions.md` (this file)

**Interfaces:**
- Produces: the branch every later task commits to; `$EXP` layout every probe uses; `claude-version.txt` and `claude-help.txt` that Tasks 3–6 consult before trusting any flag name.

- [ ] **Step 1: Create the branch**

```bash
git -C /Users/kiener/code/gascamp checkout -b phase-2-assumptions
```

Expected: `Switched to a new branch 'phase-2-assumptions'`.

- [ ] **Step 2: Commit this plan document**

```bash
git -C /Users/kiener/code/gascamp add docs/superpowers/plans/2026-07-06-phase-2-assumptions.md
git -C /Users/kiener/code/gascamp commit -m "docs: add phase 2 assumption-verification execution plan"
```

- [ ] **Step 3: Create the experiment layout and scratch rigs**

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
mkdir -p "$EXP/evidence" "$EXP/scripts" "$EXP/rig-a" "$EXP/rig-b"
for r in rig-a rig-b; do
  git -C "$EXP/$r" init -q
  echo "# $r — gascamp phase-2 scratch rig (throwaway)" > "$EXP/$r/README.md"
  git -C "$EXP/$r" add README.md
  git -C "$EXP/$r" -c user.email=scratch@example.invalid -c user.name=scratch commit -qm "init"
done
ls "$EXP"
```

Expected: `evidence  rig-a  rig-b  scripts`.

- [ ] **Step 4: Capture claude version and the real flag surface**

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
claude --version | tee "$EXP/evidence/claude-version.txt"
claude --help > "$EXP/evidence/claude-help.txt" 2>&1
grep -E -- '--(session-id|resume|output-format|input-format|allowedTools|add-dir|append-system-prompt|agents|permission-mode|verbose)' "$EXP/evidence/claude-help.txt"
```

Expected: a version string; the grep shows which of the hypothesized flags exist. Any flag named in Tasks 3–6 that is **absent** here gets recorded as a finding and its probe adapted to the real surface before running.

### Task 2: Docs research via claude-code-guide

**Files:**
- Create: `$EXP/evidence/docs-research.md` (scratchpad)

**Interfaces:**
- Produces: cited documentation answers per assumption; Task 7 embeds the citations, Task 8 uses the A1/teammate-UI citations to write exact operator instructions.

- [ ] **Step 1: Dispatch the claude-code-guide agent (synchronous)** with exactly this prompt:

```
Research current Claude Code documentation and answer each question with a
citation (doc URL or doc-page name + section). Say "docs do not cover this"
explicitly where true — do not guess. Return raw findings, not a summary.

A1 (teammates in the TUI):
 1. Can a Claude Code session spawn named teammates/subagents that appear in
    the TUI, and can the USER select one and send it a message while it is
    mid-task? What are the exact UI affordances/keystrokes?
 2. Do teammate messages deliver mid-turn or queue until the teammate's
    current turn ends?
A2 (working directory):
 3. Can a teammate/subagent run with a working directory in a DIFFERENT
    repository than the parent session? Is there a per-agent cwd option
    (agent frontmatter, Agent tool parameter, --add-dir, worktree isolation)?
 4. What does --add-dir grant exactly (read? write? cwd change?)?
A3 (persistence):
 5. Does Claude Code team/task state (TaskCreate/TaskList, teammates) persist
    across session restarts? Where is it stored on disk?
A4 + dispatch mechanics (headless):
 6. For `claude -p`: how is the session id assigned/reported? Is --session-id
    (caller-chosen id) supported?
 7. Where are transcripts stored (path scheme as a function of cwd/project
    and session id)?
 8. What exit codes does `claude -p` use (success, errors)?
 9. Can text be injected into a RUNNING headless session (stream-json input,
    IPC, anything)? Or is conversation only possible via --resume after the
    run?
10. What are the documented semantics of `claude -p --resume <id>` — same
    session id or a fork? Same transcript file or a new one?
11. Which flags configure a headless worker: model, allowed tools,
    permission mode, system-prompt append, custom agent definitions?
```

- [ ] **Step 2: Save the agent's answer verbatim**

Write the full returned text to `$EXP/evidence/docs-research.md`, prefixed with a line recording the agent type and date.

- [ ] **Step 3: Extract the citation list**

Append to the bottom of `docs-research.md` a `## Citations by assumption` section: for each of A1–A4 and "dispatch", the doc references the agent gave (or "docs do not cover"). These are the `Evidence` doc-citation halves for Task 7.

### Task 3: Probes D1–D5 — dispatch mechanics (fixture facts F1–F5)

**Files:**
- Create: `$EXP/evidence/d1-envelope.json`, `d2-sid.txt`, `d2-envelope.json`, `d3-transcript.txt`, `d4-exitcodes.txt`, `d5-stdin.txt`

**Interfaces:**
- Consumes: `$EXP` layout and `claude-help.txt` (Task 1).
- Produces: fixture facts F1–F5 with raw evidence; the session id `D2 SID` reused by nothing else (fresh ids per probe).

- [ ] **Step 1: Probe D1 — result envelope and success exit code.** Expected observation (hypothesis): exit 0; a single JSON object including a `session_id` field.

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
claude -p 'Reply with exactly: D1-OK' --output-format json > "$EXP/evidence/d1-envelope.json"
echo "d1 exit=$?" | tee -a "$EXP/evidence/d4-exitcodes.txt"
jq 'keys' "$EXP/evidence/d1-envelope.json"
```

Record: the full key list (fact F2), the `session_id`, and whether `result` contains `D1-OK`. This probe doubles as the nested-invocation sanity check — if `claude` refuses to run inside a Claude Code session, record the exact error and retry with the session-marker env vars unset (`env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT claude -p …`), noting which form campd's spawn facts are based on.

- [ ] **Step 2: Probe D2 — caller-assigned session id.** Expected (hypothesis): `--session-id` exists and the envelope echoes the same id ⇒ campd can write the registry row **before** exec (F1, registry-at-birth).

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
SID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "$SID" > "$EXP/evidence/d2-sid.txt"
claude -p 'Reply with exactly: D2-OK' --session-id "$SID" --output-format json > "$EXP/evidence/d2-envelope.json"
jq -r .session_id "$EXP/evidence/d2-envelope.json"
cat "$EXP/evidence/d2-sid.txt"
```

Record: whether the two ids match. If the flag doesn't exist, F1 becomes "capture-after from the envelope" — record that as the pinned mechanism instead.

- [ ] **Step 3: Probe D3 — transcript path scheme and munging rule (F3).**

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
SID=$(cat "$EXP/evidence/d2-sid.txt")
find ~/.claude/projects -name "$SID.jsonl" | tee "$EXP/evidence/d3-transcript.txt"
echo "cwd was: $EXP/rig-a" >> "$EXP/evidence/d3-transcript.txt"
head -c 600 "$(find ~/.claude/projects -name "$SID.jsonl" | head -1)" >> "$EXP/evidence/d3-transcript.txt"
```

Record: the parent directory name next to the literal cwd; derive the exact character-substitution rule (which characters map to what) by comparison, and state it as a function campd can implement: `transcript_path(cwd, sid) = …`. Note the transcript's on-disk format (JSONL, one event per line) from the head excerpt.

- [ ] **Step 4: Probe D4 — exit codes (F4).** Expected (hypotheses): usage error ≠ 0; bad resume ≠ 0; SIGTERM → 143; SIGKILL → 137.

Part 1, the fast cases:

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
claude --definitely-not-a-flag -p 'x' > /dev/null 2>>"$EXP/evidence/d4-exitcodes.txt"; echo "usage-error exit=$?" | tee -a "$EXP/evidence/d4-exitcodes.txt"
claude -p --resume 00000000-0000-0000-0000-000000000000 'Reply OK' > /dev/null 2>>"$EXP/evidence/d4-exitcodes.txt"; echo "bad-resume exit=$?" | tee -a "$EXP/evidence/d4-exitcodes.txt"
claude -p 'You must run the shell command `git status` via the Bash tool and reply with its first output line. Do not answer without running it.' --output-format json > "$EXP/evidence/d4-denied-tool.json" 2>&1; echo "denied-tool exit=$?" | tee -a "$EXP/evidence/d4-exitcodes.txt"
jq -r '.is_error, .result' "$EXP/evidence/d4-denied-tool.json"
```

Record: each exit code, plus how a denied tool surfaces (blocked tool call in transcript? `is_error`? exit code?) — this pins spec §8.4's "unallowed actions fail fast" mechanics.

Part 2, signal deaths — write `$EXP/scripts/d4-signals.sh` with this content, `chmod +x` it, and run it via Bash `run_in_background` (it sleeps internally):

```bash
#!/bin/bash
set -u
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
for SIG in TERM KILL; do
  claude -p 'Run the shell command `sleep 60` via Bash, then reply DONE.' --allowedTools 'Bash(sleep:*)' > /dev/null 2>&1 &
  PID=$!
  sleep 10                              # let it get into the turn
  kill -$SIG "$PID"
  wait "$PID"
  echo "SIG$SIG exit=$?" >> "$EXP/evidence/d4-exitcodes.txt"
done
echo "d4-signals done" >> "$EXP/evidence/d4-exitcodes.txt"
```

When the background run completes, read `d4-exitcodes.txt`. Record: the two exit codes (F4's SIGCHLD mapping: which wait-statuses mean `session.crashed`).

- [ ] **Step 5: Probe D5 — stdin behavior (F5).** Expected (hypotheses): piped stdin is read as prompt input; `< /dev/null` works fine.

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
echo 'Reply with exactly: STDIN-OK' | claude -p --output-format json | jq -r .result | tee "$EXP/evidence/d5-stdin.txt"
claude -p 'Reply with exactly: NULLSTDIN-OK' --output-format json < /dev/null | jq -r .result | tee -a "$EXP/evidence/d5-stdin.txt"
```

Record: both results; F5 = what campd must do with the child's stdin (pass prompt as argv + close stdin, or otherwise).

### Task 4: Probes A4-1…A4-4 — headless mid-run conversation

**Files:**
- Create: `$EXP/scripts/a4-tail.sh`, `$EXP/evidence/a4-{run.json,tail.log,resume.json,stream.jsonl,concurrent.txt}`

**Interfaces:**
- Consumes: transcript-path rule from probe D3.
- Produces: A4's Observed/Evidence, fixture fact F6, and the patrol-nudge mechanics note for Phase 11.

- [ ] **Step 1: Probe A4-1 — transcript tailability during a run.** Expected (spec's assumption): the transcript file grows while the process is still running ⇒ tail-now works.

Write `$EXP/scripts/a4-tail.sh` (content below), `chmod +x`, run via Bash `run_in_background`, then inspect the evidence files:

```bash
#!/bin/bash
set -u
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
SID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "$SID" > "$EXP/evidence/a4-sid.txt"
claude -p 'Remember the codeword GASCAMP-ZEBRA-42. Run the shell command `sleep 25` via Bash, then reply with exactly: A4-DONE' \
  --allowedTools 'Bash(sleep:*)' --session-id "$SID" --output-format json > "$EXP/evidence/a4-run.json" 2>&1 &
PID=$!
for i in $(seq 1 20); do
  TP=$(find ~/.claude/projects -name "$SID.jsonl" 2>/dev/null | head -1)
  ALIVE=$(kill -0 "$PID" 2>/dev/null && echo yes || echo no)
  LINES=$([ -n "$TP" ] && wc -l < "$TP" || echo 0)
  echo "t=${i} alive=$ALIVE transcript_lines=$LINES" >> "$EXP/evidence/a4-tail.log"
  [ "$ALIVE" = no ] && break
  sleep 3
done
wait "$PID"; echo "a4 exit=$?" >> "$EXP/evidence/a4-tail.log"
```

Record: whether `transcript_lines` grew while `alive=yes` (the tail-now claim), and the final exit code. (Polling here is probe scaffolding, not camp architecture.)

- [ ] **Step 2: Probe A4-2 — resume-and-converse after exit (F6).** Expected: the resumed turn recalls the codeword ⇒ conversation-by-resume works. Key fact to record either way: does the resumed run keep `$SID` or mint a new session id, and does a new transcript file appear?

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
SID=$(cat "$EXP/evidence/a4-sid.txt")
claude -p --resume "$SID" 'Reply with exactly the codeword you were told earlier, nothing else.' --output-format json > "$EXP/evidence/a4-resume.json"
jq -r '.result, .session_id' "$EXP/evidence/a4-resume.json"
find ~/.claude/projects -name "$(jq -r .session_id "$EXP/evidence/a4-resume.json").jsonl"
```

Record: codeword recalled yes/no; resumed `session_id` == `$SID` or forked; new transcript file yes/no. If forked, note the Phase 8/11 consequence explicitly: the registry row's session id must be updated on every nudge.

- [ ] **Step 3: Probe A4-3 — live input via stream-json stdin.** Expected (spec assumes this does NOT exist; finding it would make A4 stronger): one headless process accepts a second user turn over stdin after finishing the first.

Write `$EXP/scripts/a4-stream.sh`, `chmod +x`, run via `run_in_background`:

```bash
#!/bin/bash
set -u
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
{ echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: FIRST-OK"}]}}'
  sleep 15
  echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: SECOND-OK"}]}}'
} | claude -p --input-format stream-json --output-format stream-json --verbose > "$EXP/evidence/a4-stream.jsonl" 2>&1
echo "stream exit=$?" >> "$EXP/evidence/a4-stream.jsonl"
```

Then: `grep -c 'SECOND-OK' "$EXP/evidence/a4-stream.jsonl"`. Record: whether both turns were answered by one process, and whether the process exited on stdin EOF. If yes, A4 is **stronger** for campd-spawned workers (campd holds the stdin pipe) — exactly the live nudge path §17 mentions; note that it only applies to sessions campd started in stream mode.

- [ ] **Step 4: Probe A4-4 — resume attempt while the original is still running.** This is patrol's nudge-timing question: what happens if you `--resume` a session whose process is alive?

Write `$EXP/scripts/a4-concurrent.sh`, `chmod +x`, run via `run_in_background`:

```bash
#!/bin/bash
set -u
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
SID=$(uuidgen | tr '[:upper:]' '[:lower:]')
claude -p 'Run the shell command `sleep 40` via Bash, then reply with exactly: LONG-DONE' \
  --allowedTools 'Bash(sleep:*)' --session-id "$SID" > "$EXP/evidence/a4-concurrent-long.txt" 2>&1 &
PID=$!
sleep 12
claude -p --resume "$SID" 'Reply with exactly: NUDGE-OK' --output-format json > "$EXP/evidence/a4-concurrent.txt" 2>&1
echo "concurrent-resume exit=$?" >> "$EXP/evidence/a4-concurrent.txt"
wait "$PID"; echo "long-run exit=$?" >> "$EXP/evidence/a4-concurrent.txt"
```

Record: error / success / fork, verbatim. This pins whether patrol's nudge must wait for the turn boundary (the spec's assumed shape) or not.

### Task 5: Probes A2-1…A2-3 — working directory (scriptable half)

**Files:**
- Create: `$EXP/evidence/a2-{cwd.json,cross.json,adddir.json,subagent.json}`, plus `$EXP/rig-b/a2-probe.txt` if writes succeed

**Interfaces:**
- Produces: A2's scripted evidence (headless cwd, cross-repo file access, `--add-dir`, subagent cwd inheritance). The teammate-in-TUI half is Task 8's manual batch.

- [ ] **Step 1: Probe A2-1 — cwd is honored and the transcript follows it.**

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-b"
SID=$(uuidgen | tr '[:upper:]' '[:lower:]')
claude -p 'Run pwd via the Bash tool and reply with exactly its output, nothing else.' --allowedTools 'Bash(pwd)' --session-id "$SID" --output-format json > "$EXP/evidence/a2-cwd.json"
jq -r .result "$EXP/evidence/a2-cwd.json"
find ~/.claude/projects -name "$SID.jsonl"
```

Record: reported pwd == `$EXP/rig-b`; transcript parent dir derived from rig-b's path (confirms F3 generalizes — worktree spawns will land transcripts under per-worktree project dirs, which patrol must compute).

- [ ] **Step 2: Probe A2-2 — cross-repo write without and with --add-dir.** Expected (hypothesis): without `--add-dir` the Write outside the session dir needs permission and fails fast headlessly; with `--add-dir` it succeeds.

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
claude -p "Use the Write tool to create $EXP/rig-b/a2-probe.txt containing exactly: A2-CROSS. Then reply with exactly: DONE. If the write is refused, reply with exactly: REFUSED plus the refusal reason." --output-format json > "$EXP/evidence/a2-cross.json"
jq -r '.is_error, .result' "$EXP/evidence/a2-cross.json"; ls -l "$EXP/rig-b/a2-probe.txt" 2>&1
rm -f "$EXP/rig-b/a2-probe.txt"
claude -p "Use the Write tool to create $EXP/rig-b/a2-probe.txt containing exactly: A2-CROSS. Then reply with exactly: DONE." --add-dir "$EXP/rig-b" --output-format json > "$EXP/evidence/a2-adddir.json"
jq -r '.is_error, .result' "$EXP/evidence/a2-adddir.json"; ls -l "$EXP/rig-b/a2-probe.txt" 2>&1
```

Record: both outcomes verbatim. This pins whether a single camp session could service two rigs via `--add-dir` versus needing cwd-per-worker (camp already routes cross-rig work headless by default, §12 — this evidence bounds what the *attended same-camp* surface can do).

- [ ] **Step 3: Probe A2-3 — subagent cwd inheritance.**

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
claude -p 'Use the Agent tool (subagent_type general-purpose) with this task: "Run pwd via Bash and return exactly its output." Reply with exactly the pwd the agent returns, nothing else.' --output-format json > "$EXP/evidence/a2-subagent.json"
jq -r '.is_error, .result' "$EXP/evidence/a2-subagent.json"
```

Record: whether the Agent tool is available headlessly at all, and the subagent's effective cwd (hypothesis: inherits the parent session's cwd — meaning there is no per-agent cwd door for teammates, which would make A2's answer "same cwd, access via --add-dir/absolute paths").

### Task 6: Probes A3-1…A3-3 — harness task/team persistence

**Files:**
- Create: `$EXP/evidence/a3-{storage.txt,create.json,list.json,cleanup.json}`

**Interfaces:**
- Produces: A3's scripted evidence. The TUI-restart check joins Task 8's manual batch.

- [ ] **Step 1: Probe A3-1 — where harness state lives on disk**

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
ls -la ~/.claude/ > "$EXP/evidence/a3-storage.txt" 2>&1
find ~/.claude -maxdepth 2 \( -iname '*task*' -o -iname '*team*' -o -iname '*todo*' \) >> "$EXP/evidence/a3-storage.txt" 2>&1
cat "$EXP/evidence/a3-storage.txt"
```

Record: which directories exist and look team/task-shaped (names only — do not open other sessions' data beyond names).

- [ ] **Step 2: Probe A3-2 — task state across separate headless processes.** Two *separate* `claude -p` processes stand in for "restart".

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
claude -p 'If a TaskCreate tool is available to you, create a task with subject "A3-PROBE-GASCAMP" and reply with exactly: CREATED <its id>. If no such tool is available, reply with exactly: NO-TASK-TOOL.' --output-format json > "$EXP/evidence/a3-create.json"
jq -r .result "$EXP/evidence/a3-create.json"
claude -p 'If a TaskList tool is available to you, call it and reply with exactly YES or NO: does any task with subject "A3-PROBE-GASCAMP" exist? If no such tool is available, reply with exactly: NO-TASK-TOOL.' --output-format json > "$EXP/evidence/a3-list.json"
jq -r .result "$EXP/evidence/a3-list.json"
```

Record: tool availability headlessly, and if available whether state crossed processes. Either outcome supports A3's verdict (camp must not *depend* on persistence); the finding documents what actually persists so the spec's "free UX" note is grounded.

- [ ] **Step 3: Probe A3-3 — clean up the probe task (if one was created)**

```bash
EXP=/private/tmp/claude-501/-Users-kiener-code-gascamp/77b93c9f-0bea-4399-a951-1a8899261e81/scratchpad/phase2
cd "$EXP/rig-a"
claude -p 'If a task with subject "A3-PROBE-GASCAMP" exists and a tool lets you delete or close it, do so and reply with exactly: CLEANED. Otherwise reply with exactly: NOTHING-TO-CLEAN.' --output-format json > "$EXP/evidence/a3-cleanup.json"
jq -r .result "$EXP/evidence/a3-cleanup.json"
```

Record: the outcome; if the probe task cannot be removed programmatically, list it in the Task 8 operator batch ("delete task A3-PROBE-GASCAMP if visible") so no probe artifact outlives the phase.

### Task 7: Draft the findings document (headless verdicts only)

**Files:**
- Create: `docs/design/2026-07-06-assumption-findings.md`

**Interfaces:**
- Consumes: every `$EXP/evidence/*` file and `docs-research.md`.
- Produces: the Phase 2 deliverable, complete except the operator-gated sections; the fixture-facts table Phase 8 consumes.

- [ ] **Step 1: Instantiate the template** from the "Findings Document Template" section above, filling the header (`claude --version` from `claude-version.txt`, `uname -sr`).

- [ ] **Step 2: Fill the fixture-facts table (F1–F6)** — one row per fact, each Value stated as something Phase 8 can implement directly (e.g. F3 as a concrete `transcript_path(cwd, sid)` rule), each Evidence cell naming the probe and embedding the load-bearing excerpt (command + trimmed verbatim output). Trim outputs to the lines that carry the fact; never paraphrase them.

- [ ] **Step 3: Fill A3 and A4 completely** (Assumed verbatim from spec §17; Observed; Evidence = docs citations from Task 2 + probe excerpts; Verdict `holds`/`weaker`/`stronger`; Spec impact). Fill A1 and A2 with Assumed, the docs-research citations, the scripted evidence gathered so far (A2), and `**Verdict:** PENDING OPERATOR`.

- [ ] **Step 4: Cross-check against the master plan's exit criteria** — every fixture-fact row answered, every assumption section present, `claude --version` recorded. Fix gaps now.

- [ ] **Step 5: Commit**

```bash
git -C /Users/kiener/code/gascamp add docs/design/2026-07-06-assumption-findings.md
git -C /Users/kiener/code/gascamp commit -m "docs: assumption findings draft — headless evidence for A2/A3/A4 and dispatch fixture facts"
```

### Task 8: STOP — batched operator protocol (A1 + A2/A3 manual halves)

**Files:**
- Create: `$EXP/operator-protocol.md` (scratchpad; full text also pasted to the operator in chat)

**Interfaces:**
- Consumes: Task 2's A1/teammate-UI citations (exact keystrokes, if documented).
- Produces: the operator's report, which Task 9 turns into the A1/A2 verdicts.

- [ ] **Step 1: Assemble the protocol** from the template below. Where the template says *(per docs: …)*, insert the exact UI action from Task 2's citations; if the docs did not cover it, keep the plain-language instruction and note "undocumented — observe and report what the UI offers", which is itself evidence.

- [ ] **Step 2: Paste the full protocol to the operator and STOP.** Do not run further tasks, and do not write A1/A2 verdicts, until the operator reports. The protocol template (one sitting, ~15 minutes):

```markdown
## Operator protocol — Phase 2 manual checks (one sitting)

Setup (2 min):
 1. In a NEW terminal: cd <EXP>/rig-a   (throwaway repo — not gascamp)
 2. Run: claude   (normal TUI session)

M1 — A1: converse with a teammate mid-run
 3. Type exactly: "Spawn a teammate named probe-mate (general-purpose,
    run in background) with this task: using Bash, run `sleep 20` five
    times, announcing PROBE-STEP-1 .. PROBE-STEP-5 before each sleep.
    While it runs, stay responsive to me."
 4. While probe-mate is mid-run, try to interact with it directly
    (per docs: <insert cited affordance/keystrokes here>).
    Attempt to send it: "Also report the current step number."
 5. Observe and note:
    - HOLDS: you can select/see probe-mate as its own conversation and
      your message reaches it mid-run (it answers without being
      restarted).
    - WEAKER: any of — the teammate is not selectable; your message
      queues until its whole task finishes; interaction only works by
      relaying through the main assistant; the UI offers no teammate
      affordance at all.
 6. Report: which of those you saw, the exact keystrokes/menus you used,
    and (verbatim) any reply probe-mate gave mid-run.

M2 — A2: teammate rooted in a different repo
 7. In the SAME session (cwd rig-a), type exactly: "Spawn a teammate
    named cross-mate with this task: report the output of pwd, then
    create a file a2-tui-probe.txt containing A2-TUI in <EXP>/rig-b,
    then stop."
 8. Observe and note:
    - What pwd did cross-mate report (rig-a or rig-b path)?
    - Did the file land in rig-b (check: ls <EXP>/rig-b)? Was there a
      permission prompt, and for what path?
    - HOLDS: some supported affordance let the teammate operate with
      rig-b as its effective working directory.
    - WEAKER: teammate cwd is pinned to the session's repo; rig-b access
      only via absolute paths/approval.
 9. Report: pwd output, file present yes/no, prompts seen.

M3 — A3: does team/task state survive a restart?
 10. Note what the session shows as teammates/tasks. Quit Claude Code
     entirely (exit the TUI). Re-run: claude   (same directory, fresh
     session — do NOT use --resume/--continue).
 11. Observe and note: are the teammates or their tasks from steps 3–7
     visible in the fresh session in any UI surface?
 12. If a task named "A3-PROBE-GASCAMP" is visible anywhere, delete it.
 13. Report: what (if anything) survived, and where you saw it.

Report everything back in one message; verbatim beats summarized.
```

- [ ] **Step 3: While stopped,** leave the working tree clean (Tasks 1–7 committed). Nothing else runs until the report arrives.

### Task 9: Incorporate the operator report; final verdicts; spec edits if divergent

**Files:**
- Modify: `docs/design/2026-07-06-assumption-findings.md`
- Modify (only if a verdict diverges): `docs/design/2026-07-05-gas-camp-design.md` §17 (and any §8.4/§12 sentence the divergence invalidates)

**Interfaces:**
- Consumes: the operator's report (quoted verbatim as Evidence).
- Produces: the merged Phase 2 deliverable.

- [ ] **Step 1: Fill A1 and A2** Observed/Evidence (operator report quoted) and replace `PENDING OPERATOR` with the verdict the evidence supports. Verdict rules: `holds` = observed behavior matches the Assumed sentence; `weaker` = the assumed capability is missing/degraded (decided fallback applies); `stronger` = capability exceeds the assumption. Update A3's section with the M3 result and A4's if anything in the sitting contradicted it.

- [ ] **Step 2: Write each Spec impact line.** For `holds`: "none — §17 confirmed as written." For `weaker`/`stronger`: name the exact spec sentences affected and make the edit in the same PR — §17's assumption bullet becomes a resolution ("Resolved 2026-07-06: … — see docs/design/2026-07-06-assumption-findings.md"), applying the spec's own pre-decided fallback (A1 weaker ⇒ Tier-0 spawns headless + instant attach, which also touches the §8.4 "one surface exception" paragraph; A2 weaker ⇒ §12 already routes cross-rig headless — record that attended cross-rig teammates are out; A4 stronger ⇒ §10's nudge gains the live path note). Do not re-litigate §4 decisions; fallbacks are UX tuning, not structure.

- [ ] **Step 3: Re-read the finished findings doc top to bottom** against the master plan's Phase 2 exit criteria: findings complete per assumption, spec updated or confirmed, every Phase 8 design input a pinned fact. Fix anything missing.

- [ ] **Step 4: Commit**

```bash
git -C /Users/kiener/code/gascamp add docs/design/
git -C /Users/kiener/code/gascamp commit -m "docs: finalize spec §17 assumption findings with operator-verified A1/A2"
```

(If the spec was edited, keep it in this same commit — findings and spec resolution land together.)

### Task 10: Gates, push, PR, CI green

**Files:** none new.

- [ ] **Step 1: Run the repo gates** (no code changed; they must still pass):

```bash
cd /Users/kiener/code/gascamp && cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace
```

Expected: all green.

- [ ] **Step 2: Push and open the PR**

```bash
git -C /Users/kiener/code/gascamp push -u origin phase-2-assumptions
gh pr create --title "Phase 2: verify spec §17 assumptions A1–A4" --body "$(cat <<'EOF'
Verifies spec §17 assumptions A1–A4 against current Claude Code behavior (claude --version recorded in the findings doc) per the master plan's Phase 2 contract.

- Findings: docs/design/2026-07-06-assumption-findings.md — per assumption: Assumed / Observed / Evidence / Verdict / Spec impact
- Fixture facts F1–F6 pin the claude -p dispatch mechanics Phase 8 consumes (session-id assignment, result envelope, transcript path scheme, exit codes, stdin, resume semantics)
- Verdicts: A1 <verdict> · A2 <verdict> · A3 <verdict> · A4 <verdict>
- Spec §17: <"confirmed as written" | "updated in this PR: <what>">
- A1/A2 manual portions operator-verified (protocol + verbatim report quoted in the findings doc)

Plan: docs/superpowers/plans/2026-07-06-phase-2-assumptions.md
EOF
)"
```

Fill the `<verdict>` placeholders with the real verdicts before running — every claim in the body must already be true.

- [ ] **Step 3: Watch CI**

```bash
gh pr checks --watch
```

Expected: fmt, clippy, test (ubuntu), test (macos) all pass. Phase 2 is complete only when green.

---

## Self-Review (performed at plan-writing time)

1. **Contract coverage:** A1 → Tasks 2, 8, 9. A2 → Tasks 2, 5, 8, 9. A3 → Tasks 2, 6, 8(M3), 9. A4 → Tasks 2, 4, 9. Dispatch fixture facts → Tasks 3, 4 (F6), 7. `claude --version` → Tasks 1, 7. Findings doc → Tasks 7, 9. Spec-edit-in-same-PR → Task 9. Branch/PR/CI → Tasks 1, 10. Manual batching + no-verdict-before-report → Task 8. Scratch-repos-only → Global Constraints + every probe `cd`s into a rig.
2. **Placeholders:** the `<insert cited affordance>` slot in Task 8 and `<verdict>` slots in Task 10 are runtime data dependencies (filled from Task 2's output and Task 9's verdicts respectively), each with explicit instructions for filling — not deferred design.
3. **Consistency:** probe ids (D1–D5, A4-1..4, A2-1..3, A3-1..3) match between task steps, evidence filenames, and the fixture-facts table; `$EXP` is declared as a literal in every script because shell state does not persist.

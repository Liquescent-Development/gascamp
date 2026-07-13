# Gas Camp — Spec §17 Assumption Findings (Phase 2)

| Field | Value |
|---|---|
| Date | 2026-07-06 |
| Spec under test | docs/design/2026-07-05-gas-camp-design.md §17 |
| claude --version | `2.1.201 (Claude Code)` |
| Platform | Darwin 25.5.0 (macOS) |
| Default model observed | `claude-fable-5` (from the session init event) |
| Method | claude-code-guide docs research + scripted experiments in throwaway scratch repos (master plan, Phase 2). Raw probe outputs were session artifacts; the load-bearing excerpts are embedded verbatim below. |

Probe naming: D1–D5 (dispatch mechanics), A4-1..4, A2-1..3, A3-1..3. Every
probe ran with cwd inside a throwaway scratch rig, never inside gascamp.
Docs citations are from https://code.claude.com/docs/en/ pages
(cli-reference, headless, sessions, sub-agents, permissions, agent-teams,
tools-reference), retrieved 2026-07-06.

## Fixture facts for Phase 8 (dispatch mechanics)

| # | Fact | Value | Evidence |
|---|---|---|---|
| F1 | Session id assignment for `claude -p` | **campd can pre-assign**: `--session-id <uuid>` is accepted and the result envelope echoes the identical id — the registry row can be written *before* exec (registry-at-birth holds). Without the flag, the harness assigns one, reported in the envelope. | D2 (below); cli-reference.md "`--session-id <uuid>` — Use specific session ID" |
| F2 | Result envelope (`--output-format json`) | A JSON **array** of message objects (docs describe a single object — observed reality is an array). Element types observed: `system/init`, `system/thinking_tokens`, `assistant`, `rate_limit_event`, `result`. Parse rule: the element with `type=="result"` (last), whose keys are: `api_error_status, duration_api_ms, duration_ms, fast_mode_state, is_error, modelUsage, num_turns, permission_denials, result, session_id, stop_reason, subtype, terminal_reason, time_to_request_ms, total_cost_usd, ttft_ms, ttft_stream_ms, type, usage, uuid`. | D1 (below) |
| F3 | Transcript path scheme | `~/.claude/projects/<munge(cwd)>/<session-id>.jsonl`, where `munge` replaces **every non-alphanumeric character with `-`** (per sessions.md; verified here for `/` and `-` against a dash-heavy cwd; lossy but forward-computable). Format: JSONL, one object per line (queue-operations, hook attachments, messages). Overrides: `CLAUDE_CONFIG_DIR` moves the root; **`cleanupPeriodDays` (default 30) eventually deletes transcripts** — "transcript persists forever" is bounded by the user's retention setting. Worktree-cwd spawns land under a per-worktree project dir (verified with a second rig, A2-1) — patrol must compute the watch path from the *worker's* cwd. | D3, A2-1 (below); sessions.md "Where transcripts are stored" |
| F4 | Exit codes | success `0` · CLI usage error `1` (`error: unknown option …`) · resume of unknown id `1` (`No conversation found with session ID: …`) · SIGTERM `143` · SIGKILL `137` · **denied/blocked tool call: still `0`** with `is_error:false` — denials populate the envelope's `permission_denials` array (tool_name, tool_use_id, tool_input) instead of failing the process. SIGCHLD mapping for campd: nonzero/signal ⇒ `session.crashed`; exit 0 ⇒ `session.stopped`; a *denial* is not a crash — worker failure routing must come from the worker contract (close events) and envelope parsing, not the exit code. | D4 (below) |
| F5 | Child stdin handling | Piped stdin is read as prompt input; `< /dev/null` works. **An open non-pipe stdin costs 3 s**: `Warning: no stdin data received in 3s, proceeding without it.` campd must spawn workers with stdin at `/dev/null` — except stream-json workers, where campd deliberately holds the stdin pipe (see A4). | D5, A4-2 warning (below) |
| F6 | `--resume` semantics | Same session id (envelope echoes it), **appends to the same transcript file** (observed 28 → 35 lines), full context available (codeword recalled). Forking is a separate explicit flag (`--fork-session`). Registry rows therefore stay valid across nudges — no id churn. | A4-2 (below); sessions.md "Resume a session" |
| F7 | Ambient config inheritance | A bare `claude -p` inherits the **user's** saved settings: permission allowlists (an unflagged probe ran `git status` via Bash with zero `--allowedTools`), plugins, hooks, default model. Phase 8 must pin worker behavior explicitly per agent definition (`--permission-mode`, `--allowedTools`/`--disallowedTools`, `--model`, `--append-system-prompt`, `--agents`; `--settings`/`--bare` exist for full isolation) or worker capability silently varies per machine. | D4 denied-tool probe series (below) |

### Phase 15 e2e re-verification (2026-07-08, claude `2.1.205`)

The Phase 15 opt-in e2e suite (`make e2e`) ran the F1–F7 facts against real
claude 2.1.205 (pinned at 2.1.201; Phase 11 re-probed at 2.1.204). Verdicts:

- **F1 — HOLDS.** campd pre-assigned `--session-id`; claude honored it (the
  transcript file is named by the pre-assigned uuid).
- **F3 — HOLDS, with a computation refinement (the canary's find).** The munge
  char-scheme is **unchanged** (every non-alphanumeric char → `-`). But claude
  **canonicalizes (realpath-resolves) its cwd** before computing the project
  dir, so campd must canonicalize the *worker* cwd before munging — otherwise
  the path campd records in the registry (and patrol watches, spec §10) diverges
  from where claude actually writes whenever the rig/camp cwd contains a symlink
  component. Observed: with the rig under macOS `tempfile::tempdir()`
  (`/var/folders/…` — a symlink to `/private/var/folders/…`), claude wrote its
  transcript to `~/.claude/projects/-private-var-folders-…-rig1/<sid>.jsonl`
  (its recorded `cwd` was `/private/var/…/rig1`) while campd's raw computation
  pointed at `~/.claude/projects/-var-folders-…-rig1/` (empty, never written) —
  so patrol would watch a never-updated path and false-stall a healthy worker.
  This was latent because the Phase 2 D3 probe used an already-canonical
  `/private/tmp/…` cwd. **Fix landed in this PR:** `dispatch.rs` canonicalizes
  the worker cwd once (fail-fast, no raw fallback) and uses it for both the
  transcript path and the worker's `current_dir`; a deterministic symlink test
  (`daemon_dispatch.rs::worker_cwd_is_canonicalized_so_patrol_watches_the_real_transcript_path`)
  guards it. The spec's §10 wording ("a filesystem watch on the path recorded in
  its registry row") is unchanged — the recorded path is now correct.
- **F5 — HOLDS.** The worker received its task over the campd-held stream stdin
  (HeldStream) and executed the full contract (`camp claim`/`show`/`event
  emit`/`close --outcome pass`), no "command not found".
- **F7 — HOLDS.** The pinned non-interactive worker (`bypassPermissions` +
  explicit `--allowedTools`) edited files and ran `camp` with no prompts.
- **F2 — HOLDS** (captured on the post-fix re-run, 2026-07-09). The stream-json
  capture carried a `type=="result"` element with `is_error==false`, a
  `session_id` echoing the pre-assigned id, and `total_cost_usd` / `ttft_ms` /
  `num_turns` all present (observed Tier-0: `total_cost_usd=0.5878`,
  `ttft_ms=6481`, `num_turns=16`). Note the envelope is stream-JSONL (one event
  per line) in HeldStream mode, not the D1 `--output-format json` array — same
  `result`-element keys, so the parse rule (`type=="result"`) is unchanged.
- **F4 — HOLDS** (captured on the re-run). The Tier-0 worker's clean exit mapped
  to `session.stopped` with `exit_code == 0`.
- **F6** (`--resume`) is out of the happy-path e2e scope.

**Net: F1–F5 and F7 HOLD at claude 2.1.205** (F3 with the campd-canonicalization
fix landed in this PR; F6 out of scope). No pinned fact drifted beyond the F3
computation refinement above.

### #86 config-contamination and the $0 gate (2026-07-13)

The Phase 15 e2e re-verification above ran on a machine whose
`~/.claude/settings.json` set `"verbose": true`. That silently satisfied the
CLI's hard requirement that `--print` + `--output-format stream-json` be
accompanied by `--verbose` (`verbose` resolves flag → settings → false). So
**F5's "HOLDS" for the HeldStream argv was not portable**: on any machine
without that setting, the pre-fix argv is rejected at argv validation (exit 1,
`Error: When using --print, --output-format=stream-json requires --verbose`)
before any worker contract runs. Filed and fixed as #86 — camp now passes
`--verbose` unconditionally in the HeldStream argv, so the operator's setting
no longer participates.

**Re-validation:** the argv-acceptance portion of F5 is now re-validated
portably by the **$0 real-`claude` compatibility gate** (`make compat`,
control-plane design §8), which spawns the real CLI under a **fresh
`CLAUDE_CONFIG_DIR`** (so `verbose` defaults to false) and asserts the fixed
argv is accepted, the pre-fix argv is rejected (#86 reproduced), the
`initialize` handshake round-trips, and a pre-turn `interrupt` is acknowledged
— all at $0, no auth. The task-delivery portion of F5 and F7's capability
pinning (`--model`/`--permission-mode`/`--allowedTools` behavior over a real
turn) still ride the paid `make e2e` tier; those runs are no longer
argv-contaminated now that camp passes `--verbose` explicitly, but re-running
`make e2e` with `verbose` unset to confirm the turn-level facts remains a
follow-up, deferred out of this stream's scope.

### Key probe evidence (dispatch)

D1 — envelope shape and success exit (cwd = scratch rig-a):

```
$ claude -p 'Reply with exactly: D1-OK' --output-format json ; echo exit=$?
exit=0
jq -c '.[] | {type, subtype}':
  {"type":"system","subtype":"init"} … {"type":"result","subtype":"success"}
jq '.[-1] | {is_error, result, session_id}':
  {"is_error": false, "result": "D1-OK", "session_id": "ac779099-2f1d-4c18-bfb2-95681750d1d5"}
```

D2 — pre-assigned session id:

```
requested: 7bd2befc-b018-4080-8738-429d541b3646   (uuidgen, passed via --session-id)
reported:  7bd2befc-b018-4080-8738-429d541b3646   (envelope .session_id)
```

D3 — transcript path for the D2 session:

```
cwd:        /private/tmp/claude-501/-Users-kiener-code-gascamp/…/scratchpad/phase2/rig-a
transcript: ~/.claude/projects/-private-tmp-claude-501--Users-kiener-code-gascamp-…-scratchpad-phase2-rig-a/7bd2befc-….jsonl
```

(`/` → `-`; pre-existing `-` stays `-`, hence the `--` run; consistent with
"non-alphanumeric → `-`" from sessions.md.)

D4 — exit-code table as observed:

```
usage-error exit=1        (stderr: error: unknown option '--definitely-not-a-flag')
bad-resume  exit=1        (No conversation found with session ID: 00000000-…)
denied-tool exit=0        (unapproved `mkfifo`: result "DENIED This command requires approval",
                           permission_denials: [{tool_name:"Bash", tool_input:{command:"mkfifo d4-fifo-probe",…}}],
                           no hang, no file created)
disallowed  exit=0        (--disallowedTools 'Bash': tool absent from toolset; worker reports refusal)
SIGTERM     exit=143
SIGKILL     exit=137
```

D5 — stdin:

```
echo 'Reply with exactly: STDIN-OK' | claude -p --output-format json   → result STDIN-OK
claude -p 'Reply with exactly: NULLSTDIN-OK' … < /dev/null             → result NULLSTDIN-OK
(non-pipe open stdin: "Warning: no stdin data received in 3s, proceeding without it")
```

## A1 — Teammate interaction mechanics

**Assumed (spec §17):** the user can select an attended teammate in the
Claude Code TUI and converse mid-run. Decided fallback if weaker: Tier-0
spawns headless + instant attach.

**Observed:** the docs and the operator's TUI run agree. agent-teams.md
("Control your agent team") documents the affordance:

> "The lead's terminal lists teammates in the agent panel below the prompt
> input. From the panel: Up and down arrows: select a teammate · Enter:
> open the selected teammate's transcript and message it directly ·
> Escape: interrupt the selected teammate's current turn"

Operator check M1 (2026-07-06): a teammate `probe-mate` running five
sequential announced 20 s sleeps was selectable in the agent panel; the
operator opened it and sent "Also report the current step number." while
it was mid-task. The message appears in probe-mate's transcript **between
step 1 and step 2** — delivered into the running conversation at the next
step boundary, not held until the task ended — and probe-mate answered it
without being restarted (its report includes "The current step number is
5 — the final step, now complete"). After finishing, the teammate stays
idle-but-alive and can take follow-up messages.

The one undocumented nuance, now pinned: **mid-run delivery is not
preemption.** The message lands at the teammate's next step boundary and
the *agent* chooses when to act on it — in the observed run it
acknowledged at task end. Operator's words: "it queued my message to the
probe-mate and then ran it at the first chance after it was working."

**Evidence:** agent-teams.md citation above; operator transcripts (lead +
probe-mate) reported verbatim in the working session, key lines
reproduced here:
probe-mate transcript order was `Bash(echo PROBE-STEP-1 && sleep 20)` →
`❯ Also report the current step number.` → `Bash(echo PROBE-STEP-2 …)` →
… → final summary answering the question.

**Verdict:** holds

**Spec impact:** none structural — §17 A1 is resolved as holds in this PR;
§8.4's one-surface-exception (attended Tier-0 as teammate) stands and the
decided fallback is not needed. One behavior note for pack authors and
§10: a message to an attended teammate is delivered at its next step
boundary and answered at the agent's discretion — consistent with patrol's
annotate-only rule for attended sessions.

## A2 — Teammate working directory across repos

**Assumed (spec §17):** unresolved whether a teammate can run with cwd in
a different repo than the session. Camp already routes cross-rig work
headless by default (§12), so this only affects whether *same-rig* attended
work in a multi-rig camp can be a teammate.

**Observed:**

- Operator check M2 (2026-07-06): a teammate `cross-mate`, spawned from a
  session whose cwd was rig-a and tasked with reporting `pwd` and writing
  a file into rig-b, reported
  `pwd = …/phase2/rig-a` — **its cwd was the parent session's, not the
  target repo's** — while the rig-b write itself succeeded as file-level
  access (the attended session's permission classifier auto-allowed it;
  transcript line: "Allowed by auto mode classifier"). No affordance
  re-rooted the teammate into the other repo.

The scripted half agrees:

- No per-agent cwd exists: sub-agents.md — "A subagent starts in the main
  conversation's current working directory"; the Agent tool has no cwd
  parameter; `isolation: worktree` isolates within the *same* repository.
  Probe A2-3 confirmed headlessly: a spawned subagent reported the parent
  session's cwd (`…/phase2/rig-a`) verbatim.
- Headless worker cwd is simply process cwd, and the transcript follows
  it: probe A2-1 from rig-b reported `…/phase2/rig-b` and its transcript
  landed under the rig-b-derived project dir.
- Cross-repo *file access* (not cwd) is grantable: with
  `--permission-mode acceptEdits` a Write into the other rig **succeeded
  only with `--add-dir <rig-b>`** and was refused without it
  (`permission_denials` populated, no file created, no hang). In default
  permission mode, headless writes are refused everywhere (approval is
  impossible), `--add-dir` or not. permissions.md confirms `--add-dir`
  "extends where Claude can read and edit files" and explicitly does not
  change cwd.

**Evidence:** probes A2-1/A2-2/A2-3 excerpts:

```
A2-1 (cwd rig-b): result "/…/phase2/rig-b"; transcript under
  ~/.claude/projects/-…-scratchpad-phase2-rig-b/<sid>.jsonl
A2-2 default mode, no --add-dir:   REFUSED, permission_denials: [Write …/rig-b/a2-probe.txt], no file
A2-2 acceptEdits, no --add-dir:    REFUSED, no file
A2-2 acceptEdits + --add-dir:      DONE, file contains A2-CROSS
A2-2 acceptEdits, write in cwd:    DONE (control)
A2-3 (subagent pwd): "/…/phase2/rig-a" — inherits parent session cwd
```

Docs citations: sub-agents.md "Manage subagent context";
permissions.md "Working directories"; tools-reference.md "Agent tool behavior".
Operator transcripts (lead + cross-mate) reported verbatim in the working
session; the load-bearing lines are reproduced above.

**Verdict:** weaker

**Spec impact:** a teammate cannot run with cwd in a different repo than
the session — the open question §17 left is now answered on the
restrictive branch. Structure is unchanged by design: §12 already routes
cross-rig work to campd-spawned headless sessions "regardless of how
assumption A2 resolves," so that default is now *required* rather than
provisional; same-rig attended teammates (the only case §17 said this
affects) work exactly as designed. Cross-repo *file* access exists for
teammates (absolute paths under an approving permission mode; headless:
`--add-dir` + `acceptEdits`) but does not substitute for per-rig cwd.
§17's A2 bullet is resolved accordingly in this PR.

## A3 — No dependence on harness team persistence

**Assumed (spec §17):** camp deliberately assumes Claude Code team/task
state does **not** survive restarts; the ledger is the only durability. If
the harness persists more, camp gets free UX, not changed semantics.

**Observed:** the harness *does* write team/task state to disk, but
namespaced per session — it is not a cross-session store camp could lean
on even if it wanted to:

- Storage (docs + probe A3-1): team config at
  `~/.claude/teams/{team-name}/config.json` — **removed when the session
  ends**; task lists at `~/.claude/tasks/{team-name}/` — persist locally,
  where `{team-name}` is derived from the session id (agent-teams.md
  "Architecture"). Both directories exist on disk (probe A3-1), with
  dozens of per-session task dirs.
- Cross-process probe A3-2: headless session 1 created task
  "A3-PROBE-GASCAMP" (`CREATED 1`); a **separate** headless session 2
  answered `NO` — the task was invisible to it. A dir named by session 1's
  full session id appeared under `~/.claude/tasks/`.
- Resume probe A3-3: resuming session 1 by id found and deleted the task
  (`CLEANED`) — persistence serves *resumed* sessions only.
- Operator check M3 (TUI restart, 2026-07-06): exiting a session with a
  teammate still registered offers `Exit anyway / Move to background and
  exit / Stay`. "Move to background" preserved the subagent as an
  attachable background session (`claude agents`, `claude attach <id>`,
  `claude logs <id>`, `claude stop <id>`), **but the TUI reported "1 team
  couldn't be moved and was stopped"** — the team itself did not survive.
  A fresh session's agents list (left arrow) shows prior/backgrounded
  agents as dormant "send a prompt to start" entries — recoverable
  *conversation contexts*, not a running team or a shared task store.

**Evidence:** probe outputs quoted above; operator M3 report (verbatim in
the working session) summarized in the bullet above; agent-teams.md
"Architecture":
"The team config directory is removed when the session ends. The task list
directory persists locally … so resumed sessions keep their tasks."
M3 confirms rather than weakens the verdict — camp's assumption is about
*non-dependence*, and even the richest persistence UX observed
(backgrounded sessions) preserves individual contexts while the team was
explicitly stopped; no cross-session discovery mechanism exists to depend
on.

**Verdict:** holds

**Spec impact:** none — §17 confirmed as written (resolution recorded in
this PR). The "free UX" upside is real but resume-scoped: a camp registry
row's session id is enough to recover a worker's harness-side task state
along with its conversation, and backgrounded interactive sessions add
`claude attach <id>` alongside `claude --resume` in §7.4's reachability
ladder.

## A4 — Headless mid-run conversation

**Assumed (spec §17):** conversation with a running headless worker is
tail-the-transcript now, converse via resume after its current turn. If
input streaming into a live headless session is available, the patrol
nudge action (§10) gains a live path instead of waiting for the turn
boundary.

**Observed:** the assumed baseline holds, and the conditional upside is
real — three separate live paths exist:

1. **Tail-now holds** (probe A4-1): while a worker ran a 25 s command, its
   transcript grew continuously — line counts sampled every 3 s while the
   process was alive: `0 → 8 → 12 → 16 → 19 → 20 → 26 → 28(exit)`. The
   stall-patrol heartbeat (transcript-watch, spec §10) is real behavior.
2. **Resume-after holds** (probe A4-2): a post-exit
   `claude -p --resume <sid> '…codeword?…'` recalled `GASCAMP-ZEBRA-42`
   from the earlier turn, under the same session id, appending to the same
   transcript (F6).
3. **Live input exists — stream-json stdin** (probe A4-3): one
   `claude -p --input-format stream-json --output-format stream-json`
   process answered a first user message and then a second one written to
   its stdin 15 s later (`FIRST-OK`, `SECOND-OK`, same session id, exit 0
   on stdin EOF) [exit-on-EOF superseded at 2.1.204 — see the A4-3
   supersession note below]. A campd that spawns workers in stream mode
   and holds the stdin pipe can inject a nudge turn into the live process.
4. **Concurrent resume also works** (probe A4-4): with the original
   process *verifiably still alive* (`alive_at_nudge=yes`, and still alive
   after), `claude -p --resume <sid>` completed successfully (`NUDGE-OK`,
   same session id) — no lock, no error. Caution recorded: this makes two
   processes append to one transcript file; the stream-json path (3) is
   the cleaner live-nudge mechanism for campd-owned workers.

Docs cover the baseline (headless.md documents only run-then-`--resume`;
"docs do not cover" live injection) — the live paths above are
experiment-established facts, version-pinned to 2.1.201.

**Evidence:** probe excerpts:

```
A4-1 tail log:  t=1 alive=yes lines=0 · t=2 alive=yes lines=8 · … · t=16 alive=yes lines=26 · t=17 alive=no lines=28 · exit=0
A4-2 resume:    result "GASCAMP-ZEBRA-42", session_id unchanged (043325a5-…), transcript 28→35 lines
A4-3 stream:    two result events, both success: "FIRST-OK" then "SECOND-OK", one process, one session id, exit 0
                [SUPERSEDED at claude 2.1.204: an idle stream worker does NOT exit on stdin EOF — see probe P3,
                 docs/design/2026-07-07-phase-11-probe-findings.md; the live-input capability itself is unchanged]
A4-4 v2:        alive_at_nudge=yes · concurrent-resume exit=0 result "NUDGE-OK" (same sid) · alive_after_nudge=yes · long-run exit=0
```

**Verdict:** stronger

**Spec impact:** §17's own conditional applies — the patrol nudge action
(§10) gains a live path instead of waiting for the turn boundary, for
campd-spawned workers started in stream-json mode. Spec edit lands in this
PR (see §17/§10 resolution note). Structure unchanged: the ledger, dispatch,
and patrol designs did not assume the absence of this capability.
Phase 11 re-probed at 2.1.204: exit-on-EOF did not reproduce (P3,
docs/design/2026-07-07-phase-11-probe-findings.md) — stream-worker lifetime
is campd-managed via the release rule; the stronger verdict (live input
exists) stands.

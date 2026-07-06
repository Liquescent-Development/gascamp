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
| F3 | Transcript path scheme | `~/.claude/projects/<munge(cwd)>/<session-id>.jsonl`, where `munge` replaces **every non-alphanumeric character with `-`** (verified against a cwd containing `/`, `-`, `.`; lossy but forward-computable). Format: JSONL, one object per line (queue-operations, hook attachments, messages). Overrides: `CLAUDE_CONFIG_DIR` moves the root; **`cleanupPeriodDays` (default 30) eventually deletes transcripts** — "transcript persists forever" is bounded by the user's retention setting. Worktree-cwd spawns land under a per-worktree project dir (verified with a second rig, A2-1) — patrol must compute the watch path from the *worker's* cwd. | D3, A2-1 (below); sessions.md "Where transcripts are stored" |
| F4 | Exit codes | success `0` · CLI usage error `1` (`error: unknown option …`) · resume of unknown id `1` (`No conversation found with session ID: …`) · SIGTERM `143` · SIGKILL `137` · **denied/blocked tool call: still `0`** with `is_error:false` — denials populate the envelope's `permission_denials` array (tool_name, tool_use_id, tool_input) instead of failing the process. SIGCHLD mapping for campd: nonzero/signal ⇒ `session.crashed`; exit 0 ⇒ `session.stopped`; a *denial* is not a crash — worker failure routing must come from the worker contract (close events) and envelope parsing, not the exit code. | D4 (below) |
| F5 | Child stdin handling | Piped stdin is read as prompt input; `< /dev/null` works. **An open non-pipe stdin costs 3 s**: `Warning: no stdin data received in 3s, proceeding without it.` campd must spawn workers with stdin at `/dev/null` — except stream-json workers, where campd deliberately holds the stdin pipe (see A4). | D5, A4-2 warning (below) |
| F6 | `--resume` semantics | Same session id (envelope echoes it), **appends to the same transcript file** (observed 28 → 35 lines), full context available (codeword recalled). Forking is a separate explicit flag (`--fork-session`). Registry rows therefore stay valid across nudges — no id churn. | A4-2 (below); sessions.md "Resume a session" |
| F7 | Ambient config inheritance | A bare `claude -p` inherits the **user's** saved settings: permission allowlists (an unflagged probe ran `git status` via Bash with zero `--allowedTools`), plugins, hooks, default model. Phase 8 must pin worker behavior explicitly per agent definition (`--permission-mode`, `--allowedTools`/`--disallowedTools`, `--model`, `--append-system-prompt`, `--agents`; `--settings`/`--bare` exist for full isolation) or worker capability silently varies per machine. | D4 denied-tool probe series (below) |

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

**Observed (docs so far; TUI check pending):** agent-teams.md ("Control
your agent team") documents exactly the assumed affordance:

> "The lead's terminal lists teammates in the agent panel below the prompt
> input. From the panel: Up and down arrows: select a teammate · Enter:
> open the selected teammate's transcript and message it directly ·
> Escape: interrupt the selected teammate's current turn"

Whether a message sent mid-turn is *delivered* mid-turn or queues until the
teammate's turn boundary is **not documented** — that is the operator
check (protocol M1).

**Evidence:** agent-teams.md citation above; operator report pending.

**Verdict:** PENDING OPERATOR

**Spec impact:** PENDING OPERATOR

## A2 — Teammate working directory across repos

**Assumed (spec §17):** unresolved whether a teammate can run with cwd in
a different repo than the session. Camp already routes cross-rig work
headless by default (§12), so this only affects whether *same-rig* attended
work in a multi-rig camp can be a teammate.

**Observed (scripted half; TUI check pending):**

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

**Verdict:** PENDING OPERATOR

**Spec impact:** PENDING OPERATOR

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

**Evidence:** probe outputs quoted above; agent-teams.md "Architecture":
"The team config directory is removed when the session ends. The task list
directory persists locally … so resumed sessions keep their tasks."
Operator check M3 (TUI restart) pending as confirmation; it cannot weaken
this verdict — camp's assumption is about *non-dependence*, and no
cross-session discovery mechanism exists to depend on.

**Verdict:** holds

**Spec impact:** none — §17 confirmed as written. The "free UX" upside is
real but resume-scoped: a camp registry row's session id is enough to
recover a worker's harness-side task state along with its conversation.

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
   on stdin EOF). A campd that spawns workers in stream mode and holds the
   stdin pipe can inject a nudge turn into the live process.
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
A4-4 v2:        alive_at_nudge=yes · concurrent-resume exit=0 result "NUDGE-OK" (same sid) · alive_after_nudge=yes · long-run exit=0
```

**Verdict:** stronger

**Spec impact:** §17's own conditional applies — the patrol nudge action
(§10) gains a live path instead of waiting for the turn boundary, for
campd-spawned workers started in stream-json mode. Spec edit lands in this
PR (see §17/§10 resolution note). Structure unchanged: the ledger, dispatch,
and patrol designs did not assume the absence of this capability.

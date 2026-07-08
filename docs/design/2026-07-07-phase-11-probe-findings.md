# Gas Camp — Phase 11 Probe Findings (transcript munge, stream-json worker lifetime)

| Field | Value |
|---|---|
| Date | 2026-07-07 |
| claude --version | `2.1.204 (Claude Code)` |
| Platform | Darwin 25.5.0 (macOS) |
| Method | Scripted probes in the session scratchpad (throwaway dirs, never inside gascamp), per the Phase 2 method. Raw probe outputs were session artifacts; the load-bearing excerpts are embedded verbatim below. |

Context: Phase 2's fixture facts (docs/design/2026-07-06-assumption-findings.md)
were pinned at claude 2.1.201. Phase 11 consumes two of them in load-bearing
ways — F3's transcript-path munge (the patrol transcript watches compute the
watched path from it) and A4-3's stream-json mechanics (the nudge live path) —
so both were re-probed at the current version before the Phase 11 plan relied
on them. P1 confirms; P3 partially supersedes.

## P1 — Transcript munge is per-CHARACTER for non-ASCII cwds (F3 confirmed; PR #14 review note resolved)

**Question (carried PR #14 review finding 6):** camp's `spawn::munge` maps
every non-ASCII-alphanumeric CHARACTER to one `-`. F3 verified ASCII only;
whether real claude munges unicode cwds per BYTE instead was unverified, and
the patrol watches consume this exact path.

**Probe:** a session run from a cwd whose basename is `héllo-日本`
(`é` = 2 UTF-8 bytes, `日`/`本` = 3 bytes each), session id pre-assigned.

```
cwd:        …/scratchpad/p11/héllo-日本
sid:        c7f93f71-b630-41dd-b47c-deec481105bb   (exit=0)
transcript: ~/.claude/projects/…-scratchpad-p11-h-llo---/c7f93f71-….jsonl
```

Basename mapping observed: `héllo-日本` → `h-llo---` — `h` + one dash for
`é` + `llo` + one dash for `-` + one dash EACH for `日` and `本`. One dash
per character, regardless of byte width: **exactly `spawn::munge`'s
behavior.** A per-byte scheme would have produced `h--llo------`.

**Consequence:** `spawn::transcript_path_under` is faithful as written; no
behavior change. The "unverified" caveats in spawn.rs's comments are removed
in this Phase 11 change, citing this probe.

## P2 — Stream-json flag composition and message wire shape

**Probe:** `claude -p --input-format stream-json --output-format stream-json
--session-id <uuid> --append-system-prompt <text>` with stdin held on a fifo;
two user messages written 3+ s apart, each as one line:

```
{"type":"user","message":{"role":"user","content":"Reply with exactly: S1-OK"}}
```

**Observed (sid `85cc86d9-13f8-4565-ba0c-9ffad7275a88`):**
- All flags accepted together; no `--verbose` requirement at 2.1.204.
- Plain-string `content` accepted.
- Both turns answered; both result lines echo the pre-assigned session id:

```
{'type': 'result', 'subtype': 'success', 'is_error': False, 'result': 'S1-OK', 'session_id': '85cc86d9-…'}
{'type': 'result', 'subtype': 'success', 'is_error': False, 'result': 'S2-OK', 'session_id': '85cc86d9-…'}
```

- Stream-json stdout is JSONL (system/hook events, init, deltas, result — one
  object per line). The F2 parse rule for stream captures is "the line whose
  `type == "result"`".
- F7 reconfirmed in passing: the unpinned probe inherited the user's
  SessionStart hooks (visible as `hook_started`/`hook_response` lines).

**Consequence:** the Phase 11 stream spawn argv and the nudge wire write are
pinned by tests to these shapes.

## P3 — An idle stream-json worker does NOT exit on stdin EOF (supersedes A4-3's exit-on-EOF at 2.1.204)

**Probe:** one message, wait for its result, then close the last stdin
writer (fifo EOF) with the process idle; watch for exit.

```
result_after=8s
ALIVE_PRE_EOF=yes
(fd3 closed — stdin EOF delivered)
STILL_ALIVE_AFTER=45s        ← still running 45 s after EOF
killed 137                    (SIGKILL required; probe 2's run suggests >3 min)
```

The process DOES stay alive between turns with stdin held
(`ALIVE_BETWEEN=yes`, `ALIVE_AFTER_PAUSE=yes` in P2) and answers a later
turn — the held-pipe nudge path is real.

**Supersession statement:** A4-3's "exit 0 on stdin EOF" (pinned at 2.1.201)
did not reproduce at 2.1.204 for an idle process; camp's stream-worker
lifetime is therefore campd-managed (the release rule, spec §10 mechanics).
The A4 verdict itself (stronger — live input exists) stands unchanged.

**Design consequences (plan Decisions C2/C4):**
- **Release rule (C2):** when a tracked worker's bead closes, campd releases
  it — drop the held stdin (EOF), arm a bounded release-grace timer, SIGKILL
  if still alive at fire — and the reap records `session.stopped` with the
  reason, never `session.crashed` (campd initiated the termination of a
  worker whose work was done).
- **Crash resilience (C4):** orphaned stream workers survive a campd
  `kill -9` (EOF does not kill them), finish their current turn, and linger
  idle; adoption releases the lingering-finished and re-arms the genuinely
  working. Spec §8.5's "workers outlive campd" holds in stream mode.

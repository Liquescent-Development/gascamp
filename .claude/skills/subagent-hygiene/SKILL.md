---
name: subagent-hygiene
description: Use when a session in this repo is waiting on an asynchronous result it did not compute inline — spawning a helper or background agent, OR waiting on CI / a GitHub Actions run / a deploy / any long-running command you backgrounded and expect to be "notified" about — before writing a helper's kickoff prompt, when deciding how a result comes back, when about to end a turn while that work is still running or counting on a tracked background task / Monitor / completion notification to re-invoke you, or when a helper or watch has gone quiet or looks stuck.
---

# Subagent Hygiene

## Overview

Completion callbacks do not reliably wake stopped sessions, and a
stopped session cannot poll. This holds for ANY asynchronous result you
did not compute inline — a helper you spawned, and equally an EXTERNAL
one: a CI / GitHub Actions run, a deploy, or any long-running command you
backgrounded and are "waiting to be notified" about. Nothing external
wakes a stopped session; only inbound MESSAGES do. Design every wait to
survive a lost callback: watch it to a terminal result in the
foreground, or hand the wait to a party whose own completion messages
you — and for spawned helpers, payload in a file, addressing by explicit
agent ID, and poll whenever you wake.

Live incidents in this project: four stranded callbacks (parents idling
forever "waiting to be notified"; one helper wedged on an undelivered
permission escalation that its parent could not even TaskStop), two
misrouted reports ("main" routes to the child itself; display names
like "phase-14" don't resolve — a human pasted the reports back in by
hand), and repeated stalls on an external CI run — a teammate pushed and
reported "CI running, it'll notify me," the lead parked on "once CI is
green I'll do the next step," and neither woke until the human operator
poked. CI is not a spawned agent, so nobody mapped the callback rule onto
it — same trap, external signal.

The 2026-07-09/10 dispatch-lifecycle session re-proved it against this
skill's earlier revision: seven stops across four agents parked on "my
armed watcher will wake me" — tracked background tasks, heartbeat
loops, a one-hour Monitor armed on process exit, a `gh pr checks
--watch` piped through tail — and every single one was confirmed fully
stopped when resumed ("was stopped (completed)"). One agent finished
its work, pushed, saw CI go green, then stranded before REPORTING — the
finished result sat invisible for ~5 hours until the human operator
noticed. Armed watchers are hints, not wake guarantees.

## Rules

**1. Never end your turn waiting to be notified of an async result.**
This covers a helper you spawned AND any external async state — a CI /
GitHub Actions run, a deploy, a long-running command you backgrounded. A
stopped session receives no external signal; only an inbound message
wakes it. To learn the result, do ONE of these — never stop on "it'll
notify me":

- **(a) Foreground-watch to a terminal state inside the turn that needs
  it.** Run it to completion and read the settled outcome before you
  stop. For CI: run `gh pr checks <pr> --watch` to completion, then
  report the settled pass/fail — not "CI is running."
- **(b) Hand the wait to a party whose own completion wakes you** — a
  helper you spawned that will SendMessage you at your explicit agent ID
  when done (an inbound message wakes a stopped session; external
  completion does not).
- **(c) Stay in-turn and poll.** If the harness demotes your long
  commands to background tasks (so a blocking foreground wait is
  unavailable), do NOT end your turn — keep issuing cheap, fast
  foreground calls in a loop (read the task's output-file tail, check
  the process) within the SAME turn until the verdict lands. An agent
  strands only when it ends its turn, never between tool calls; a few
  dozen cheap polls are nothing against a stranded pipeline.

**Armed watchers are not wake guarantees.** A tracked background task, a
Monitor until-loop, a heartbeat, a completion notification — each claims
it will re-invoke you. Treat every such claim as unreliable: if your
plan for learning a result is "something will wake me," your plan is
(a), (b), or (c) above — never a stop.

**The report ships in the turn that verified the last result — and
"ships" means TRANSMITTED, not written.** Work is not delivered until
the report reaches the party waiting on it: if your harness returns
your final message to the parent, that final message is the report; if
you are a teammate whose plain-text output nobody receives, you MUST
SendMessage it (for substantial output, the Rule 2 pointer message —
"done, report at <path>" — is itself the transmission: the file is the
payload, the send is the delivery). Text printed in your own turn is not a delivered report
— a tested agent verified its result in-turn, "wrote" the report as
plain output, went idle, and the finished deliverable stayed invisible
until the lead pinged it. When unsure which channel applies,
SendMessage: a duplicate report is free; a stranded one cost this
project ~5 hours. Never park a finished result behind a stop ("I'll
report once CI settles").

Whenever you wake for any reason, poll every outstanding result directly
(`gh pr checks`, the helper's status/output) before anything else.

**2. File handoffs for anything substantial.** Agree the exact output
file path in the helper's kickoff prompt (your worktree or the session
scratchpad); the helper writes its report there, and its completion
message says only "done, report at <path>". A path you named in the
prompt is requested output — the general advice against unsolicited
report files does not apply to it. Files survive every routing and
callback failure.

**3. Address by explicit agent ID.** If you want messages back, put
YOUR agent ID (from your own spawn context) in the kickoff prompt. From
a child's position, "main" routes to the child itself, and human-facing
display names do not resolve.

**4. Keep helpers inside their own permission envelope.** Never design
a helper task that needs a permission escalation — escalations route to
the top-level team lead and can be lost, wedging the helper (and
ownership may mean you can't TaskStop it). If a task needs broader
permissions, do it yourself or ask the lead.

**5. Silent or stuck helper: resume it, don't respawn.** SendMessage
wakes stopped sessions. Check its transcript/output file first; a
respawn duplicates work and orphans the original. A stop whose final
message carries waiting or intention language — "watchers will wake
me," "I'll resume when it completes," "let me check X next" — IS a
stalled helper: resume it with a direct message immediately; the send
result tells you whether it was truly stopped, and the message is
harmless if it wasn't.

## Kickoff checklist

Every helper prompt names: (a) the exact output file path; (b) your
explicit agent ID for the completion ping; (c) a task fully inside the
helper's permission envelope.

## Red flags — stop, you're about to strand work

| Thought | Reality |
|---------|---------|
| "It'll notify me when it's done" | Callbacks don't reliably wake stopped sessions. Collect before stopping, or poll on every wake. |
| "I'll just wait for the report" | A stopped session cannot poll. Keep working, run the helper synchronously, or ensure an inbound message — and still poll on wake. |
| "CI is running, it'll notify me when it's green" | Nothing external wakes a stopped session. Foreground-watch to a terminal result (`gh pr checks <pr> --watch`), or hand the wait to a helper that messages you — never stop waiting to be notified. |
| "Send your findings to main" | From the child, "main" IS the child. Give your explicit agent ID. |
| "Report to phase-14 when done" | Display names don't resolve. Explicit agent ID, plus the file path. |
| "The final message will carry the full report" | Substantial output goes to the agreed file; the message says "done, report at <path>". Messages get lost; files don't. |
| "The helper can just ask for permission" | Escalations route to the top lead and can be lost — the helper wedges. Keep the task in-envelope. |
| "No answer — I'll spawn a fresh one" | Resume with SendMessage; check its transcript/output file first. |
| "My tracked background task / Monitor / heartbeat will re-invoke me" | Every such stop in the 2026-07-09/10 session — seven across four agents — was confirmed fully stopped. Foreground-watch, hand the wait to a messaging helper, or poll in-turn; never stop on an armed watcher. |
| "The harness demotes my foreground calls, so I can't block in-turn" | You don't need to block. Stay alive with cheap, fast polls in a loop — you strand only by ending your turn, never between tool calls. |
| "I'll deliver the report once CI / the run settles" | The report ships in the same turn as the last verified result. A finished, unreported result idled a pipeline ~5 hours. |
| "Let me check X next" (as a final message) | That's a stop wearing progress clothing — you stay stopped until someone notices. Finish the deliverable in-turn. |
| "I wrote the report in my turn output" | Un-sent text is not a delivered report — nobody receives a teammate's plain output. Transmit it: final message (if your harness returns it to the parent) or SendMessage. When unsure, send — duplicates are free. |

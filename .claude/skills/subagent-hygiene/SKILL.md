---
name: subagent-hygiene
description: Use when spawning helper or background agents from any session in this repo — before writing a helper's kickoff prompt, when deciding how its results come back, when about to end a turn while a helper is still running, or when a helper has gone quiet or looks stuck.
---

# Subagent Hygiene

## Overview

Completion callbacks do not reliably wake stopped sessions, and a
stopped session cannot poll. Inbound MESSAGES do wake stopped sessions.
Design every helper handoff to survive a lost callback: payload in a
file, addressing by explicit agent ID, and poll whenever you wake.

Live incidents in this project: four stranded callbacks (parents idling
forever "waiting to be notified"; one helper wedged on an undelivered
permission escalation that its parent could not even TaskStop) and two
misrouted reports ("main" routes to the child itself; display names
like "phase-14" don't resolve — a human pasted the reports back in by
hand).

## Rules

**1. Never end your turn expecting a completion callback to wake you.**
Either collect results before you stop (run the helper synchronously,
or keep doing other work and check), or make the helper actively
SendMessage you at your explicit agent ID. Whenever you wake for any
reason, poll every outstanding helper's status/output directly before
anything else.

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
respawn duplicates work and orphans the original.

## Kickoff checklist

Every helper prompt names: (a) the exact output file path; (b) your
explicit agent ID for the completion ping; (c) a task fully inside the
helper's permission envelope.

## Red flags — stop, you're about to strand work

| Thought | Reality |
|---------|---------|
| "It'll notify me when it's done" | Callbacks don't reliably wake stopped sessions. Collect before stopping, or poll on every wake. |
| "I'll just wait for the report" | A stopped session cannot poll. Keep working, run the helper synchronously, or ensure an inbound message — and still poll on wake. |
| "Send your findings to main" | From the child, "main" IS the child. Give your explicit agent ID. |
| "Report to phase-14 when done" | Display names don't resolve. Explicit agent ID, plus the file path. |
| "The final message will carry the full report" | Substantial output goes to the agreed file; the message says "done, report at <path>". Messages get lost; files don't. |
| "The helper can just ask for permission" | Escalations route to the top lead and can be lost — the helper wedges. Keep the task in-envelope. |
| "No answer — I'll spawn a fresh one" | Resume with SendMessage; check its transcript/output file first. |

---
description: Converse with any running or exited camp session — deliver a turn live (held stdin) or via resume. Wraps the camp CLI.
argument-hint: "<session> \"<message>\""
allowed-tools: Bash(camp:*)
---
Send the message to the session (live into its current turn when campd holds
its stdin; otherwise via `claude --resume` after its turn — the reply prints
below):

```!
camp nudge $ARGUMENTS
```

Session names: /camp:status or `camp top` for live sessions; for an exited
worker (the resume path), find the name with `camp show <bead>` or in
`camp events` (the session.woke event's `name`). Report the outcome (and
any printed reply) to the user.

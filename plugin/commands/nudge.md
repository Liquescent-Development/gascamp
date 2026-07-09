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

Session names come from /camp:status or `camp top`. Report the outcome (and
any printed reply) to the user.

---
description: Sling work into the camp — a Tier-0 bead or a formula run. Wraps the camp CLI.
argument-hint: "\"<title>\" [--agent A] [--rig R]  |  --formula NAME [--rig R]"
allowed-tools: Bash(camp:*)
---
Create the work; campd — the one dispatcher — takes it from there
(Tier 0 = one worker dispatch, ~3 ledger writes):

```!
camp sling $ARGUMENTS
```

This command only enqueues; there is no second dispatch path (spec §8.4).
Watch progress with /camp:status or `camp top`. To converse with the running
worker, use /camp:nudge (`camp nudge <session> "<message>"`) — delivered live
into its current turn, or via `claude --resume` after the turn if it has
exited. Report the created bead id (or run id) to the user.

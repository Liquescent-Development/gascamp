---
description: Print the camp event log — the whole story, one row per event. Wraps the camp CLI.
argument-hint: "[--json] [--from N] [--to N]"
allowed-tools: Bash(camp:*)
---
The append-only event log reconstructs the entire system (spec §13.1):

```!
camp events $ARGUMENTS
```

---
description: Reconcile the session registry against reality — crashed sessions to ready, re-arm stalls. Wraps the camp CLI.
allowed-tools: Bash(camp:*)
---
Reconcile the session registry against the process table and transcripts
(spec §8.5) — the routine campd runs at startup, on demand:

```!
camp adopt
```

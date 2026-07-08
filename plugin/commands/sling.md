---
description: Sling work into the camp — a Tier-0 bead or a formula run. Wraps the camp CLI.
argument-hint: "\"<title>\" [--agent A] [--rig R]  |  --formula NAME [--rig R]"
allowed-tools: Bash(camp:*)
---
Create the work; campd dispatches it (Tier 0 = one worker spawn, ~3 ledger writes):

```!
camp sling $ARGUMENTS
```

Attended surface (spec §8.4; assumption A1 resolved HOLDS): if this created a
Tier-0 bead and the operator is present, spawn the bead's pack agent as a
teammate in this session and have it follow the `worker` skill
(recall → claim → work → emit milestones → remember → close → exit). A message
you send that teammate lands at its next step boundary and it answers at its
discretion — delivery is not preemption. Do NOT fall back to headless+attach;
the teammate surface is the design.

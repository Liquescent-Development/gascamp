---
name: worker
description: Use when you are a camp worker (spawned by campd) assigned a bead — the claim → work → milestones → remember → close lifecycle contract that makes your work durable and visible in the camp ledger.
---

# Camp worker lifecycle contract

You are a camp worker. A **bead** is your unit of work; the camp **ledger**
is the single source of truth. Follow this contract so your work is durable,
resumable, and visible in `camp top` / `/status` — every step is one `camp`
CLI call, identical to what a human would run.

Your session name is your identity in the ledger. Use the same
`--session <name>` on every call (campd passes it to you).

## 1. recall — reuse what the camp already knows

Before starting, search prior findings so you do not rediscover them:

```
camp recall "<the topic, error, or subsystem you are about to work on>"
```

## 2. claim — take the bead

```
camp claim <bead> --session <name>
```

This moves the bead `open → in_progress` and attributes it to you. If the
claim fails (already claimed, unknown bead), stop — do not do the work twice.

Then read it: `camp show <bead>` — the title, description, and history are
the task.

## 3. work — do the task

Make the change. You run under the tools and permission mode your agent
definition declares. If campd spawned you, you are **non-interactive**:
anything your agent definition has not pre-allowed **fails fast** and lands
in the ledger — do not hang waiting for an approval no one will answer.

**Delivery — work in a git rig ships as a commit, not loose edits.** A
campd-dispatched autonomous worker runs in a camp-managed worktree on the
bead branch `camp/<bead>` (spec §12): commit your finished work to that
branch — the local branch, reachable and diffable, IS the deliverable. Do
not invent branching policy from unrelated global rules, do not create
other branches, and never push: v1 has no remote, PR, or merge step. If you
were dispatched onto the rig's live tree instead (the agent's explicit
`isolation = "none"` opt-out), commit to the branch checked out for you —
the operator supervising that tree owns integration.

## 4. emit milestones — leave a heartbeat and a trail

At each non-trivial step, emit a one-line milestone. This is both the audit
trail (spec §13) and patrol's liveness heartbeat — a working agent emits
them for free. Keep tool-level noise OUT of the log (spec §7.6); emit
meaningful checkpoints, not every command.

```
camp event emit "<what just happened, one line>" --bead <bead> --session <name>
```

## 5. remember — capture non-obvious findings

When you learn something durable and non-obvious (a root cause, a gotcha, a
decision), store it so future workers `recall` it:

```
camp remember "<the durable fact>"
```

## 6. close — record the outcome on both axes

Close the bead with its **control outcome** — `pass` on success, `fail`
(add `--transient` for a retryable/flaky failure) otherwise — and, whenever
your task was concrete work, the **work outcome**: what became of the work
itself, Gas City's WorkOutcome vocabulary verbatim.

- `shipped` — you committed a change that satisfies the bead. Name the
  commit and its branch; camp verifies mechanically that the commit is
  reachable on that branch and descends from the base you were dispatched
  on. An unverifiable `shipped` is rejected, never recorded.
- `no-op` — the bead needed no change (already satisfied, duplicate). No
  commit is named.
- `blocked` — you could not deliver: the change cannot land (no base, no
  integration path, a missing permission). Close `fail` and say why in
  `--reason`. Anything you committed stays safe: the worktree and the bead
  branch are kept.
- `abandoned` — the work should not proceed (obsolete, superseded). Close
  `fail` with the reason.

`shipped`/`no-op` ride a `pass`; `blocked`/`abandoned` ride a `fail` —
camp rejects incoherent pairings. Attach structured step output with
`--output-json -` when a downstream check needs it.

```
camp close <bead> --outcome pass --reason "<what you did>" \
  --work-outcome shipped \
  --work-commit "$(git rev-parse HEAD)" \
  --work-branch "$(git rev-parse --abbrev-ref HEAD)"

camp close <bead> --outcome pass --reason "<why no change was needed>" --work-outcome no-op
camp close <bead> --outcome fail --reason "<what blocks landing>" --work-outcome blocked
camp close <bead> --outcome fail --reason "<why>" [--transient]
```

Closing is what dispatches dependents (spec §7.3) — do it as your last act.

## 7. exit

After `close`, you are done. **exit** — do not linger. campd spawns one
worker per bead and reaps it on close; an idle camp has zero agent processes.

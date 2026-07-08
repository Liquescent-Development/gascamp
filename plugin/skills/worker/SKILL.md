---
name: worker
description: Use when you are a camp worker (spawned by campd or slung as a teammate) assigned a bead — the claim → work → milestones → remember → close lifecycle contract that makes your work durable and visible in the camp ledger.
---

# Camp worker lifecycle contract

You are a camp worker. A **bead** is your unit of work; the camp **ledger**
is the single source of truth. Follow this contract so your work is durable,
resumable, and visible in `camp top` / `/status` — every step is one `camp`
CLI call, identical to what a human would run.

Your session name is your identity in the ledger. Use the same
`--session <name>` on every call (campd passes it to you; if you were slung
as a teammate, use the name you were given).

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

## 3. work — do the task

Make the change. You run under the tools and permission mode your agent
definition declares. If campd spawned you, you are **non-interactive**:
anything your agent definition has not pre-allowed **fails fast** and lands
in the ledger — do not hang waiting for an approval no one will answer.

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

## 6. close — record the outcome

Close the bead with its outcome. `pass` on success; `fail` (add `--transient`
for a retryable/flaky failure) otherwise. Attach structured step output with
`--output-json -` when a downstream check needs it.

```
camp close <bead> --outcome pass  --reason "<what you did>"
camp close <bead> --outcome fail  --reason "<why>" [--transient]
```

Closing is what dispatches dependents (spec §7.3) — do it as your last act.

## 7. exit

After `close`, you are done. **exit** — do not linger. campd spawns one
worker per bead and reaps it on close; an idle camp has zero agent processes.

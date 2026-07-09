# Gas Camp

**A lightweight, single-user, local way to run Claude Code workers — one small
Rust binary, one SQLite ledger, zero cost when idle.**

Gas Camp (`camp`) is a single-user, local, event-sourced orchestrator for task
tracking *and* AI-agent work. Everything durable lives in one SQLite ledger
(`camp.db`): an append-only event log plus the state folded from it, so every
action has a cause and `kill -9` never loses history. Its daemon (`campd`)
sleeps on OS events and burns no CPU when nothing is happening; real work is
dispatched to [Claude Code](https://docs.claude.com/en/docs/claude-code)
workers you can watch, tail, and talk to.

**The most approachable way in is the Claude Code plugin.** Install it and drive
your local agent fleet with `/camp:sling`, `/camp:status`, `/camp:adopt`, and `/camp:events` from
inside a Claude Code session — no raw-CLI ceremony. Every command also works
straight from the `camp` terminal CLI; the plugin is a thin wrapper over it.

Think of camp as a **simpler local sibling of [Gas
City](https://github.com/gastownhall/gascity)** — what **k3s is to k8s**: the
same six primitives and convergence model, with the heavyweight store (Dolt)
swapped for one proportionate SQLite file. The compatibility is exact where it
counts: **every valid camp formula is a valid Gas City formula-v2 file**, and
when a job outgrows camp you graduate it with `camp export --city` (§ [Export /
graduation](#export--graduation-to-gas-city)). Camp for lunch; the city for
fleets.

## Highlights

- **Orchestrate from inside Claude Code.** camp ships a Claude Code plugin: run
  and watch a local fleet of agents with `/camp:sling`, `/camp:status`, `/camp:adopt`, and
  `/camp:events` — slash commands that are thin wrappers over the `camp` CLI — plus
  session-lifecycle hooks, the worker skill, and a fleet statusline.
- **Idle is free.** No ticks, no polling loops. Idle `campd` targets < 20 MB
  RSS and 0.0% CPU; an idle camp has zero agent processes.
- **One SQLite ledger = the whole story.** Append-only events + folded state in
  one documented-schema file. `camp events --json` exports canonical JSONL for
  any range, so text-tool audit is one command away.
- **Kill-9-safe.** `campd` holds no private state. Crash anything, restart, and
  it picks up from the ledger. `camp doctor --refold` rebuilds state from
  history and reports any drift.
- **Dispatches real Claude Code workers.** `camp sling "…"` (or `/camp:sling`) spawns
  a worker that claims a bead, does the work, emits milestones, and closes with
  an outcome — every worker registered at birth, tailable, and resumable.
- **Formula graphs when you want them.** Dependency-gated steps with script
  verification (`check`), bounded transient retries (`retry`), and runtime
  fan-out (`on_complete`) — all declared in TOML.
- **Cron & event orders.** Scheduled or event-triggered formulas, including
  while you're away, with an optional launchd agent for fire-at-login.

## Requirements

- A recent **stable Rust** toolchain — edition 2024, so Rust **1.85 or newer**.
  The repo pins `channel = "stable"` in `rust-toolchain.toml`.
- **git** on your `PATH`.
- **For AI dispatch only** (`camp sling`, formula/order runs): an authenticated
  **`claude` CLI** (Claude Code) on your `PATH`, plus a pack that provides the
  agent you route to. The free local task-lifecycle commands (below) need
  neither.

## Install

Build the release binary and install it (plus the `campd` symlink) with `make`:

```sh
git clone https://github.com/richardkiene/gascamp
cd gascamp
make install                 # installs to ~/.local/bin by default
```

`make install` runs `cargo build --release`, copies `camp` into `$PREFIX/bin`,
and creates a `campd` symlink beside it. `campd` is the **same binary in daemon
mode** — `main` dispatches on argv0, so the symlink is how the daemon gets its
name (you can also run `camp daemon`).

Override the prefix (any writable dir works — no root required):

```sh
make install PREFIX=/usr/local        # -> /usr/local/bin/camp + campd
make install PREFIX=$HOME/.local      # the default
```

Make sure `$PREFIX/bin` is on your `PATH`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Uninstall removes both the binary and the `campd` symlink:

```sh
make uninstall                        # honors the same PREFIX
```

You almost never start the daemon by hand: **`campd` auto-starts** the first
time a `camp` verb needs it (it spawns detached, records the cause in the
ledger, and waits for readiness), and `camp stop` shuts it down. That's the
"idle is free" model — nothing runs until there's work.

## Quickstart

Two ways in — the Claude Code plugin (recommended) or the raw CLI. They do the
same thing; the plugin is a thin wrapper.

### Use camp from inside Claude Code (recommended)

**1. Install the `camp` binary and put it on your `PATH`.** The plugin's slash
commands shell out to `camp`, so the binary MUST be installed and on `PATH` for
the plugin to do anything:

```sh
git clone https://github.com/richardkiene/gascamp
cd gascamp
make install                              # -> ~/.local/bin/camp (+ campd symlink)
export PATH="$HOME/.local/bin:$PATH"      # if it isn't already
```

(See [Install](#install) for `PREFIX` overrides and `make uninstall`.)

**2. Install the Claude Code plugin.** From inside a Claude Code session, add
this repo as a plugin marketplace, install the `camp` plugin, and reload:

```
/plugin marketplace add richardkiene/gascamp
/plugin install camp@gascamp
/reload-plugins
```

Claude Code reads the repo's `.claude-plugin/marketplace.json`; `camp` is the
plugin name and `gascamp` is the marketplace name. `/reload-plugins` activates
it without a restart. The plugin's commands are namespaced under the plugin
name, so they appear as `/camp:sling`, `/camp:status`, `/camp:adopt`, and
`/camp:events`.

**3. Make a camp, then drive it with slash commands.** Create a camp once and
start Claude Code from inside it (the plugin's SessionStart hook registers the
session):

```sh
mkdir demo && cd demo
camp init                                  # create ./.camp (ledger + config)
camp rig add . --prefix demo               # register this repo as a rig
# now start Claude Code in this directory
```

From that session:

```
/camp:status                                    # fleet snapshot: live sessions, ready/open beads
/camp:sling "add a --json flag to toy ls, TDD it"
/camp:events                                    # the append-only event log — the whole story
/camp:adopt                                     # reconcile the session registry against reality
```

`/camp:sling` hands the bead to a **real Claude Code worker** that follows the
plugin's **worker skill** (recall → claim → work → emit milestones → remember →
close → exit) and, when you're present, spawns it as a teammate you can talk to
mid-run. That one step needs an authenticated `claude` CLI and a routable agent
— see [The AI step](#the-ai-step). `/camp:status`, `/camp:events`, and `/camp:adopt` are free
and need neither.

### …or drive it straight from the CLI

Every verb works identically in the terminal — the plugin adds no privilege.
The whole free bead lifecycle against a throwaway camp, no API spend:

```sh
mkdir demo && cd demo
camp init                                  # create ./.camp (ledger + config)
camp rig add . --prefix demo               # register this repo as a rig
camp create "add a --json flag to ls"      # -> prints the bead id: demo-1
camp ls --ready                            # demo-1  open  demo  add a --json flag to ls
camp claim demo-1 --session me             # open -> in_progress
camp close demo-1 --outcome pass --reason "shipped it"
camp show demo-1                           # full record + event history
camp doctor --refold                       # refold: replayed 4 events; 0 drift rows
```

`camp show demo-1` prints the current state and the complete, append-only
history:

```
bead     demo-1
rig      demo
type     task
title    add a --json flag to ls
status   closed
claimed  me
outcome  pass
created  2026-07-09T04:26:18Z
updated  2026-07-09T04:26:18Z

history:
     2  ...Z  bead.created    {"title":"add a --json flag to ls"}
     3  ...Z  bead.claimed    {"session":"me"}
     4  ...Z  bead.closed     {"outcome":"pass","reason":"shipped it"}
```

### The AI step

`camp sling` (or `/camp:sling` in the plugin) is the 90% path: one write, one worker
spawn. It hands the bead to a **real Claude Code worker** instead of you.

```sh
camp sling "add a --json flag to ls, TDD it"
```

This needs two things the free lifecycle did not: an **authenticated `claude`
CLI** and a **routable agent**. Install the [starter pack](packs/starter/) and
name a default agent in `camp.toml`:

```toml
packs = ["packs/starter"]

[dispatch]
default_agent = "dev"          # packs/starter/agents/dev.md
```

Then `camp sling "…"` (or `/camp:sling "…"`) creates the bead, auto-starts `campd`,
and dispatches the worker. Route to a specific role with `--agent reviewer`.
Watch the fleet with `camp top` or `/camp:status`.

## Concepts

Camp implements Gas City's **six primitives** — the same mental model in both
directions ([AGENTS.md](AGENTS.md), design § 6):

| Primitive | In camp |
|---|---|
| **Agent** (who) | a Claude Code agent definition file in a pack — frontmatter (model, tools, permissions) + prompt |
| **Bead** (what) | a ledger record: append-only events + current state. Tasks, mail, and memory are all beads that differ by `type`; `needs` are dependency edges; *ready* = open ∧ no open blockers |
| **Formula** (how) | a TOML file, a strict subset of Gas City formula-v2; cooking materializes a run into the ledger |
| **Rig** (where) | a registered repo path; beads carry a rig; dispatch sets the worker's cwd (or a worktree) |
| **Pack** (configures) | a directory of agents, formulas, and orders — optionally installed as a Claude Code plugin |
| **Event** (observe) | a row in the ledger's event log, which is simultaneously the store's history and the bus |

## Features

### Rigs & beads

A **rig** is a registered repository; beads get per-rig id prefixes (`gc-142`,
`demo-1`) so one ledger can drive work across several repos with scoped
queries. `camp.toml` is the source of truth for rigs (`rig add` appends a
`[[rigs]]` block and records a `rig.added` event).

```sh
camp rig add ~/code/gascity --prefix gc      # register a repo
camp rig ls                                  # name  prefix  path
camp create "fix the flaky test" --needs gc-141 --label ci   # a dependency + label
camp ls --ready                              # only open, unblocked beads
camp ls --json                               # machine-readable (id, status, rig, title, …)
```

Readiness is computed on write, never polled: a bead with open `needs` stays
out of `--ready` until its blockers close.

### Memory & search

Ranked FTS5 full-text search over everything, all time, plus bd-style
persistent memory (memory-type beads):

```sh
camp search "auth refactor"                  # ranked hits across titles, notes, memory
camp remember "the ledger is one WAL-mode SQLite file"
camp recall "sqlite"                         # search memories only
```

The worker skill instructs workers to `recall` before starting and `remember`
non-obvious findings at close, so knowledge survives sessions the way work
does.

### campd & the daemon model

`campd` is the only standing process, and only while there's work: it watches
the ledger, dispatches ready work, schedules orders, and arms stall timers —
all event-driven, never on a tick.

```sh
camp top                                     # one status snapshot (auto-starts campd)
camp top --statusline                        # compact fleet badge (▲live ●ready ✖red); never auto-starts
camp stop                                    # graceful shutdown
```

`camp top` output:

```
campd pid: 25106
live sessions: 0
ready: 1
open: 1
red: 0
```

Liveness *is* the socket (`<camp>/campd.sock`) — no pidfiles, no
lockfiles-as-status. `campd` is crash-only: `kill -9` is a supported shutdown.
Idle it holds no worker processes and, per invariant 1, targets < 20 MB RSS and
0.0% CPU (asserted by the local-only `make perf` suite).

### Formulas & graph execution

For work bigger than one step, a **formula** is a dependency-gated graph. Camp
accepts a strict subset of Gas City formula-v2 — `[[steps]]` with `needs`,
`[steps.check]` (a script verification loop), `[steps.retry]` (bounded
transient retries), and `[steps.on_complete]` (runtime fan-out over structured
output). The everyday guarded change:

```toml
formula = "guarded-change"
description = "Implement with script verification and bounded retries"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "implement"
title = "Implement the change"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "scripts/verify.sh"
timeout = "5m"

[[steps]]
id = "review"
title = "Review the final diff"
needs = ["implement"]
```

Validate any formula against the camp subset (which guarantees it also compiles
under the real `gc` compiler):

```sh
camp doctor --formula packs/starter/formulas/guarded-change.toml
# -> formula ok: guarded-change (2 step(s))
```

Cook and run one with `camp sling --formula guarded-change`: camp pins a copy
into `runs/<run-id>/`, materializes the step beads and edges, and from that
moment `campd` advances the graph — a close unblocks dependents, a `check`
verdict routes mechanically, `retry` follows the pass/hard/transient rules, and
the last step finalizes the run. `campd` executes structure; agents and your
`check` scripts make every judgment.

### Orders & automation

An **order** is a cron- or event-triggered formula, declared in `camp.toml`:

```toml
[[order]]
name    = "morning-triage"
on      = "cron:0 7 * * 1-5"
formula = "guarded-change"
rig     = "gascity"

[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "fix-ci"
```

```sh
camp order ls                                # NAME  ON  FORMULA  RIG  WINDOW  NEXT
camp order run morning-triage                # fire now (campd cooks and dispatches it)
```

Cron orders are a min-heap of deadlines — a timer, not a tick — with a
catch-up window for fires missed while asleep. Event orders match on the same
post-commit path as readiness, so they add no standing cost. **Away-mode is the
same code path**: orders fire, `campd` dispatches headless workers, everything
lands in the ledger. Honest limits: with the default on-demand daemon, orders
fire only while `campd` is running; install the optional launchd agent
([contrib/launchd/](contrib/launchd/README.md)) for fire-at-login coverage; a
powered-off laptop fires nothing until wake.

### Packs & the Claude Code plugin

A **pack is a directory** of Claude Code content — `agents/`, `formulas/`,
`orders.toml`, optional `skills/`/`commands/` — imported by path in
`camp.toml`. Layering is last-wins with your local definitions highest. The
[starter pack](packs/starter/) ships `dev` and `reviewer` roles and a
gc-validated formula as an example to copy, not a dependency.

The **camp plugin** ([plugin/](plugin/)) makes a Claude Code session the
control plane. It is machinery only — it ships **zero roles**:

- Slash commands `/camp:sling`, `/camp:status`, `/camp:adopt`, `/camp:events` (thin wrappers over
  the `camp` CLI — the session's scripting surface is identical to the
  terminal's).
- SessionStart / SessionEnd lifecycle hooks that register and end attended
  sessions.
- An opt-in statusline rendering the fleet badge from a read-only socket query.
- The **worker skill** (`skills/worker/SKILL.md`): the worker lifecycle
  contract — recall → claim → work → emit milestones → remember → close → exit.

Install it from this repo (see the [quickstart](#use-camp-from-inside-claude-code-recommended)):

```
/plugin marketplace add richardkiene/gascamp
/plugin install camp@gascamp
/reload-plugins
```

The statusline is opt-in: a plugin cannot set your main status line for you, so
wire it into your own `~/.claude/settings.json`. It renders `▲live ●ready ✖red`
from a read-only socket query, never auto-starts `campd`, and degrades to empty
output when `campd` is down.

```json
{ "statusLine": { "type": "command",
                  "command": "\"${CLAUDE_PLUGIN_ROOT}\"/statusline/statusline.sh" } }
```

### Export / graduation to Gas City

When a job outgrows camp, graduate it — don't grow camp city-shaped:

```sh
camp export --city ./city-out
```

This writes a directory a Gas City operator imports with standard tooling:
`beads.jsonl` (bd-importable open and historical beads — ids, titles, status,
`needs` edges, labels, outcome metadata), the pinned formulas from `runs/`
(already valid v2-subset files), and a `pack/` directory with a generated
`pack.toml`, the pack's agent definitions, and your orders translated to gc
order files. It is **read-only** — camp never writes into a live city's store.
Because camp's formulas and vocabulary are a strict subset/mirror of Gas
City's, exported history reads natively in a city.

## Design principles

Camp is held to seven invariants — "violations are bugs, not trade-offs"
([AGENTS.md](AGENTS.md)). Condensed:

1. **Idle is free.** No ticks, no polling loops; components sleep on OS events.
   Idle `campd`: < 20 MB RSS, 0.0% CPU.
2. **Cost proportional to job.** The smallest job pays one worker spawn and ~3
   ledger writes. Graphs, retries, and fan-out are opt-in per job.
3. **Nothing hidden.** All durable truth is one SQLite ledger plus
   human-readable TOML and run files. Every `campd` action is an event with its
   cause; `kill -9` anything and the ledger tells the whole story.
4. **Six primitives, zero roles in code.** `campd` moves work; it never reasons
   about it. A role name or judgment call in Rust is a bug.
5. **Fail fast.** No fallbacks, no silenced errors, no placeholders, no panics
   in library code. Every error surfaces to the caller or lands in the ledger.
6. **Formula subset invariant.** Every valid camp formula is a valid Gas City
   formula-v2 file; CI validates the corpus against the real `gc` compiler.
7. **Vocabulary mirror.** Event names and outcome metadata match Gas City
   verbatim where the concept exists; camp-specific names are additive.

The authoritative spec is
[`docs/design/2026-07-05-gas-camp-design.md`](docs/design/2026-07-05-gas-camp-design.md);
repository rules and the invariants live in [AGENTS.md](AGENTS.md).

## Development

```sh
cargo build --workspace                 # build
cargo test --workspace                  # the full test suite (unit + integration)
cargo fmt --all --check                 # formatting gate
cargo clippy --workspace --all-targets --all-features -- -D warnings   # lint gate
```

CI runs five required checks on every PR: **fmt**, **clippy**, **test** (Ubuntu
and macOS), and **gc-compat** — the last validates camp's formula corpus
against the real Gas City compiler pinned in `ci/gc-compat/GASCITY_REF` and
cross-checks the event/outcome vocabulary.

Two suites are **local-only**, never run in CI:

```sh
make perf        # asserts the design § 14 cost budget (write < 1 ms, search < 50 ms,
                 # idle 0.0% CPU, 1M-event volume) in --release, single-threaded
make e2e         # opt-in real-`claude` end-to-end run; needs CAMP_E2E=1, an
                 # authenticated claude, python3, and git — it spends real API money
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

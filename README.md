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
  while you're away — campd is supervised (launchd, systemd `--user`, or your
  container runtime), so orders fire whether or not you ran a `camp` command.

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
git clone https://github.com/Liquescent-Development/gascamp
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

You almost never start the daemon by hand: on a desktop, `camp init` puts
campd under the host's service manager, so it's **always-on** — restarted
across crashes and reboots by that supervisor (see [Supervised
campd](#supervised-campd--camp-service)). Without one — a container, CI —
nothing is standing until you run `camp daemon` yourself, and `camp stop`
shuts down what you started. Either way, "idle is free" means no ticks, no
polling — an idle campd costs ~0% CPU, not that no process exists.

## Quickstart

Two ways in — the Claude Code plugin (recommended) or the raw CLI. They do the
same thing; the plugin is a thin wrapper.

### Use camp from inside Claude Code (recommended)

**1. Install the `camp` binary and put it on your `PATH`.** The plugin's slash
commands shell out to `camp`, so the binary MUST be installed and on `PATH` for
the plugin to do anything:

```sh
git clone https://github.com/Liquescent-Development/gascamp
cd gascamp
make install                              # -> ~/.local/bin/camp (+ campd symlink)
export PATH="$HOME/.local/bin:$PATH"      # if it isn't already
```

(See [Install](#install) for `PREFIX` overrides and `make uninstall`.)

**2. Install the Claude Code plugin.** From inside a Claude Code session, add
this repo as a plugin marketplace, install the `camp` plugin, and reload:

```
/plugin marketplace add Liquescent-Development/gascamp
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

`/camp:sling` hands the bead to a **real Claude Code worker** dispatched by
campd that follows the plugin's **worker skill** (recall → claim → work → emit
milestones → remember → close → exit). Talk to it mid-run with
`camp nudge <session> "<message>"`, or attach any time with
`claude --resume <session-id>`.
That one step needs an authenticated `claude` CLI and a routable agent
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
name a default agent in `camp.toml` — here is the full file for the `demo` camp
above, `packs` added:

```toml
# Gas Camp configuration (spec §7.1)
packs = ["/absolute/path/to/gascamp/packs/starter"]

[camp]
name = "demo"

[[rigs]]
name = "demo"
path = "/absolute/path/to/demo"
prefix = "demo"

[dispatch]
default_agent = "dev"          # packs/starter/agents/dev.md
```

Two things to get right, both easy to miss:

- **`packs` is a top-level key, and it must come before every `[section]`
  header** (`[camp]`, `[[rigs]]`, `[dispatch]`) — TOML binds a bare key to
  whichever table precedes it, not to the file as a whole. `camp init` writes
  `[camp]` first and `camp rig add` appends `[[rigs]]` after it, so if you
  paste `packs = [...]` at the *bottom* of the file (as you'd naturally do
  after running those commands), TOML attaches it to the last `[[rigs]]`
  entry instead of the top level. Since `camp.toml` rejects unknown fields,
  that surfaces as a confusing `` unknown field `packs` `` error rather than
  anything mentioning placement — add the line above `[camp]`, not below
  `[[rigs]]`.
- **Relative pack paths resolve against the directory holding `camp.toml`**
  (the `.camp/` directory `camp init` creates), not your shell's cwd. A bare
  `packs = ["packs/starter"]` in the `demo` camp above would look for
  `demo/.camp/packs/starter`, which doesn't exist. Use an absolute path to
  your gascamp clone's `packs/starter` (as above), or copy the pack directory
  into `.camp/` instead.

Then `camp sling "…"` (or `/camp:sling "…"`) creates the bead and campd
dispatches the worker. Route to a specific role with `--agent reviewer`.
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

`campd` watches the ledger, dispatches ready work, schedules orders, and arms
stall timers — all event-driven, never on a tick. Whether it is *standing*
depends on supervision: under a host service manager (the `camp init`
default) it is always-on, restarted by that supervisor across crashes and
reboots; unsupervised — a container, CI, anywhere with no service manager —
nothing is standing until you run `camp daemon` yourself, and once started it
keeps serving until you `camp stop` it (or kill it) — there is no idle-exit
path; "idle is free" means near-zero CPU, not that the process goes away.

```sh
camp top                                     # one status snapshot (campd must be running)
camp top --statusline                        # compact fleet badge (▲live ●ready ✖red); empty + a stderr note when campd is down
camp stop                                    # graceful shutdown (unsupervised camps only — see below)
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
lockfiles-as-status. `campd` is crash-only: it holds no private state, so
`kill -9` loses nothing and is always safe — the ledger tells the whole
story on the next start. That is not the same as a shutdown, though: on a
supervised camp the unit's `KeepAlive`/`Restart=always` respawns campd
within moments, so `kill -9` there is a **restart**, not a stop — use `camp
service stop` to actually stop a supervised campd. Only on an unsupervised
camp does `kill -9` shut it down. Idle it holds no worker processes and, per
invariant 1, targets < 20 MB RSS and 0.0% CPU (asserted by the local-only
`make perf` suite).

#### Supervised campd — `camp service`

campd is a foreground, socket-serving process. On a desktop, `camp init` puts
it under the host's service manager, so it survives crashes, comes back at
login, and can be cycled after a binary upgrade:

    camp service install     # macOS: a KeepAlive LaunchAgent in ~/Library/LaunchAgents
                             # Linux: a Restart=always systemd --user unit
    camp service status      # the unit's load/run state + campd's liveness answer
    camp service restart     # cycle the daemon after upgrading the binary
    camp service stop        # stop campd until the next login (the unit stays installed)
    camp service start       # …and bring it back
    camp service uninstall   # stop, unload, remove the unit — the durable "off"
    camp service list        # every camp with a managed unit, and its state

`camp init` does this for you when it detects a usable host service manager
(macOS launchd; Linux systemd `--user`). Where there is none — a container, a
CI box — it does not fail: it says so on stderr and hands off, and you run
`camp daemon --camp <dir>` under your own supervisor (the container runtime).
`camp init --no-service` skips the unit; `camp init --service` insists on one
and fails loudly if the host cannot provide it.

`camp service stop` is not durable across a login: the unit stays installed, so
launchd re-bootstraps it (and systemd starts the still-enabled unit) the next
time you log in. `camp service uninstall` is the durable off switch.

**While the supervisor is running campd, `camp stop` refuses.** Such a campd is
kept alive by its unit (`KeepAlive` / `Restart=always`), so a socket-level stop
would be undone by the supervisor moments later — and a verb that says "campd
stopped" about a daemon that is already coming back is lying. `camp stop`
therefore hard-errors and points you at `camp service stop` (stop it) or
`camp service uninstall` (un-manage it). Once the supervisor is no longer
running campd, nothing will restart it behind your back, so `camp stop` goes
back to doing exactly what it says — and it is then the verb for a campd the
supervisor does not own. On an unsupervised camp — a container, CI, a camp you
never installed a unit for — `camp stop` behaves exactly as it always has.

"Is the supervisor running campd?" is answered by the supervisor, not guessed:
on launchd it means the label is **bootstrapped** (`KeepAlive` is
unconditional); on systemd it means the unit is **active** (`Restart=always`
acts only on a running unit — a stopped unit is still `LoadState=loaded`, which
says only that its unit file parsed).

**The verbs that hand campd to the supervisor check first.** `camp service
install` and `camp service start` refuse if a campd already holds the camp's
socket: a supervised campd cannot take over a live socket — it would exit, and
the supervisor would respawn it forever while the command told you the camp was
supervised. Stop the running campd (`camp stop`) and install then works. This is
the ordinary upgrade path for a camp still running a campd of its own.

There is no registry file: the installed units ARE the registry, and
`camp service list` reads them.

#### campd's PATH

A supervisor does not give campd your shell's environment. launchd runs a
LaunchAgent with `PATH=/usr/bin:/bin:/usr/sbin:/sbin`; a `systemd --user`
service gets `/usr/local/bin:/usr/bin:/bin:…`. Neither contains
`~/.local/bin` — which is where Claude Code installs `claude`, the process
campd spawns to do the work. A campd with that PATH comes up healthy, serves
its socket, accepts beads, and then fails **every** dispatch with
`spawn failed: spawning claude: No such file or directory`.

So `camp service install` captures the PATH of the shell that runs it — the
one place your tools demonstrably resolve — and writes it into the unit. It
prints what it captured, and warns if the configured worker command is not on
it, because the alternative is finding out from a `session.crashed` event after
you have already slung work at a camp that was never going to run it.

It is a **snapshot, not a live link**. Change your PATH, move `claude`, and the
unit still names the old one — re-run `camp service install` to re-capture it.
(`camp service restart` only cycles the daemon; it does not re-read your shell.)

> **What has actually been exercised against a live service manager:** both.
> The end-to-end lifecycle test (`make service-e2e`) runs against **launchd on
> macOS**. The **systemd** path was driven by hand against a live
> `systemd --user` (Ubuntu 24.04, aarch64) on 2026-07-11: `camp init` detecting
> the user manager and installing a unit, `status`, `list`, `stop`, `start`,
> `restart` (the supervised campd's pid really does change), `uninstall` leaving
> nothing behind, `camp stop` refusing while the unit is running and falling
> through to a socket stop once it is not, and the `install`/`restart` refusals
> firing on a campd systemd does not own. A camp path containing `%` was included
> deliberately: systemd expands `%` specifiers in `ExecStart`, and the escaping
> holds — the unit stores `%%`, systemd resolves it back to the literal path, and
> campd binds. No CI job runs either path (neither can run on a hosted runner);
> the systemd flows also stay pinned by unit tests against a faked `systemctl`.

#### In a container

The container runtime is just another supervisor: campd is the container's main
process, `restart: unless-stopped` is its KeepAlive, and `docker stop` is a
SIGTERM campd answers gracefully. A reference `Dockerfile`, entrypoint and
`compose.yaml` ship in [contrib/docker/](contrib/docker/README.md):

    docker compose -f contrib/docker/compose.yaml up -d --build
    docker exec gascamp camp sling "fix the flaky auth test"
    docker stop gascamp     # graceful: campd.stopped in the ledger, exit 0

The CLI is a pure socket client, so drive the camp with `docker exec` — that
puts the CLI on the same side of `<camp>/campd.sock` as campd. Reaching the
socket from the host means bind-mounting the camp dir and works only on a native
Linux host; cross-host access is out of scope (there is no network transport).

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
lands in the ledger. Honest limits: a supervised campd (`camp service install`,
or a container with `restart: unless-stopped`) is kept alive by its supervisor,
so orders fire at login, after a crash, and after a reboot without you running
anything. Where there is no supervisor — CI, a bare box, a container you did not
keep running — orders fire only while a `camp daemon` you started is alive. And
a powered-off or sleeping machine fires nothing until wake, when the catch-up
window applies.

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
/plugin marketplace add Liquescent-Development/gascamp
/plugin install camp@gascamp
/reload-plugins
```

The statusline is opt-in: a plugin cannot set your main status line for you, so
wire it into your own `~/.claude/settings.json`. It renders `▲live ●ready ✖red`
from a read-only socket query and degrades to empty output plus a stderr note
when `campd` is down.

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

Copyright © 2026 Liquescent Development LLC.

Licensed under the GNU Affero General Public License v3.0 only
(`AGPL-3.0-only`) — see [LICENSE](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work shall be licensed under `AGPL-3.0-only`, without any
additional terms or conditions.

# Gas Camp — a hands-on tutorial

This walks you from nothing to a running agent workflow: install the `camp`
binary, install the Claude Code plugin, create a camp, register a rig, import a
pack, and run a full workflow through it — first the **$0** parts you can prove
without spending anything, then the real Claude Code worker.

Every command below is one you actually run; expected output is shown inline.
Where a step **spends API money**, it says so in bold — nothing before it does.

**What you'll have at the end:** a local `.camp/` ledger, a registered repo, an
imported pack whose agents you can route work to, and a dispatched worker (or a
cooked formula graph) that campd drives to completion.

**Time:** ~10 minutes for the free path; the AI step is however long one small
task takes.

---

## 0. Prerequisites

- **Rust, stable, 1.85+** (edition 2024). The repo pins `channel = "stable"` in
  `rust-toolchain.toml`.
- **git** on your `PATH`.
- **For the AI step only:** an authenticated **`claude` CLI**
  ([Claude Code](https://docs.claude.com/en/docs/claude-code)) on your `PATH`.
  Steps 1–7, the whole free bead lifecycle, and *cooking* a formula need neither
  `claude` nor any spend — only dispatching a worker (§8) does.

---

## 1. Install Gas Camp

Build the release binary and install it (plus the `campd` symlink):

```sh
git clone https://github.com/Liquescent-Development/gascamp
cd gascamp
make install                         # -> ~/.local/bin/camp (+ campd symlink)
```

`make install` runs `cargo build --release`, copies `camp` into `$PREFIX/bin`,
and creates a `campd` symlink beside it. `campd` is the **same binary in daemon
mode** — `main` dispatches on argv0, so the symlink is how the daemon gets its
name. Override the location with `PREFIX` (any writable dir; no root needed):

```sh
make install PREFIX=/usr/local       # -> /usr/local/bin/camp + campd
```

Put it on your `PATH` and confirm:

```sh
export PATH="$HOME/.local/bin:$PATH"  # if it isn't already
camp --version                        # -> camp 0.x.y
```

> `make uninstall` (honoring the same `PREFIX`) removes both the binary and the
> `campd` symlink.

---

## 2. Install the Claude Code plugin

The plugin makes a Claude Code session the control plane — it adds slash
commands, session-lifecycle hooks, the worker skill, and a fleet statusline.
It's a **thin wrapper over the `camp` CLI**, so the binary from step 1 must be
installed and on `PATH` for the plugin to do anything.

From inside a Claude Code session:

```
/plugin marketplace add Liquescent-Development/gascamp
/plugin install camp@gascamp
/reload-plugins
```

Claude Code reads the repo's `.claude-plugin/marketplace.json`: `gascamp` is the
**marketplace** name and `camp` is the **plugin** name (hence `camp@gascamp`).
`/reload-plugins` activates it without a restart.

The commands are namespaced under the plugin name:

| Command | Wraps | Needs `claude`? |
|---|---|---|
| `/camp:status` | `camp top` — fleet snapshot | no |
| `/camp:events` | `camp events` — the append-only log | no |
| `/camp:adopt` | `camp adopt` — reconcile the session registry | no |
| `/camp:sling` | `camp sling` — dispatch work | **yes** (dispatches a worker) |
| `/camp:nudge` | `camp nudge` — talk to a running worker | only if a worker is live |

Everything the plugin does also works straight from the terminal `camp` CLI —
the plugin adds no privilege. The rest of this tutorial uses the CLI so it's
copy-pasteable; each command has a `/camp:*` equivalent where one exists.

> **Optional — the fleet statusline.** A plugin can't set your status line for
> you, so wire it into `~/.claude/settings.json` yourself:
> ```json
> { "statusLine": { "type": "command",
>     "command": "\"${CLAUDE_PLUGIN_ROOT}\"/statusline/statusline.sh" } }
> ```
> It renders `▲live ●ready ✖red` from a read-only socket query and degrades to
> empty output plus a stderr note when campd is down.

---

## 3. Initialize a camp

A **camp** is one directory (`.camp/`) holding the SQLite ledger (`camp.db`) and
its config (`camp.toml`). Make a working directory and initialize it:

```sh
mkdir demo && cd demo
camp init --no-service --no-import
```

```
initialized camp at /…/demo/.camp
service: skipped (--no-service) — run `camp daemon --camp /…/demo/.camp` under your supervisor
```

That created just two files — nothing hidden:

```sh
ls -A .camp        # camp.db  camp.toml
```

> **Why the two flags — and what the real default is.** A **bare `camp init`**
> is the batteries-included desktop path, and it's what you'll most likely run
> for real. It does two extra things this tutorial deliberately turns off:
>
> - **It supervises campd** — puts it under your host service manager (launchd on
>   macOS, `systemd --user` on Linux) so it's always-on, surviving crashes,
>   logout, and reboot. `--no-service` skips that so *you* start campd by hand in
>   §7 and can watch it run in the foreground and `camp stop` it — a cleaner
>   mental model for a first pass, and one that behaves the same on every OS.
> - **It offers to import the starter pack** (an interactive prompt in a
>   terminal). `--no-import` skips the offer so importing a pack is an explicit,
>   deterministic step you do on purpose in §5, instead of something `init`
>   already did behind a `y/n`.
>
> **For real use, drop both flags.** A supervised campd is what makes orders fire
> while you're away and survives you closing the terminal (a foreground `camp
> daemon` dies with its shell). To put an already-created camp under the
> supervisor later, run `camp service install`. Two things to know about
> supervised mode: `camp stop` refuses (use `camp service stop`), and the
> supervisor bakes a **snapshot of your PATH** into the unit — so if `claude`
> isn't on that PATH, campd comes up healthy but fails every dispatch. See the
> README's [Supervised campd](README.md#supervised-campd--camp-service) section.

---

## 4. Register a rig

A **rig** is a registered repository. Beads (camp's unit of work) carry a rig and
get a per-rig id prefix, so one ledger can drive several repos with scoped
queries. Register the current directory:

```sh
camp rig add . --prefix demo
```

```
added rig demo (demo) -> /…/demo
```

```sh
camp rig ls
```

```
demo    demo    /…/demo
```

`camp.toml` is the source of truth for rigs — `rig add` appends a `[[rigs]]`
block and records a `rig.added` event in the ledger.

> **The rig is a real git repo.** When campd dispatches a worker it works in a
> git worktree of the rig and lands the result as a commit, so the rig needs to
> be a git repository with at least one commit. If `demo` is fresh:
> ```sh
> git init -b work && git add -A && git commit -m "baseline"
> ```
> (This matters for the AI step in §8, not for the free lifecycle in §4.5.)

### 4.5. Warm-up: the free bead lifecycle ($0)

Before any pack or worker, prove the core loop by hand — this needs no daemon,
no `claude`, no spend:

```sh
camp create "add a --json flag to ls"     # -> prints the bead id
```
```
demo-1
```
```sh
camp ls --ready                            # only open, unblocked beads
```
```
demo-1  open  demo  add a --json flag to ls
```
```sh
camp claim demo-1 --session me             # open -> in_progress
camp close demo-1 --outcome pass --reason "shipped it"
camp show demo-1                           # current state + full history
```
```
bead     demo-1
rig      demo
type     task
title    add a --json flag to ls
status   closed
claimed  me
outcome  pass
…
history:
     2  …Z  bead.created    {"title":"add a --json flag to ls"}
     3  …Z  bead.claimed    {"session":"me"}
     4  …Z  bead.closed     {"outcome":"pass","reason":"shipped it"}
```

Everything is in the one ledger. `camp doctor --refold` rebuilds state from the
event log and reports any drift:

```sh
camp doctor --refold        # -> refold: replayed 4 events; 0 drift rows
```

---

## 5. Install a pack

An **agent** — the "who" that does AI work — comes from a **pack**: a directory
of `agents/`, `formulas/`, `orders/`, and optional `skills/`, imported under a
**binding**. This tutorial uses the bundled **starter pack**
([`packs/starter/`](packs/starter/)), which ships three agents (`dev`,
`reviewer`, `committer`) and a gc-validated `guarded-change` formula — a
template to copy, not a dependency.

Import it as a local path, binding it to the name `starter`:

```sh
# from inside the demo camp; use the absolute path to your gascamp clone
camp import add /absolute/path/to/gascamp/packs/starter --name starter
```
```
imported starter from /…/gascamp/packs/starter (commit )
```

That wrote an `[imports.starter]` block into `camp.toml` and made the pack's
agents resolvable by their **qualified name** `<binding>.<agent>`:
`starter.dev`, `starter.reviewer`, `starter.committer`.

> **Verifying a local import.** A *local-path* import is layered in place — it's
> referenced straight from its source, not cloned into `.camp/imports/`. So
> `camp import list` and `camp import check` (which track **git-locked** imports)
> will report "no imports" for it. Confirm a local import instead by looking at
> `camp.toml`:
> ```sh
> grep -A1 '\[imports' .camp/camp.toml    # -> [imports.starter] / source = "…/packs/starter"
> ```
> Git-sourced packs (see §9) *are* locked and materialized, and show up in
> `camp import list` with their pinned commit.

Validate the pack's formula against the camp subset (which guarantees it also
compiles under the real Gas City `gc` compiler):

```sh
camp doctor --formula /absolute/path/to/gascamp/packs/starter/formulas/guarded-change.toml
```
```
formula ok: guarded-change (2 step(s))
```

---

## 6. Wire the dispatch config

Model, permission mode, and tools are **operator-owned** — they live in your
`camp.toml`, never in the pack. A pack you import can't silently grant its agents
`Bash`; you decide the allowlist. Point `[dispatch]` at a default agent and
declare the worker pins. Append to `.camp/camp.toml`:

```toml
[dispatch]
command = "claude"              # the worker binary campd spawns
default_agent = "starter.dev"   # <binding>.<agent>

[agent_defaults]
model = "sonnet"
permission_mode = "acceptEdits"
tools = ["Read", "Edit", "Write", "Bash"]
```

Two rules worth internalizing:

- **Agents resolve as `<binding>.<agent>`.** `starter.dev` is the `dev` directory
  under the `starter` binding. `--agent starter.reviewer` routes to a specific
  role.
- **No resolvable `tools` means no spawn** — a loud refusal that names the
  remedy, never a silent half-configured worker. A pack that ships `skills/`
  additionally needs `"Skill"` in the allowlist for its agents to resolve.

---

## 7. Start campd

campd is the one dispatcher: it watches the ledger, advances ready work, and
tails each worker — all event-driven, zero polling, ~0% CPU when idle. Start it
in the foreground (a second terminal, or backgrounded):

```sh
camp daemon --camp .camp &
```
```
campd listening on .camp/campd.sock
```

Confirm it's up:

```sh
camp top
```
```
campd pid: 25106
live sessions: 0
ready: 0
open: 0
red: 0
```

> Liveness *is* the socket (`.camp/campd.sock`) — no pidfiles. campd is
> crash-only; `kill -9` loses nothing because all durable truth is the ledger.
> `camp stop` shuts down a campd you started this way.

---

## 8. Run a full workflow through the pack

There are two shapes of "workflow": a **single dispatch** (one worker, the 90%
path) and a **formula graph** (dependency-gated steps campd advances for you).
Both route through the pack you imported.

> **A note on the bead ids below.** Ids are assigned in sequence as beads are
> created, so they depend on what you've already run. The ids shown assume you
> did §4.5 (`demo-1`) and then §8a (`demo-2`) in order. If you skip the
> money-spending §8a, §8b's ids shift down by one (`demo-2`/`demo-3`/`demo-4`).

### 8a. One worker — `camp sling` ⚠️ spends API money

This hands a bead to a **real Claude Code worker** that claims it, does the work
in a git worktree of the rig, emits milestones, and closes with an outcome:

```sh
camp sling --agent starter.dev "add a --version flag; keep it tiny"
```
```
demo-2
```

campd spawns `claude` as `starter.dev` with the model, permission mode, and tools
from `[agent_defaults]`, in a fresh worktree. The worker follows the plugin's
**worker skill** — the `recall → claim → work → emit milestones → remember →
close → exit` contract — and lands its change as a commit on a `camp/<bead>`
branch. Watch it run (next section). Route to a different role with
`--agent starter.reviewer`; omit `--agent` to use `default_agent`.

> Everything *before* this step is free. This one spawns a real `claude`, so it
> spends — as does the formula run in §8b once campd advances it.

### 8b. A formula graph — `camp sling --formula`

For work bigger than one step, cook the pack's **formula** into a run. The
starter's `guarded-change` is a two-step graph (implement → review) with a script
`check` on the first step. **Cooking is free** — it only materializes the graph
into the ledger. But with campd running (§7) and a real `claude` configured,
campd immediately dispatches a worker to the first ready step, and *that* spends.
To inspect a cooked graph without spending, cook it with campd **stopped** (see
the tip at the end of this section).

```sh
camp sling --formula guarded-change
```
```
20260717T170413Z-e25464 root demo-3
```

Cooking materializes the whole graph into the ledger as beads and edges:

```sh
camp ls
```
```
demo-3  open  demo  guarded-change
demo-4  open  demo  Implement the change
demo-5  open  demo  Review the final diff
```

From here campd advances the graph on its own: a close unblocks dependents, the
`check` script's verdict routes mechanically, bounded `retry` follows the
pass/hard/transient rules, and the last step finalizes the run. **campd executes
structure; the agents and your `check` scripts make every judgment.**

> If campd isn't running when you cook, `sling --formula` still pins the run and
> tells you so — *"cooked and pinned, but NOT started — campd advances runs"* —
> and campd picks it up the moment it starts. Nothing is lost.

---

## 9. Watch and talk to the workers

A dispatched worker isn't a black box — campd holds its stdin and tails its
output, so you can watch, steer, and answer it live. Each of these is a pure
socket client (campd must be running):

```sh
camp watch                       # the whole fleet, live — leave this open
camp attach demo/starter.dev/1   # one worker's typed stream: tool calls, results, text
camp nudge demo/starter.dev/1 "also update the README"   # send a turn into a running worker
camp interrupt demo/starter.dev/1                        # stop its current turn
```

While attached, a line you type is a turn, `/interrupt` stops the turn, and `/q`
detaches.

**Permissions.** If a worker asks to use a tool it isn't pre-allowed (a
`can_use_tool` request), it BLOCKS — holding no dispatch slot — and `camp watch`
shows a BLOCKED row with a request id. Answer it:

```sh
camp decide demo/starter.dev/1 <request-id> allow          # or allow_always | deny
camp decide demo/starter.dev/1 <request-id> deny --reason "not on prod"
```

**Mail.** A worker escalates to you by sending mail:

```sh
camp mail inbox        # unread messages
camp mail read <id>    # print one and mark it read
```

When you're done, stop the daemon:

```sh
camp stop
```

---

## 10. Going further

**Real Gas City packs.** The starter pack is a template; the real thing is the
official Gas City packs, imported from git. They live in a monorepo, so you
select a pack with `//<subpath>` and pin a ref with `#<ref>`:

```sh
# bmad (transitively pulls gascity), then the gascity roles as the `gc` binding
camp import add "https://github.com/gastownhall/gascity-packs//bmad#main" --name bmad
camp import add "https://github.com/gastownhall/gascity-packs//gascity/roles#main" --name gc
camp import list                  # git imports show here, with pinned commits
```

`bmad` ships `skills/`, so its agents need `"Skill"` in `[agent_defaults].tools`.
Prove the whole pack machinery — and real Gas City compatibility — end to end,
**$0, no worker**:

```sh
make demo-pack        # fetches the pinned corpus, imports bmad + gstack,
                      # compiles them vs gc's own compiler, cooks a bead graph
```

See [docs/demos/2026-07-15-real-gc-packs.md](docs/demos/2026-07-15-real-gc-packs.md)
for what it proves.

**Orders — scheduled and event-triggered workflows.** An **order** is a cron- or
event-triggered formula. A pack ships orders as gc-compatible files; they are
**inert until you arm them**, so importing a pack can never start spending on your
behalf:

```sh
camp order ls                             # every order + next fire time (disabled shown)
camp order enable starter.morning-triage  # arm an imported order (money invariant: inert until armed)
camp order run starter.morning-triage     # fire one now, by its qualified name (campd cooks + dispatches)
```

An imported order's name is qualified `<binding>.<stem>` for every verb; your
own inline `[[order]]` entries use a bare stem.

**Graduate to Gas City.** When a job outgrows camp, export it (read-only — camp
never writes into a live city):

```sh
camp export --city ./city-out    # beads.jsonl, pinned formulas, a pack/ directory
```

---

## Command reference

| Goal | Command |
|---|---|
| Install | `make install` · `make uninstall` |
| Plugin | `/plugin marketplace add Liquescent-Development/gascamp` · `/plugin install camp@gascamp` |
| New camp | `camp init [--no-service] [--no-import]` |
| Rig | `camp rig add <path> --prefix <p>` · `camp rig ls` |
| Bead lifecycle | `camp create` · `camp ls --ready` · `camp claim` · `camp close` · `camp show` |
| Import a pack | `camp import add <source> --name <binding>` · `camp import list` |
| Validate a formula | `camp doctor --formula <file>` |
| Daemon | `camp daemon --camp .camp` · `camp top` · `camp stop` |
| Dispatch (money) | `camp sling --agent <b.a> "…"` · `camp sling --formula <name>` |
| Watch / steer | `camp watch` · `camp attach <s>` · `camp nudge <s> "…"` · `camp decide <s> <id> allow` |
| Integrity | `camp doctor --refold` |
| Export | `camp export --city <dir>` |

The authoritative reference is [README.md](README.md); the invariants and repo
rules live in [AGENTS.md](AGENTS.md).

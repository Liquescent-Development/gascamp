# Gas City pack compatibility — design

**Date:** 2026-07-12
**Issues:** [#80](https://github.com/Liquescent-Development/gascamp/issues/80) · [#84](https://github.com/Liquescent-Development/gascamp/issues/84) · [#85](https://github.com/Liquescent-Development/gascamp/issues/85)
**Incorporates:** `2026-07-12-camp-pack-imports-design.md` — the import machinery (source grammar, lock, verbs, `camp init` flow, error table) in full detail. That document is the component spec for §4.7; this one is the umbrella. Where they disagree, this one wins.
**Status:** proposed

## 1. Why

README: camp is *"what k3s is to k8s"* for Gas City. **k3s runs k8s manifests.** A camp that cannot run a Gas City pack is not a lighter Gas City; it is a different tool wearing the vocabulary. Packs and formulas *are* the process — without them camp is a daemon with a ledger.

Today camp and Gas City are mutually unusable, in both directions:

- **A fresh camp knows zero agents** (#80). `camp init` writes `camp.toml`, `camp.db`, a gitignore entry, and nothing else. The first `camp sling` correctly refuses, and there is no supported way past it.
- **Pack layering was half-built and the code admits it.** `pack.rs:175` pushes `<pack>/agents` and *nothing else*. `orders/mod.rs:264` carries the comment *"Phase 12's pack layering replaces this body"* — it never landed. So `packs/starter/orders.toml` is read by **no code path**, and pack formulas are unreachable.
- **`camp export` produces packs Gas City silently discards** (#85). gc takes each *immediate subdirectory* of `agents/` as an agent and `continue`s past files (`agent_discovery.go:33`). Camp exports `agents/dev.md` — a file. Zero agents, no error, on either side.

## 2. The evidence base

Every number below is measured against the **public `gastownhall/gascity-packs` repo** (100 formulas, 80 agents, 11 packs) and Gas City's source at the ref this repo already pins (`ci/gc-compat/GASCITY_REF` = `12410301…`). This section is the spine of the design; the decisions are downstream of it.

**Formulas — 79 of 100 declare `contract = "graph.v2"`: a DAG of steps with dependencies.** That is the execution model campd *already implements* (`dispatch.rs:959-976` — "campd as purely mechanical control dispatcher for check loops, retry classification, on_complete fan-out, and run finalization").

Cumulative formulas that load, per feature added:

| add | loads |
|---|---|
| camp today | **1 / 100** |
| ignore keys gc itself ignores (§4.1) | 5 |
| + `vars` + `condition` | 48 |
| + `extends` | 75 |
| + `expand` / `template` / `expand_vars` / `children` | **87** |
| + `drain` | **100** |

**Control plane — camp already has 6 of gc's 8 control kinds.** gc's `ControlKinds` = `{retry, ralph, check, retry-eval, fanout, drain, scope-check, workflow-finalize}` (`beadmeta/kindsets.go:35-44`). Camp's `GraphRuntime` covers check/ralph (`PendingCheck`), retry/retry-eval, fanout (`PendingFanout`), and workflow-finalize. Missing: **`drain`** (13/100 formulas) and **`scope-check`** (**1**/100).

gc *externalizes* its control plane as an agent process (`gc convoy control --serve`); camp *internalizes* it in the daemon. Same job, different placement. **This is a vocabulary gap, not a missing subsystem** — and therefore a compiler concern, not a runtime verb.

**Agents — 80 real ones, and they are simple:**

| | count |
|---|---|
| a directory with a prompt + `agent.toml` | 80 / 80 |
| use only `scope` + `description` + `fallback` + prompt | 72 / 80 |
| pooled (`max_active_sessions > 1`) | **2** |
| `pre_start` shell hooks | **2** |
| `work_dir` templates | **8** |
| declare a model or permission mode | **0** |

**Prompts — 67 of 80 need a real Go template engine**: `{{template "…"}}` fragment includes (84 uses), `{{cmd}}`/`{{basename}}`/`{{session}}`/`{{templateFirst}}` functions, and `if`/`range` (4). None of these execute shell: `cmd` returns `filepath.Base(os.Args[0])` — the binary's own name (`prompt.go:394`).

**The worker contract — prompts call gc's CLI.** Measured across 157 prompt/fragment files: `bd` (308 refs, mostly **bare `bd`**, not `gc bd`), `runtime` (203, of which `drain-ack` is 185), `hook` (154, essentially all `--claim --json`), `mail` (72), `prime` (31), `convoy` (20, of which only `status` ×9 is worker-facing).

**Inter-agent mail — gc's messages ARE beads** (`beadmail.Provider`, `Type="message"`). Camp already has a `mail` bead type (`fold.rs:13`) that already exports to bd's native `message` type (`export.rs:189`). The substrate exists and is already wire-compatible.

## 3. Goal, and what is explicitly not the goal

**Goal:** `camp import add https://github.com/gastownhall/gascity-packs/tree/main/gastown --name gastown` fetches a real Gas City pack, and its agents, formulas, and orders run in camp **without editing the pack**.

**Not the goal:** being Gas City. Camp stays single-binary, SQLite-ledgered, zero-idle-cost, daily-driver scale. Where fidelity and camp's laws conflict, camp **refuses loudly and names what it refused** — it never silently approximates.

## 4. Decisions

### 4.1 The permissiveness rule (and why "implement or refuse" is wrong as stated)

Gas City parses formulas with plain `toml.Unmarshal` — **no unknown-field check** (`parser.go:233`). Its own spec says so: *"Unknown top-level keys are silently ignored."* The corpus is full of keys the engine never implemented:

| key | formulas using it | in gc's `Formula` struct? |
|---|---|---|
| `version` | 93 | **no** |
| `target_required` | 64 | **no** (0 hits repo-wide) |
| `internal` | 40 | **no** |
| top-level `mode` | 7 | **no** |
| top-level `single_lane` | 6 | **no** |
| `sling_container_mode` | 1 | **no** |

Same for agents: **`fallback` is set by 72 of 80 agents and is not a field in gc's `Agent` struct.**

**93 of 100 formulas name at least one dead key** — including Gas City's own formulas, which Gas City runs correctly. A consumer that refuses unknown keys is *stricter than the reference implementation* and rejects packs that work.

**The rule, therefore:**

1. A key with **semantics camp does not implement** → **refuse the formula, naming the key.**
2. A key with **no semantics in Gas City** → **ignore it, and warn once**, so a future gc that gives it meaning surfaces as divergence rather than silent drift.
3. A key that is a **pure annotation** (`metadata` at formula level, `notes`, `catalog`) → ignore silently.

**Two traps this creates, and both must be honoured:**

- **`mode` and `single_lane` are position-overloaded.** Top-level `mode = "report"` is dead; **`[steps.check.check].mode = "exec"` is load-bearing** (49 uses). Top-level `single_lane` is dead; **`[steps.drain.item].single_lane` is required for shared drains**. Camp must key off *nesting*, never name.
- **`target_required` looks semantic and is not.** Gas City derives "needs a target" *structurally* (a `{{convoy_id}}` reference or a `drain` step). Reading the key would diverge from gc wherever the two disagree.

### 4.2 One agent format: the Gas City directory. The Claude Code `.md` file is retired.

Two formats for one concept is two parsers, two test suites, and two ways to be wrong. Camp adopts gc's layout as its **native** agent format:

```
agents/<name>/
  agent.toml            optional — gc treats it as optional (agent_discovery.go:47)
  prompt.template.md    canonical; or prompt.md.tmpl; or prompt.md (plain markdown)
  namepool.txt          optional
```

Identity is the **directory name**; a `name` inside `agent.toml` is parsed then overwritten (`agent_discovery.go:51`).

**gc silently ignores unknown `agent.toml` keys** — it decodes with `toml.Decode` and *discards the metadata* (`agent_discovery.go:48`); the fatal-unknown-key check guards only `pack.toml` (`pack.go:1268`). So camp's own keys ride along harmlessly:

```toml
description = "…"
scope = "rig"
option_defaults = { model = "sonnet", permission_mode = "acceptEdits" }   # gc-native
isolation = "worktree"        # camp-only; gc ignores
stall_after = "10m"           # camp-only; gc ignores
tools = ["Read", "Edit", "Bash"]   # camp-only; gc ignores
```

**Consequences, all good:**
- **#85 disappears by construction.** If camp's native format *is* gc's, `camp export` copies `agents/` verbatim and the result is a valid gc pack. No translation ⇒ no translation bug.
- One parser, one format, one test suite.
- **Cost:** camp packs stop being usable in bare Claude Code. Master spec §11's *"a role is a Claude Code agent file, so packs are useful in bare Claude Code too"* is **retired** — see §9. `packs/starter/` converts; every agent-`.md` test fixture changes.

### 4.3 Camp renders Go templates (a defined subset)

67 of 80 real prompts require it. Camp implements: `{{.Var}}` substitution, `{{template "name"}}` fragment includes with gc's precedence (pack → source-pack → city/camp → per-agent), `if`/`range`, and the function set `cmd` / `basename` / `session` / `templateFirst`. `{{cmd}}` renders **`camp`**.

Variables: `.Session .Agent .AgentBase .Rig .RigRoot .WorkDir .ConfigDir` (+ `.CampRoot`, `.CampName` as camp's spelling of `.CityRoot`/`.CityName`). `.ConfigDir` = **the pack root**.

**Camp does NOT copy gc's fail-soft behaviour.** gc keeps the raw command string when a template fails to parse (`pool.go:227-237`, "graceful fallback"). That is a silent-corruption path. **Camp fails fast**, naming the template and the error. A prompt containing an unsupported construct is a **refused pack**, not a mangled prompt.

### 4.4 Camp serves the gc worker contract

Pack prompts are 140-line bash blocks with inline `python3` JSON parsers. **Rewriting them at import is not realistic**, and hand-editing forks every pack. So camp *serves* the vocabulary.

`{{cmd}}` abstracts the binary name, so these become camp verbs:

| verb | refs | contract |
|---|---|---|
| `camp hook --claim --json` | 154 | discovery **+ atomic claim**. JSON: `{schema_version, ok, command, action:"work"\|"drain", reason, bead_id, assignee, route, …}`. Exit 0 on work; **exit 1 on drain**, unless `--drain-ack` → 0. Route match on `gc.routed_to` / `gc.run_target`. |
| `camp runtime drain-ack` | 185 | the session-exit handshake. **A gc worker may not legally exit without it.** |
| `camp mail send\|inbox\|read\|reply` | 72 | §4.6 |
| `camp prime` | 31 | render the agent's prompt template to stdout |
| `camp convoy status <id> --json` | 9 | worker-facing read |

**Workers call bare `bd`, not `{{cmd}} bd`** (~155 refs of `bd update` / `bd close` / `bd show`). The binary name is *not* abstracted there. So camp ships a **`bd`-compatible shim binary** on the worker's PATH, speaking bd's CLI against camp's SQLite ledger. Camp's outcome vocabulary already matches gc's (`pass`/`fail`; `shipped`/`no-op`/`blocked`/`abandoned` — `vocab.rs:47,57`), which is not luck: camp pinned it to gc's on purpose.

`camp claim` today takes an explicit bead id and flips status. `hook --claim` **discovers and claims atomically**, stamps `gc.work_branch`, writes session pointers, and pre-assigns the continuation group. The gap is not the claim — it is everything before it.

### 4.5 Lifecycle: camp keeps its model, and serves gc's contract on top

Camp dispatches one worker per bead and kills it when the bead closes. gc workers **loop** (`while true`: claim → work → close → drain-ack) inside a long-lived tmux session, and *ask* to exit.

**Camp does not adopt pools, tmux, or long-lived sessions.** `min_active_sessions > 0` is a standing process with no bead — a direct violation of invariant 2 ("cost proportional to job"), and it is used by **2 of 80** real agents. Camp:

- serves `hook --claim` and `runtime drain-ack` so a gc worker's loop **terminates correctly** against camp;
- **refuses** `pre_start` (2/80), `work_dir` (8/80), `wake_mode`/`idle_timeout` (7/80), and `min/max_active_sessions` (2/80) — itemized at import, never silently ignored;
- gains **restart-survivable sessions** the cheap way: persist the agent's session id and re-spawn `claude --resume <id>` on campd restart (~50 LoC; the one idea worth taking from herdr).

**herdr is rejected** as a dependency or substrate: it has **zero** inter-agent messaging; it infers agent state by **regex-matching the rendered TUI** (camp already holds a typed `stream-json` channel — adopting it is a fidelity *downgrade*); and it is poll-based top to bottom — 300ms unconditional per-pane wakeups, and even `events.wait`, the "blocking" API, is a 100ms sleep loop over a pull-only ring buffer with no fd or condvar. That is the precise inverse of invariant 1.

### 4.6 Inter-agent mail, built on camp's ledger

An orchestration tool whose agents cannot talk is scheduling, not orchestrating. gc's model, adopted:

- **Messages are beads.** Camp already has the `mail` bead type and already exports it as bd's `message`.
- **Data model:** `{id, from, to, subject, body, created_at, read, thread_id, reply_to, priority, cc[], rig}`.
- **Addressing:** to a session alias or a human; display names resolve to stable session ids via metadata (`mail.from_session_id` / `mail.to_session_id`). `--all` broadcasts to live sessions.
- **Delivery is polled, plus two push assists:** `camp mail check --inject` emits a `<system-reminder>` for hook injection, and `mail send --notify` nudges the recipient over campd's existing nudge path. **No polling loop in campd** — invariant 1 holds because delivery rides the worker's own turn boundary, not a timer.
- Verbs: `send`, `inbox`, `read`, `peek`, `reply`, `mark-read`, `archive`, `thread`, `count`, `check`.

### 4.7 Import machinery

As designed in the superseded rev-2 spec, retained wholesale: `[imports.<name>]` in `camp.toml`; a tracked `packs.lock` (`schema = 1`; keyed by the **verbatim source string**, `{version, commit, fetched}` — gc's shape); materialization into `.camp/imports/<name>/` (gitignored); gc's source grammar verbatim (local paths; `https://github.com/{owner}/{repo}/tree/{ref}[/{path}]`; and `<repo>//<subpath>#<ref>`, which is gc's own escape hatch for slash-branches, non-GitHub hosts, and the `file://` repos camp's tests need); `camp import add | install | list | remove | upgrade | check`.

`pack.toml` is **required**, with `[pack].name` + `[pack].schema` (≤2) — gc's rule exactly (`pack.go:2372-2391`). `version` is **not** required (gastown ships without it; gc's generated JSON schema wrongly says otherwise).

**Symlinks are dereferenced on materialization.** `packs/starter/formulas/guarded-change.toml` is a relative symlink into the gc-validated corpus, whose target lives *outside* the pack subpath; materializing the subpath alone would leave a dangling link and break formula layering on the very pack we ship.

### 4.8 Security

**Fetch** — camp's first production git subprocess. gc's untrusted-remote hardening is ported verbatim (`internal/git/git.go:385-395`): `http.followRedirects=false`, `protocol.allow=never` plus an explicit `https/http/ssh/git/file` allowlist (this is what blocks the `ext::` transport — arbitrary command execution), `core.hooksPath=/dev/null`, `core.fsmonitor=false`, `core.untrackedCache=false`, and a sanitized `GIT_*` environment. One helper owns these flags; a test asserts the argv byte-for-byte.

The threat model is **not** "the operator typed the URL": `camp.toml` and `packs.lock` are **tracked**, so a source URL arrives via `git pull`, a PR branch, or CI.

**Execute — `trust_exec`, default deny.** Everything a pack can execute is inert until the operator opts in *per import*:

```toml
[imports.gastown]
trust_exec = true      # nothing pack-supplied runs without this
```

This gates: a formula's `check.path` script, `pre_start` hooks, `exec` orders, and `condition` shell checks. `camp import add` **inventories the executable content** and prints the command to enable it. Note this closes a hole that was about to open silently: today `check.path` scripts are operator-authored, and the moment pack formulas load they become pack-authored.

**The money invariant.** An order fires a formula; a formula dispatches workers; workers cost real money.

> **Nothing an import brings may fire until the operator names it in `[orders] enabled`.**

Imported orders load, validate, and appear in `camp order ls` as **disabled**, with their source. `camp order enable <import>.<name>` arms them. Without this, `camp import add <url>` could arm a cron from a pack you just downloaded.

## 5. What camp refuses, loudly

Never silently ignored; always itemized at import, naming the pack, the file, and the construct:

| refused | corpus usage |
|---|---|
| `drain` steps (until phase 3) | 13 / 100 formulas |
| `scope-check` / `gc.scope_*` | 1 / 100 |
| `phase = "vapor"` (v1 materialization) | 2 / 100 |
| `pour`, `advice`, `pointcuts`, `compose`, `loop`, `gate`, `tally` | 0 / 100 |
| agent `pre_start`, `work_dir`, `wake_mode`, `idle_timeout`, `min/max_active_sessions` | 2–8 / 80 |
| `exec` orders, `condition` triggers | — |
| prompts using template constructs camp does not implement | — |

## 6. Phases

1. **Import machinery + pack loader.** Fetch, lock, install, hardening, `trust_exec`. `pack.toml`, agent directories, flat `formulas/`, flat `orders/`. Fixes #80. Retires the `.md` agent format; fixes #85 by construction.
2. **Formula compatibility to 87/100.** The permissiveness rule; `vars` + `condition`; `extends` (child steps replace parent steps **by id, in place**, preserving position — no field-level merge); `description_file` (contents replace the description; `../assets/…` resolves *through the formula layers*; >4096 bytes ⇒ a pointer prompt); step `metadata` (incl. `gc.run_target` routing); `expand`/`template`/`expand_vars`; `children` flattened (v2 creates **no** parent-child edges).
3. **The worker contract.** `hook --claim --json`, the `bd` shim, `runtime drain-ack`. A gc worker closes a gc bead end-to-end.
4. **Mail + `prime`.** Multi-agent packs work.
5. **`drain` → 100/100.** The runtime fan-out, incl. `member_access = "exclusive"` — a per-member reservation that **fails if another drain owns the member**. Ignoring it means two drains concurrently mutate one bead: silent data corruption.

## 7. The compatibility gate

Today's CI proves **camp ⊆ gc** (camp's corpus compiles under the real gc compiler). That is **the wrong direction** for this work and proves nothing about running gc packs.

A new gate, in the same shape: **vendor the real `gascity-packs` corpus at a pinned ref, and assert camp loads and compiles the formulas and agents it claims to support** — and *refuses, by name*, the ones it does not. The claimed number (87/100) becomes a test, not a boast. A regression that silently drops to 60 must fail CI.

Both gates run. Without the new one, "compatible" is a claim, not a fact.

## 8. Testing

- **No network.** Git-backed imports run against local `file://` repos built in a temp dir — the real clone/lock/materialize path.
- **No API spend.** Workers are `#!/bin/sh` fakes. Never a real `claude`.
- **The money invariant gets a test that can fail:** an imported pack with a due cron order fires **nothing** until `[orders] enabled` names it.
- **`trust_exec` gets one too:** a pack whose formula carries a `check.path` executes nothing until trusted.
- The git hardening argv is asserted byte-for-byte. A silently dropped flag is a silently removed fence.
- Every new test must die against a mutation of the code it guards.

## 9. Spec §11 must change, and it is the spec owner's call

Master spec §11 says:

> *"**Zero invented formats.** A role is a Claude Code agent file, so packs are useful in bare Claude Code too."*
> *"The camp plugin … ships **no agent definitions**. … if the machinery mentions a role, it is a bug."*

Decision 4.2 **retires the first sentence.** A gc agent directory is not usable in bare Claude Code. This is a deliberate trade: Gas City compatibility is worth more than bare-Claude-Code portability, because packs+formulas *are* the process.

The second sentence **survives, narrowed**: camp's binary still carries no role content — only a default pack *source URL*, which names a pack, not a role. Worth noting: **Gas City's own machinery ships one agent** (`core/control-dispatcher`, `prompt_mode = "none"` — an infrastructure worker, not an LLM role), so "machinery ships zero roles" is defensible only under that narrower reading, in gc as in camp.

AGENTS.md forbids re-litigating the settled decision record without a spec PR. This section **is** that PR.

## 10. Blast radius

`pack.rs` (agent parser, layering, collisions) · `orders/mod.rs` (`formula_path` → layered `resolve_formula`) · `orders/parse.rs` (gc order format, the enabled gate) · `formula/{parse,validate,ast,runtime}.rs` (the tiered key set) · `dispatch.rs` (`GraphRuntime` control-kind mapping; spawn argv from the new `AgentDef`) · `config.rs` (`packs` → `[imports.*]`, `[orders] enabled`; `deny_unknown_fields` makes the removal a **hard parse error**, including on campd's hot-reload path — so config load must detect the old `packs` key and name the rewrite) · `gitignore.rs` (`imports`) · `export.rs` (becomes a verbatim copy) · `packs/starter/` (agents → directories, gains `pack.toml`) · `contrib/docker/` · and every test fixture that writes an agent `.md`.

The inverted test is named: `resolve_agent_layers_packs_last_wins_with_local_agents_highest` (`pack.rs:377-422`) — cross-import collisions become a hard error, because `[imports.*]` are TOML *tables* and agent resolution must not depend on table iteration order.

## 11. Out of scope (#84)

The transitive import graph, `[[exports]]`/namespaces, semver constraint solving, the pack registry, a shared machine-local cache, credentials for private pack repos, `why`/`--tree`/`prune`/`status`/`migrate`, gc's `overlay/` and `template-fragments/` *mutation* semantics beyond prompt fragments, `commands/`, and `doctor/` checks.
